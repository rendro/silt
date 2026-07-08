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

use crate::ast::{
    Decl, Expr, ExprKind, FnDecl, Pattern, PatternKind, Program, Stmt, TypeBody, TypeDecl,
    TypeExpr, TypeExprKind,
};
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
            // Round-101: type-position references in the signature
            // (param annotations, return type, where-clause args).
            collect_references_in_fn_signature(f, name, out);
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
                for a in &wc.trait_args {
                    collect_references_in_type_expr(a, name, out);
                }
            }
            // Round-101: type-position references in the impl header
            // (trait args, target type args) and assoc-type bindings.
            for a in &ti.trait_args {
                collect_references_in_type_expr(a, name, out);
            }
            for a in &ti.target_type_args {
                collect_references_in_type_expr(a, name, out);
            }
            for b in &ti.assoc_type_bindings {
                collect_references_in_type_expr(&b.ty, name, out);
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
                collect_references_in_fn_signature(method, name, out);
                collect_references_in_expr(&method.body, name, source, out);
            }
        }
        Decl::Trait(t) => {
            // Round-75 DX-4: supertrait references (`trait Sub: Super`)
            // and trait-level where-clause refs must be tracked so a
            // rename of `Super` updates the supertrait reference too.
            for (super_name, super_args, super_span) in &t.supertraits {
                if *super_name == name {
                    push_named_span(out, *super_span);
                }
                for a in super_args {
                    collect_references_in_type_expr(a, name, out);
                }
            }
            for wc in &t.param_where_clauses {
                if wc.trait_name == name {
                    push_named_span(out, wc.trait_name_span);
                }
                for a in &wc.trait_args {
                    collect_references_in_type_expr(a, name, out);
                }
            }
            // Round-101: assoc-type bound ARGUMENTS (`type Item:
            // TryInto(Pt)`) are type positions. The bound names carry no
            // span in `AssocTypeDecl`, so only the args are walkable.
            for at in &t.assoc_types {
                for (_, bargs) in &at.bounds {
                    for a in bargs {
                        collect_references_in_type_expr(a, name, out);
                    }
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
                collect_references_in_fn_signature(method, name, out);
                // Default method bodies, if any.
                collect_references_in_expr(&method.body, name, source, out);
            }
        }
        Decl::Let {
            value, pattern, ty, ..
        } => {
            collect_references_in_pattern(pattern, name, source, out);
            if let Some(t) = ty {
                collect_references_in_type_expr(t, name, out);
            }
            collect_references_in_expr(value, name, source, out);
        }
        // Round-101: type-decl BODIES reference other types (record
        // field types, enum variant payload types, alias targets) —
        // renaming a type must update them or the decls dangle.
        Decl::Type(t) => match &t.body {
            TypeBody::Record(fields) => {
                for fld in fields {
                    collect_references_in_type_expr(&fld.ty, name, out);
                }
            }
            TypeBody::Enum(variants) => {
                for v in variants {
                    for te in &v.fields {
                        collect_references_in_type_expr(te, name, out);
                    }
                }
            }
            TypeBody::Alias(te) => collect_references_in_type_expr(te, name, out),
        },
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

/// Round-101 BROKEN fix: walk a type expression, collecting `Named` /
/// `Generic` head references to `name`. Type-position references (param
/// annotations, return types, record-field types, ascriptions, …) must
/// be collected alongside value-position references — without them,
/// renaming a user type from its declaration rewrote only the decl and
/// left every annotation/construction/pattern site dangling, breaking
/// the program. `TypeExpr::span` sits on the head-name token for
/// `Named`/`Generic` (parse_type_expr captures it at the ident), so the
/// span is directly edit-safe.
fn collect_references_in_type_expr(te: &TypeExpr, name: Symbol, out: &mut Vec<Span>) {
    match &te.kind {
        TypeExprKind::Named(n) => {
            if *n == name {
                push_named_span(out, te.span);
            }
        }
        TypeExprKind::Generic(n, args) => {
            if *n == name {
                push_named_span(out, te.span);
            }
            for a in args {
                collect_references_in_type_expr(a, name, out);
            }
        }
        TypeExprKind::Tuple(elems) => {
            for e in elems {
                collect_references_in_type_expr(e, name, out);
            }
        }
        TypeExprKind::Function(params, ret) => {
            for p in params {
                collect_references_in_type_expr(p, name, out);
            }
            collect_references_in_type_expr(ret, name, out);
        }
        TypeExprKind::SelfType => {}
        TypeExprKind::AssocProj { receiver, .. } => {
            collect_references_in_type_expr(receiver, name, out);
        }
        TypeExprKind::AnonRecord { fields, .. } => {
            for (_, t) in fields {
                collect_references_in_type_expr(t, name, out);
            }
        }
    }
}

/// Walk one fn signature's type positions: param annotations, the
/// return type, and where-clause trait ARGUMENTS (`where a: TryInto(Pt)`
/// — the trait NAME is already handled by the round-75 DX-4 matching at
/// each call site).
fn collect_references_in_fn_signature(f: &FnDecl, name: Symbol, out: &mut Vec<Span>) {
    for param in &f.params {
        if let Some(ty) = &param.ty {
            collect_references_in_type_expr(ty, name, out);
        }
    }
    if let Some(rt) = &f.return_type {
        collect_references_in_type_expr(rt, name, out);
    }
    for wc in &f.where_clauses {
        for a in &wc.trait_args {
            collect_references_in_type_expr(a, name, out);
        }
    }
}

/// Resolve the span of the head NAME token in a module-qualified head
/// (`util.Pt { .. }` expr, `shapes.Circle(r)` pattern). The AST span for
/// these sits on the module qualifier, not the name — pushing it raw
/// would corrupt the qualifier on rename. Returns `None` (no edit —
/// conservative) when the token can't be located.
fn qualified_head_span(head_span: Span, name: Symbol, source: &str) -> Option<Span> {
    let name_str = resolve_sym(name);
    let off = super::text_utils::qualified_head_name_offset(source, head_span.offset, &name_str)?;
    // Shared offset->Span math — see text_utils::span_at_offset (dedup lock:
    // tests/lsp_span_at_offset_dedup_lock_tests.rs).
    Some(super::text_utils::span_at_offset(source, off))
}

/// Resolve the precise span of a shorthand record-field binder (`{ x }`,
/// `Point { x }`). `record_span` is the record pattern head; the
/// brace-depth-aware scan in `text_utils::find_shorthand_binder` walks the
/// record's own braces only, so a nested braced sub-pattern before the
/// binder (`Point { a: Inner { y }, x }` — round-101 BROKEN) no longer
/// truncates the search at the first `}`. Mirrors
/// `ast_walk::check_shorthand_field_binder` so hover/definition and rename
/// agree on where the binder lives. Returns `None` when the token can't be
/// located (the caller then emits NO edit — never the head span).
fn shorthand_binder_span(record_span: Span, field: &str, source: &str) -> Option<Span> {
    let start = record_span.offset.min(source.len());
    let off = super::text_utils::find_shorthand_binder(source, start, field)?;
    // Offset→Span math is deliberately NOT inlined here — every raw-scan
    // rename/hover site must share text_utils::span_at_offset so the
    // line/col convention (codepoint columns, lexer-compatible) cannot
    // silently diverge between call sites.
    Some(super::text_utils::span_at_offset(source, off))
}

fn collect_references_in_expr(expr: &Expr, name: Symbol, source: &str, out: &mut Vec<Span>) {
    match &expr.kind {
        ExprKind::Ident(n) if *n == name => {
            out.push(expr.span);
        }
        // Round-71 DX-2 fix: do NOT match on FieldAccess by symbol equality.
        // `FieldAccess.span` is the receiver's span (parser.rs:2620/2696),
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
        // Round-101: ascription types (`expr: Point`) are type-position
        // references; `visit_expr_children` only walks the value side.
        ExprKind::Ascription(inner, te) => {
            collect_references_in_type_expr(te, name, out);
            collect_references_in_expr(inner, name, source, out);
        }
        // Round-101: match-arm PATTERNS are not child exprs, so
        // `visit_expr_children` never reaches them — without this arm,
        // pattern heads (`Point { x, .. }`) and pattern binders inside
        // `match` were invisible to references/rename. Mirrors
        // `ast_walk::find_ident_in_expr`, which already visits them.
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                collect_references_in_pattern(&arm.pattern, name, source, out);
            }
            visit_expr_children(expr, |child| {
                collect_references_in_expr(child, name, source, out);
            });
        }
        // Round-101: record-construction HEAD (`Point { x: 3 }`) is a
        // type-name reference. For the bare form `expr.span` sits on the
        // name token; for the qualified form (`util.Pt { .. }`) it sits
        // on the module qualifier, so resolve the name token's own span.
        ExprKind::RecordCreate {
            module, name: head, ..
        } if *head == name => {
            match module {
                None => push_named_span(out, expr.span),
                Some(_) => {
                    if let Some(sp) = qualified_head_span(expr.span, name, source) {
                        out.push(sp);
                    }
                }
            }
            visit_expr_children(expr, |child| {
                collect_references_in_expr(child, name, source, out);
            });
        }
        // Round-102: lambda PARAMS are binders (and may carry type
        // annotations), but `visit_expr_children` walks only the lambda
        // body (ast_walk.rs). Without this arm, renaming a lambda param
        // from a body use-site edited the uses and never the `fn(n)` /
        // `{ n -> ... }` binder token — `fn(n) { m * 2 }` no longer
        // compiles — and `textDocument/references` omitted the binder.
        // Walking `param.ty` also keeps `fn(p: Point) { ... }`
        // annotations in sync on a type rename (mirrors the round-101
        // fn-signature handling in `collect_references_in_fn_signature`).
        ExprKind::Lambda { params, .. } => {
            for param in params {
                collect_references_in_pattern(&param.pattern, name, source, out);
                if let Some(ty) = &param.ty {
                    collect_references_in_type_expr(ty, name, out);
                }
            }
            visit_expr_children(expr, |child| {
                collect_references_in_expr(child, name, source, out);
            });
        }
        // Round-102 (same class): `loop x = init { ... }` binders. The
        // AST keeps only the `Symbol` for a loop binding — no span — so
        // recover the binder token by scanning backward from the init
        // expression's start. Without this, body uses of `x` were
        // renamed while the `loop x = init` binder token was not.
        ExprKind::Loop { bindings, .. } => {
            for (bname, init) in bindings {
                if *bname == name
                    && let Some(sp) = loop_binder_span(init.span, name, source)
                {
                    out.push(sp);
                }
            }
            visit_expr_children(expr, |child| {
                collect_references_in_expr(child, name, source, out);
            });
        }
        _ => {
            visit_expr_children(expr, |child| {
                collect_references_in_expr(child, name, source, out);
            });
        }
    }
}

