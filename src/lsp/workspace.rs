//! Workspace-wide queries over open documents.
#![allow(deprecated)] // SymbolInformation.deprecated field is LSP-required
//!
//! Backs cross-file goto-definition, `textDocument/references`,
//! `textDocument/rename`, and `workspace/symbol`. All queries iterate
//! `self.documents`; there is no separate index structure. This is
//! O(docs × symbols) per query — fine for reasonable-size workspaces
//! and trivially correct (no index to keep in sync).
//!
//! Scope limitation: only documents the editor has opened are visible.
//! A silt package with many unopened files will not surface them until
//! the user navigates to each. A workspace-root preload on initialize
//! is a natural future extension.

use std::collections::HashSet;

use lsp_types::{Location, SymbolInformation, SymbolKind, Uri};

use crate::ast::{Decl, Expr, ExprKind, Pattern, PatternKind, Program, Stmt, TypeBody, TypeDecl};
use crate::intern::{Symbol, resolve as resolve_sym};
use crate::lexer::Span;

use super::Server;
use super::ast_walk::visit_expr_children;
use super::conversions::span_to_range;

impl Server {
    /// Find every top-level definition of `name` across all open
    /// documents. Returns `(uri, span)` per hit.
    pub(super) fn workspace_lookup_definition(&self, name: Symbol) -> Vec<(Uri, Span)> {
        let mut hits = Vec::new();
        for (uri, doc) in &self.documents {
            if let Some(def) = doc.definitions.get(&name) {
                hits.push((uri.clone(), def.span));
            }
        }
        hits
    }

    /// Find every identifier reference to `name` across all open
    /// documents. Returns `(uri, span)` per hit, including the
    /// definition site. For simplicity we match by `Symbol` equality —
    /// shadowing in inner scopes is not currently distinguished.
    pub(super) fn workspace_find_references(
        &self,
        name: Symbol,
        include_definition: bool,
    ) -> Vec<Location> {
        let mut locations = Vec::new();
        for (uri, doc) in &self.documents {
            let Some(program) = &doc.program else {
                continue;
            };
            let mut spans: Vec<Span> = Vec::new();
            collect_references(program, name, &mut spans);
            if include_definition && let Some(def) = doc.definitions.get(&name) {
                spans.push(def.span);
            }
            // Deduplicate by (offset, line, col) — definition and first
            // use can overlap for top-level `let` bindings.
            let mut seen: HashSet<(usize, usize, usize)> = HashSet::new();
            for span in spans {
                let key = (span.offset, span.line, span.col);
                if seen.insert(key) {
                    locations.push(Location::new(
                        uri.clone(),
                        span_to_range(&span, &doc.source),
                    ));
                }
            }
        }
        locations
    }

