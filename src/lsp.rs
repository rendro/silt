//! Language Server Protocol implementation for Silt.
//!
//! Provides diagnostics, hover (inferred types), and go-to-definition
//! over the standard LSP JSON-RPC transport (stdin/stdout).

use std::collections::HashMap;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
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

struct Document {
    source: String,
    program: Option<Program>,
    /// Definition map: name → definition info (built from top-level declarations).
    definitions: HashMap<String, DefInfo>,
}

// ── Span ↔ LSP conversion ─────────────────────────────────────────

fn span_to_position(span: &Span) -> Position {
    Position::new(
        span.line.saturating_sub(1) as u32,
        span.col.saturating_sub(1) as u32,
    )
}

fn span_to_range(span: &Span) -> Range {
    let start = span_to_position(span);
    let end = Position::new(start.line, start.character + 1);
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
        offset += line.len() + 1; // +1 for '\n'
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
        match req.method.as_str() {
            HoverRequest::METHOD => {
                if let Some((id, params)) = extract_request::<HoverRequest>(req) {
                    let result = self.hover(params);
                    let resp = Response::new_ok(id, result);
                    self.connection.sender.send(Message::Response(resp)).ok();
                }
            }
            GotoDefinition::METHOD => {
                if let Some((id, params)) = extract_request::<GotoDefinition>(req) {
                    let result = self.goto_definition(params);
                    let resp = Response::new_ok(id, result);
                    self.connection.sender.send(Message::Response(resp)).ok();
                }
            }
            Formatting::METHOD => {
                if let Some((id, params)) = extract_request::<Formatting>(req) {
                    let result = self.format(params);
                    let resp = Response::new_ok(id, result);
                    self.connection.sender.send(Message::Response(resp)).ok();
                }
            }
            Completion::METHOD => {
                if let Some((id, params)) = extract_request::<Completion>(req) {
                    let result = self.completion(params);
                    let resp = Response::new_ok(id, result);
                    self.connection.sender.send(Message::Response(resp)).ok();
                }
            }
            SignatureHelpRequest::METHOD => {
                if let Some((id, params)) = extract_request::<SignatureHelpRequest>(req) {
                    let result = self.signature_help(params);
                    let resp = Response::new_ok(id, result);
                    self.connection.sender.send(Message::Response(resp)).ok();
                }
            }
            DocumentSymbolRequest::METHOD => {
                if let Some((id, params)) = extract_request::<DocumentSymbolRequest>(req) {
                    let result = self.document_symbols(params);
                    let resp = Response::new_ok(id, result);
                    self.connection.sender.send(Message::Response(resp)).ok();
                }
            }
            _ => {}
        }
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
                ));
                self.documents.insert(
                    uri.clone(),
                    Document {
                        source,
                        program: None,
                        definitions: HashMap::new(),
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
            ));
        }

        let type_errors = typechecker::check(&mut program);
        for e in &type_errors {
            let severity = match e.severity {
                typechecker::Severity::Error => DiagnosticSeverity::ERROR,
                typechecker::Severity::Warning => DiagnosticSeverity::WARNING,
            };
            diagnostics.push(make_diagnostic(&e.message, &e.span, severity));
        }

        let definitions = build_definitions(&program);

        self.documents.insert(
            uri.clone(),
            Document {
                source,
                program: Some(program),
                definitions,
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

        // Still filter out completely unresolved types.
        if has_unresolved_vars(&ty) {
            return None;
        }

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
        let name = find_ident_at_offset(program, cursor)?;
        let def = doc.definitions.get(&name)?;

        Some(GotoDefinitionResponse::Scalar(Location::new(
            uri.clone(),
            span_to_range(&def.span),
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
        for (name, kind) in BUILTINS {
            let detail = self.builtin_sigs.get(*name).cloned();
            items.push(CompletionItem {
                label: name.to_string(),
                kind: Some(*kind),
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
                    label: name.clone(),
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
        for decl in &program.decls {
            if let Decl::Type(td) = decl
                && td.name == prefix
                && let TypeBody::Record(fields) = &td.body
            {
                for field in fields {
                    items.push(CompletionItem {
                        label: field.name.clone(),
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

        // Replace the entire document.
        let line_count = doc.source.lines().count() as u32;
        let last_line_len = doc.source.lines().last().map_or(0, |l| l.len()) as u32;
        Some(vec![TextEdit {
            range: Range::new(
                Position::new(0, 0),
                Position::new(line_count, last_line_len),
            ),
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

        // Count commas at the current nesting level to determine active param.
        let mut active_param = 0u32;
        let mut depth = 0i32;
        for ch in before.chars().rev() {
            match ch {
                ')' | ']' | '}' => depth += 1,
                '(' | '[' | '{' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                ',' if depth == 0 => active_param += 1,
                _ => {}
            }
        }

        // Find the function name: scan back past the `(` to the ident.
        let paren_pos = before.rfind('(')?;
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
        let (label, params_info) = if let Some(def) = doc.definitions.get(&fn_name) {
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
                        name: f.name.clone(),
                        detail,
                        kind: SymbolKind::FUNCTION,
                        range: span_to_range(&f.span),
                        selection_range: span_to_range(&f.span),
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
                        name: t.name.clone(),
                        detail: None,
                        kind,
                        range: span_to_range(&t.span),
                        selection_range: span_to_range(&t.span),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Trait(t) => {
                    symbols.push(DocumentSymbol {
                        name: t.name.clone(),
                        detail: None,
                        kind: SymbolKind::INTERFACE,
                        range: span_to_range(&t.span),
                        selection_range: span_to_range(&t.span),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Let {
                    pattern: Pattern::Ident(name),
                    span,
                    value,
                    ..
                } => {
                    let detail = value.ty.as_ref().map(|t| format!("{t}"));
                    symbols.push(DocumentSymbol {
                        name: name.clone(),
                        detail,
                        kind: SymbolKind::VARIABLE,
                        range: span_to_range(span),
                        selection_range: span_to_range(span),
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
        Type::List(inner) | Type::Set(inner) => has_unresolved_vars(inner),
        Type::Tuple(elems) => elems.iter().any(has_unresolved_vars),
        Type::Record(_, fields) => fields.iter().any(|(_, t)| has_unresolved_vars(t)),
        Type::Variant(_, args) | Type::Generic(_, args) => args.iter().any(has_unresolved_vars),
        Type::Map(k, v) => has_unresolved_vars(k) || has_unresolved_vars(v),
        _ => false,
    }
}

// ── Diagnostics helper ─────────────────────────────────────────────

fn make_diagnostic(message: &str, span: &Span, severity: DiagnosticSeverity) -> Diagnostic {
    Diagnostic {
        range: span_to_range(span),
        severity: Some(severity),
        message: message.to_string(),
        ..Diagnostic::default()
    }
}

// ── Build definitions map from declarations ────────────────────────

fn build_definitions(program: &Program) -> HashMap<String, DefInfo> {
    let mut defs = HashMap::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                let fn_ty = build_fn_type(f);
                let params = fn_param_names(f);
                defs.insert(
                    f.name.clone(),
                    DefInfo {
                        span: f.span,
                        ty: fn_ty,
                        params,
                    },
                );
            }
            Decl::Type(t) => {
                defs.insert(
                    t.name.clone(),
                    DefInfo {
                        span: t.span,
                        ty: None,
                        params: vec![],
                    },
                );
                if let TypeBody::Enum(variants) = &t.body {
                    for v in variants {
                        defs.insert(
                            v.name.clone(),
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
                    t.name.clone(),
                    DefInfo {
                        span: t.span,
                        ty: None,
                        params: vec![],
                    },
                );
            }
            Decl::Let {
                pattern: Pattern::Ident(name),
                span,
                value,
                ..
            } => {
                defs.insert(
                    name.clone(),
                    DefInfo {
                        span: *span,
                        ty: value.ty.clone(),
                        params: vec![],
                    },
                );
            }
            _ => {}
        }
    }
    defs
}

fn fn_param_names(f: &FnDecl) -> Vec<String> {
    f.params
        .iter()
        .map(|p| match &p.pattern {
            Pattern::Ident(name) => name.clone(),
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
    let param_names: Vec<&str> = f
        .params
        .iter()
        .filter_map(|p| {
            if let Pattern::Ident(name) = &p.pattern {
                Some(name.as_str())
            } else {
                None
            }
        })
        .collect();

    let mut param_types = Vec::new();
    for name in &param_names {
        if let Some(ty) = find_param_type(&f.body, name) {
            param_types.push(ty);
        } else {
            return None; // Can't determine a param type
        }
    }

    Some(Type::Fun(param_types, Box::new(ret_ty.clone())))
}

/// Find the type of the first Ident expression matching `name` in the body.
fn find_param_type(expr: &Expr, name: &str) -> Option<Type> {
    if let ExprKind::Ident(n) = &expr.kind
        && n == name
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
        let expr_start = expr.span.offset;
        if cursor >= expr_start {
            // Find the dot position in the source after the receiver
            if let Some(dot_rel) = source[expr_start..].find('.') {
                let field_start = expr_start + dot_rel + 1;
                let field_end = field_start + field.len();
                if cursor >= field_start && cursor < field_end {
                    // Cursor is on the field name — look up the field type
                    if let Some(receiver_ty) = &receiver.ty
                        && let Some(field_ty) = get_field_type(receiver_ty, field)
                    {
                        *result = Some((field.clone(), field_ty));
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
            _ => {}
        }
    }
}

/// Look up a field's type within a record type.
fn get_field_type(ty: &Type, field_name: &str) -> Option<Type> {
    match ty {
        Type::Record(_, fields) => fields
            .iter()
            .find(|(n, _)| n == field_name)
            .map(|(_, t)| t.clone()),
        Type::Tuple(elems) => field_name
            .parse::<usize>()
            .ok()
            .and_then(|i| elems.get(i).cloned()),
        _ => None,
    }
}

/// Find the identifier name at the cursor byte offset.
fn find_ident_at_offset(program: &Program, cursor: usize) -> Option<String> {
    let mut best: Option<String> = None;
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

fn find_ident_in_expr(expr: &Expr, cursor: usize, best: &mut Option<String>) {
    if let ExprKind::Ident(name) = &expr.kind {
        let start = token_start(&expr.span);
        if cursor >= start && cursor < start + name.len() {
            *best = Some(name.clone());
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
    match pattern {
        Pattern::Ident(name) if name != "_" => {
            locals.push(LocalVar {
                name: name.clone(),
                ty: None,
            });
        }
        Pattern::Constructor(_, fields) => {
            for p in fields {
                collect_pattern_names(p, locals);
            }
        }
        Pattern::Tuple(pats) => {
            for p in pats {
                collect_pattern_names(p, locals);
            }
        }
        Pattern::Record { fields, .. } => {
            for (name, sub) in fields {
                if let Some(p) = sub {
                    collect_pattern_names(p, locals);
                } else {
                    locals.push(LocalVar {
                        name: name.clone(),
                        ty: None,
                    });
                }
            }
        }
        Pattern::List(pats, rest) => {
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
                        name: name.clone(),
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
    match pattern {
        Pattern::Ident(name) if name != "_" => {
            locals.push(LocalVar {
                name: name.clone(),
                ty: ty.cloned(),
            });
        }
        _ => collect_pattern_names(pattern, locals),
    }
}

/// For `when Ok(x) = expr` where expr has type Result(T, E), set x's type to T.
fn resolve_when_pattern_types(pattern: &Pattern, expr_ty: Option<&Type>, locals: &mut [LocalVar]) {
    if let (Pattern::Constructor(ctor, fields), Some(Type::Generic(_, args))) = (pattern, expr_ty) {
        // Result(T, E): Ok(x) → x has type T, Err(x) → x has type E
        // Option(T): Some(x) → x has type T
        let inner_ty = match ctor.as_str() {
            "Ok" => args.first(),
            "Err" => args.get(1),
            "Some" => args.first(),
            _ => None,
        };
        if let Some(ty) = inner_ty {
            for field_pat in fields {
                if let Pattern::Ident(name) = field_pat {
                    // Update the last local with this name to have the resolved type
                    if let Some(local) = locals.iter_mut().rev().find(|l| l.name == *name) {
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
    let mut result: Option<Type> = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => find_ident_type_in_expr(&f.body, name, &mut result),
            Decl::Let { value, .. } => find_ident_type_in_expr(value, name, &mut result),
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    find_ident_type_in_expr(&method.body, name, &mut result);
                }
            }
            _ => {}
        }
    }
    result
}

fn find_ident_type_in_expr(expr: &Expr, name: &str, result: &mut Option<Type>) {
    if let ExprKind::Ident(ident_name) = &expr.kind
        && ident_name == name
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
        Type::Record(_, fields) => Some(fields.clone()),
        // If it's a named type (Generic or Variant), look up the type declaration
        Type::Generic(name, _) => lookup_record_fields(program, name),
        _ => None,
    }
}

/// Look up a type declaration by name and return its record fields.
fn lookup_record_fields(program: &Program, type_name: &str) -> Option<Vec<(String, Type)>> {
    for decl in &program.decls {
        if let Decl::Type(td) = decl
            && td.name == type_name
            && let TypeBody::Record(fields) = &td.body
        {
            return Some(
                fields
                    .iter()
                    .map(|f| (f.name.clone(), type_expr_to_type(&f.ty)))
                    .collect(),
            );
        }
    }
    None
}

/// Simple conversion from AST TypeExpr to the type system's Type for display.
fn type_expr_to_type(te: &TypeExpr) -> Type {
    match te {
        TypeExpr::Named(n) => match n.as_str() {
            "Int" => Type::Int,
            "Float" => Type::Float,
            "String" => Type::String,
            "Bool" => Type::Bool,
            _ => Type::Generic(n.clone(), vec![]),
        },
        TypeExpr::Generic(name, args) => {
            let targs: Vec<Type> = args.iter().map(type_expr_to_type).collect();
            match name.as_str() {
                "List" => {
                    if let Some(inner) = targs.into_iter().next() {
                        Type::List(Box::new(inner))
                    } else {
                        Type::Generic("List".into(), vec![])
                    }
                }
                "Option" => Type::Generic("Option".into(), targs),
                _ => Type::Generic(name.clone(), targs),
            }
        }
        TypeExpr::SelfType => Type::Generic("Self".into(), vec![]),
        _ => Type::String, // fallback
    }
}

// ── Completion data ────────────────────────────────────────────────

const KEYWORDS: &[&str] = &[
    "as", "else", "fn", "import", "let", "loop", "match", "mod", "pub", "return", "trait", "type",
    "when", "where",
];

const BUILTINS: &[(&str, CompletionItemKind)] = &[
    // Globals
    ("print", CompletionItemKind::FUNCTION),
    ("println", CompletionItemKind::FUNCTION),
    ("panic", CompletionItemKind::FUNCTION),
    ("Ok", CompletionItemKind::CONSTRUCTOR),
    ("Err", CompletionItemKind::CONSTRUCTOR),
    ("Some", CompletionItemKind::CONSTRUCTOR),
    ("None", CompletionItemKind::CONSTRUCTOR),
    ("true", CompletionItemKind::CONSTANT),
    ("false", CompletionItemKind::CONSTANT),
    // list
    ("list.map", CompletionItemKind::FUNCTION),
    ("list.filter", CompletionItemKind::FUNCTION),
    ("list.fold", CompletionItemKind::FUNCTION),
    ("list.each", CompletionItemKind::FUNCTION),
    ("list.find", CompletionItemKind::FUNCTION),
    ("list.sort", CompletionItemKind::FUNCTION),
    ("list.sort_by", CompletionItemKind::FUNCTION),
    ("list.reverse", CompletionItemKind::FUNCTION),
    ("list.head", CompletionItemKind::FUNCTION),
    ("list.tail", CompletionItemKind::FUNCTION),
    ("list.last", CompletionItemKind::FUNCTION),
    ("list.length", CompletionItemKind::FUNCTION),
    ("list.contains", CompletionItemKind::FUNCTION),
    ("list.append", CompletionItemKind::FUNCTION),
    ("list.concat", CompletionItemKind::FUNCTION),
    ("list.zip", CompletionItemKind::FUNCTION),
    ("list.flatten", CompletionItemKind::FUNCTION),
    ("list.flat_map", CompletionItemKind::FUNCTION),
    ("list.filter_map", CompletionItemKind::FUNCTION),
    ("list.any", CompletionItemKind::FUNCTION),
    ("list.all", CompletionItemKind::FUNCTION),
    ("list.get", CompletionItemKind::FUNCTION),
    ("list.take", CompletionItemKind::FUNCTION),
    ("list.drop", CompletionItemKind::FUNCTION),
    ("list.enumerate", CompletionItemKind::FUNCTION),
    ("list.group_by", CompletionItemKind::FUNCTION),
    ("list.fold_until", CompletionItemKind::FUNCTION),
    ("list.unfold", CompletionItemKind::FUNCTION),
    // string
    ("string.from", CompletionItemKind::FUNCTION),
    ("string.split", CompletionItemKind::FUNCTION),
    ("string.trim", CompletionItemKind::FUNCTION),
    ("string.join", CompletionItemKind::FUNCTION),
    ("string.length", CompletionItemKind::FUNCTION),
    ("string.byte_length", CompletionItemKind::FUNCTION),
    ("string.contains", CompletionItemKind::FUNCTION),
    ("string.replace", CompletionItemKind::FUNCTION),
    ("string.to_upper", CompletionItemKind::FUNCTION),
    ("string.to_lower", CompletionItemKind::FUNCTION),
    ("string.starts_with", CompletionItemKind::FUNCTION),
    ("string.ends_with", CompletionItemKind::FUNCTION),
    ("string.chars", CompletionItemKind::FUNCTION),
    ("string.repeat", CompletionItemKind::FUNCTION),
    ("string.index_of", CompletionItemKind::FUNCTION),
    ("string.slice", CompletionItemKind::FUNCTION),
    ("string.pad_left", CompletionItemKind::FUNCTION),
    ("string.pad_right", CompletionItemKind::FUNCTION),
    ("string.is_empty", CompletionItemKind::FUNCTION),
    ("string.is_alpha", CompletionItemKind::FUNCTION),
    ("string.is_digit", CompletionItemKind::FUNCTION),
    ("string.is_upper", CompletionItemKind::FUNCTION),
    ("string.is_lower", CompletionItemKind::FUNCTION),
    ("string.is_alnum", CompletionItemKind::FUNCTION),
    ("string.is_whitespace", CompletionItemKind::FUNCTION),
    // int
    ("int.parse", CompletionItemKind::FUNCTION),
    ("int.abs", CompletionItemKind::FUNCTION),
    ("int.min", CompletionItemKind::FUNCTION),
    ("int.max", CompletionItemKind::FUNCTION),
    ("int.to_float", CompletionItemKind::FUNCTION),
    ("int.to_string", CompletionItemKind::FUNCTION),
    // float
    ("float.parse", CompletionItemKind::FUNCTION),
    ("float.round", CompletionItemKind::FUNCTION),
    ("float.ceil", CompletionItemKind::FUNCTION),
    ("float.floor", CompletionItemKind::FUNCTION),
    ("float.abs", CompletionItemKind::FUNCTION),
    ("float.to_string", CompletionItemKind::FUNCTION),
    ("float.to_int", CompletionItemKind::FUNCTION),
    ("float.min", CompletionItemKind::FUNCTION),
    ("float.max", CompletionItemKind::FUNCTION),
    // map
    ("map.get", CompletionItemKind::FUNCTION),
    ("map.set", CompletionItemKind::FUNCTION),
    ("map.delete", CompletionItemKind::FUNCTION),
    ("map.keys", CompletionItemKind::FUNCTION),
    ("map.values", CompletionItemKind::FUNCTION),
    ("map.length", CompletionItemKind::FUNCTION),
    ("map.merge", CompletionItemKind::FUNCTION),
    ("map.filter", CompletionItemKind::FUNCTION),
    ("map.map", CompletionItemKind::FUNCTION),
    ("map.entries", CompletionItemKind::FUNCTION),
    ("map.from_entries", CompletionItemKind::FUNCTION),
    ("map.each", CompletionItemKind::FUNCTION),
    ("map.update", CompletionItemKind::FUNCTION),
    // set
    ("set.new", CompletionItemKind::FUNCTION),
    ("set.add", CompletionItemKind::FUNCTION),
    ("set.remove", CompletionItemKind::FUNCTION),
    ("set.contains", CompletionItemKind::FUNCTION),
    ("set.union", CompletionItemKind::FUNCTION),
    ("set.intersection", CompletionItemKind::FUNCTION),
    ("set.difference", CompletionItemKind::FUNCTION),
    ("set.size", CompletionItemKind::FUNCTION),
    ("set.to_list", CompletionItemKind::FUNCTION),
    ("set.from_list", CompletionItemKind::FUNCTION),
    ("set.filter", CompletionItemKind::FUNCTION),
    ("set.map", CompletionItemKind::FUNCTION),
    ("set.fold", CompletionItemKind::FUNCTION),
    ("set.each", CompletionItemKind::FUNCTION),
    ("set.is_subset", CompletionItemKind::FUNCTION),
    // result
    ("result.unwrap_or", CompletionItemKind::FUNCTION),
    ("result.map_ok", CompletionItemKind::FUNCTION),
    ("result.map_err", CompletionItemKind::FUNCTION),
    ("result.flatten", CompletionItemKind::FUNCTION),
    ("result.flat_map", CompletionItemKind::FUNCTION),
    ("result.is_ok", CompletionItemKind::FUNCTION),
    ("result.is_err", CompletionItemKind::FUNCTION),
    // option
    ("option.map", CompletionItemKind::FUNCTION),
    ("option.unwrap_or", CompletionItemKind::FUNCTION),
    ("option.to_result", CompletionItemKind::FUNCTION),
    ("option.is_some", CompletionItemKind::FUNCTION),
    ("option.is_none", CompletionItemKind::FUNCTION),
    // io
    ("io.read_file", CompletionItemKind::FUNCTION),
    ("io.write_file", CompletionItemKind::FUNCTION),
    ("io.read_line", CompletionItemKind::FUNCTION),
    ("io.inspect", CompletionItemKind::FUNCTION),
    ("io.args", CompletionItemKind::FUNCTION),
    // math
    ("math.sqrt", CompletionItemKind::FUNCTION),
    ("math.pow", CompletionItemKind::FUNCTION),
    ("math.log", CompletionItemKind::FUNCTION),
    ("math.log10", CompletionItemKind::FUNCTION),
    ("math.sin", CompletionItemKind::FUNCTION),
    ("math.cos", CompletionItemKind::FUNCTION),
    ("math.tan", CompletionItemKind::FUNCTION),
    ("math.asin", CompletionItemKind::FUNCTION),
    ("math.acos", CompletionItemKind::FUNCTION),
    ("math.atan", CompletionItemKind::FUNCTION),
    ("math.atan2", CompletionItemKind::FUNCTION),
    ("math.pi", CompletionItemKind::CONSTANT),
    ("math.e", CompletionItemKind::CONSTANT),
    // channel
    ("channel.new", CompletionItemKind::FUNCTION),
    ("channel.send", CompletionItemKind::FUNCTION),
    ("channel.receive", CompletionItemKind::FUNCTION),
    ("channel.close", CompletionItemKind::FUNCTION),
    ("channel.try_send", CompletionItemKind::FUNCTION),
    ("channel.try_receive", CompletionItemKind::FUNCTION),
    ("channel.select", CompletionItemKind::FUNCTION),
    // task
    ("task.spawn", CompletionItemKind::FUNCTION),
    ("task.join", CompletionItemKind::FUNCTION),
    ("task.cancel", CompletionItemKind::FUNCTION),
    // regex
    ("regex.is_match", CompletionItemKind::FUNCTION),
    ("regex.find", CompletionItemKind::FUNCTION),
    ("regex.find_all", CompletionItemKind::FUNCTION),
    ("regex.split", CompletionItemKind::FUNCTION),
    ("regex.replace", CompletionItemKind::FUNCTION),
    ("regex.replace_all", CompletionItemKind::FUNCTION),
    ("regex.replace_all_with", CompletionItemKind::FUNCTION),
    ("regex.captures", CompletionItemKind::FUNCTION),
    // json
    ("json.parse", CompletionItemKind::FUNCTION),
    ("json.stringify", CompletionItemKind::FUNCTION),
    ("json.pretty", CompletionItemKind::FUNCTION),
    // test
    ("test.assert", CompletionItemKind::FUNCTION),
    ("test.assert_eq", CompletionItemKind::FUNCTION),
    ("test.assert_ne", CompletionItemKind::FUNCTION),
    // time
    ("time.now", CompletionItemKind::FUNCTION),
    ("time.today", CompletionItemKind::FUNCTION),
    ("time.date", CompletionItemKind::FUNCTION),
    ("time.time", CompletionItemKind::FUNCTION),
    ("time.datetime", CompletionItemKind::FUNCTION),
    ("time.to_datetime", CompletionItemKind::FUNCTION),
    ("time.to_instant", CompletionItemKind::FUNCTION),
    ("time.to_utc", CompletionItemKind::FUNCTION),
    ("time.from_utc", CompletionItemKind::FUNCTION),
    ("time.format", CompletionItemKind::FUNCTION),
    ("time.format_date", CompletionItemKind::FUNCTION),
    ("time.parse", CompletionItemKind::FUNCTION),
    ("time.parse_date", CompletionItemKind::FUNCTION),
    ("time.add_days", CompletionItemKind::FUNCTION),
    ("time.add_months", CompletionItemKind::FUNCTION),
    ("time.add", CompletionItemKind::FUNCTION),
    ("time.since", CompletionItemKind::FUNCTION),
    ("time.hours", CompletionItemKind::FUNCTION),
    ("time.minutes", CompletionItemKind::FUNCTION),
    ("time.seconds", CompletionItemKind::FUNCTION),
    ("time.ms", CompletionItemKind::FUNCTION),
    ("time.weekday", CompletionItemKind::FUNCTION),
    ("time.days_between", CompletionItemKind::FUNCTION),
    ("time.days_in_month", CompletionItemKind::FUNCTION),
    ("time.is_leap_year", CompletionItemKind::FUNCTION),
    ("time.sleep", CompletionItemKind::FUNCTION),
    // http
    ("http.get", CompletionItemKind::FUNCTION),
    ("http.request", CompletionItemKind::FUNCTION),
    ("http.serve", CompletionItemKind::FUNCTION),
    ("http.segments", CompletionItemKind::FUNCTION),
];

// ── Helpers ────────────────────────────────────────────────────────

fn extract_request<R: lsp_types::request::Request>(req: Request) -> Option<(RequestId, R::Params)> {
    req.extract::<R::Params>(R::METHOD).ok()
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

    let init_value = serde_json::to_value(&server_capabilities).unwrap();
    connection.initialize(init_value).unwrap();

    let mut server = Server::new(connection);
    server.run();
    io_threads.join().unwrap();
}