/// Resolve the span of a `loop` binding's binder token (`x` in
/// `loop x = init { ... }`). Loop bindings carry only a `Symbol` in the
/// AST (parser.rs `parse_loop_expr` discards the ident span), so recover
/// the token by scanning backward from the init expression's start over
/// the mandatory `<binder> = <init>` shape: skip whitespace, require the
/// `=` sign, skip whitespace again, and match the identifier ending
/// there. Returns `None` (no edit — conservative) when the shape doesn't
/// match, so a failed scan can never corrupt unrelated text.
fn loop_binder_span(init_span: Span, name: Symbol, source: &str) -> Option<Span> {
    let init_start = init_span.offset.min(source.len());
    let before = source[..init_start].trim_end();
    // Last non-whitespace byte before the init must be the binding `=`.
    // (`=` is ASCII, so byte indexing is safe; a multibyte char here
    // simply fails the comparison.)
    let eq = before.len().checked_sub(1)?;
    if before.as_bytes()[eq] != b'=' {
        return None;
    }
    let ident_part = before[..eq].trim_end();
    let name_str = resolve_sym(name);
    if !ident_part.ends_with(name_str.as_str()) {
        return None;
    }
    let off = ident_part.len() - name_str.len();
    // Whole-token check: the byte before the match must not be an
    // identifier character (guards `loop xy = ...` matching name `y`).
    if off > 0 {
        let prev = ident_part.as_bytes()[off - 1];
        if prev == b'_' || prev.is_ascii_alphanumeric() {
            return None;
        }
    }
    // Shared offset->Span math — see text_utils::span_at_offset (dedup lock:
    // tests/lsp_span_at_offset_dedup_lock_tests.rs).
    Some(super::text_utils::span_at_offset(source, off))
}

