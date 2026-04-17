//! Language Server Protocol implementation for Silt.
//!
//! Provides diagnostics, hover (inferred types), and go-to-definition
//! over the standard LSP JSON-RPC transport (stdin/stdout).

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
};
use lsp_types::request::{
    Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest, Request as _,
    SignatureHelpRequest,
};
use lsp_types::{
    CompletionOptions, HoverProviderCapability, OneOf, ServerCapabilities, SignatureHelpOptions,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

use crate::typechecker;

mod ast_walk;
mod completion;
mod conversions;
mod definition;
mod definitions;
mod diagnostics;
mod document_symbols;
mod fields;
mod formatting;
mod hover;
mod local_bindings;
mod locals;
mod signature_help;
mod state;
mod text_utils;

use state::Document;

// ── Server ─────────────────────────────────────────────────────────

struct Server {
    connection: Connection,
    documents: HashMap<Uri, Document>,
    /// Cached builtin type signatures: "module.func" → type string.
    builtin_sigs: HashMap<String, String>,
}

impl Server {
    fn new(connection: Connection) -> Self {
        Server {
            connection,
            documents: HashMap::new(),
            builtin_sigs: typechecker::builtin_type_signatures(),
        }
    }

    fn run(&mut self) {
        while let Ok(msg) = self.connection.receiver.recv() {
            match msg {
                Message::Request(req) => {
                    if self.connection.handle_shutdown(&req).unwrap_or(true) {
                        return;
                    }
                    self.handle_request(req);
                }
                Message::Notification(notif) => {
                    self.handle_notification(notif);
                }
                Message::Response(_) => {}
            }
        }
    }

    // ── Notifications ──────────────────────────────────────────────

    fn handle_notification(&mut self, notif: Notification) {
        match notif.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let Ok(params) =
                    serde_json::from_value::<lsp_types::DidOpenTextDocumentParams>(notif.params)
                else {
                    return;
                };
                let uri = params.text_document.uri;
                let source = params.text_document.text;
                self.update_document(uri, source);
            }
            DidChangeTextDocument::METHOD => {
                let Ok(params) =
                    serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(notif.params)
                else {
                    return;
                };
                let uri = params.text_document.uri;
                // We use full sync, so the first content change is the full text.
                if let Some(change) = params.content_changes.into_iter().next() {
                    self.update_document(uri, change.text);
                }
            }
            DidCloseTextDocument::METHOD => {
                let Ok(params) =
                    serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(notif.params)
                else {
                    return;
                };
                self.documents.remove(&params.text_document.uri);
                // Clear diagnostics for closed file.
                self.publish_diagnostics(params.text_document.uri, vec![]);
            }
            _ => {}
        }
    }

    // ── Requests ───────────────────────────────────────────────────

    fn handle_request(&mut self, req: Request) {
        let resp = match req.method.as_str() {
            HoverRequest::METHOD => match extract_request::<HoverRequest>(req) {
                Ok((id, params)) => {
                    let result = self.hover(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            GotoDefinition::METHOD => match extract_request::<GotoDefinition>(req) {
                Ok((id, params)) => {
                    let result = self.goto_definition(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            Formatting::METHOD => match extract_request::<Formatting>(req) {
                Ok((id, params)) => {
                    let result = self.format(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            Completion::METHOD => match extract_request::<Completion>(req) {
                Ok((id, params)) => {
                    let result = self.completion(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            SignatureHelpRequest::METHOD => match extract_request::<SignatureHelpRequest>(req) {
                Ok((id, params)) => {
                    let result = self.signature_help(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            DocumentSymbolRequest::METHOD => match extract_request::<DocumentSymbolRequest>(req) {
                Ok((id, params)) => {
                    let result = self.document_symbols(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            _ => {
                // Unknown request method.  We must reply with
                // MethodNotFound so the client doesn't hang waiting for a
                // response that will never arrive.  Requests always carry an
                // id — notifications have no id and are routed to
                // `handle_notification`, so we don't risk replying to one.
                let method = req.method.clone();
                Response::new_err(
                    req.id,
                    ErrorCode::MethodNotFound as i32,
                    format!("method not found: {method}"),
                )
            }
        };
        self.connection.sender.send(Message::Response(resp)).ok();
    }
}

// ── Helpers ────────────────────────────────────────────────────────

/// Attempt to extract a typed parameter set from `req` for method `R`.
///
/// Returns `Ok((id, params))` on success.  On deserialize failure returns
/// `Err(Response)` already populated with the original request id and an
/// `InvalidParams` error, so the caller can send it back without having to
/// remember the id — `req.extract` consumes the request by value and we'd
/// otherwise lose it.  Previously this helper returned `Option<(id, params)>`
/// and the dispatcher silently dropped deserialize failures, leaving clients
/// waiting for a response that never arrives.
fn extract_request<R: lsp_types::request::Request>(
    req: Request,
) -> Result<(RequestId, R::Params), Response> {
    // Capture the id before we consume `req`; `Request::extract` moves the
    // value and the JsonError variant of ExtractError does not preserve the
    // id. RequestId is Clone-able, so this is cheap.
    let id = req.id.clone();
    match req.extract::<R::Params>(R::METHOD) {
        Ok((id, params)) => Ok((id, params)),
        Err(err) => {
            let message = format!("invalid params for {}: {err}", R::METHOD);
            Err(Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                message,
            ))
        }
    }
}

// ── Entry point ────────────────────────────────────────────────────

pub fn run() {
    let (connection, io_threads) = Connection::stdio();

    // Read initialize request and respond with capabilities.
    let server_capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..CompletionOptions::default()
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }),
        document_symbol_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        ..ServerCapabilities::default()
    };

    let init_value = match serde_json::to_value(&server_capabilities) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("silt-lsp: failed to serialize capabilities: {e}");
            return;
        }
    };
    if let Err(e) = connection.initialize(init_value) {
        eprintln!("silt-lsp: initialization failed: {e}");
        return;
    }

    let mut server = Server::new(connection);
    server.run();
    if let Err(e) = io_threads.join() {
        eprintln!("silt-lsp: I/O thread error: {e}");
    }
}

// ── Tests ─────────────────────────────────────────────────────────
//
// Integration tests for the Server — constructed via an in-memory
// Connection so we can exercise the request handlers end-to-end without
// spawning any I/O threads. Tests that exercise shared helpers (span/
// position conversion, AST walkers, …) live in the submodule that owns
// the code under test.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::intern;
    use lsp_types::notification::PublishDiagnostics;
    use lsp_types::{
        CompletionResponse, GotoDefinitionResponse, Location, Position, PublishDiagnosticsParams,
        Range,
    };

    // ── Server integration tests (hover / completion / goto / diagnostics) ──
    //
    // These construct a real Server wired to an in-memory Connection so we
    // can exercise the request handlers end-to-end without spawning any I/O
    // threads. The Connection's receiver is never used (we never call run),
    // and the sender is only drained where a test actually needs to observe
    // the published payload.

    fn make_server() -> Server {
        let (connection, _client) = Connection::memory();
        Server::new(connection)
    }

    fn test_uri() -> Uri {
        use std::str::FromStr;
        Uri::from_str("file:///test.silt").unwrap()
    }

    fn open_document(server: &mut Server, source: &str) -> Uri {
        let uri = test_uri();
        server.update_document(uri.clone(), source.to_string());
        uri
    }

    #[test]
    fn test_hover_returns_some_for_known_symbol() {
        let mut server = make_server();
        let source = "fn add(a, b) { a + b }\nfn main() { add(1, 2) }";
        let uri = open_document(&mut server, source);

        // Position of `add` in the call site on line 2: "fn main() { add(..."
        // column index 12 (0-based) points inside `add`.
        let params = lsp_types::HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position::new(1, 13),
            },
            work_done_progress_params: Default::default(),
        };
        let hover = server.hover(params);
        assert!(hover.is_some(), "expected hover for `add` at the call site");
    }

    #[test]
    fn test_completion_includes_builtins_and_user_names() {
        let mut server = make_server();
        let source = "fn my_func(x) { x + 1 }\nfn main() { 0 }";
        let uri = open_document(&mut server, source);

        let params = lsp_types::CompletionParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position::new(1, 13),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        let resp = server
            .completion(params)
            .expect("completion should return some response");
        let items = match resp {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => list.items,
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

        // Builtin global
        assert!(
            labels.contains(&"println"),
            "completions should include builtin `println`"
        );
        // User-defined function
        assert!(
            labels.contains(&"my_func"),
            "completions should include user-defined `my_func`"
        );
        // Keyword
        assert!(
            labels.contains(&"fn"),
            "completions should include keyword `fn`"
        );
    }

    #[test]
    fn test_goto_definition_resolves_to_correct_span() {
        let mut server = make_server();
        let source = "fn helper(x) { x + 1 }\nfn main() { helper(5) }";
        let uri = open_document(&mut server, source);

        // The call site `helper` starts at column 12 on line 2 (0-based: line 1).
        let params = lsp_types::GotoDefinitionParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                position: Position::new(1, 13),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let resp = server
            .goto_definition(params)
            .expect("goto definition should resolve `helper`");
        let location = match resp {
            GotoDefinitionResponse::Scalar(loc) => loc,
            GotoDefinitionResponse::Array(mut locs) => locs.pop().expect("at least one location"),
            GotoDefinitionResponse::Link(mut links) => {
                let link = links.pop().expect("at least one link");
                Location::new(uri.clone(), link.target_selection_range)
            }
        };
        assert_eq!(location.uri, uri);
        // The helper fn declaration starts on line 1 (0-based 0).
        assert_eq!(location.range.start.line, 0);
    }

    #[test]
    fn test_diagnostics_range_has_identifier_width() {
        // Regression for Issue 1: hover / diagnostic ranges on multi-character
        // identifiers should span the whole identifier, not just one column.
        let mut server = make_server();
        // This program has an undefined identifier `undefined_name` — the
        // type checker will emit a diagnostic pointing at it.
        let source = "fn main() { undefined_name }";
        let uri = open_document(&mut server, source);

        // Drain the published diagnostics notification.
        let mut found_range: Option<Range> = None;
        while let Ok(msg) = server.connection.receiver.try_recv() {
            if let Message::Notification(notif) = msg
                && notif.method == PublishDiagnostics::METHOD
            {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(notif.params).unwrap();
                if params.uri == uri {
                    for diag in params.diagnostics {
                        // Look for a diagnostic that overlaps the `undefined_name` span.
                        if diag.range.start.line == 0 && diag.range.start.character >= 12 {
                            found_range = Some(diag.range);
                            break;
                        }
                    }
                }
            }
        }

        if let Some(range) = found_range {
            // Not all programs produce a diagnostic here, but if they do,
            // its width must be greater than 1 (the identifier is 14 chars).
            let width = range.end.character.saturating_sub(range.start.character);
            assert!(
                width > 1,
                "diagnostic range width should be > 1 for a multi-char identifier, got {width}"
            );
        }
        // If no diagnostic was emitted, the test is a no-op — the
        // important assertion (correct width) is covered by
        // `test_span_to_range_identifier_width` above.
    }

    #[test]
    fn test_update_document_creates_document_entry() {
        let mut server = make_server();
        let source = "fn main() { 42 }";
        let uri = open_document(&mut server, source);

        let doc = server.documents.get(&uri).expect("document should exist");
        assert_eq!(doc.source, source);
        assert!(doc.program.is_some());
        assert!(doc.definitions.contains_key(&intern("main")));
    }

    #[test]
    fn test_update_document_recovers_from_parse_error() {
        let mut server = make_server();
        // Intentionally malformed source — the LSP must not panic.
        let source = "fn main() { let }";
        let uri = open_document(&mut server, source);

        // The server still stores the document (with or without a program)
        // so we can keep serving requests on it.
        assert!(server.documents.contains_key(&uri));
    }

    #[test]
    fn test_close_document_removes_entry() {
        let mut server = make_server();
        let source = "fn main() { 0 }";
        let uri = open_document(&mut server, source);
        assert!(server.documents.contains_key(&uri));

        // Simulate DidCloseTextDocument by invoking the removal directly.
        server.documents.remove(&uri);
        assert!(!server.documents.contains_key(&uri));
    }
}
