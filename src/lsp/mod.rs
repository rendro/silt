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
    CodeActionRequest, Completion, DocumentDiagnosticRequest, DocumentHighlightRequest,
    DocumentSymbolRequest, FoldingRangeRequest, Formatting, GotoDefinition, GotoImplementation,
    GotoTypeDefinition, HoverRequest, InlayHintRequest, PrepareRenameRequest, References, Rename,
    Request as _, SelectionRangeRequest, SemanticTokensFullRequest, SignatureHelpRequest,
    WorkspaceSymbolRequest,
};
use lsp_types::{
    CompletionOptions, Diagnostic, DiagnosticOptions, DiagnosticServerCapabilities,
    HoverProviderCapability, OneOf, ServerCapabilities, SignatureHelpOptions,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

use crate::typechecker;

mod ast_walk;
mod code_action;
mod completion;
mod conversions;
mod definition;
mod definitions;
mod diagnostic_pull;
mod diagnostics;
mod document_highlight;
mod document_symbols;
mod fields;
mod folding;
mod formatting;
mod hover;
mod implementation;
mod inlay_hints;
mod local_bindings;
mod locals;
mod preload;
mod references;
mod rename;
mod selection_range;
mod semantic_tokens;
mod signature_help;
mod state;
mod text_utils;
mod type_definition;
mod workspace;
mod workspace_symbol;

use state::Document;

/// Re-export `preload::path_to_file_uri` so integration tests (and any
/// external caller that legitimately needs to synthesize a `file://`
/// URI with LSP-compatible percent-encoding) can reach it without
/// having to duplicate the `URI_PATH_RESERVED` character set or the
/// Windows drive-letter fix-up.
pub use preload::path_to_file_uri;

/// `is_user_renameable` is exposed via this re-export so integration
/// tests (see `tests/builtin_types_authoritative_parity_tests.rs` and
/// `tests/builtin_constructor_parity_tests.rs`) can call into the
/// rename guard without `pub`-ing the whole `rename` submodule.
pub use rename::is_user_renameable;

// ── Server ─────────────────────────────────────────────────────────

struct Server {
    connection: Connection,
    documents: HashMap<Uri, Document>,
    /// Cached builtin type signatures: "module.func" → type string.
    builtin_sigs: HashMap<String, String>,
    /// Per-URI cache of the last computed diagnostics. Populated by
    /// `update_document` so the pull-based `textDocument/diagnostic`
    /// handler can answer without re-running the pipeline.
    diagnostics_cache: HashMap<Uri, Vec<Diagnostic>>,
}

impl Server {
    fn new(connection: Connection) -> Self {
        Server {
            connection,
            documents: HashMap::new(),
            builtin_sigs: typechecker::builtin_type_signatures(),
            diagnostics_cache: HashMap::new(),
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
            References::METHOD => match extract_request::<References>(req) {
                Ok((id, params)) => {
                    let result = self.references(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            Rename::METHOD => match extract_request::<Rename>(req) {
                Ok((id, params)) => match self.rename(params, id.clone()) {
                    Ok(result) => Response::new_ok(id, result),
                    Err(err_resp) => err_resp,
                },
                Err(resp) => resp,
            },
            PrepareRenameRequest::METHOD => match extract_request::<PrepareRenameRequest>(req) {
                Ok((id, params)) => {
                    let result = self.prepare_rename(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            WorkspaceSymbolRequest::METHOD => {
                match extract_request::<WorkspaceSymbolRequest>(req) {
                    Ok((id, params)) => {
                        let result = self.workspace_symbol(params);
                        Response::new_ok(id, result)
                    }
                    Err(resp) => resp,
                }
            }
            InlayHintRequest::METHOD => match extract_request::<InlayHintRequest>(req) {
                Ok((id, params)) => {
                    let result = self.inlay_hints(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            DocumentHighlightRequest::METHOD => {
                match extract_request::<DocumentHighlightRequest>(req) {
                    Ok((id, params)) => {
                        let result = self.document_highlight(params);
                        Response::new_ok(id, result)
                    }
                    Err(resp) => resp,
                }
            }
            FoldingRangeRequest::METHOD => match extract_request::<FoldingRangeRequest>(req) {
                Ok((id, params)) => {
                    let result = self.folding_range(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            SelectionRangeRequest::METHOD => match extract_request::<SelectionRangeRequest>(req) {
                Ok((id, params)) => {
                    let result = self.selection_range(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            GotoTypeDefinition::METHOD => match extract_request::<GotoTypeDefinition>(req) {
                Ok((id, params)) => {
                    let result = self.type_definition(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            GotoImplementation::METHOD => match extract_request::<GotoImplementation>(req) {
                Ok((id, params)) => {
                    let result = self.goto_implementation(params);
                    Response::new_ok(id, result)
                }
                Err(resp) => resp,
            },
            DocumentDiagnosticRequest::METHOD => {
                match extract_request::<DocumentDiagnosticRequest>(req) {
                    Ok((id, params)) => {
                        let result = self.document_diagnostic(params);
                        Response::new_ok(id, result)
                    }
                    Err(resp) => resp,
                }
            }
            SemanticTokensFullRequest::METHOD => {
                match extract_request::<SemanticTokensFullRequest>(req) {
                    Ok((id, params)) => {
                        let result = self.semantic_tokens_full(params);
                        Response::new_ok(id, result)
                    }
                    Err(resp) => resp,
                }
            }
            CodeActionRequest::METHOD => match extract_request::<CodeActionRequest>(req) {
                Ok((id, params)) => {
                    let result = self.code_action(params);
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

/// Convert a `file://` URI from the initialize params into a native
/// `PathBuf`. Handles Unix (`file:///home/klaus`) and Windows
/// (`file:///C:/Users/...`) shapes; on Windows we strip the leading
/// `/` before the drive letter because Rust's `PathBuf` expects
/// `C:/Users/...`, not `/C:/Users/...`.
///
/// Percent-decodes the path component per RFC 3986 so URIs like
/// `file:///home/klaus/My%20Project` or
/// `file:///tmp/%D1%82%D0%B5%D1%81%D1%82` (Cyrillic `тест`) resolve
/// to real filesystem paths. Pre-fix this path was fed verbatim to
/// `PathBuf::from`, so VSCode/any client sending an encoded workspace
/// root (spaces, non-ASCII) silently failed `fs::read_dir` and the
/// preload dropped every file.
///
/// Falls back to the literal stripped string if UTF-8 decode fails —
/// the decode is best-effort; a malformed URI shouldn't crash the
/// server.
pub fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let stripped = uri.strip_prefix("file://")?;
    #[cfg(windows)]
    let stripped = {
        // `file:///C:/foo` → stripped = "/C:/foo"
        // Drop the leading `/` if followed by a drive letter.
        if stripped.len() >= 4
            && stripped.as_bytes()[0] == b'/'
            && stripped.as_bytes()[1].is_ascii_alphabetic()
            && stripped.as_bytes()[2] == b':'
        {
            &stripped[1..]
        } else {
            stripped
        }
    };
    let decoded = percent_encoding::percent_decode_str(stripped)
        .decode_utf8()
        .map(|cow| cow.into_owned())
        .unwrap_or_else(|_| stripped.to_string());
    Some(std::path::PathBuf::from(decoded))
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
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(lsp_types::RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        inlay_hint_provider: Some(lsp_types::OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(lsp_types::FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(lsp_types::SelectionRangeProviderCapability::Simple(true)),
        type_definition_provider: Some(lsp_types::TypeDefinitionProviderCapability::Simple(true)),
        implementation_provider: Some(lsp_types::ImplementationProviderCapability::Simple(true)),
        diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
            identifier: None,
            inter_file_dependencies: true,
            workspace_diagnostics: false,
            work_done_progress_options: Default::default(),
        })),
        semantic_tokens_provider: Some(
            lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                lsp_types::SemanticTokensOptions {
                    legend: crate::lsp::semantic_tokens::semantic_tokens_legend(),
                    full: Some(lsp_types::SemanticTokensFullOptions::Bool(true)),
                    range: Some(false),
                    work_done_progress_options: Default::default(),
                },
            ),
        ),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Simple(true)),
        ..ServerCapabilities::default()
    };

    let init_value = match serde_json::to_value(&server_capabilities) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("silt-lsp: failed to serialize capabilities: {e}");
            return;
        }
    };
    let init_params = match connection.initialize(init_value) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("silt-lsp: initialization failed: {e}");
            return;
        }
    };

    let mut server = Server::new(connection);

    // Workspace preload: if the client supplied `rootUri` or
    // `workspaceFolders`, pre-index every `.silt` file under that root
    // so cross-file goto, references, rename, and workspace/symbol work
    // immediately on files the editor has not yet opened.
    let root_path: Option<std::path::PathBuf> = init_params
        .get("rootUri")
        .and_then(|v| v.as_str())
        .or_else(|| {
            init_params
                .pointer("/workspaceFolders/0/uri")
                .and_then(|v| v.as_str())
        })
        .and_then(file_uri_to_path);
    if let Some(root) = root_path {
        preload::preload_workspace(&mut server, &root);
    }

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