fn collect_references_in_stmt(stmt: &Stmt, name: Symbol, source: &str, out: &mut Vec<Span>) {
    match stmt {
        Stmt::Let { value, pattern, ty } => {
            collect_references_in_pattern(pattern, name, source, out);
            // Round-101: `let p: Point = ...` annotation.
            if let Some(t) = ty {
                collect_references_in_type_expr(t, name, out);
            }
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
    // Round-101: Constructor / nominal-record pattern HEADS are type- or
    // variant-name references (`Point { x }` matches the record type;
    // `Circle(r)` matches the enum variant). Without matching them, a
    // type/variant rename left pattern heads dangling. For the bare form
    // `pattern.span` sits on the head-name token; for the qualified form
    // (`shapes.Circle(r)`) it sits on the module qualifier, so resolve
    // the name token's own span.
    match &pattern.kind {
        PatternKind::Constructor {
            module, name: head, ..
        }
        | PatternKind::Record {
            module,
            name: Some(head),
            ..
        } if *head == name => match module {
            None => push_named_span(out, pattern.span),
            Some(_) => {
                if let Some(sp) = qualified_head_span(pattern.span, name, source) {
                    out.push(sp);
                }
            }
        },
        _ => {}
    }
    // Patterns bind new names, so matching identifier-binding positions
    // here is useful for rename (the binding itself) but not for
    // general reference collection in a reader role. For rename to
    // work correctly, we include the binding site as a reference.
    match &pattern.kind {
        PatternKind::Ident(n) if *n == name => {
            out.push(pattern.span);
        }
        PatternKind::Tuple(pats) | PatternKind::Or(pats) => {
            for p in pats {
                collect_references_in_pattern(p, name, source, out);
            }
        }
        PatternKind::List(pats, rest) => {
            for p in pats {
                collect_references_in_pattern(p, name, source, out);
            }
            // Round-101: the rest sub-pattern (`[h, ..t]`) binds too.
            if let Some(r) = rest {
                collect_references_in_pattern(r, name, source, out);
            }
        }
        PatternKind::Constructor { args: fields, .. } => {
            for p in fields {
                collect_references_in_pattern(p, name, source, out);
            }
        }
        PatternKind::Record { fields, .. } => {
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
                    //
                    // Round-101 BROKEN: when the offset can't be resolved,
                    // push NOTHING. An edit on the record HEAD is never a
                    // correct rename of a field binder — the old
                    // `out.push(pattern.span)` fallback rewrote the
                    // constructor / brace token whenever the scan failed,
                    // producing non-compiling source. Missing one edit is
                    // strictly safer than a wrong edit.
                    let field_str = crate::intern::resolve(*fname);
                    if let Some(span) = shorthand_binder_span(pattern.span, &field_str, source) {
                        out.push(span);
                    }
                }
            }
        }
        PatternKind::AnonRecord { fields, rest } => {
            // Round-101: same shorthand handling as the nominal `Record`
            // arm above, PLUS the named rest binder (`{ x, ...rest }`),
            // which binds `rest` — mirror the typechecker's
            // `collect_pattern_vars`. Both the shorthand field and the
            // rest binder lack a dedicated Pattern node; recover the
            // precise token offset with `shorthand_binder_span`.
            //
            // Round-103: on a failed scan push NOTHING, mirroring the
            // `Record` arm above. `pattern.span` here is the opening `{`
            // of the anon record; the old head-span fallback made a
            // failed binder scan rewrite that brace into the new name —
            // the exact source-corrupting failure mode the round-101 fix
            // removed from the nominal arm. Missing one edit is strictly
            // safer than a wrong edit.
            for (fname, sub) in fields {
                if let Some(p) = sub {
                    collect_references_in_pattern(p, name, source, out);
                } else if *fname == name {
                    let field_str = crate::intern::resolve(*fname);
                    if let Some(span) = shorthand_binder_span(pattern.span, &field_str, source) {
                        out.push(span);
                    }
                }
            }
            if let Some(r) = rest
                && *r == name
            {
                let rest_str = crate::intern::resolve(*r);
                if let Some(span) = shorthand_binder_span(pattern.span, &rest_str, source) {
                    out.push(span);
                }
            }
        }
        PatternKind::Map(entries) => {
            // Round-101: map-pattern values bind (`#{ "k": v }` binds
            // `v`); keys are string literals, never binders.
            for (_, p) in entries {
                collect_references_in_pattern(p, name, source, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::intern;

    /// Build an `AnonRecord` pattern whose head span sits on the `{` at
    /// `offset` in the source under test — exactly where the parser puts
    /// it for an anon-record pattern.
    fn anon_pat(
        fields: Vec<(Symbol, Option<Pattern>)>,
        rest: Option<Symbol>,
        offset: usize,
    ) -> Pattern {
        Pattern::new(
            PatternKind::AnonRecord { fields, rest },
            Span::with_offset(1, offset + 1, offset),
        )
    }

    /// Round-103 lock: when `shorthand_binder_span` cannot locate a
    /// shorthand FIELD binder of an anon-record pattern, the collector
    /// must emit NO span — never the head span. `pattern.span` is the
    /// record's opening `{`; the old fallback made `textDocument/rename`
    /// replace that brace with the new name, producing non-compiling
    /// source (the failure mode round 101 removed from the nominal
    /// `Record` arm). The parser guarantees the scan succeeds for
    /// well-formed source, so the invariant break is simulated with a
    /// pattern whose field is absent from the scanned text.
    #[test]
    fn anon_record_shorthand_scan_failure_emits_no_edit() {
        let source = "{ y: q }";
        let field = intern("r103_missing_field");
        let pat = anon_pat(vec![(field, None)], None, 0);
        let mut out = Vec::new();
        collect_references_in_pattern(&pat, field, source, &mut out);
        assert!(
            out.is_empty(),
            "a failed shorthand-binder scan must yield zero edits, not a \
             head edit on the opening `{{`; got {out:?}"
        );
    }

    /// Same lock for the named REST binder (`{ x, ...rest }`): a failed
    /// scan must not fall back to the head `{`.
    #[test]
    fn anon_record_rest_scan_failure_emits_no_edit() {
        let source = "{ y: q }";
        let rest = intern("r103_missing_rest");
        let pat = anon_pat(vec![], Some(rest), 0);
        let mut out = Vec::new();
        collect_references_in_pattern(&pat, rest, source, &mut out);
        assert!(
            out.is_empty(),
            "a failed rest-binder scan must yield zero edits, not a head \
             edit on the opening `{{`; got {out:?}"
        );
    }

    /// Positive control: for well-formed source the shorthand binder IS
    /// found and the emitted span targets the binder token, never the
    /// pattern head — the fix must not silence real edits.
    #[test]
    fn anon_record_shorthand_scan_success_targets_binder_token() {
        let source = "{ x }";
        let field = intern("x");
        let pat = anon_pat(vec![(field, None)], None, 0);
        let mut out = Vec::new();
        collect_references_in_pattern(&pat, field, source, &mut out);
        assert_eq!(out.len(), 1, "exactly the binder token; got {out:?}");
        assert_eq!(out[0].offset, 2, "span must sit on `x`, got {out:?}");
    }

    /// Positive control for the rest binder: `{ x, ...rest }` emits a
    /// span on the `rest` token itself.
    #[test]
    fn anon_record_rest_scan_success_targets_binder_token() {
        let source = "{ x, ...rest }";
        let rest = intern("rest");
        let pat = anon_pat(vec![(intern("x"), None)], Some(rest), 0);
        let mut out = Vec::new();
        collect_references_in_pattern(&pat, rest, source, &mut out);
        assert_eq!(out.len(), 1, "exactly the rest token; got {out:?}");
        assert_eq!(out[0].offset, 8, "span must sit on `rest`, got {out:?}");
    }
}
