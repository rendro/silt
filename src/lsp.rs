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
use lsp_types::request::{GotoDefinition, HoverRequest, Request as _};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, GotoDefinitionResponse, Hover, HoverContents,
    HoverProviderCapability, Location, MarkupContent, MarkupKind,
    OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

use crate::ast::*;
use crate::lexer::{Lexer, Span};
use crate::parser::Parser;
use crate::typechecker;
use crate::types::Type;

// ── Document state ─────────────────────────────────────────────────

struct DefInfo {
    span: Span,
    ty: Option<Type>,
}

struct Document {
    source: String,
    program: Option<Program>,
    /// Definition map: name → definition info (built from top-level declarations).
    definitions: HashMap<String, DefInfo>,
}

// ── Span ↔ LSP conversion ─────────────────────────────────────────

fn span_to_position(span: &Span) -> Position {
    // The lexer captures span AFTER advancing past the first char of a token,
    // so col is 1 past the actual start. Subtract 2: one for 1-based→0-based,
    // one for the lexer's off-by-one.
    Position::new(
        span.line.saturating_sub(1) as u32,
        span.col.saturating_sub(2) as u32,
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
            return offset + (pos.character as usize).min(line.len());
        }
        offset += line.len() + 1; // +1 for '\n'
    }
    offset
}

// ── Server ─────────────────────────────────────────────────────────

struct Server {
    connection: Connection,
    documents: HashMap<Uri, Document>,
}

