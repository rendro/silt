//! Workspace-wide queries over open documents.
#![allow(deprecated)] // SymbolInformation.deprecated field is LSP-required
//!
//! Backs the workspace fallback of goto-definition,
//! `textDocument/references`, `textDocument/rename`, and
//! `workspace/symbol`. All queries iterate `self.documents`: the open
//! documents and the workspace files the preload indexed. This is
//! O(docs × symbols) per query — fine for reasonable-size workspaces
//! and trivially correct (no index to keep in sync). Names are matched
//! by symbol; a member of an imported module is resolved through the
//! session first (see `modules.rs`).

use std::collections::HashSet;

use lsp_types::{Location, SymbolInformation, SymbolKind, Uri};

use crate::ast::{
    Decl, Expr, ExprKind, FnDecl, Pattern, PatternKind, Program, Stmt, TypeBody, TypeDecl,
    TypeExpr, TypeExprKind,
};
use crate::intern::{Symbol, resolve as resolve_sym};
use crate::source::{SourceFile, Span};

use super::Server;
use super::ast_walk::visit_expr_children;
use super::conversions::span_to_range;

impl Server {
    /// Find every top-level definition of `name` across all
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

    /// Find every identifier reference to `name` across all
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
            collect_references(program, name, &doc.source.text, &mut spans);
            if include_definition && let Some(def) = doc.definitions.get(&name) {
                spans.push(def.span);
            }
            // Deduplicate by start offset — definition and first use can
            // overlap for top-level `let` bindings.
            let mut seen: HashSet<u32> = HashSet::new();
            for span in spans {
                let key = span.start;
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
    source: &SourceFile,
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
                    location: Location::new(uri.clone(), span_to_range(&v.name_span, source)),
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

/// The span of `name` written at the start of `span`: the head name of a
/// type expression, record literal or pattern whose span covers more
/// than the name (`Box(Int)`, `Point { x: 1 }`, `Circle(r)`).
fn head_name_span(span: Span, name: Symbol) -> Span {
    Span {
        end: span.start + resolve_sym(name).len() as u32,
        ..span
    }
}

/// Push a span to the references list, skipping spans of silt's own
/// declarations (`Span::BUILTIN`) — those are not user-renameable.
fn push_named_span(out: &mut Vec<Span>, span: Span) {
    if !span.is_in_source() {
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
/// the program. A `Named`/`Generic` type expression starts with its
/// head name, so the name's span is the start of `TypeExpr::span`.
fn collect_references_in_type_expr(te: &TypeExpr, name: Symbol, out: &mut Vec<Span>) {
    match &te.kind {
        TypeExprKind::Named(n) => {
            if *n == name {
                push_named_span(out, head_name_span(te.span, name));
            }
        }
        TypeExprKind::Generic(n, args) => {
            if *n == name {
                push_named_span(out, head_name_span(te.span, name));
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
/// these starts at the module qualifier, not the name — pushing it raw
/// would corrupt the qualifier on rename. Returns `None` (no edit —
/// conservative) when the token can't be located.
fn qualified_head_span(head_span: Span, name: Symbol, source: &str) -> Option<Span> {
    let name_str = resolve_sym(name);
    let off =
        super::text_utils::qualified_head_name_offset(source, head_span.start as usize, &name_str)?;
    Some(Span {
        file: head_span.file,
        start: off as u32,
        end: (off + name_str.len()) as u32,
    })
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
                None => push_named_span(out, head_name_span(expr.span, name)),
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
        // from a body use-site edited the uses and never the
        // `{ n -> ... }` binder token — `{ n -> m * 2 }` no longer
        // compiles — and `textDocument/references` omitted the binder.
        // Walking `param.ty` also keeps `{ p: Point -> ... }`
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
        // Round-102 (same class): `loop x = init { ... }` binders, at
        // the span of the binder's name. Without this, body uses of `x`
        // were renamed while the `loop x = init` binder token was not.
        ExprKind::Loop { bindings, .. } => {
            for (bname, bspan, _) in bindings {
                if *bname == name {
                    out.push(*bspan);
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
    // `pattern.span` starts with the head name; for the qualified form
    // (`shapes.Circle(r)`) it starts with the module qualifier, so
    // resolve the name token's own span.
    match &pattern.kind {
        PatternKind::Constructor {
            module, name: head, ..
        }
        | PatternKind::Record {
            module,
            name: Some(head),
            ..
        } if *head == name => match module {
            None => push_named_span(out, head_name_span(pattern.span, name)),
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
        PatternKind::Record { fields, .. } | PatternKind::AnonRecord { fields, .. } => {
            // Round-62 B8 + B9: shorthand binders (`{ x, y }` with `sub
            // = None`) bind the field name itself, at the field name's
            // own span. Match against `name` so rename across a
            // shorthand binder picks up every site (binder + uses).
            for (fname, fspan, sub) in fields {
                if let Some(p) = sub {
                    collect_references_in_pattern(p, name, source, out);
                } else if *fname == name {
                    out.push(*fspan);
                }
            }
            // Round-101: the named rest binder (`{ x, ...rest }`) binds
            // `rest` — mirror the typechecker's `collect_pattern_vars`.
            if let PatternKind::AnonRecord {
                rest: Some((r, rspan)),
                ..
            } = &pattern.kind
                && *r == name
            {
                out.push(*rspan);
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
    use crate::source::FileId;

    fn span(start: u32, end: u32) -> Span {
        Span {
            file: FileId::default(),
            start,
            end,
        }
    }

    /// Shorthand field and rest binders of an anon-record pattern are
    /// collected at their own spans, never at the pattern head (the
    /// opening `{`): an edit on the head rewrote the brace into the new
    /// name, producing non-compiling source (rounds 100, 101, 103).
    #[test]
    fn anon_record_binders_are_collected_at_their_own_spans() {
        // `{ x, ...rest }`
        let x = intern("x");
        let rest = intern("rest");
        let pat = Pattern::new(
            PatternKind::AnonRecord {
                fields: vec![(x, span(2, 3), None)],
                rest: Some((rest, span(8, 12))),
            },
            span(0, 14),
        );
        let mut out = Vec::new();
        collect_references_in_pattern(&pat, x, "{ x, ...rest }", &mut out);
        assert_eq!(out, vec![span(2, 3)]);
        let mut out = Vec::new();
        collect_references_in_pattern(&pat, rest, "{ x, ...rest }", &mut out);
        assert_eq!(out, vec![span(8, 12)]);
    }

    /// The head of a constructor pattern is collected as the name alone,
    /// not the whole pattern: a rename edits `Circle`, not `Circle(r)`.
    #[test]
    fn a_pattern_head_is_collected_as_its_name() {
        let circle = intern("Circle");
        let pat = Pattern::new(
            PatternKind::Constructor {
                module: None,
                name: circle,
                args: vec![Pattern::new(PatternKind::Ident(intern("r")), span(7, 8))],
            },
            span(0, 9),
        );
        let mut out = Vec::new();
        collect_references_in_pattern(&pat, circle, "Circle(r)", &mut out);
        assert_eq!(out, vec![span(0, 6)]);
    }
}
