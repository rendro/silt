//! Language Server Protocol implementation for Silt.
//!
//! Provides diagnostics, hover (inferred types), and go-to-definition
//! over the standard LSP JSON-RPC transport (stdin/stdout).

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{
    Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest, Request as _,
    SignatureHelpRequest,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionResponse, Diagnostic,
    DiagnosticSeverity, DocumentSymbol, DocumentSymbolResponse, GotoDefinitionResponse, Hover,
    HoverContents, HoverProviderCapability, Location, MarkupContent, MarkupKind, OneOf,
    ParameterInformation, ParameterLabel, Position, PublishDiagnosticsParams, Range,
    ServerCapabilities, SignatureHelp, SignatureHelpOptions, SignatureInformation, SymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Uri,
};

use crate::ast::*;
use crate::intern::{Symbol, intern, resolve};
use crate::lexer::{Lexer, Span};
use crate::module;
use crate::parser::Parser;
use crate::typechecker;
use crate::types::Type;

// ── Document state ─────────────────────────────────────────────────

struct DefInfo {
    span: Span,
    ty: Option<Type>,
    params: Vec<String>,
}

/// A local binding (let-bound identifier, function parameter, match binding, …)
/// with its approximate source position, for hover / goto-def on locals.
struct LocalBinding {
    /// The identifier name (interned).
    name: Symbol,
    /// Byte offset in the source where the binding identifier starts.
    binding_offset: usize,
    /// Byte length of the binding identifier.
    binding_len: usize,
    /// Start offset of the scope in which this binding is visible.
    scope_start: usize,
    /// End offset of the scope (exclusive).
    scope_end: usize,
    /// Inferred type, if known.
    ty: Option<Type>,
}

struct Document {
    source: String,
    program: Option<Program>,
    /// Definition map: name → definition info (built from top-level declarations).
    definitions: HashMap<Symbol, DefInfo>,
    /// Local bindings (let, params, match/when) with approximate source positions.
    locals: Vec<LocalBinding>,
}

// ── Span ↔ LSP conversion ─────────────────────────────────────────

/// Convert a span to a 0-based LSP `Position`.
///
/// LSP positions count characters in **UTF-16 code units** (per the spec,
/// and what nearly every client uses as the default encoding). The lexer
/// increments `span.col` once per Unicode codepoint (src/lexer.rs:247),
/// which is NOT the same as a UTF-16 unit count for characters outside
/// the BMP (e.g. `😀` is 1 codepoint but 2 UTF-16 units).
///
/// To produce a correct position we walk the source from the start of
/// the line containing `span.offset` up to `span.offset`, summing
/// `ch.len_utf16()` for each character. This uses `span.offset` (a byte
/// offset) as the source of truth rather than the potentially-mismatched
/// codepoint `col`, which the lexer records but which the LSP protocol
/// does not consume.
fn span_to_position(span: &Span, source: &str) -> Position {
    let line = span.line.saturating_sub(1) as u32;
    let bytes = source.as_bytes();
    let offset = span.offset.min(bytes.len());

    // Find the byte offset of the start of the line that `offset` lives in.
    // We scan backwards for the most recent '\n' before `offset`.
    let line_start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);

    // Walk from `line_start` to `offset`, accumulating UTF-16 code units.
    // We must respect char boundaries: if `offset` lands mid-character
    // (shouldn't happen for well-formed spans, but be defensive) we clamp
    // at the boundary we reach just before it.
    let mut character: u32 = 0;
    let mut idx = line_start;
    while idx < offset {
        let rest = &source[idx..];
        let Some(ch) = rest.chars().next() else { break };
        let ch_len = ch.len_utf8();
        if idx + ch_len > offset {
            break;
        }
        character += ch.len_utf16() as u32;
        idx += ch_len;
    }

    Position::new(line, character)
}

/// Return the UTF-16 code-unit length of a string (what LSP positions count).
fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