impl Server {
    fn new(connection: Connection) -> Self {
        Server {
            connection,
            documents: HashMap::new(),
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
                let params: lsp_types::DidOpenTextDocumentParams =
                    serde_json::from_value(notif.params).unwrap();
                let uri = params.text_document.uri;
                let source = params.text_document.text;
                self.update_document(uri, source);
            }
            DidChangeTextDocument::METHOD => {
                let params: lsp_types::DidChangeTextDocumentParams =
                    serde_json::from_value(notif.params).unwrap();
                let uri = params.text_document.uri;
                // We use full sync, so the first content change is the full text.
                if let Some(change) = params.content_changes.into_iter().next() {
                    self.update_document(uri, change.text);
                }
            }
            DidCloseTextDocument::METHOD => {
                let params: lsp_types::DidCloseTextDocumentParams =
                    serde_json::from_value(notif.params).unwrap();
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
                let (id, params) = extract_request::<HoverRequest>(req);
                let result = self.hover(params);
                let resp = Response::new_ok(id, result);
                self.connection.sender.send(Message::Response(resp)).ok();
            }
            GotoDefinition::METHOD => {
                let (id, params) = extract_request::<GotoDefinition>(req);
                let result = self.goto_definition(params);
                let resp = Response::new_ok(id, result);
                self.connection.sender.send(Message::Response(resp)).ok();
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
                diagnostics.push(make_diagnostic(&e.message, &e.span, DiagnosticSeverity::ERROR));
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
            diagnostics.push(make_diagnostic(&e.message, &e.span, DiagnosticSeverity::ERROR));
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
        let notif = lsp_server::Notification::new(
            PublishDiagnostics::METHOD.to_string(),
            params,
        );
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
}

// ── Type display helpers ───────────────────────────────────────────

/// Returns true if the type contains any unresolved type variables (e.g. Var(189)).
fn has_unresolved_vars(ty: &Type) -> bool {
    match ty {
        Type::Var(_) => true,
        Type::Fun(params, ret) => params.iter().any(has_unresolved_vars) || has_unresolved_vars(ret),
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
                // Build the function type from param types and return type.
                let fn_ty = build_fn_type(f);
                defs.insert(f.name.clone(), DefInfo { span: f.span, ty: fn_ty });
            }
            Decl::Type(t) => {
                defs.insert(t.name.clone(), DefInfo { span: t.span, ty: None });
                if let TypeBody::Enum(variants) = &t.body {
                    for v in variants {
                        defs.insert(v.name.clone(), DefInfo { span: t.span, ty: None });
                    }
                }
            }
            Decl::Trait(t) => {
                defs.insert(t.name.clone(), DefInfo { span: t.span, ty: None });
            }
            Decl::Let { pattern, span, value, .. } => {
                if let Pattern::Ident(name) = pattern {
                    defs.insert(name.clone(), DefInfo { span: *span, ty: value.ty.clone() });
                }
            }
            _ => {}
        }
    }
    defs
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
    let param_names: Vec<&str> = f.params.iter().filter_map(|p| {
        if let Pattern::Ident(name) = &p.pattern { Some(name.as_str()) } else { None }
    }).collect();

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
    if let ExprKind::Ident(n) = &expr.kind {
        if n == name {
            return expr.ty.clone();
        }
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

/// The lexer records span.offset AFTER consuming the first character of a token,
/// so the actual start byte of a token is `span.offset - 1`.
fn token_start(span: &Span) -> usize {
    span.offset.saturating_sub(1)
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
    if cursor >= start {
        if let Some(ref ty) = expr.ty {
            *best = Some(ty);
        }
    }

    // Recurse into children (inlined to satisfy the borrow checker).
    match &expr.kind {
        ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
            find_type_in_expr(l, cursor, best); find_type_in_expr(r, cursor, best);
        }
        ExprKind::Unary(_, e) | ExprKind::QuestionMark(e) | ExprKind::Return(Some(e))
        | ExprKind::FieldAccess(e, _) => find_type_in_expr(e, cursor, best),
        ExprKind::Call(callee, args) => {
            find_type_in_expr(callee, cursor, best);
            for a in args { find_type_in_expr(a, cursor, best); }
        }
        ExprKind::Lambda { body, .. } => find_type_in_expr(body, cursor, best),
        ExprKind::Match { expr, arms } => {
            if let Some(e) = expr { find_type_in_expr(e, cursor, best); }
            for arm in arms {
                if let Some(ref g) = arm.guard { find_type_in_expr(g, cursor, best); }
                find_type_in_expr(&arm.body, cursor, best);
            }
        }
        ExprKind::Block(stmts) => for stmt in stmts {
            match stmt {
                Stmt::Let { value, .. } => find_type_in_expr(value, cursor, best),
                Stmt::Expr(e) => find_type_in_expr(e, cursor, best),
                Stmt::When { expr, else_body, .. } => {
                    find_type_in_expr(expr, cursor, best);
                    find_type_in_expr(else_body, cursor, best);
                }
                Stmt::WhenBool { condition, else_body } => {
                    find_type_in_expr(condition, cursor, best);
                    find_type_in_expr(else_body, cursor, best);
                }
            }
        },
        ExprKind::List(elems) => for elem in elems {
            match elem {
                ListElem::Single(e) | ListElem::Spread(e) => find_type_in_expr(e, cursor, best),
            }
        },
        ExprKind::Map(entries) => for (k, v) in entries {
            find_type_in_expr(k, cursor, best); find_type_in_expr(v, cursor, best);
        },
        ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
            for e in elems { find_type_in_expr(e, cursor, best); }
        }
        ExprKind::RecordCreate { fields, .. } => {
            for (_, v) in fields { find_type_in_expr(v, cursor, best); }
        }
        ExprKind::RecordUpdate { expr, fields, .. } => {
            find_type_in_expr(expr, cursor, best);
            for (_, v) in fields { find_type_in_expr(v, cursor, best); }
        }
        ExprKind::Loop { bindings, body } => {
            for (_, init) in bindings { find_type_in_expr(init, cursor, best); }
            find_type_in_expr(body, cursor, best);
        }
        ExprKind::Recur(args) => for a in args { find_type_in_expr(a, cursor, best); },
        ExprKind::StringInterp(parts) => for part in parts {
            if let StringPart::Expr(e) = part { find_type_in_expr(e, cursor, best); }
        },
        _ => {}
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
            f(lhs); f(rhs);
        }
        ExprKind::Unary(_, e) | ExprKind::QuestionMark(e) | ExprKind::Return(Some(e))
        | ExprKind::FieldAccess(e, _) => f(e),
        ExprKind::Call(callee, args) => {
            f(callee);
            for a in args { f(a); }
        }
        ExprKind::Lambda { body, .. } => f(body),
        ExprKind::Match { expr, arms } => {
            if let Some(e) = expr { f(e); }
            for arm in arms {
                if let Some(ref guard) = arm.guard { f(guard); }
                f(&arm.body);
            }
        }
        ExprKind::Block(stmts) => {
            for stmt in stmts {
                match stmt {
                    Stmt::Let { value, .. } => f(value),
                    Stmt::Expr(e) => f(e),
                    Stmt::When { expr, else_body, .. } => { f(expr); f(else_body); }
                    Stmt::WhenBool { condition, else_body } => { f(condition); f(else_body); }
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
        ExprKind::Map(entries) => { for (k, v) in entries { f(k); f(v); } }
        ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => { for e in elems { f(e); } }
        ExprKind::RecordCreate { fields, .. } => { for (_, v) in fields { f(v); } }
        ExprKind::RecordUpdate { expr, fields, .. } => {
            f(expr);
            for (_, v) in fields { f(v); }
        }
        ExprKind::Loop { bindings, body } => {
            for (_, init) in bindings { f(init); }
            f(body);
        }
        ExprKind::Recur(args) => { for a in args { f(a); } }
        ExprKind::StringInterp(parts) => {
            for part in parts {
                if let StringPart::Expr(e) = part { f(e); }
            }
        }
        _ => {}
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn extract_request<R: lsp_types::request::Request>(
    req: Request,
) -> (RequestId, R::Params) {
    let (id, params) = req.extract::<R::Params>(R::METHOD).unwrap();
    (id, params)
}

// ── Entry point ────────────────────────────────────────────────────

pub fn run() {
    let (connection, io_threads) = Connection::stdio();

    // Read initialize request and respond with capabilities.
    let server_capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        ..ServerCapabilities::default()
    };

    let init_value = serde_json::to_value(&server_capabilities).unwrap();
    connection.initialize(init_value).unwrap();

    let mut server = Server::new(connection);
    server.run();
    io_threads.join().unwrap();
}