    /// Collect workspace symbols matching a query string. Empty query
    /// returns every symbol. Non-empty query does a case-insensitive
    /// substring match — more friendly than exact prefix for
    /// `workspace/symbol` UX.
    pub(super) fn workspace_symbols_matching(&self, query: &str) -> Vec<SymbolInformation> {
        let query_lower = query.to_lowercase();
        let mut results = Vec::new();
        for (uri, doc) in &self.documents {
            let Some(program) = &doc.program else {
                continue;
            };
            for decl in &program.decls {
                match decl {
                    Decl::Fn(f) => {
                        let name = resolve_sym(f.name);
                        if matches_query(&name, &query_lower) {
                            results.push(SymbolInformation {
                                name,
                                kind: SymbolKind::FUNCTION,
                                tags: None,
                                deprecated: None,
                                location: Location::new(
                                    uri.clone(),
                                    span_to_range(&f.span, &doc.source),
                                ),
                                container_name: None,
                            });
                        }
                    }
                    Decl::Type(t) => {
                        push_type_symbols(t, uri, &doc.source, &query_lower, &mut results)
                    }
                    Decl::Trait(tr) => {
                        let name = resolve_sym(tr.name);
                        if matches_query(&name, &query_lower) {
                            results.push(SymbolInformation {
                                name,
                                kind: SymbolKind::INTERFACE,
                                tags: None,
                                deprecated: None,
                                location: Location::new(
                                    uri.clone(),
                                    span_to_range(&tr.span, &doc.source),
                                ),
                                container_name: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        results
    }
}

fn matches_query(name: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    name.to_lowercase().contains(query_lower)
}

fn push_type_symbols(
    t: &TypeDecl,
    uri: &Uri,
    source: &str,
    query_lower: &str,
    results: &mut Vec<SymbolInformation>,
) {
    let name = resolve_sym(t.name);
    let kind = match &t.body {
        TypeBody::Enum(_) => SymbolKind::ENUM,
        TypeBody::Record(_) => SymbolKind::STRUCT,
    };
    if matches_query(&name, query_lower) {
        results.push(SymbolInformation {
            name,
            kind,
            tags: None,
            deprecated: None,
            location: Location::new(uri.clone(), span_to_range(&t.span, source)),
            container_name: None,
        });
    }
    if let TypeBody::Enum(variants) = &t.body {
        let container = resolve_sym(t.name);
        for v in variants {
            let vname = resolve_sym(v.name);
            if matches_query(&vname, query_lower) {
                results.push(SymbolInformation {
                    name: vname,
                    kind: SymbolKind::ENUM_MEMBER,
                    tags: None,
                    deprecated: None,
                    location: Location::new(uri.clone(), span_to_range(&t.span, source)),
                    container_name: Some(container.clone()),
                });
            }
        }
    }
}

// ── AST walk for references ────────────────────────────────────────

fn collect_references(program: &Program, name: Symbol, out: &mut Vec<Span>) {
    for decl in &program.decls {
        collect_references_in_decl(decl, name, out);
    }
}

fn collect_references_in_decl(decl: &Decl, name: Symbol, out: &mut Vec<Span>) {
    match decl {
        Decl::Fn(f) => {
            // Include the param-pattern binders so renaming a parameter
            // updates the param list AND every body use (round-60 B8).
            for param in &f.params {
                collect_references_in_pattern(&param.pattern, name, out);
            }
            collect_references_in_expr(&f.body, name, out);
        }
        Decl::TraitImpl(ti) => {
            for method in &ti.methods {
                for param in &method.params {
                    collect_references_in_pattern(&param.pattern, name, out);
                }
                collect_references_in_expr(&method.body, name, out);
            }
        }
        Decl::Trait(t) => {
            for method in &t.methods {
                for param in &method.params {
                    collect_references_in_pattern(&param.pattern, name, out);
                }
                // Default method bodies, if any.
                collect_references_in_expr(&method.body, name, out);
            }
        }
        Decl::Let { value, pattern, .. } => {
            collect_references_in_pattern(pattern, name, out);
            collect_references_in_expr(value, name, out);
        }
        _ => {}
    }
}

fn collect_references_in_expr(expr: &Expr, name: Symbol, out: &mut Vec<Span>) {
    match &expr.kind {
        ExprKind::Ident(n) if *n == name => {
            out.push(expr.span);
        }
        ExprKind::FieldAccess(obj, field) if *field == name => {
            out.push(expr.span);
            collect_references_in_expr(obj, name, out);
        }
        ExprKind::Block(stmts) => {
            for s in stmts {
                collect_references_in_stmt(s, name, out);
            }
        }
        _ => {
            visit_expr_children(expr, |child| {
                collect_references_in_expr(child, name, out);
            });
        }
    }
}

fn collect_references_in_stmt(stmt: &Stmt, name: Symbol, out: &mut Vec<Span>) {
    match stmt {
        Stmt::Let { value, pattern, .. } => {
            collect_references_in_pattern(pattern, name, out);
            collect_references_in_expr(value, name, out);
        }
        Stmt::When {
            expr,
            else_body,
            pattern,
            ..
        } => {
            collect_references_in_pattern(pattern, name, out);
            collect_references_in_expr(expr, name, out);
            collect_references_in_expr(else_body, name, out);
        }
        Stmt::WhenBool {
            condition,
            else_body,
            ..
        } => {
            collect_references_in_expr(condition, name, out);
            collect_references_in_expr(else_body, name, out);
        }
        Stmt::Expr(e) => collect_references_in_expr(e, name, out),
    }
}

fn collect_references_in_pattern(pattern: &Pattern, name: Symbol, out: &mut Vec<Span>) {
    // Patterns bind new names, so matching identifier-binding positions
    // here is useful for rename (the binding itself) but not for
    // general reference collection in a reader role. For rename to
    // work correctly, we include the binding site as a reference.
    match &pattern.kind {
        PatternKind::Ident(n) if *n == name => {
            out.push(pattern.span);
        }
        PatternKind::Tuple(pats) | PatternKind::List(pats, _) | PatternKind::Or(pats) => {
            for p in pats {
                collect_references_in_pattern(p, name, out);
            }
        }
        PatternKind::Constructor(_, fields) => {
            for p in fields {
                collect_references_in_pattern(p, name, out);
            }
        }
        PatternKind::Record { fields, .. } => {
            for (_, sub) in fields {
                if let Some(p) = sub {
                    collect_references_in_pattern(p, name, out);
                }
            }
        }
        _ => {}
    }
}