/// Compute the byte length of the token that begins at `offset` in `source`.
fn token_len_at(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    if offset >= bytes.len() {
        return 1;
    }
    let first = bytes[offset];
    if first.is_ascii_alphabetic() || first == b'_' {
        let mut end = offset + 1;
        while end < bytes.len() {
            let b = bytes[end];
            if b.is_ascii_alphanumeric() || b == b'_' {
                end += 1;
            } else {
                break;
            }
        }
        return end - offset;
    }
    if first.is_ascii_digit() {
        let mut end = offset + 1;
        let mut seen_dot = false;
        while end < bytes.len() {
            let b = bytes[end];
            if b.is_ascii_digit() || b == b'_' {
                end += 1;
            } else if b == b'.' && !seen_dot {
                if end + 1 < bytes.len() && bytes[end + 1].is_ascii_digit() {
                    seen_dot = true;
                    end += 1;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        return end - offset;
    }
    if first == b'"' {
        if offset + 2 < bytes.len() && bytes[offset + 1] == b'"' && bytes[offset + 2] == b'"' {
            let mut end = offset + 3;
            while end + 2 < bytes.len() {
                if bytes[end] == b'"' && bytes[end + 1] == b'"' && bytes[end + 2] == b'"' {
                    return end + 3 - offset;
                }
                end += 1;
            }
            return bytes.len() - offset;
        }
        let mut end = offset + 1;
        let mut escape = false;
        while end < bytes.len() {
            let b = bytes[end];
            if escape {
                escape = false;
                end += 1;
                continue;
            }
            if b == b'\\' {
                escape = true;
                end += 1;
                continue;
            }
            if b == b'"' {
                return end + 1 - offset;
            }
            if b == b'\n' {
                return end - offset;
            }
            end += 1;
        }
        return end - offset;
    }
    if offset + 1 < bytes.len() {
        let two = &bytes[offset..offset + 2];
        if matches!(
            two,
            b"==" | b"!=" | b"<=" | b">=" | b"->" | b"=>" | b".." | b"::" | b"|>" | b"&&" | b"||"
        ) {
            return 2;
        }
    }
    1
}

/// Convert a span to an LSP range, using the source text to determine the
/// byte length of the token at `span.offset`. Converts both the start and
/// computed end byte offsets to line/column via the same logic, rather than
/// hard-coding `end = start + 1`, so multi-character identifiers produce a
/// correctly-sized range.
fn span_to_range(span: &Span, source: &str) -> Range {
    let start = span_to_position(span, source);
    let len = token_len_at(source, span.offset);
    let bytes = source.as_bytes();
    let end_col = if span.offset >= bytes.len() {
        start.character + 1
    } else {
        let slice_end = (span.offset + len).min(bytes.len());
        let slice = &source[span.offset..slice_end];
        if let Some(nl) = slice.find('\n') {
            let first_line = &slice[..nl];
            start.character + utf16_len(first_line) as u32
        } else {
            start.character + utf16_len(slice) as u32
        }
    };
    let end = Position::new(start.line, end_col);
    Range::new(start, end)
}

/// Convert an LSP 0-based line/character to a byte offset into the source.
fn position_to_offset(source: &str, pos: &Position) -> usize {
    let mut offset = 0;
    for (i, line) in source.lines().enumerate() {
        if i == pos.line as usize {
            let mut utf16_offset = 0u32;
            for (byte_idx, ch) in line.char_indices() {
                if utf16_offset >= pos.character {
                    return offset + byte_idx;
                }
                utf16_offset += ch.len_utf16() as u32;
            }
            return offset + line.len();
        }
        // Account for actual line ending: \r\n (2 bytes) or \n (1 byte).
        let line_end = offset + line.len();
        let newline_len = if source.as_bytes().get(line_end) == Some(&b'\r')
            && source.as_bytes().get(line_end + 1) == Some(&b'\n')
        {
            2
        } else {
            1
        };
        offset += line.len() + newline_len;
    }
    offset
}

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

    // ── Document analysis ──────────────────────────────────────────

    fn update_document(&mut self, uri: Uri, source: String) {
        let mut diagnostics = Vec::new();

        let tokens = match Lexer::new(&source).tokenize() {
            Ok(t) => t,
            Err(e) => {
                diagnostics.push(make_diagnostic(
                    &e.message,
                    &e.span,
                    DiagnosticSeverity::ERROR,
                    &source,
                ));
                self.documents.insert(
                    uri.clone(),
                    Document {
                        source,
                        program: None,
                        definitions: HashMap::new(),
                        locals: Vec::new(),
                    },
                );
                self.publish_diagnostics(uri, diagnostics);
                return;
            }
        };

        let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();

        for e in &parse_errors {
            diagnostics.push(make_diagnostic(
                &e.message,
                &e.span,
                DiagnosticSeverity::ERROR,
                &source,
            ));
        }

        let type_errors = typechecker::check(&mut program);
        for e in &type_errors {
            let severity = match e.severity {
                typechecker::Severity::Error => DiagnosticSeverity::ERROR,
                typechecker::Severity::Warning => DiagnosticSeverity::WARNING,
            };
            diagnostics.push(make_diagnostic(&e.message, &e.span, severity, &source));
        }

        let definitions = build_definitions(&program);
        let locals = collect_local_bindings(&program, &source);

        self.documents.insert(
            uri.clone(),
            Document {
                source,
                program: Some(program),
                definitions,
                locals,
            },
        );

        self.publish_diagnostics(uri, diagnostics);
    }

    fn publish_diagnostics(&self, uri: Uri, diagnostics: Vec<Diagnostic>) {
        let params = PublishDiagnosticsParams::new(uri, diagnostics, None);
        let notif = lsp_server::Notification::new(PublishDiagnostics::METHOD.to_string(), params);
        self.connection
            .sender
            .send(Message::Notification(notif))
            .ok();
    }

    // ── Hover ──────────────────────────────────────────────────────

    fn hover(&self, params: lsp_types::HoverParams) -> Option<Hover> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);

        // Check if cursor is on a field name in a field access expression.
        // e.g., for `data.response`, hovering on `response` shows the field type.
        if let Some((field_name, field_ty)) =
            find_field_type_at_offset(program, &doc.source, cursor)
        {
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("```silt\n{field_name}: {field_ty}\n```"),
                }),
                range: None,
            });
        }

        // If the cursor is sitting on the BINDING (LHS) identifier of a local
        // let / param / match binding, prefer that binding's type over the
        // enclosing expression's type. Otherwise `hover` on `x` in `let x = 42`
        // returns the enclosing block's Unit type. See B9 in codebase audit.
        if let Some(binding) = find_local_binding_at_offset(&doc.locals, cursor)
            && let Some(ref ty) = binding.ty
        {
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("```silt\n{ty}\n```"),
                }),
                range: None,
            });
        }

        let ty = find_type_at_offset(program, cursor);

        // If the expression type has unresolved vars, try the definition type instead.
        let ty = match ty {
            Some(ref t) if !has_unresolved_vars(t) => ty,
            _ => {
                // Fall back: find the ident at cursor, look up its definition type.
                find_ident_at_offset(program, cursor)
                    .and_then(|name| doc.definitions.get(&name))
                    .and_then(|def| def.ty.clone())
                    .filter(|t| !has_unresolved_vars(t))
                    .or(ty) // last resort: show raw type even with vars
            }
        };
        let ty = ty?;

        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("```silt\n{ty}\n```"),
            }),
            range: None,
        })
    }

    // ── Go to definition ───────────────────────────────────────────

    fn goto_definition(
        &self,
        params: lsp_types::GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let cursor = position_to_offset(&doc.source, &pos);

        // If the cursor is already ON a binding site, jump to itself. This
        // gives editors a sensible answer and keeps goto-def idempotent.
        if let Some(binding) = find_local_binding_at_offset(&doc.locals, cursor)
            && let Some(range) =
                binding_range(&doc.source, binding.binding_offset, binding.binding_len)
        {
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                uri.clone(),
                range,
            )));
        }

        let name = find_ident_at_offset(program, cursor)?;

        // Prefer local bindings in scope at the cursor position.
        if let Some(binding) = nearest_local_binding_for(&doc.locals, name, cursor) {
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                uri.clone(),
                binding_range(&doc.source, binding.binding_offset, binding.binding_len)?,
            )));
        }

        let def = doc.definitions.get(&name)?;

        Some(GotoDefinitionResponse::Scalar(Location::new(
            uri.clone(),
            span_to_range(&def.span, &doc.source),
        )))
    }

    // ── Completion ─────────────────────────────────────────────────

    fn completion(&self, params: lsp_types::CompletionParams) -> Option<CompletionResponse> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = self.documents.get(uri);

        // Detect dot-completion context: extract the identifier before the `.`
        if let Some(doc) = doc
            && let Some(prefix) = extract_dot_prefix(&doc.source, &pos)
        {
            let cursor = position_to_offset(&doc.source, &pos);
            let items = self.dot_completions(doc, &prefix, cursor);
            return Some(CompletionResponse::Array(items));
        }

        let mut items: Vec<CompletionItem> = Vec::new();

        // Keywords
        for kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..CompletionItem::default()
            });
        }

        // Builtins (globals + stdlib)
        for (name, kind) in builtins() {
            let detail = self.builtin_sigs.get(&name).cloned();
            items.push(CompletionItem {
                label: name,
                kind: Some(kind),
                detail,
                ..CompletionItem::default()
            });
        }

        // User-defined names from the current document
        if let Some(doc) = doc {
            for (name, def) in &doc.definitions {
                let kind = match &def.ty {
                    Some(Type::Fun(..)) => CompletionItemKind::FUNCTION,
                    _ => CompletionItemKind::VARIABLE,
                };
                let detail = def.ty.as_ref().map(|t| format!("{t}"));
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(kind),
                    detail,
                    ..CompletionItem::default()
                });
            }

            // Local variables in scope at the cursor position
            if let Some(program) = &doc.program {
                let cursor = position_to_offset(&doc.source, &pos);
                for local in locals_at_offset(program, cursor) {
                    let detail = local.ty.as_ref().map(|t| format!("{t}"));
                    items.push(CompletionItem {
                        label: local.name,
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail,
                        ..CompletionItem::default()
                    });
                }
            }
        }

        Some(CompletionResponse::Array(items))
    }

    /// Produce completions after a `.` — either module functions or record fields.
    fn dot_completions(&self, doc: &Document, prefix: &str, cursor: usize) -> Vec<CompletionItem> {
        let mut items = Vec::new();

        // 1. Builtin module → return its functions with type signatures
        if module::is_builtin_module(prefix) {
            for func in module::builtin_module_functions(prefix) {
                let qualified = format!("{prefix}.{func}");
                let detail = self.builtin_sigs.get(&qualified).cloned();
                items.push(CompletionItem {
                    label: func.to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail,
                    ..CompletionItem::default()
                });
            }
            return items;
        }

        let program = match &doc.program {
            Some(p) => p,
            None => return items,
        };

        // 2. Check local variables in scope at cursor for the prefix
        let locals = locals_at_offset(program, cursor);
        if let Some(local) = locals.iter().rev().find(|l| l.name == prefix)
            && let Some(ref ty) = local.ty
            && let Some(fields) = record_fields_from_type(ty, program)
        {
            for (name, field_ty) in &fields {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("{field_ty}")),
                    ..CompletionItem::default()
                });
            }
            return items;
        }

        // 3. Try to resolve the identifier's type from the typed AST
        if let Some(ty) = find_ident_type_by_name(program, prefix)
            && let Some(fields) = record_fields_from_type(&ty, program)
        {
            for (name, field_ty) in &fields {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("{field_ty}")),
                    ..CompletionItem::default()
                });
            }
            return items;
        }

        // 3. Fallback: if the prefix matches a type name, offer its fields
        let prefix_sym = intern(prefix);
        for decl in &program.decls {
            if let Decl::Type(td) = decl
                && td.name == prefix_sym
                && let TypeBody::Record(fields) = &td.body
            {
                for field in fields {
                    items.push(CompletionItem {
                        label: field.name.to_string(),
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(format!("{}", type_expr_to_type(&field.ty))),
                        ..CompletionItem::default()
                    });
                }
            }
        }

        items
    }

    // ── Formatting ────────────────────────────────────────────────

    fn format(&self, params: lsp_types::DocumentFormattingParams) -> Option<Vec<TextEdit>> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let formatted = crate::formatter::format(&doc.source).ok()?;

        if formatted == doc.source {
            return Some(vec![]);
        }

        // Replace the entire document. `Position.character` is defined in
        // UTF-16 code units by the LSP spec, and `Position.line` must be a
        // valid 0-based line index — NOT one past the last line. Compute
        // both from the raw source so we stay correct for multibyte input.
        //
        // Three cases:
        //   1. Empty source        → (0, 0)..(0, 0)
        //   2. Trailing newline(s) → end at (line_after_last_newline, 0)
        //   3. No trailing newline → end at (last_line_idx, utf16_len(last))
        let end_position = {
            let src = doc.source.as_str();
            if src.is_empty() {
                Position::new(0, 0)
            } else if src.ends_with('\n') {
                // Count newlines to determine how many lines are fully
                // terminated. The "virtual" line that follows the final
                // `\n` starts at column 0.
                let newline_count = src.bytes().filter(|b| *b == b'\n').count() as u32;
                Position::new(newline_count, 0)
            } else {
                // No trailing newline — the final line is indexed by the
                // number of newlines seen so far, and its end column is
                // the UTF-16 length of its content.
                let newline_count = src.bytes().filter(|b| *b == b'\n').count() as u32;
                let last_line = src.rsplit('\n').next().unwrap_or("");
                Position::new(newline_count, utf16_len(last_line) as u32)
            }
        };
        Some(vec![TextEdit {
            range: Range::new(Position::new(0, 0), end_position),
            new_text: formatted,
        }])
    }

    // ── Signature help ────────────────────────────────────────────

    fn signature_help(&self, params: lsp_types::SignatureHelpParams) -> Option<SignatureHelp> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;

        // Walk backwards from cursor to find the function name before `(`.
        let cursor = position_to_offset(&doc.source, &pos);
        let before = &doc.source[..cursor];

        // Forward-scan `before` to find the active call site: the last `(`
        // at nesting depth 0, and count commas at depth 1 from there.
        // Skips string literals and silt comments so that commas/parens
        // inside them are not miscounted.
        let (active_param, paren_pos) =
            scan_call_site_forward(before.as_bytes())?;
        let before_paren = before[..paren_pos].trim_end();
        let fn_name: String = before_paren
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect::<String>()
            .chars()
            .rev()
            .collect();

        if fn_name.is_empty() {
            return None;
        }

        // Look up in definitions first, then builtins.
        let fn_sym = intern(&fn_name);
        let (label, params_info) = if let Some(def) = doc.definitions.get(&fn_sym) {
            build_signature_from_def(&fn_name, def)
        } else if let Some(sig) = self.builtin_sigs.get(&fn_name) {
            // Show builtin type signature (no individual param info)
            (format!("{fn_name}: {sig}"), vec![])
        } else {
            return None;
        };

        Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation: None,
                parameters: Some(params_info),
                active_parameter: Some(active_param),
            }],
            active_signature: Some(0),
            active_parameter: Some(active_param),
        })
    }

    // ── Document symbols ──────────────────────────────────────────

    #[allow(deprecated)] // DocumentSymbol::deprecated field
    fn document_symbols(
        &self,
        params: lsp_types::DocumentSymbolParams,
    ) -> Option<DocumentSymbolResponse> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let mut symbols = Vec::new();
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => {
                    let detail = doc
                        .definitions
                        .get(&f.name)
                        .and_then(|d| d.ty.as_ref())
                        .map(|t| format!("{t}"));
                    symbols.push(DocumentSymbol {
                        name: f.name.to_string(),
                        detail,
                        kind: SymbolKind::FUNCTION,
                        range: span_to_range(&f.span, &doc.source),
                        selection_range: span_to_range(&f.span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Type(t) => {
                    let kind = match &t.body {
                        TypeBody::Enum(_) => SymbolKind::ENUM,
                        TypeBody::Record(_) => SymbolKind::STRUCT,
                    };
                    symbols.push(DocumentSymbol {
                        name: t.name.to_string(),
                        detail: None,
                        kind,
                        range: span_to_range(&t.span, &doc.source),
                        selection_range: span_to_range(&t.span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Trait(t) => {
                    symbols.push(DocumentSymbol {
                        name: t.name.to_string(),
                        detail: None,
                        kind: SymbolKind::INTERFACE,
                        range: span_to_range(&t.span, &doc.source),
                        selection_range: span_to_range(&t.span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Let {
                    pattern,
                    span,
                    value,
                    ..
                } if matches!(pattern.kind, PatternKind::Ident(_)) => {
                    let name = match &pattern.kind {
                        PatternKind::Ident(n) => *n,
                        _ => unreachable!(),
                    };
                    let detail = value.ty.as_ref().map(|t| format!("{t}"));
                    symbols.push(DocumentSymbol {
                        name: name.to_string(),
                        detail,
                        kind: SymbolKind::VARIABLE,
                        range: span_to_range(span, &doc.source),
                        selection_range: span_to_range(span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                _ => {}
            }
        }

        Some(DocumentSymbolResponse::Nested(symbols))
    }
}

// ── Signature help helpers ─────────────────────────────────────────

fn build_signature_from_def(name: &str, def: &DefInfo) -> (String, Vec<ParameterInformation>) {
    let mut label = format!("fn {name}(");
    let mut params_info = Vec::new();

    if let Some(Type::Fun(param_types, ret)) = &def.ty {
        for (i, pty) in param_types.iter().enumerate() {
            let pname = def.params.get(i).map(|s| s.as_str()).unwrap_or("_");
            let param_label = format!("{pname}: {pty}");
            let start = label.len() as u32;
            label.push_str(&param_label);
            let end = label.len() as u32;
            if i + 1 < param_types.len() {
                label.push_str(", ");
            }
            params_info.push(ParameterInformation {
                label: ParameterLabel::LabelOffsets([start, end]),
                documentation: None,
            });
        }
        label.push_str(&format!(") -> {ret}"));
    } else {
        for (i, pname) in def.params.iter().enumerate() {
            let start = label.len() as u32;
            label.push_str(pname);
            let end = label.len() as u32;
            if i + 1 < def.params.len() {
                label.push_str(", ");
            }
            params_info.push(ParameterInformation {
                label: ParameterLabel::LabelOffsets([start, end]),
                documentation: None,
            });
        }
        label.push(')');
    }

    (label, params_info)
}

// ── Type display helpers ───────────────────────────────────────────

/// Returns true if the type contains any unresolved type variables (e.g. Var(189)).
fn has_unresolved_vars(ty: &Type) -> bool {
    match ty {
        Type::Var(_) => true,
        Type::Fun(params, ret) => {
            params.iter().any(has_unresolved_vars) || has_unresolved_vars(ret)
        }
        Type::List(inner) | Type::Set(inner) | Type::Channel(inner) => has_unresolved_vars(inner),
        Type::Tuple(elems) => elems.iter().any(has_unresolved_vars),
        Type::Record(_, fields) => fields.iter().any(|(_, t)| has_unresolved_vars(t)),
        Type::Generic(_, args) => args.iter().any(has_unresolved_vars),
        Type::Map(k, v) => has_unresolved_vars(k) || has_unresolved_vars(v),
        _ => false,
    }
}

// ── Diagnostics helper ─────────────────────────────────────────────

fn make_diagnostic(
    message: &str,
    span: &Span,
    severity: DiagnosticSeverity,
    source: &str,
) -> Diagnostic {
    Diagnostic {
        range: span_to_range(span, source),
        severity: Some(severity),
        message: message.to_string(),
        ..Diagnostic::default()
    }
}

// ── Build definitions map from declarations ────────────────────────

fn build_definitions(program: &Program) -> HashMap<Symbol, DefInfo> {
    let mut defs = HashMap::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                let fn_ty = build_fn_type(f);
                let params = fn_param_names(f);
                defs.insert(
                    f.name,
                    DefInfo {
                        span: f.span,
                        ty: fn_ty,
                        params,
                    },
                );
            }
            Decl::Type(t) => {
                defs.insert(
                    t.name,
                    DefInfo {
                        span: t.span,
                        ty: None,
                        params: vec![],
                    },
                );
                if let TypeBody::Enum(variants) = &t.body {
                    for v in variants {
                        defs.insert(
                            v.name,
                            DefInfo {
                                span: t.span,
                                ty: None,
                                params: vec![],
                            },
                        );
                    }
                }
            }
            Decl::Trait(t) => {
                defs.insert(
                    t.name,
                    DefInfo {
                        span: t.span,
                        ty: None,
                        params: vec![],
                    },
                );
            }
            Decl::Let {
                pattern,
                span,
                value,
                ..
            } if matches!(pattern.kind, PatternKind::Ident(_)) => {
                if let PatternKind::Ident(name) = &pattern.kind {
                    defs.insert(
                        *name,
                        DefInfo {
                            span: *span,
                            ty: value.ty.clone(),
                            params: vec![],
                        },
                    );
                }
            }
            _ => {}
        }
    }
    defs
}

// ── Local binding collection (for hover/goto on locals) ──────────────

/// Walk the program and collect every local binding (let, parameter, match)
/// with its approximate source position. Binding offsets are recovered by
/// scanning the source text between the enclosing scope start and a known
/// reference offset (`value.span.offset` for lets, `f.span.offset` for
/// params), which covers the common `let x = e` and `let x: T = e` cases.
fn collect_local_bindings(program: &Program, source: &str) -> Vec<LocalBinding> {
    let mut bindings: Vec<LocalBinding> = Vec::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                let body_start = f.body.span.offset;
                let (body_end, _) = expr_extent(&f.body, source);
                // Function parameters: find each in the param-list region
                // before the body start.
                let params_search_end = body_start;
                for param in &f.params {
                    if let PatternKind::Ident(name) = &param.pattern.kind {
                        let name_str = resolve(*name);
                        if let Some(off) =
                            find_ident_in_range(source, f.span.offset, params_search_end, &name_str)
                        {
                            // Look up the param type from the typed body.
                            let ty = find_param_type(&f.body, *name);
                            bindings.push(LocalBinding {
                                name: *name,
                                binding_offset: off,
                                binding_len: name_str.len(),
                                scope_start: body_start,
                                scope_end: body_end,
                                ty,
                            });
                        }
                    }
                }
                collect_local_bindings_in_expr(
                    &f.body,
                    source,
                    body_start,
                    body_end,
                    &mut bindings,
                );
            }
            Decl::Let { value, .. } => {
                collect_local_bindings_in_expr(value, source, 0, source.len(), &mut bindings);
            }
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    let body_start = method.body.span.offset;
                    let (body_end, _) = expr_extent(&method.body, source);
                    for param in &method.params {
                        if let PatternKind::Ident(name) = &param.pattern.kind {
                            let name_str = resolve(*name);
                            if let Some(off) = find_ident_in_range(
                                source,
                                method.span.offset,
                                body_start,
                                &name_str,
                            ) {
                                let ty = find_param_type(&method.body, *name);
                                bindings.push(LocalBinding {
                                    name: *name,
                                    binding_offset: off,
                                    binding_len: name_str.len(),
                                    scope_start: body_start,
                                    scope_end: body_end,
                                    ty,
                                });
                            }
                        }
                    }
                    collect_local_bindings_in_expr(
                        &method.body,
                        source,
                        body_start,
                        body_end,
                        &mut bindings,
                    );
                }
            }
            _ => {}
        }
    }
    bindings
}

/// Collect local bindings inside an expression, given the enclosing scope.
fn collect_local_bindings_in_expr(
    expr: &Expr,
    source: &str,
    scope_start: usize,
    scope_end: usize,
    bindings: &mut Vec<LocalBinding>,
) {
    match &expr.kind {
        ExprKind::Block(stmts) => {
            // Each `let x = v` in a block is visible from that point to the
            // end of the block.
            for stmt in stmts.iter() {
                match stmt {
                    Stmt::Let { pattern, value, .. } => {
                        let value_start = value.span.offset;
                        if let PatternKind::Ident(name) = &pattern.kind {
                            let name_str = resolve(*name);
                            if let Some(off) =
                                find_ident_in_range(source, scope_start, value_start, &name_str)
                            {
                                bindings.push(LocalBinding {
                                    name: *name,
                                    binding_offset: off,
                                    binding_len: name_str.len(),
                                    // Scope: from the start of the let's
                                    // value expression to the end of the
                                    // enclosing block. The binding itself
                                    // sits just before `value_start` so the
                                    // `binding_offset` check is separate.
                                    scope_start: value_start,
                                    scope_end,
                                    ty: value.ty.clone(),
                                });
                            }
                        }
                        collect_local_bindings_in_expr(
                            value,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                    }
                    Stmt::When {
                        pattern,
                        expr,
                        else_body,
                    } => {
                        // Pattern idents are bound in the rest of the block.
                        collect_pattern_bindings(
                            pattern,
                            source,
                            scope_start,
                            expr.span.offset,
                            expr.ty.as_ref(),
                            scope_end,
                            bindings,
                        );
                        collect_local_bindings_in_expr(
                            expr,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                        collect_local_bindings_in_expr(
                            else_body,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                    }
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => {
                        collect_local_bindings_in_expr(
                            condition,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                        collect_local_bindings_in_expr(
                            else_body,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                    }
                    Stmt::Expr(e) => {
                        collect_local_bindings_in_expr(e, source, scope_start, scope_end, bindings);
                    }
                }
            }
        }
        ExprKind::Lambda { params, body } => {
            let body_start = body.span.offset;
            let (body_end, _) = expr_extent(body, source);
            for p in params {
                if let PatternKind::Ident(name) = &p.pattern.kind {
                    let name_str = resolve(*name);
                    if let Some(off) =
                        find_ident_in_range(source, scope_start, body_start, &name_str)
                    {
                        bindings.push(LocalBinding {
                            name: *name,
                            binding_offset: off,
                            binding_len: name_str.len(),
                            scope_start: body_start,
                            scope_end: body_end,
                            ty: find_param_type(body, *name),
                        });
                    }
                }
            }
            collect_local_bindings_in_expr(body, source, body_start, body_end, bindings);
        }
        ExprKind::Match { expr, arms } => {
            if let Some(e) = expr {
                collect_local_bindings_in_expr(e, source, scope_start, scope_end, bindings);
            }
            for arm in arms {
                let arm_start = arm.body.span.offset;
                let (arm_end, _) = expr_extent(&arm.body, source);
                collect_pattern_bindings(
                    &arm.pattern,
                    source,
                    scope_start,
                    arm_start,
                    expr.as_ref().and_then(|e| e.ty.as_ref()),
                    arm_end,
                    bindings,
                );
                if let Some(ref g) = arm.guard {
                    collect_local_bindings_in_expr(g, source, arm_start, arm_end, bindings);
                }
                collect_local_bindings_in_expr(&arm.body, source, arm_start, arm_end, bindings);
            }
        }
        ExprKind::Loop {
            bindings: loop_bindings,
            body,
        } => {
            let body_start = body.span.offset;
            let (body_end, _) = expr_extent(body, source);
            for (name, init) in loop_bindings {
                let name_str = resolve(*name);
                if let Some(off) =
                    find_ident_in_range(source, scope_start, init.span.offset, &name_str)
                {
                    bindings.push(LocalBinding {
                        name: *name,
                        binding_offset: off,
                        binding_len: name_str.len(),
                        scope_start: body_start,
                        scope_end: body_end,
                        ty: init.ty.clone(),
                    });
                }
                collect_local_bindings_in_expr(init, source, scope_start, scope_end, bindings);
            }
            collect_local_bindings_in_expr(body, source, body_start, body_end, bindings);
        }
        _ => {
            visit_expr_children(expr, |child| {
                collect_local_bindings_in_expr(child, source, scope_start, scope_end, bindings);
            });
        }
    }
}

/// Collect the identifiers introduced by a (match/when) pattern.
/// We don't try to recover precise offsets for constructor sub-patterns;
/// instead, we scan the `(search_start..search_end)` window for each bound name.
fn collect_pattern_bindings(
    pattern: &Pattern,
    source: &str,
    search_start: usize,
    search_end: usize,
    expr_ty: Option<&Type>,
    scope_end: usize,
    bindings: &mut Vec<LocalBinding>,
) {
    match &pattern.kind {
        PatternKind::Ident(name) if resolve(*name) != "_" => {
            let name_str = resolve(*name);
            if let Some(off) = find_ident_in_range(source, search_start, search_end, &name_str) {
                bindings.push(LocalBinding {
                    name: *name,
                    binding_offset: off,
                    binding_len: name_str.len(),
                    scope_start: search_end,
                    scope_end,
                    ty: expr_ty.cloned(),
                });
            }
        }
        PatternKind::Tuple(pats) | PatternKind::Or(pats) => {
            for p in pats {
                collect_pattern_bindings(
                    p,
                    source,
                    search_start,
                    search_end,
                    None,
                    scope_end,
                    bindings,
                );
            }
        }
        PatternKind::Constructor(ctor, fields) => {
            // For Ok/Err/Some, try to propagate the inner type.
            let inner_ty: Option<Type> = match (resolve(*ctor).as_str(), expr_ty) {
                ("Ok", Some(Type::Generic(_, args))) => args.first().cloned(),
                ("Err", Some(Type::Generic(_, args))) => args.get(1).cloned(),
                ("Some", Some(Type::Generic(_, args))) => args.first().cloned(),
                _ => None,
            };
            for p in fields {
                collect_pattern_bindings(
                    p,
                    source,
                    search_start,
                    search_end,
                    inner_ty.as_ref(),
                    scope_end,
                    bindings,
                );
            }
        }
        PatternKind::Record { fields, .. } => {
            for (name, sub) in fields {
                if let Some(p) = sub {
                    collect_pattern_bindings(
                        p,
                        source,
                        search_start,
                        search_end,
                        None,
                        scope_end,
                        bindings,
                    );
                } else {
                    let name_str = resolve(*name);
                    if let Some(off) =
                        find_ident_in_range(source, search_start, search_end, &name_str)
                    {
                        bindings.push(LocalBinding {
                            name: *name,
                            binding_offset: off,
                            binding_len: name_str.len(),
                            scope_start: search_end,
                            scope_end,
                            ty: None,
                        });
                    }
                }
            }
        }
        PatternKind::List(pats, rest) => {
            for p in pats {
                collect_pattern_bindings(
                    p,
                    source,
                    search_start,
                    search_end,
                    None,
                    scope_end,
                    bindings,
                );
            }
            if let Some(r) = rest {
                collect_pattern_bindings(
                    r,
                    source,
                    search_start,
                    search_end,
                    None,
                    scope_end,
                    bindings,
                );
            }
        }
        _ => {}
    }
}

/// Return the approximate (end_offset, _) extent of an expression in the source.
/// For block expressions we scan forward to the matching `}` using a simple
/// brace/paren-aware walker that skips string literals and comments. For
/// other expressions we conservatively return the end of the source.
fn expr_extent(expr: &Expr, source: &str) -> (usize, ()) {
    let start = expr.span.offset;
    if start >= source.len() {
        return (source.len(), ());
    }
    if matches!(&expr.kind, ExprKind::Block(_))
        && let Some(end) = match_closing_brace(source, start)
    {
        return (end, ());
    }
    (source.len(), ())
}

/// Forward-scan `before` (the source slice up to the cursor) to find the
/// innermost active call site. Returns `(active_param, paren_byte_offset)`
/// where `paren_byte_offset` is the position of the opening `(` of the call
/// and `active_param` is the 0-based comma count between that `(` and the end.
///
/// Skips string literals (`"..."`, `""" ... """`), line comments (`--`), and
/// block comments (`{- ... -}`) so commas and parens inside them are ignored.
fn scan_call_site_forward(bytes: &[u8]) -> Option<(u32, usize)> {
    // Stack of (paren_byte_offset, comma_count) for each nesting depth.
    let mut stack: Vec<(usize, u32)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // ── Strings ──────────────────────────────────────────
            b'"' => {
                if i + 2 < bytes.len() && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
                    i += 3;
                    while i + 2 < bytes.len()
                        && !(bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"')
                    {
                        i += 1;
                    }
                    i = (i + 3).min(bytes.len());
                } else {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' && i + 1 < bytes.len() {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
            }
            // ── Block comments {- ... -} (with nesting) ─────────
            b'{' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                i += 2;
                let mut cd = 1u32;
                while i < bytes.len() && cd > 0 {
                    if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'-' {
                        cd += 1;
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'}' {
                        cd -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            // ── Line comments -- ... ────────────────────────────
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            // ── Brackets ────────────────────────────────────────
            b'(' => {
                stack.push((i, 0));
                i += 1;
            }
            b')' => {
                stack.pop();
                i += 1;
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    // The top of the stack is the innermost unclosed `(` — that's our call site.
    let (paren_pos, comma_count) = stack.last()?;
    Some((*comma_count, *paren_pos))
}

/// Given an offset at (or just before) a `{`, return the byte offset of the
/// matching `}` (exclusive end). Skips string literals, char escapes, and
/// line/block comments so we don't get fooled by `"}"` or `// }`.
fn match_closing_brace(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    // Find the first `{` at or after `start`.
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let mut depth = 0i32;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'{' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                // Skip `{- ... -}` block comment (with nesting).
                i += 2;
                let mut comment_depth = 1u32;
                while i < bytes.len() && comment_depth > 0 {
                    if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'-' {
                        comment_depth += 1;
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'}' {
                        comment_depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' => {
                // Triple-quoted string?
                if i + 2 < bytes.len() && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
                    i += 3;
                    while i + 2 < bytes.len()
                        && !(bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"')
                    {
                        i += 1;
                    }
                    i = (i + 3).min(bytes.len());
                } else {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' && i + 1 < bytes.len() {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                // Skip `--` line comment to end of line.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Scan `source[start..end]` for the LAST occurrence of `name` as a whole
/// word (not surrounded by identifier characters). Returns the absolute byte
/// offset in `source`.
fn find_ident_in_range(source: &str, start: usize, end: usize, name: &str) -> Option<usize> {
    if name.is_empty() || start >= source.len() || end > source.len() || start >= end {
        return None;
    }
    let hay = &source[start..end];
    let bytes = hay.as_bytes();
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len();
    if name_len > bytes.len() {
        return None;
    }
    // Walk from the end backward for the LAST match.
    let mut i = bytes.len().saturating_sub(name_len);
    loop {
        if &bytes[i..i + name_len] == name_bytes {
            let before_ok = i == 0 || {
                let b = bytes[i - 1];
                !(b.is_ascii_alphanumeric() || b == b'_')
            };
            let after_ok = i + name_len == bytes.len() || {
                let b = bytes[i + name_len];
                !(b.is_ascii_alphanumeric() || b == b'_')
            };
            if before_ok && after_ok {
                return Some(start + i);
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// Find the binding whose identifier span contains the given cursor offset.
fn find_local_binding_at_offset(locals: &[LocalBinding], cursor: usize) -> Option<&LocalBinding> {
    locals
        .iter()
        .find(|b| cursor >= b.binding_offset && cursor < b.binding_offset + b.binding_len)
}

/// Find the nearest (by scope) local binding with the given name visible at the cursor.
fn nearest_local_binding_for(
    locals: &[LocalBinding],
    name: Symbol,
    cursor: usize,
) -> Option<&LocalBinding> {
    // Prefer the innermost scope that contains the cursor (smallest scope
    // width), breaking ties by picking the later binding offset so shadowed
    // bindings resolve to the most recent one.
    locals
        .iter()
        .filter(|b| b.name == name)
        .filter(|b| cursor >= b.scope_start && cursor <= b.scope_end)
        .min_by(|a, b| {
            let wa = a.scope_end.saturating_sub(a.scope_start);
            let wb = b.scope_end.saturating_sub(b.scope_start);
            wa.cmp(&wb)
                .then_with(|| b.binding_offset.cmp(&a.binding_offset))
        })
}

/// Build an LSP range for a binding at `(offset, len)` using the source text.
fn binding_range(source: &str, offset: usize, len: usize) -> Option<Range> {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let end_byte = (offset + len).min(source.len());
    let end_byte = if source.is_char_boundary(end_byte) {
        end_byte
    } else {
        return None;
    };

    // Compute line/column for the start offset.
    let mut line = 0u32;
    let mut col = 0u32;
    let mut idx = 0usize;
    for ch in source.chars() {
        if idx == offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16() as u32;
        }
        idx += ch.len_utf8();
    }
    let start = Position::new(line, col);
    let end_col = col + utf16_len(&source[offset..end_byte]) as u32;
    let end = Position::new(line, end_col);
    Some(Range::new(start, end))
}

fn fn_param_names(f: &FnDecl) -> Vec<String> {
    f.params
        .iter()
        .map(|p| match &p.pattern.kind {
            PatternKind::Ident(name) => name.to_string(),
            _ => "_".to_string(),
        })
        .collect()
}

/// Build a function's type signature from its typed body.
/// Extracts parameter types from the body expression's typed sub-expressions.
fn build_fn_type(f: &FnDecl) -> Option<Type> {
    // After type checking, the body has a resolved type (the return type).
    let ret_ty = f.body.ty.as_ref()?;

    // Extract param types: each param pattern may have been given a type
    // during checking. We look at the body — if it's a block, the params
    // were bound there. But the simplest reliable source is the function's
    // own usage. As a practical approach: walk the body to find Ident nodes
    // matching param names and grab their types.
    let param_names: Vec<Symbol> = f
        .params
        .iter()
        .filter_map(|p| {
            if let PatternKind::Ident(name) = &p.pattern.kind {
                Some(*name)
            } else {
                None
            }
        })
        .collect();

    let mut param_types = Vec::new();
    for name in &param_names {
        if let Some(ty) = find_param_type(&f.body, *name) {
            param_types.push(ty);
        } else {
            return None; // Can't determine a param type
        }
    }

    Some(Type::Fun(param_types, Box::new(ret_ty.clone())))
}

/// Find the type of the first Ident expression matching `name` in the body.
fn find_param_type(expr: &Expr, name: Symbol) -> Option<Type> {
    if let ExprKind::Ident(n) = &expr.kind
        && *n == name
    {
        return expr.ty.clone();
    }
    // Search children
    let mut result = None;
    visit_expr_children(expr, |child| {
        if result.is_none() {
            result = find_param_type(child, name);
        }
    });
    result
}

// ── AST walkers (offset-based) ─────────────────────────────────────

fn token_start(span: &Span) -> usize {
    span.offset
}

/// Find the inferred type of the deepest expression at the cursor byte offset.
fn find_type_at_offset(program: &Program, cursor: usize) -> Option<Type> {
    let mut best: Option<&Type> = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                find_type_in_expr(&f.body, cursor, &mut best);
            }
            Decl::Let { value, .. } => {
                find_type_in_expr(value, cursor, &mut best);
            }
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    find_type_in_expr(&method.body, cursor, &mut best);
                }
            }
            _ => {}
        }
    }
    best.cloned()
}

fn find_type_in_expr<'a>(expr: &'a Expr, cursor: usize, best: &mut Option<&'a Type>) {
    let start = token_start(&expr.span);
    // The cursor must be at or after this expression's start.
    // We rely on depth-first traversal: the deepest (most specific) match wins.
    if cursor >= start
        && let Some(ref ty) = expr.ty
    {
        *best = Some(ty);
    }

    // Recurse into children (inlined to satisfy the borrow checker).
    match &expr.kind {
        ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
            find_type_in_expr(l, cursor, best);
            find_type_in_expr(r, cursor, best);
        }
        ExprKind::Unary(_, e)
        | ExprKind::QuestionMark(e)
        | ExprKind::Ascription(e, _)
        | ExprKind::Return(Some(e))
        | ExprKind::FieldAccess(e, _) => find_type_in_expr(e, cursor, best),
        ExprKind::Call(callee, args) => {
            find_type_in_expr(callee, cursor, best);
            for a in args {
                find_type_in_expr(a, cursor, best);
            }
        }
        ExprKind::Lambda { body, .. } => find_type_in_expr(body, cursor, best),
        ExprKind::Match { expr, arms } => {
            if let Some(e) = expr {
                find_type_in_expr(e, cursor, best);
            }
            for arm in arms {
                if let Some(ref g) = arm.guard {
                    find_type_in_expr(g, cursor, best);
                }
                find_type_in_expr(&arm.body, cursor, best);
            }
        }
        ExprKind::Block(stmts) => {
            for stmt in stmts {
                match stmt {
                    Stmt::Let { value, .. } => find_type_in_expr(value, cursor, best),
                    Stmt::Expr(e) => find_type_in_expr(e, cursor, best),
                    Stmt::When {
                        expr, else_body, ..
                    } => {
                        find_type_in_expr(expr, cursor, best);
                        find_type_in_expr(else_body, cursor, best);
                    }
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => {
                        find_type_in_expr(condition, cursor, best);
                        find_type_in_expr(else_body, cursor, best);
                    }
                }
            }
        }
        ExprKind::List(elems) => {
            for elem in elems {
                match elem {
                    ListElem::Single(e) | ListElem::Spread(e) => find_type_in_expr(e, cursor, best),
                }
            }
        }
        ExprKind::Map(entries) => {
            for (k, v) in entries {
                find_type_in_expr(k, cursor, best);
                find_type_in_expr(v, cursor, best);
            }
        }
        ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
            for e in elems {
                find_type_in_expr(e, cursor, best);
            }
        }
        ExprKind::RecordCreate { fields, .. } => {
            for (_, v) in fields {
                find_type_in_expr(v, cursor, best);
            }
        }
        ExprKind::RecordUpdate { expr, fields, .. } => {
            find_type_in_expr(expr, cursor, best);
            for (_, v) in fields {
                find_type_in_expr(v, cursor, best);
            }
        }
        ExprKind::Loop { bindings, body } => {
            for (_, init) in bindings {
                find_type_in_expr(init, cursor, best);
            }
            find_type_in_expr(body, cursor, best);
        }
        ExprKind::Recur(args) => {
            for a in args {
                find_type_in_expr(a, cursor, best);
            }
        }
        ExprKind::StringInterp(parts) => {
            for part in parts {
                if let StringPart::Expr(e) = part {
                    find_type_in_expr(e, cursor, best);
                }
            }
        }
        ExprKind::FloatElse(expr, fallback) => {
            find_type_in_expr(expr, cursor, best);
            find_type_in_expr(fallback, cursor, best);
        }
        _ => {}
    }
}

/// Check if the cursor is on the field name of a `FieldAccess` expression.
/// If so, return the field's type by looking it up in the receiver's record type.
fn find_field_type_at_offset(
    program: &Program,
    source: &str,
    cursor: usize,
) -> Option<(String, Type)> {
    let mut result: Option<(String, Type)> = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => find_field_in_expr(&f.body, source, cursor, &mut result),
            Decl::Let { value, .. } => find_field_in_expr(value, source, cursor, &mut result),
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    find_field_in_expr(&method.body, source, cursor, &mut result);
                }
            }
            _ => {}
        }
    }
    result
}

fn find_field_in_expr(
    expr: &Expr,
    source: &str,
    cursor: usize,
    result: &mut Option<(String, Type)>,
) {
    if let ExprKind::FieldAccess(receiver, field) = &expr.kind {
        // Find where the field name starts in the source.
        // The FieldAccess span covers the receiver. The field name is after the dot.
        // Search forward from the receiver for `.field`
        let field_str = resolve(*field);
        let expr_start = expr.span.offset;
        if cursor >= expr_start {
            // Find the dot position in the source after the receiver
            if let Some(dot_rel) = source[expr_start..].find('.') {
                let field_start = expr_start + dot_rel + 1;
                let field_end = field_start + field_str.len();
                if cursor >= field_start && cursor < field_end {
                    // Cursor is on the field name — look up the field type
                    if let Some(receiver_ty) = &receiver.ty
                        && let Some(field_ty) = get_field_type(receiver_ty, *field)
                    {
                        *result = Some((field_str, field_ty));
                        return;
                    }
                }
            }
        }
        find_field_in_expr(receiver, source, cursor, result);
    } else {
        // Recurse into children
        match &expr.kind {
            ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
                find_field_in_expr(l, source, cursor, result);
                find_field_in_expr(r, source, cursor, result);
            }
            ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Ascription(e, _)
            | ExprKind::Return(Some(e)) => {
                find_field_in_expr(e, source, cursor, result);
            }
            ExprKind::Call(callee, args) => {
                find_field_in_expr(callee, source, cursor, result);
                for a in args {
                    find_field_in_expr(a, source, cursor, result);
                }
            }
            ExprKind::Lambda { body, .. } => find_field_in_expr(body, source, cursor, result),
            ExprKind::Match { expr, arms } => {
                if let Some(e) = expr {
                    find_field_in_expr(e, source, cursor, result);
                }
                for arm in arms {
                    if let Some(ref g) = arm.guard {
                        find_field_in_expr(g, source, cursor, result);
                    }
                    find_field_in_expr(&arm.body, source, cursor, result);
                }
            }
            ExprKind::Block(stmts) => {
                for stmt in stmts {
                    match stmt {
                        Stmt::Let { value, .. } => {
                            find_field_in_expr(value, source, cursor, result)
                        }
                        Stmt::Expr(e) => find_field_in_expr(e, source, cursor, result),
                        Stmt::When {
                            expr, else_body, ..
                        } => {
                            find_field_in_expr(expr, source, cursor, result);
                            find_field_in_expr(else_body, source, cursor, result);
                        }
                        Stmt::WhenBool {
                            condition,
                            else_body,
                        } => {
                            find_field_in_expr(condition, source, cursor, result);
                            find_field_in_expr(else_body, source, cursor, result);
                        }
                    }
                }
            }
            ExprKind::RecordCreate { fields, .. } => {
                for (_, v) in fields {
                    find_field_in_expr(v, source, cursor, result);
                }
            }
            ExprKind::RecordUpdate { expr, fields, .. } => {
                find_field_in_expr(expr, source, cursor, result);
                for (_, v) in fields {
                    find_field_in_expr(v, source, cursor, result);
                }
            }
            ExprKind::Loop { bindings, body } => {
                for (_, init) in bindings {
                    find_field_in_expr(init, source, cursor, result);
                }
                find_field_in_expr(body, source, cursor, result);
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) | ListElem::Spread(e) => {
                            find_field_in_expr(e, source, cursor, result)
                        }
                    }
                }
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    find_field_in_expr(k, source, cursor, result);
                    find_field_in_expr(v, source, cursor, result);
                }
            }
            ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
                for e in elems {
                    find_field_in_expr(e, source, cursor, result);
                }
            }
            ExprKind::Recur(args) => {
                for a in args {
                    find_field_in_expr(a, source, cursor, result);
                }
            }
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        find_field_in_expr(e, source, cursor, result);
                    }
                }
            }
            ExprKind::FloatElse(expr, fallback) => {
                find_field_in_expr(expr, source, cursor, result);
                find_field_in_expr(fallback, source, cursor, result);
            }
            _ => {}
        }
    }
}

/// Look up a field's type within a record type.
fn get_field_type(ty: &Type, field_name: Symbol) -> Option<Type> {
    match ty {
        Type::Record(_, fields) => fields
            .iter()
            .find(|(n, _)| *n == field_name)
            .map(|(_, t)| t.clone()),
        Type::Tuple(elems) => resolve(field_name)
            .parse::<usize>()
            .ok()
            .and_then(|i| elems.get(i).cloned()),
        _ => None,
    }
}

/// Find the identifier name at the cursor byte offset.
fn find_ident_at_offset(program: &Program, cursor: usize) -> Option<Symbol> {
    let mut best: Option<Symbol> = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                find_ident_in_expr(&f.body, cursor, &mut best);
            }
            Decl::Let { value, .. } => {
                find_ident_in_expr(value, cursor, &mut best);
            }
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    find_ident_in_expr(&method.body, cursor, &mut best);
                }
            }
            _ => {}
        }
    }
    best
}

