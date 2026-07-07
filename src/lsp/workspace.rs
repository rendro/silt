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
            collect_references(program, name, &doc.source, &mut spans);
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
        // Phase D: type aliases surface in workspace symbols as a generic
        // type-parameter category — they don't form a new nominal type.
        TypeBody::Alias(_) => SymbolKind::TYPE_PARAMETER,
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

fn collect_references(program: &Program, name: Symbol, source: &str, out: &mut Vec<Span>) {
    for decl in &program.decls {
        collect_references_in_decl(decl, name, source, out);
    }
}

fn collect_references_in_decl(decl: &Decl, name: Symbol, source: &str, out: &mut Vec<Span>) {
    match decl {
        Decl::Fn(f) => {
            // Include the param-pattern binders so renaming a parameter
            // updates the param list AND every body use (round-60 B8).
            for param in &f.params {
                collect_references_in_pattern(&param.pattern, name, source, out);
            }
            // Round-75 DX-4: where-clause trait references.
            for wc in &f.where_clauses {
                if wc.trait_name == name {
                    push_named_span(out, wc.trait_name_span);
                }
            }
            collect_references_in_expr(&f.body, name, source, out);
        }
        Decl::TraitImpl(ti) => {
            if ti.is_auto_derived {
                return;
            }
            // Round-75 DX-4: impl's trait_name and target_type are
            // user-written references — rename of either must update them.
            if ti.trait_name == name {
                push_named_span(out, ti.trait_name_span);
            }
            if ti.target_type == name {
                push_named_span(out, ti.target_type_span);
            }
            for wc in &ti.where_clauses {
                if wc.trait_name == name {
                    push_named_span(out, wc.trait_name_span);
                }
            }
            for method in &ti.methods {
                for param in &method.params {
                    collect_references_in_pattern(&param.pattern, name, source, out);
                }
                for wc in &method.where_clauses {
                    if wc.trait_name == name {
                        push_named_span(out, wc.trait_name_span);
                    }
                }
                collect_references_in_expr(&method.body, name, source, out);
            }
        }
        Decl::Trait(t) => {
            // Round-75 DX-4: supertrait references (`trait Sub: Super`)
            // and trait-level where-clause refs must be tracked so a
            // rename of `Super` updates the supertrait reference too.
            for (super_name, _, super_span) in &t.supertraits {
                if *super_name == name {
                    push_named_span(out, *super_span);
                }
            }
            for wc in &t.param_where_clauses {
                if wc.trait_name == name {
                    push_named_span(out, wc.trait_name_span);
                }
            }
            for method in &t.methods {
                for param in &method.params {
                    collect_references_in_pattern(&param.pattern, name, source, out);
                }
                for wc in &method.where_clauses {
                    if wc.trait_name == name {
                        push_named_span(out, wc.trait_name_span);
                    }
                }
                // Default method bodies, if any.
                collect_references_in_expr(&method.body, name, source, out);
            }
        }
        Decl::Let { value, pattern, .. } => {
            collect_references_in_pattern(pattern, name, source, out);
            collect_references_in_expr(value, name, source, out);
        }
        _ => {}
    }
}

/// Push a span to the references list, skipping synthesized (auto-derive,
/// builtin) spans whose offset is 0 — those are not user-renameable.
fn push_named_span(out: &mut Vec<Span>, span: Span) {
    if span.line == 0 && span.col == 0 && span.offset == 0 {
        return;
    }
    out.push(span);
}