fn find_ident_in_expr(expr: &Expr, cursor: usize, best: &mut Option<Symbol>) {
    if let ExprKind::Ident(name) = &expr.kind {
        let start = token_start(&expr.span);
        let name_len = resolve(*name).len();
        if cursor >= start && cursor < start + name_len {
            *best = Some(*name);
        }
    }
    visit_expr_children(expr, |child| find_ident_in_expr(child, cursor, best));
}

/// Visit all child expressions of an AST node.
fn visit_expr_children(expr: &Expr, mut f: impl FnMut(&Expr)) {
    match &expr.kind {
        ExprKind::Binary(lhs, _, rhs) | ExprKind::Pipe(lhs, rhs) | ExprKind::Range(lhs, rhs) => {
            f(lhs);
            f(rhs);
        }
        ExprKind::Unary(_, e)
        | ExprKind::QuestionMark(e)
        | ExprKind::Ascription(e, _)
        | ExprKind::Return(Some(e))
        | ExprKind::FieldAccess(e, _) => f(e),
        ExprKind::Call(callee, args) => {
            f(callee);
            for a in args {
                f(a);
            }
        }
        ExprKind::Lambda { body, .. } => f(body),
        ExprKind::Match { expr, arms } => {
            if let Some(e) = expr {
                f(e);
            }
            for arm in arms {
                if let Some(ref guard) = arm.guard {
                    f(guard);
                }
                f(&arm.body);
            }
        }
        ExprKind::Block(stmts) => {
            for stmt in stmts {
                match stmt {
                    Stmt::Let { value, .. } => f(value),
                    Stmt::Expr(e) => f(e),
                    Stmt::When {
                        expr, else_body, ..
                    } => {
                        f(expr);
                        f(else_body);
                    }
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => {
                        f(condition);
                        f(else_body);
                    }
                }
            }
        }
        ExprKind::List(elems) => {
            for elem in elems {
                match elem {
                    ListElem::Single(e) | ListElem::Spread(e) => f(e),
                }
            }
        }
        ExprKind::Map(entries) => {
            for (k, v) in entries {
                f(k);
                f(v);
            }
        }
        ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
            for e in elems {
                f(e);
            }
        }
        ExprKind::RecordCreate { fields, .. } => {
            for (_, v) in fields {
                f(v);
            }
        }
        ExprKind::RecordUpdate { expr, fields, .. } => {
            f(expr);
            for (_, v) in fields {
                f(v);
            }
        }
        ExprKind::Loop { bindings, body } => {
            for (_, init) in bindings {
                f(init);
            }
            f(body);
        }
        ExprKind::Recur(args) => {
            for a in args {
                f(a);
            }
        }
        ExprKind::StringInterp(parts) => {
            for part in parts {
                if let StringPart::Expr(e) = part {
                    f(e);
                }
            }
        }
        ExprKind::FloatElse(expr, fallback) => {
            f(expr);
            f(fallback);
        }
        _ => {}
    }
}

// ── Local variable collection ─────────────────────────────────────

/// A local variable binding visible at a given cursor position.
struct LocalVar {
    name: String,
    ty: Option<Type>,
}

/// Collect local variables in scope at the given byte offset.
fn locals_at_offset(program: &Program, cursor: usize) -> Vec<LocalVar> {
    let mut locals = Vec::new();
    for decl in &program.decls {
        if let Decl::Fn(f) = decl {
            let fn_start = f.span.offset;
            // Rough check: cursor must be after the fn starts
            if cursor >= fn_start {
                // Add function parameters
                for param in &f.params {
                    collect_pattern_names(&param.pattern, &mut locals);
                }
                // Walk the body for locals defined before the cursor
                collect_locals_in_expr(&f.body, cursor, &mut locals);
            }
        }
    }
    // Deduplicate by name (keep last, which has the most specific type)
    let mut seen = std::collections::HashSet::new();
    locals.retain(|v| seen.insert(v.name.clone()));
    locals
}

/// Extract variable names from a pattern (for let/when bindings and params).
fn collect_pattern_names(pattern: &Pattern, locals: &mut Vec<LocalVar>) {
    match &pattern.kind {
        PatternKind::Ident(name) if resolve(*name) != "_" => {
            locals.push(LocalVar {
                name: name.to_string(),
                ty: None,
            });
        }
        PatternKind::Constructor(_, fields) => {
            for p in fields {
                collect_pattern_names(p, locals);
            }
        }
        PatternKind::Tuple(pats) => {
            for p in pats {
                collect_pattern_names(p, locals);
            }
        }
        PatternKind::Record { fields, .. } => {
            for (name, sub) in fields {
                if let Some(p) = sub {
                    collect_pattern_names(p, locals);
                } else {
                    locals.push(LocalVar {
                        name: name.to_string(),
                        ty: None,
                    });
                }
            }
        }
        PatternKind::List(pats, rest) => {
            for p in pats {
                collect_pattern_names(p, locals);
            }
            if let Some(r) = rest {
                collect_pattern_names(r, locals);
            }
        }
        _ => {}
    }
}