/// Resolve the precise span of a shorthand record-field binder (`{ x }`,
/// `Point { x }`). `record_span` is the record pattern head; we scan
/// forward to the closing `}` for the field-name token. Mirrors
/// `ast_walk::check_shorthand_field_binder` so hover/definition and rename
/// agree on where the binder lives. Returns `None` when the token can't be
/// located (the caller then falls back to the head span).
fn shorthand_binder_span(record_span: Span, field: &str, source: &str) -> Option<Span> {
    let start = record_span.offset.min(source.len());
    let end = source[start..]
        .find('}')
        .map(|p| start + p)
        .unwrap_or(source.len());
    let off = super::text_utils::find_ident_in_range(source, start, end, field)?;
    let line = source[..off].bytes().filter(|&b| b == b'\n').count() + 1;
    let line_start = source[..off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = source[line_start..off].chars().count() + 1;
    Some(Span::with_offset(line, col, off))
}

fn collect_references_in_expr(expr: &Expr, name: Symbol, source: &str, out: &mut Vec<Span>) {
    match &expr.kind {
        ExprKind::Ident(n) if *n == name => {
            out.push(expr.span);
        }
        // Round-71 DX-2 fix: do NOT match on FieldAccess by symbol equality.
        // `FieldAccess.span` is the receiver's span (parser.rs:2598/2674),
        // not the field's, so pushing it here would corrupt the receiver
        // identifier on rename. Field names live in a separate namespace
        // from let/fn names; symbol-collision matching across the two
        // namespaces silently mangled unrelated code (e.g. renaming a
        // top-level `let name` mangled `r.name` into `<newname>.name`).
        ExprKind::Block(stmts) => {
            for s in stmts {
                collect_references_in_stmt(s, name, source, out);
            }
        }
        _ => {
            visit_expr_children(expr, |child| {
                collect_references_in_expr(child, name, source, out);
            });
        }
    }
}

fn collect_references_in_stmt(stmt: &Stmt, name: Symbol, source: &str, out: &mut Vec<Span>) {
    match stmt {
        Stmt::Let { value, pattern, .. } => {
            collect_references_in_pattern(pattern, name, source, out);
            collect_references_in_expr(value, name, source, out);
        }
        Stmt::When {
            expr,
            else_body,
            pattern,
            ..
        } => {
            collect_references_in_pattern(pattern, name, source, out);
            collect_references_in_expr(expr, name, source, out);
            collect_references_in_expr(else_body, name, source, out);
        }
        Stmt::WhenBool {
            condition,
            else_body,
            ..
        } => {
            collect_references_in_expr(condition, name, source, out);
            collect_references_in_expr(else_body, name, source, out);
        }
        Stmt::Expr(e) => collect_references_in_expr(e, name, source, out),
    }
}

fn collect_references_in_pattern(
    pattern: &Pattern,
    name: Symbol,
    source: &str,
    out: &mut Vec<Span>,
) {
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
                collect_references_in_pattern(p, name, source, out);
            }
        }
        PatternKind::Constructor { args: fields, .. } => {
            for p in fields {
                collect_references_in_pattern(p, name, source, out);
            }
        }
        PatternKind::Record { fields, .. } | PatternKind::AnonRecord { fields, .. } => {
            // Round-62 B8 + B9: shorthand binders (`{ x, y }` with `sub
            // = None`) bind the field name itself. Match against `name`
            // so rename across a shorthand binder picks up every site
            // (binder + uses).
            for (fname, sub) in fields {
                if let Some(p) = sub {
                    collect_references_in_pattern(p, name, source, out);
                } else if *fname == name {
                    // Round-100 BROKEN: a shorthand binder (`{ x }`) has
                    // no sub-pattern, so the field name token IS the
                    // binder. `pattern.span` is the record HEAD (the
                    // constructor name for a nominal record, the opening
                    // `{` for an anon record), not the field token —
                    // pushing it made `textDocument/rename` rewrite the
                    // constructor / brace and leave the binder untouched,
                    // corrupting the source into uncompilable code.
                    // Resolve the precise field offset instead.
                    let field_str = crate::intern::resolve(*fname);
                    match shorthand_binder_span(pattern.span, &field_str, source) {
                        Some(span) => out.push(span),
                        None => out.push(pattern.span),
                    }
                }
            }
        }
        _ => {}
    }
}