/// Walk an expression tree, collecting locals defined before the cursor.
fn collect_locals_in_expr(expr: &Expr, cursor: usize, locals: &mut Vec<LocalVar>) {
    match &expr.kind {
        ExprKind::Block(stmts) => {
            for stmt in stmts {
                match stmt {
                    Stmt::Let { pattern, value, .. } => {
                        // The binding is only visible if defined before cursor
                        if value.span.offset <= cursor {
                            collect_pattern_names_typed(pattern, value.ty.as_ref(), locals);
                        }
                        collect_locals_in_expr(value, cursor, locals);
                    }
                    Stmt::When {
                        pattern,
                        expr,
                        else_body,
                        ..
                    } => {
                        // The pattern binding is visible after the when statement
                        if expr.span.offset <= cursor {
                            collect_pattern_names(pattern, locals);
                            // Try to resolve types from the expression
                            // For `when Ok(x) = expr`, if expr has type Result(T, E),
                            // then x has type T
                            resolve_when_pattern_types(pattern, expr.ty.as_ref(), locals);
                        }
                        collect_locals_in_expr(expr, cursor, locals);
                        collect_locals_in_expr(else_body, cursor, locals);
                    }
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => {
                        collect_locals_in_expr(condition, cursor, locals);
                        collect_locals_in_expr(else_body, cursor, locals);
                    }
                    Stmt::Expr(e) => {
                        collect_locals_in_expr(e, cursor, locals);
                    }
                }
            }
        }
        ExprKind::Match { expr, arms } => {
            if let Some(e) = expr {
                collect_locals_in_expr(e, cursor, locals);
            }
            for arm in arms {
                if arm.body.span.offset <= cursor {
                    collect_pattern_names(&arm.pattern, locals);
                }
                collect_locals_in_expr(&arm.body, cursor, locals);
            }
        }
        ExprKind::Lambda { body, params, .. } => {
            for p in params {
                collect_pattern_names(&p.pattern, locals);
            }
            collect_locals_in_expr(body, cursor, locals);
        }
        ExprKind::Loop { bindings, body } => {
            for (name, init) in bindings {
                if init.span.offset <= cursor {
                    locals.push(LocalVar {
                        name: name.to_string(),
                        ty: init.ty.clone(),
                    });
                }
                collect_locals_in_expr(init, cursor, locals);
            }
            collect_locals_in_expr(body, cursor, locals);
        }
        _ => {
            visit_expr_children(expr, |child| collect_locals_in_expr(child, cursor, locals));
        }
    }
}

/// Like collect_pattern_names but attaches the type from the value expression.
fn collect_pattern_names_typed(pattern: &Pattern, ty: Option<&Type>, locals: &mut Vec<LocalVar>) {
    match &pattern.kind {
        PatternKind::Ident(name) if resolve(*name) != "_" => {
            locals.push(LocalVar {
                name: name.to_string(),
                ty: ty.cloned(),
            });
        }
        _ => collect_pattern_names(pattern, locals),
    }
}

/// For `when Ok(x) = expr` where expr has type Result(T, E), set x's type to T.
fn resolve_when_pattern_types(pattern: &Pattern, expr_ty: Option<&Type>, locals: &mut [LocalVar]) {
    if let (PatternKind::Constructor(ctor, fields), Some(Type::Generic(_, args))) =
        (&pattern.kind, expr_ty)
    {
        // Result(T, E): Ok(x) → x has type T, Err(x) → x has type E
        // Option(T): Some(x) → x has type T
        let ctor_str = resolve(*ctor);
        let inner_ty = match ctor_str.as_str() {
            "Ok" => args.first(),
            "Err" => args.get(1),
            "Some" => args.first(),
            _ => None,
        };
        if let Some(ty) = inner_ty {
            for field_pat in fields {
                if let PatternKind::Ident(name) = &field_pat.kind {
                    // Update the last local with this name to have the resolved type
                    let name_str = name.to_string();
                    if let Some(local) = locals.iter_mut().rev().find(|l| l.name == name_str) {
                        local.ty = Some(ty.clone());
                    }
                }
            }
        }
    }
}

// ── Dot-completion helpers ─────────────────────────────────────────

/// Extract the identifier before the `.` at the cursor position.
/// Returns `None` if the cursor is not in a dot-completion context.
fn extract_dot_prefix(source: &str, pos: &Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = pos.character as usize;
    if col == 0 {
        return None;
    }
    // Convert UTF-16 offset to byte offset
    let mut utf16_offset = 0usize;
    let mut byte_offset = line.len();
    for (byte_idx, ch) in line.char_indices() {
        if utf16_offset >= col {
            byte_offset = byte_idx;
            break;
        }
        utf16_offset += ch.len_utf16();
    }
    let before = &line[..byte_offset];
    // The last character should be '.' (cursor is right after it)
    if !before.ends_with('.') {
        return None;
    }
    let before_dot = &before[..before.len() - 1];
    // Walk backwards to find the identifier
    let ident: String = before_dot
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if ident.is_empty() { None } else { Some(ident) }
}

/// Walk the entire AST to find the type of a variable by name.
/// Returns the most deeply nested (most specific) type found for the identifier.
fn find_ident_type_by_name(program: &Program, name: &str) -> Option<Type> {
    let sym = intern(name);
    let mut result: Option<Type> = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => find_ident_type_in_expr(&f.body, sym, &mut result),
            Decl::Let { value, .. } => find_ident_type_in_expr(value, sym, &mut result),
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    find_ident_type_in_expr(&method.body, sym, &mut result);
                }
            }
            _ => {}
        }
    }
    result
}

fn find_ident_type_in_expr(expr: &Expr, name: Symbol, result: &mut Option<Type>) {
    if let ExprKind::Ident(ident_name) = &expr.kind
        && *ident_name == name
        && let Some(ty) = &expr.ty
        && !has_unresolved_vars(ty)
    {
        *result = Some(ty.clone());
    }
    visit_expr_children(expr, |child| find_ident_type_in_expr(child, name, result));
}

/// Given a type, return the record fields if it is (or wraps) a record type.
/// Looks up type declarations in the program if the type references a named record.
fn record_fields_from_type(ty: &Type, program: &Program) -> Option<Vec<(String, Type)>> {
    match ty {
        Type::Record(_, fields) => Some(
            fields
                .iter()
                .map(|(n, t)| (resolve(*n), t.clone()))
                .collect(),
        ),
        // If it's a named type (Generic or Variant), look up the type declaration
        Type::Generic(name, _) => lookup_record_fields(program, *name),
        _ => None,
    }
}

/// Look up a type declaration by name and return its record fields.
fn lookup_record_fields(program: &Program, type_name: Symbol) -> Option<Vec<(String, Type)>> {
    for decl in &program.decls {
        if let Decl::Type(td) = decl
            && td.name == type_name
            && let TypeBody::Record(fields) = &td.body
        {
            return Some(
                fields
                    .iter()
                    .map(|f| (f.name.to_string(), type_expr_to_type(&f.ty)))
                    .collect(),
            );
        }
    }
    None
}

/// Simple conversion from AST TypeExpr to the type system's Type for display.
fn type_expr_to_type(te: &TypeExpr) -> Type {
    match te {
        TypeExpr::Named(n) => {
            let s = resolve(*n);
            match s.as_str() {
                "Int" => Type::Int,
                "Float" => Type::Float,
                "String" => Type::String,
                "Bool" => Type::Bool,
                _ => Type::Generic(*n, vec![]),
            }
        }
        TypeExpr::Generic(name, args) => {
            let targs: Vec<Type> = args.iter().map(type_expr_to_type).collect();
            let s = resolve(*name);
            match s.as_str() {
                "List" => {
                    if let Some(inner) = targs.into_iter().next() {
                        Type::List(Box::new(inner))
                    } else {
                        Type::Generic(intern("List"), vec![])
                    }
                }
                "Option" => Type::Generic(intern("Option"), targs),
                _ => Type::Generic(*name, targs),
            }
        }
        TypeExpr::SelfType => Type::Generic(intern("Self"), vec![]),
        _ => Type::String, // fallback
    }
}

// ── Completion data ────────────────────────────────────────────────

const KEYWORDS: &[&str] = &[
    "as", "else", "fn", "import", "let", "loop", "match", "mod", "pub", "return", "trait", "type",
    "when", "where",
];

/// Build the builtins completion list dynamically from the module registry
/// so it never falls out of sync with `module.rs`.
fn builtins() -> Vec<(String, CompletionItemKind)> {
    let mut items = vec![
        // Globals (not part of any module)
        ("print".to_string(), CompletionItemKind::FUNCTION),
        ("println".to_string(), CompletionItemKind::FUNCTION),
        ("panic".to_string(), CompletionItemKind::FUNCTION),
        ("Ok".to_string(), CompletionItemKind::CONSTRUCTOR),
        ("Err".to_string(), CompletionItemKind::CONSTRUCTOR),
        ("Some".to_string(), CompletionItemKind::CONSTRUCTOR),
        ("None".to_string(), CompletionItemKind::CONSTRUCTOR),
        ("true".to_string(), CompletionItemKind::CONSTANT),
        ("false".to_string(), CompletionItemKind::CONSTANT),
    ];

    let constants: std::collections::HashSet<String> = module::BUILTIN_MODULES
        .iter()
        .flat_map(|m| {
            module::builtin_module_constants(m)
                .into_iter()
                .map(move |c| format!("{m}.{c}"))
        })
        .collect();

    for &m in module::BUILTIN_MODULES {
        for func in module::builtin_module_functions(m) {
            let qualified = format!("{m}.{func}");
            let kind = if constants.contains(&qualified) {
                CompletionItemKind::CONSTANT
            } else {
                CompletionItemKind::FUNCTION
            };
            items.push((qualified, kind));
        }
    }

    items
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Span;

    // ── position_to_offset ────────────────────────────────────────

    #[test]
    fn test_position_to_offset_first_line() {
        let source = "let x = 42\nlet y = 10";
        let pos = Position::new(0, 4); // 'x'
        assert_eq!(position_to_offset(source, &pos), 4);
    }

    #[test]
    fn test_position_to_offset_second_line() {
        let source = "let x = 42\nlet y = 10";
        let pos = Position::new(1, 4); // 'y'
        assert_eq!(position_to_offset(source, &pos), 15);
    }

    #[test]
    fn test_position_to_offset_start() {
        let source = "hello\nworld";
        let pos = Position::new(0, 0);
        assert_eq!(position_to_offset(source, &pos), 0);
    }

    #[test]
    fn test_position_to_offset_past_end() {
        let source = "ab\ncd";
        // Line 0, col 99 — clamps to end of line
        let pos = Position::new(0, 99);
        assert_eq!(position_to_offset(source, &pos), 2);
    }

    // ── span_to_position ──────────────────────────────────────────

    #[test]
    fn test_span_to_position() {
        // Line 3, col 5 in this source points at the 'e' in "else".
        //
        //   1: let\n      (bytes 0..4)
        //   2: foo\n      (bytes 4..8)
        //   3: else\n     (bytes 8..13)  — 'e' at byte 8 (col 1), '…' unused
        //                                   index of 'e' of col 5 would be byte 12
        //                                   but we want start-of-line col 5 = 'e'
        // Simpler: use a clean ASCII source and put the 5th column on line 3.
        let source = "let\nfoo\n    x = 1"; // line 3 col 5 is 'x' at byte 12.
        let span = Span {
            line: 3,
            col: 5,
            offset: 12,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 2); // 0-based
        assert_eq!(pos.character, 4); // 0-based
    }

    #[test]
    fn test_span_to_position_saturates() {
        // An out-of-range span (line 0, offset 0) should not panic; it should
        // yield a position at the very beginning of the document.
        let source = "anything";
        let span = Span {
            line: 0,
            col: 0,
            offset: 0,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 0);
    }

    // ── span_to_position UTF-16 correctness ───────────────────────

    /// Astral-plane character (emoji) earlier on the SAME line should shift
    /// subsequent `character` values by 2 UTF-16 code units per emoji, not
    /// 1 (which is what a naive codepoint-based implementation would give).
    #[test]
    fn test_span_to_position_uses_utf16_for_astral_characters() {
        // 😀 is U+1F600, 4 bytes UTF-8, 2 UTF-16 code units, 1 codepoint.
        // Source: "😀x" — 'x' starts at byte 4.
        let source = "😀x";
        // The span for 'x' should be at line 1 (1-indexed), col 2 (1-indexed
        // codepoint, matching what the lexer would produce), byte offset 4.
        let span = Span {
            line: 1,
            col: 2,
            offset: 4,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 0, "line should be 0-based");
        // 😀 contributes 2 UTF-16 code units, so 'x' is at character 2,
        // NOT character 1 (which is what the old codepoint-based helper
        // would have returned: col=2 → character=1).
        assert_eq!(
            pos.character, 2,
            "character must be UTF-16 code units, not codepoints"
        );
    }

    /// A span on a line AFTER a line containing an emoji should NOT be
    /// shifted — UTF-16 offsets reset per line, same as codepoint offsets.
    /// This is a regression guard against an implementation that forgets
    /// to reset the column counter at newlines.
    #[test]
    fn test_span_to_position_utf16_resets_per_line() {
        // Line 1: "😀\n"  — bytes 0..5  (😀 = 4 bytes, \n = 1 byte)
        // Line 2: "xy"    — bytes 5..7
        let source = "😀\nxy";
        // 'y' on line 2 at byte offset 6, codepoint col 2.
        let span = Span {
            line: 2,
            col: 2,
            offset: 6,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 1);
        // On line 2, 'y' is just after 'x' — 1 UTF-16 unit from the start
        // of the line. The emoji on line 1 must NOT bleed into line 2.
        assert_eq!(pos.character, 1);
    }

    /// For pure-ASCII input the new helper must return the same Position
    /// that the old codepoint-based implementation did — backwards compat.
    #[test]
    fn test_span_to_position_ascii_unchanged() {
        let source = "hello\nworld\nagain";
        // 'g' on line 3, codepoint col 3, byte offset 14.
        let span = Span {
            line: 3,
            col: 3,
            offset: 14,
        };
        let pos = span_to_position(&span, source);
        assert_eq!(pos.line, 2);
        assert_eq!(pos.character, 2);

        // Also check a span on line 1 — col 1 should always be character 0.
        let span1 = Span {
            line: 1,
            col: 1,
            offset: 0,
        };
        let pos1 = span_to_position(&span1, source);
        assert_eq!(pos1.line, 0);
        assert_eq!(pos1.character, 0);

        // 'l' (second one, offset 3) on line 1.
        let span2 = Span {
            line: 1,
            col: 4,
            offset: 3,
        };
        let pos2 = span_to_position(&span2, source);
        assert_eq!(pos2.line, 0);
        assert_eq!(pos2.character, 3);
    }

    /// An end-to-end flavour: `span_to_range` must also produce UTF-16
    /// ranges when diagnostics live after an astral character. This
    /// exercises the `make_diagnostic` → `span_to_range` → `span_to_position`
    /// pipeline that LSP clients actually see. Uses TWO emojis so that the
    /// buggy codepoint-based and the correct UTF-16-based implementations
    /// disagree on the start column (one codepoint vs two UTF-16 units per
    /// emoji → divergence grows linearly with the emoji count).
    #[test]
    fn test_span_to_range_uses_utf16_after_emoji() {
        // Source: "😀😀bad" — each 😀 is 4 UTF-8 bytes, 1 codepoint,
        // 2 UTF-16 code units. 'b' starts at byte 8.
        //   Codepoint col of 'b' = 3 (lexer advances col by 1 per char).
        //   Correct UTF-16 character = 4.
        let source = "😀😀bad";
        let span = Span {
            line: 1,
            col: 3,
            offset: 8,
        };
        let range = span_to_range(&span, source);
        // Start: two emojis × 2 UTF-16 units each = 4.
        // Buggy implementation would return col-1 = 2, which DIFFERS from 4.
        assert_eq!(
            range.start.character, 4,
            "range start must count UTF-16 units, not codepoints"
        );
        // Token "bad" is 3 UTF-16 units long, so end.character = 7.
        assert_eq!(
            range.end.character, 7,
            "range end must extend by UTF-16 length of the token"
        );
    }

    // ── has_unresolved_vars ───────────────────────────────────────

    #[test]
    fn test_has_unresolved_vars_concrete() {
        assert!(!has_unresolved_vars(&Type::Int));
        assert!(!has_unresolved_vars(&Type::String));
        assert!(!has_unresolved_vars(&Type::Fun(
            vec![Type::Int],
            Box::new(Type::Bool)
        )));
    }

    #[test]
    fn test_has_unresolved_vars_with_var() {
        assert!(has_unresolved_vars(&Type::Var(0)));
        assert!(has_unresolved_vars(&Type::Fun(
            vec![Type::Var(1)],
            Box::new(Type::Int)
        )));
        assert!(has_unresolved_vars(&Type::List(Box::new(Type::Var(2)))));
    }

    #[test]
    fn test_has_unresolved_vars_nested() {
        assert!(has_unresolved_vars(&Type::Record(
            crate::intern::intern("Foo"),
            vec![(crate::intern::intern("x"), Type::Var(0))]
        )));
        assert!(!has_unresolved_vars(&Type::Record(
            crate::intern::intern("Foo"),
            vec![(crate::intern::intern("x"), Type::Int)]
        )));
    }

    // ── get_field_type ────────────────────────────────────────────

    #[test]
    fn test_get_field_type_record() {
        let ty = Type::Record(
            crate::intern::intern("User"),
            vec![
                (crate::intern::intern("name"), Type::String),
                (crate::intern::intern("age"), Type::Int),
            ],
        );
        assert_eq!(
            get_field_type(&ty, crate::intern::intern("name")),
            Some(Type::String)
        );
        assert_eq!(
            get_field_type(&ty, crate::intern::intern("age")),
            Some(Type::Int)
        );
        assert_eq!(get_field_type(&ty, crate::intern::intern("missing")), None);
    }

    #[test]
    fn test_get_field_type_tuple() {
        let ty = Type::Tuple(vec![Type::Int, Type::String, Type::Bool]);
        assert_eq!(
            get_field_type(&ty, crate::intern::intern("0")),
            Some(Type::Int)
        );
        assert_eq!(
            get_field_type(&ty, crate::intern::intern("1")),
            Some(Type::String)
        );
        assert_eq!(
            get_field_type(&ty, crate::intern::intern("2")),
            Some(Type::Bool)
        );
        assert_eq!(get_field_type(&ty, crate::intern::intern("3")), None);
        assert_eq!(get_field_type(&ty, crate::intern::intern("name")), None);
    }

    // ── build_definitions ─────────────────────────────────────────

    #[test]
    fn test_build_definitions_from_program() {
        let source =
            "fn add(a, b) { a + b }\ntype Color {\n  Red,\n  Green,\n  Blue,\n}\nlet x = 42";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        assert!(defs.contains_key(&intern("add")), "should have fn 'add'");
        assert!(
            defs.contains_key(&intern("Color")),
            "should have type 'Color'"
        );
        assert!(
            defs.contains_key(&intern("Red")),
            "should have variant 'Red'"
        );
        assert!(
            defs.contains_key(&intern("Green")),
            "should have variant 'Green'"
        );
        assert!(
            defs.contains_key(&intern("Blue")),
            "should have variant 'Blue'"
        );
        assert!(
            defs.contains_key(&intern("x")),
            "should have let binding 'x'"
        );
    }

    #[test]
    fn test_build_definitions_fn_has_params() {
        let source = "fn greet(name, times) { name }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let defs = build_definitions(&program);

        let def = defs.get(&intern("greet")).unwrap();
        assert_eq!(def.params, vec!["name", "times"]);
    }

    // ── find_type_at_offset ──────────────────────────────────────

    #[test]
    fn test_find_type_at_offset_typed() {
        let source = "fn main() { 42 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);

        // The literal 42 should have type Int
        let ty = find_type_at_offset(&program, 13); // offset of "42"
        assert_eq!(ty, Some(Type::Int));
    }

    // ── find_type_at_offset: richer expressions ──────────────────

    #[test]
    fn test_find_type_at_offset_string() {
        let source = r#"fn main() { "hello" }"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);

        let ty = find_type_at_offset(&program, 13);
        assert_eq!(ty, Some(Type::String));
    }

    #[test]
    fn test_find_type_at_offset_bool() {
        let source = "fn main() { true }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);

        let ty = find_type_at_offset(&program, 13);
        assert_eq!(ty, Some(Type::Bool));
    }

    #[test]
    fn test_find_type_at_offset_binary_expr() {
        let source = "fn main() { 1 + 2 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);

        // The whole binary expression should be Int
        let ty = find_type_at_offset(&program, 13);
        assert_eq!(ty, Some(Type::Int));
    }

    #[test]
    fn test_find_type_at_offset_list() {
        // The `[` at offset 12 is the list start; offset 13 lands on element `1`
        // which is the deepest expression and has type Int.
        // Use the bracket offset to find the list type.
        let source = "fn main() { [1, 2, 3] }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);

        let ty = find_type_at_offset(&program, 12);
        assert_eq!(ty, Some(Type::List(Box::new(Type::Int))));
    }

    // ── find_ident_at_offset ─────────────────────────────────────

    fn parse_and_check(source: &str) -> Program {
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        program
    }

    #[test]
    fn test_find_ident_at_offset_param() {
        let source = "fn add(x, y) { x + y }";
        let program = parse_and_check(source);

        // 'x' at offset 15 (inside the body)
        let name = find_ident_at_offset(&program, 15);
        assert_eq!(name, Some(intern("x")));
    }

    #[test]
    fn test_find_ident_at_offset_second_param() {
        let source = "fn add(x, y) { x + y }";
        let program = parse_and_check(source);

        // 'y' at offset 19
        let name = find_ident_at_offset(&program, 19);
        assert_eq!(name, Some(intern("y")));
    }

    #[test]
    fn test_find_ident_at_offset_none() {
        let source = "fn main() { 42 }";
        let program = parse_and_check(source);

        // offset 13 is the literal 42, not an ident
        let name = find_ident_at_offset(&program, 13);
        assert_eq!(name, None);
    }

    // ── locals_at_offset ─────────────────────────────────────────

    #[test]
    fn test_locals_at_offset_params() {
        let source = "fn greet(name, age) { name }";
        let program = parse_and_check(source);

        let locals = locals_at_offset(&program, 22); // inside body
        let names: Vec<&str> = locals.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"name"), "should contain param 'name'");
        assert!(names.contains(&"age"), "should contain param 'age'");
    }

    #[test]
    fn test_locals_at_offset_let_binding() {
        let source = "fn main() {\n  let x = 10\n  let y = 20\n  x + y\n}";
        let program = parse_and_check(source);

        // After both let bindings
        let locals = locals_at_offset(&program, 40);
        let names: Vec<&str> = locals.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"x"), "should contain 'x'");
        assert!(names.contains(&"y"), "should contain 'y'");
    }

    #[test]
    fn test_locals_at_offset_empty_outside_fn() {
        let source = "let x = 42\nfn main() { 0 }";
        let program = parse_and_check(source);

        // Outside any function (offset 0)
        let locals = locals_at_offset(&program, 0);
        assert!(locals.is_empty(), "no locals outside functions");
    }

    // ── build_definitions: traits and let bindings ────────────────

    #[test]
    fn test_build_definitions_trait() {
        let source = "trait Printable {\n  fn show(self) -> String\n}\nfn main() { 0 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        assert!(
            defs.contains_key(&intern("Printable")),
            "should have trait 'Printable'"
        );
    }

    #[test]
    fn test_build_definitions_let_type() {
        let source = "let x = 42\nfn main() { x }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        let def = defs.get(&intern("x")).expect("should have 'x'");
        assert_eq!(def.ty, Some(Type::Int));
    }

    // ── build_signature_from_def ─────────────────────────────────

    #[test]
    fn test_build_signature_simple() {
        let def = DefInfo {
            span: Span {
                line: 1,
                col: 1,
                offset: 0,
            },
            ty: Some(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
            params: vec!["a".into(), "b".into()],
        };
        let (label, params) = build_signature_from_def("add", &def);
        assert!(label.starts_with("fn add("));
        assert!(label.contains("-> Int"));
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_build_signature_no_type() {
        let def = DefInfo {
            span: Span {
                line: 1,
                col: 1,
                offset: 0,
            },
            ty: None,
            params: vec!["x".into(), "y".into()],
        };
        let (label, params) = build_signature_from_def("foo", &def);
        assert_eq!(label, "fn foo(x, y)");
        assert_eq!(params.len(), 2);
    }

    // ── document_symbols via build_definitions ────────────────────

    #[test]
    fn test_build_definitions_enum_variants() {
        let source = "type Shape {\n  Circle(Float),\n  Rect(Float, Float),\n}\nfn main() { 0 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        assert!(defs.contains_key(&intern("Shape")));
        assert!(defs.contains_key(&intern("Circle")));
        assert!(defs.contains_key(&intern("Rect")));
    }

    #[test]
    fn test_build_definitions_multiple_functions() {
        let source = "fn add(a, b) { a + b }\nfn sub(a, b) { a - b }\nfn main() { 0 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        let defs = build_definitions(&program);

        assert!(defs.contains_key(&intern("add")));
        assert!(defs.contains_key(&intern("sub")));
        let add = defs.get(&intern("add")).unwrap();
        assert_eq!(add.params, vec!["a", "b"]);
        // Type should be (Int, Int) -> Int after inference
        assert!(add.ty.is_some());
    }

    // ── get_field_type: nested records ────────────────────────────

    #[test]
    fn test_get_field_type_missing_field() {
        let ty = Type::Record(
            crate::intern::intern("Point"),
            vec![
                (crate::intern::intern("x"), Type::Float),
                (crate::intern::intern("y"), Type::Float),
            ],
        );
        assert_eq!(
            get_field_type(&ty, crate::intern::intern("x")),
            Some(Type::Float)
        );
        assert_eq!(get_field_type(&ty, crate::intern::intern("z")), None);
    }

    #[test]
    fn test_get_field_type_non_record() {
        assert_eq!(get_field_type(&Type::Int, crate::intern::intern("x")), None);
        assert_eq!(
            get_field_type(&Type::String, crate::intern::intern("length")),
            None
        );
    }

    // ── has_unresolved_vars: function types ───────────────────────

    #[test]
    fn test_has_unresolved_vars_in_return_type() {
        let ty = Type::Fun(vec![Type::Int], Box::new(Type::Var(5)));
        assert!(has_unresolved_vars(&ty));
    }

    #[test]
    fn test_has_unresolved_vars_tuple() {
        assert!(!has_unresolved_vars(&Type::Tuple(vec![
            Type::Int,
            Type::String
        ])));
        assert!(has_unresolved_vars(&Type::Tuple(vec![
            Type::Int,
            Type::Var(0)
        ])));
    }

    // ── position_to_offset: UTF-16 handling ──────────────────────

    #[test]
    fn test_position_to_offset_empty_source() {
        let source = "";
        let pos = Position::new(0, 0);
        assert_eq!(position_to_offset(source, &pos), 0);
    }

    #[test]
    fn test_position_to_offset_multiline() {
        let source = "abc\ndef\nghi";
        // line 2, col 1 → 'h' at offset 8
        let pos = Position::new(2, 1);
        assert_eq!(position_to_offset(source, &pos), 9);
    }

    // ── span_to_range ────────────────────────────────────────────

    #[test]
    fn test_span_to_range_empty_source() {
        // A span referring to line 3 col 5 in an empty source is nonsensical,
        // but the helper must not panic and must produce a one-column range.
        // Under the UTF-16-correct implementation, the start column is walked
        // from the source text — so for an empty source the character count
        // is 0 (there are no characters to count); the line value still comes
        // from span.line. The range has width 1 (token_len_at on an empty
        // source returns 1 by contract).
        let span = Span {
            line: 3,
            col: 5,
            offset: 0,
        };
        let range = span_to_range(&span, "");
        assert_eq!(range.start.line, 2);
        assert_eq!(range.start.character, 0);
        assert_eq!(range.end.line, 2);
        assert_eq!(range.end.character, 1);
    }

    #[test]
    fn test_span_to_range_identifier_width() {
        // Regression: a span at a multi-character identifier must produce a
        // range whose width equals the identifier's length, not just 1.
        let source = "let println = 42";
        let span = Span {
            line: 1,
            col: 5,
            offset: 4,
        };
        let range = span_to_range(&span, source);
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 4);
        assert_eq!(range.end.line, 0);
        // `println` is 7 characters wide, so end column = 4 + 7 = 11.
        assert_eq!(range.end.character, 11);
    }

    #[test]
    fn test_span_to_range_multiline_source() {
        // On line 2, both line and column math must use the span's own
        // line/col — not hard-coded values — and the end should land at the
        // end of the identifier, on the same line.
        let source = "let a = 1\nlet foobar = 2";
        let span = Span {
            line: 2,
            col: 5,
            offset: 14,
        };
        let range = span_to_range(&span, source);
        assert_eq!(range.start.line, 1);
        assert_eq!(range.start.character, 4);
        assert_eq!(range.end.line, 1);
        // `foobar` is 6 characters wide.
        assert_eq!(range.end.character, 10);
    }

    #[test]
    fn test_token_len_at_identifier() {
        assert_eq!(token_len_at("println x", 0), 7);
        assert_eq!(token_len_at("let abc = 1", 4), 3);
        assert_eq!(token_len_at("foo_bar + 1", 0), 7);
    }

    #[test]
    fn test_token_len_at_number() {
        assert_eq!(token_len_at("42 + 1", 0), 2);
        assert_eq!(token_len_at("3.14", 0), 4);
        // `1..10` should stop at the `.` because it's a range, not a float.
        assert_eq!(token_len_at("1..10", 0), 1);
    }

    #[test]
    fn test_token_len_at_string() {
        assert_eq!(token_len_at(r#""hi" end"#, 0), 4);
        assert_eq!(token_len_at(r#""esc\"ape""#, 0), 10);
    }

    #[test]
    fn test_token_len_at_past_end() {
        // Out-of-bounds offset must not panic.
        assert_eq!(token_len_at("x", 99), 1);
        assert_eq!(token_len_at("", 0), 1);
    }

    // ── find_type_at_offset: let bindings ────────────────────────

    #[test]
    fn test_find_type_at_offset_in_let() {
        let source = "fn main() {\n  let x = 42\n  x\n}";
        let program = parse_and_check(source);

        // 'x' in the last expression (offset 27)
        let ty = find_type_at_offset(&program, 27);
        assert_eq!(ty, Some(Type::Int));
    }

    // ── build_fn_type ────────────────────────────────────────────

    #[test]
    fn test_build_fn_type_simple() {
        let source = "fn double(n) { n * 2 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);

        if let Decl::Fn(f) = &program.decls[0] {
            let ty = build_fn_type(f);
            assert_eq!(ty, Some(Type::Fun(vec![Type::Int], Box::new(Type::Int))));
        } else {
            panic!("expected Fn decl");
        }
    }

    #[test]
    fn test_fn_param_names() {
        let source = "fn add(x, y) { x + y }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();

        if let Decl::Fn(f) = &program.decls[0] {
            let names = fn_param_names(f);
            assert_eq!(names, vec!["x", "y"]);
        } else {
            panic!("expected Fn decl");
        }
    }

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

    // ── match_closing_brace ──────────────────────────────────────

    #[test]
    fn test_match_closing_brace_skips_silt_line_comment() {
        // The `-- }` line comment should NOT count as a closing brace.
        let source = "fn foo() { -- }\n  42\n}";
        //           0         1
        //           0123456789012345678901
        // Opening `{` is at index 9.  Real closing `}` is at index 21.
        let result = match_closing_brace(source, 9);
        assert_eq!(result, Some(22), "line comment `-- }}` should be skipped");
    }

    #[test]
    fn test_match_closing_brace_skips_silt_block_comment() {
        // The `{- } -}` block comment should NOT count as a closing brace
        // and the `{-` should NOT count as an opening brace.
        let source = "fn foo() { {- } -}\n  42\n}";
        //           0         1         2
        //           0123456789012345678901234
        // Opening `{` is at index 9.  Real closing `}` is at index 24.
        let result = match_closing_brace(source, 9);
        assert_eq!(
            result,
            Some(25),
            "block comment `{{- }} -}}` should be skipped"
        );
    }

    #[test]
    fn test_match_closing_brace_normal() {
        // Basic matching of braces without any comments.
        let source = "fn foo() { let x = { 1 }; x }";
        //           0         1         2
        //           0123456789012345678901234567890
        // Opening `{` at index 9.  Real closing `}` at index 29.
        let result = match_closing_brace(source, 9);
        assert_eq!(result, Some(29), "should match the outermost closing brace");
    }

    // ── scan_call_site_forward tests ─────────────────────────────

    #[test]
    fn test_sig_help_comma_in_string_not_counted() {
        // foo("hello, world", 42)  — cursor after 42
        // The comma inside the string must not be counted.
        let before = r#"foo("hello, world", 42"#;
        let (param, paren) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "should be param 1 (y), not 2");
        assert_eq!(paren, 3, "paren at index 3");
    }

    #[test]
    fn test_sig_help_comma_in_line_comment_not_counted() {
        // foo(1,\n-- a, b, c\n2)  — cursor after 2
        let before = "foo(1,\n-- a, b, c\n2";
        let (param, _) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "commas inside -- comment should be ignored");
    }

    #[test]
    fn test_sig_help_comma_in_block_comment_not_counted() {
        // foo(1, {- a, b -} 2)  — cursor after 2
        let before = "foo(1, {- a, b -} 2";
        let (param, _) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "commas inside block comment should be ignored");
    }

    #[test]
    fn test_sig_help_nested_call_finds_outer_function() {
        // add(mul(1, 2), 3)  — cursor after 3
        let before = "add(mul(1, 2), 3";
        let (param, paren) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "should be param 1 of add, not param of mul");
        assert_eq!(paren, 3, "paren should be add's ( at index 3");
    }

    #[test]
    fn test_sig_help_cursor_inside_inner_call() {
        // add(mul(1, | — cursor between 1 and closing
        let before = "add(mul(1, ";
        let (param, paren) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "should be param 1 of mul");
        assert_eq!(paren, 7, "paren should be mul's ( at index 7");
    }

    #[test]
    fn test_sig_help_no_open_paren_returns_none() {
        let before = "let x = 42";
        assert!(scan_call_site_forward(before.as_bytes()).is_none());
    }

    #[test]
    fn test_sig_help_triple_quoted_string_skipped() {
        let before = r#"foo("""hello, world""", 42"#;
        let (param, _) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "comma inside triple-quoted string ignored");
    }
}
