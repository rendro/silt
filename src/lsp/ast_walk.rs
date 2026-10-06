//! Generic AST walkers used by the LSP handlers.
//!
//! Handlers use byte-offset cursor positions to locate the expression,
//! identifier, or binding relevant to a request. These helpers traverse
//! the typed AST produced by the parser/typechecker and surface the
//! deepest (most specific) match for a given cursor.

use crate::ast::*;
use crate::intern::{Symbol, intern};
use crate::types::Type;

// ── Type display helpers ───────────────────────────────────────────

/// Returns true if the type contains any unresolved type variables (e.g. Var(189)).
pub(super) fn has_unresolved_vars(ty: &Type) -> bool {
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

// ── AST walkers (offset-based) ─────────────────────────────────────

pub(super) fn token_start(span: &crate::source::Span) -> usize {
    span.start as usize
}

/// Find the inferred type of the deepest expression at the cursor byte offset.
pub(super) fn find_type_at_offset(program: &Program, cursor: usize) -> Option<Type> {
    let mut best: Option<Type> = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                find_type_in_expr(&f.body, cursor, &mut best);
            }
            Decl::Let { value, .. } => {
                find_type_in_expr(value, cursor, &mut best);
            }
            Decl::TraitImpl(ti) => {
                // Skip auto-derived (synthesized) impls: their AST nodes
                // all carry the span of the type declaration, so the walk
                // would treat them as "at" any cursor in the declaration
                // and pollute hover/find-type results with the
                // synthesized body's intermediate types. The user never
                // wrote them; LSP affordances should ignore them.
                if ti.is_auto_derived {
                    continue;
                }
                for method in &ti.methods {
                    find_type_in_expr(&method.body, cursor, &mut best);
                }
            }
            // A default method's body is typed where it is written: in
            // the trait. (The copy an impl that omits the method gets
            // carries no types.)
            Decl::Trait(t) => {
                for method in &t.methods {
                    find_type_in_expr(&method.body, cursor, &mut best);
                }
            }
            _ => {}
        }
    }
    best
}

/// Recurse depth-first into `expr`, updating `best` with the deepest
/// `expr.ty` whose span holds the cursor. Delegates the
/// child enumeration to [`visit_expr_children`] — there is no second
/// inlined copy of the `ExprKind::*` arms here. The pre-check on the
/// outer expression runs first so a deeper match overwrites it.
///
/// Round-85 dedup: prior to this refactor this function inlined a
/// ~100-line second copy of `visit_expr_children`'s arm table. Drift
/// in either copy would silently miss expression kinds; the two are
/// now collapsed to a single source of truth. The `Option<&'a Type>`
/// previously used to avoid an intermediate clone is now `Option<Type>`
/// because `visit_expr_children`'s `FnMut(&Expr)` closure must accept
/// any borrow lifetime (HRTB) and cannot escape a `&'a Type` borrow.
/// The clone is cheap (one per visited typed expression on the cursor
/// path) and the only caller, `find_type_at_offset`, was already
/// `.cloned()`-ing the final result anyway.
fn find_type_in_expr(expr: &Expr, cursor: usize, best: &mut Option<Type>) {
    // The cursor must be inside this expression's span. We rely on
    // depth-first traversal: the deepest (most specific) match wins.
    if (expr.span.start as usize..expr.span.end as usize).contains(&cursor)
        && let Some(ref ty) = expr.ty
    {
        *best = Some(ty.clone());
    }
    // A match arm's pattern is inside the match but is no expression: on
    // the head name of an arm's constructor or record pattern
    // (`Circle(r) -> ...`), the type is the scrutinee's, the innermost
    // node that has one. A binder inside the pattern is a local binding,
    // which the callers look up first.
    if let ExprKind::Match {
        expr: Some(scrutinee),
        arms,
    } = &expr.kind
    {
        for arm in arms {
            if let Some(head) = pattern_head_span(&arm.pattern)
                && let Some(ref ty) = scrutinee.ty
            {
                if (head.start as usize..head.end as usize).contains(&cursor) {
                    *best = Some(ty.clone());
                }
            }
        }
    }
    visit_expr_children(expr, |child| find_type_in_expr(child, cursor, best));
}

/// The span of the head name of a constructor or record pattern.
fn pattern_head_span(pattern: &Pattern) -> Option<crate::source::Span> {
    match &pattern.kind {
        PatternKind::Constructor { name_span, .. }
        | PatternKind::Record {
            name: Some(_),
            name_span,
            ..
        } => Some(*name_span),
        _ => None,
    }
}

/// Find the identifier name at the cursor byte offset.
///
/// Visits `ExprKind::Ident` use-sites AND binding sites:
///   * `let xvar = ...` / `match` arm pattern binders / `fn foo(x)` params,
///   * `fn foo(...)` declaration name (its `name_span`).
///
/// Without the binding-site visits, `prepareRename`/`rename`/`hover` on the
/// LHS of a let, on a `fn` parameter, or on a `fn` declaration name would
/// silently no-op (round-60 B8 + G4).
pub(super) fn find_ident_at_offset(program: &Program, cursor: usize) -> Option<Symbol> {
    let mut best: Option<Symbol> = None;
    for decl in &program.decls {
        find_ident_in_decl(decl, cursor, &mut best);
    }
    best
}

fn find_ident_in_decl(decl: &Decl, cursor: usize, best: &mut Option<Symbol>) {
    match decl {
        Decl::Fn(f) => {
            check_span_match(f.name, f.name_span, cursor, best);
            for param in &f.params {
                find_ident_in_pattern(&param.pattern, cursor, best);
            }
            // Round-75 DX-4: where-clause trait references must be
            // walkable so cursor on a `where a: Greet` trait name
            // resolves to the trait — without this, rename of `Greet`
            // skipped every where-clause reference.
            for wc in &f.where_clauses {
                check_span_match(wc.trait_name, wc.trait_name_span, cursor, best);
            }
            // Round-101: type-position references in the signature
            // (param annotations, return type, where-clause args).
            find_ident_in_fn_signature(f, cursor, best);
            find_ident_in_expr(&f.body, cursor, best);
        }
        Decl::Let {
            pattern, value, ty, ..
        } => {
            find_ident_in_pattern(pattern, cursor, best);
            if let Some(t) = ty {
                find_ident_in_type_expr(t, cursor, best);
            }
            find_ident_in_expr(value, cursor, best);
        }
        Decl::TraitImpl(ti) => {
            if ti.is_auto_derived {
                return;
            }
            // Round-75 DX-4: TraitImpl's trait_name and target_type are
            // user-written references that LSP rename / references /
            // goto-def must resolve. Without these, cursor on `Greet`
            // or `Int` in `trait Greet for Int { ... }` returned None.
            check_span_match(ti.trait_name, ti.trait_name_span, cursor, best);
            check_span_match(ti.target_type, ti.target_type_span, cursor, best);
            // Round-101: type-position references in the impl header
            // (trait args, target type args) and assoc-type bindings.
            for a in &ti.trait_args {
                find_ident_in_type_expr(a, cursor, best);
            }
            for a in &ti.target_type_args {
                find_ident_in_type_expr(a, cursor, best);
            }
            for b in &ti.assoc_type_bindings {
                find_ident_in_type_expr(&b.ty, cursor, best);
            }
            // Impl-level where-clause trait refs.
            for wc in &ti.where_clauses {
                check_span_match(wc.trait_name, wc.trait_name_span, cursor, best);
                for a in &wc.trait_args {
                    find_ident_in_type_expr(a, cursor, best);
                }
            }
            for method in &ti.methods {
                check_span_match(method.name, method.name_span, cursor, best);
                for param in &method.params {
                    find_ident_in_pattern(&param.pattern, cursor, best);
                }
                // Method-level where-clause trait refs (rare today but
                // legal: `fn foo(self, x: a) where a: Compare` in an impl).
                for wc in &method.where_clauses {
                    check_span_match(wc.trait_name, wc.trait_name_span, cursor, best);
                }
                find_ident_in_fn_signature(method, cursor, best);
                find_ident_in_expr(&method.body, cursor, best);
            }
        }
        Decl::Type(t) => {
            // Match the cursor against the type name at the `type Name`
            // binder. Phase-1 doc surfacing requires hover to identify
            // the decl binder so `DefInfo.doc` can be looked up.
            check_span_match(t.name, t.name_span, cursor, best);
            // Round-101: type-decl BODIES reference other types (record
            // field types, enum variant payload types, alias targets).
            // Enum variant NAME binders (`Circle` in `Circle(Int)`) also
            // resolve: the parser records each variant's `name_span`, so
            // cursor on a variant's own declaration works for
            // prepareRename / rename / goto-def. Without this, rename on
            // the variant decl line returned null while usage sites
            // renamed fine.
            match &t.body {
                TypeBody::Record(fields) => {
                    for fld in fields {
                        find_ident_in_type_expr(&fld.ty, cursor, best);
                    }
                }
                TypeBody::Enum(variants) => {
                    for v in variants {
                        check_span_match(v.name, v.name_span, cursor, best);
                        for te in &v.fields {
                            find_ident_in_type_expr(te, cursor, best);
                        }
                    }
                }
                TypeBody::Alias(te) => find_ident_in_type_expr(te, cursor, best),
            }
        }
        Decl::Trait(t) => {
            // Round-75 DX-2: the parser-recorded name_span.
            check_span_match(t.name, t.name_span, cursor, best);
            // Round-75 DX-4: supertrait references and trait-level
            // where-clause trait refs.
            for r in &t.supertraits {
                check_span_match(r.name, r.span, cursor, best);
                for a in &r.args {
                    find_ident_in_type_expr(a, cursor, best);
                }
            }
            for wc in &t.param_where_clauses {
                check_span_match(wc.trait_name, wc.trait_name_span, cursor, best);
                for a in &wc.trait_args {
                    find_ident_in_type_expr(a, cursor, best);
                }
            }
            // Round-101: assoc-type bounds, and their ARGUMENTS, which
            // are type positions.
            for at in &t.assoc_types {
                for bound in &at.bounds {
                    check_span_match(bound.name, bound.span, cursor, best);
                    for a in &bound.args {
                        find_ident_in_type_expr(a, cursor, best);
                    }
                }
            }
            // Trait method binders + default method bodies — round-62 B11.
            // `Decl::Fn` and `Decl::TraitImpl` walk param patterns AND the
            // method body so hover/rename works on default-method param
            // and body identifiers too. Without this, cursor on `x` in
            // `trait T { fn foo(x: Int) -> Int { x + 1 } }` returned None.
            for method in &t.methods {
                check_span_match(method.name, method.name_span, cursor, best);
                for param in &method.params {
                    find_ident_in_pattern(&param.pattern, cursor, best);
                }
                for wc in &method.where_clauses {
                    check_span_match(wc.trait_name, wc.trait_name_span, cursor, best);
                }
                find_ident_in_fn_signature(method, cursor, best);
                find_ident_in_expr(&method.body, cursor, best);
            }
        }
        _ => {}
    }
}

/// Match the cursor against an identifier whose span is known precisely.
/// Used by trait-name / target-type / supertrait references where the
/// parser records the name's span directly (no source-scan fallback
/// needed). Spans of silt's own declarations (`Span::BUILTIN`) are
/// skipped — they have no user-renameable source location.
fn check_span_match(
    name: Symbol,
    span: crate::source::Span,
    cursor: usize,
    best: &mut Option<Symbol>,
) {
    // Builtin spans (builtin trait decls) are not in the document and
    // would spuriously match cursor 0 on a fresh document.
    if !span.is_in_source() {
        return;
    }
    let name_str = crate::intern::resolve(name);
    let start = span.start as usize;
    let end = start + name_str.len();
    if cursor >= start && cursor < end {
        *best = Some(name);
    }
}

/// Round-101 BROKEN fix: match the cursor against type-position
/// references inside a type expression (`Named` / `Generic` heads),
/// recursing into nested type args. Mirrors
/// `workspace::collect_references_in_type_expr` so goto-definition /
/// references / prepareRename resolve cursors sitting on a param
/// annotation, return type, record-field type, or ascription —
/// previously those cursors returned `None` and every type-position
/// affordance silently no-opped. `TypeExpr::span` sits on the head-name
/// token for `Named`/`Generic` (parse_type_expr captures it at the
/// ident), so `check_span_match` applies directly.
fn find_ident_in_type_expr(te: &TypeExpr, cursor: usize, best: &mut Option<Symbol>) {
    match &te.kind {
        TypeExprKind::Named {
            name, name_span, ..
        } => check_span_match(*name, *name_span, cursor, best),
        TypeExprKind::Generic {
            name,
            name_span,
            args,
            ..
        } => {
            check_span_match(*name, *name_span, cursor, best);
            for a in args {
                find_ident_in_type_expr(a, cursor, best);
            }
        }
        TypeExprKind::Tuple(elems) => {
            for e in elems {
                find_ident_in_type_expr(e, cursor, best);
            }
        }
        TypeExprKind::Function(params, ret) => {
            for p in params {
                find_ident_in_type_expr(p, cursor, best);
            }
            find_ident_in_type_expr(ret, cursor, best);
        }
        TypeExprKind::SelfType => {}
        TypeExprKind::AssocProj { receiver, .. } => {
            find_ident_in_type_expr(receiver, cursor, best);
        }
        TypeExprKind::AnonRecord { fields, .. } => {
            for (_, t) in fields {
                find_ident_in_type_expr(t, cursor, best);
            }
        }
    }
}

/// Walk one fn signature's type positions: param annotations, the
/// return type, and where-clause trait ARGUMENTS. Mirrors
/// `workspace::collect_references_in_fn_signature`.
fn find_ident_in_fn_signature(f: &FnDecl, cursor: usize, best: &mut Option<Symbol>) {
    for param in &f.params {
        if let Some(ty) = &param.ty {
            find_ident_in_type_expr(ty, cursor, best);
        }
    }
    if let Some(rt) = &f.return_type {
        find_ident_in_type_expr(rt, cursor, best);
    }
    for wc in &f.where_clauses {
        for a in &wc.trait_args {
            find_ident_in_type_expr(a, cursor, best);
        }
    }
}

/// Recurse into a pattern, matching the cursor against any leaf
/// `PatternKind::Ident` binder — and, since round-101, Constructor /
/// nominal-record HEAD names (`Circle(r)`, `Point { x }`) so that
/// goto-def / references / rename resolve cursors on user type and
/// variant references in pattern position. Stdlib heads (`Some`, `Ok`,
/// `IoNotFound`, ...) now resolve to their `Symbol` too; the rename
/// gate (`is_symbol_user_renameable_at_cursor` →
/// `is_user_renameable`) still rejects them, so prepareRename keeps
/// producing a clean `null` for builtins.
fn find_ident_in_pattern(pattern: &Pattern, cursor: usize, best: &mut Option<Symbol>) {
    match &pattern.kind {
        PatternKind::Ident(name) => {
            let start = pattern.span.start as usize;
            let name_len = crate::intern::resolve(*name).len();
            if cursor >= start && cursor < start + name_len {
                *best = Some(*name);
            }
        }
        PatternKind::Tuple(pats) | PatternKind::Or(pats) => {
            for p in pats {
                find_ident_in_pattern(p, cursor, best);
            }
        }
        PatternKind::Constructor {
            name,
            name_span,
            args: fields,
            ..
        } => {
            check_span_match(*name, *name_span, cursor, best);
            for p in fields {
                find_ident_in_pattern(p, cursor, best);
            }
        }
        PatternKind::Record {
            name,
            name_span,
            fields,
            ..
        } => {
            if let Some(head) = name {
                check_span_match(*head, *name_span, cursor, best);
            }
            // Round-62 B8: field-shorthand binders (`let Point { x, y } = p`)
            // match the cursor at the field name's own span.
            for (name, name_span, sub) in fields {
                if let Some(p) = sub {
                    find_ident_in_pattern(p, cursor, best);
                } else {
                    check_span_match(*name, *name_span, cursor, best);
                }
            }
        }
        PatternKind::AnonRecord { fields, rest } => {
            // Round-62 B9: anon-record destructure `let { x, y } = p`.
            // Same handling as nominal `Record`.
            for (name, name_span, sub) in fields {
                if let Some(p) = sub {
                    find_ident_in_pattern(p, cursor, best);
                } else {
                    check_span_match(*name, *name_span, cursor, best);
                }
            }
            // Round-101: the named rest binder (`{ x, ...rest }`) binds
            // `rest`.
            if let Some((r, r_span)) = rest {
                check_span_match(*r, *r_span, cursor, best);
            }
        }
        PatternKind::List(pats, rest) => {
            for p in pats {
                find_ident_in_pattern(p, cursor, best);
            }
            if let Some(r) = rest {
                find_ident_in_pattern(r, cursor, best);
            }
        }
        PatternKind::Map(entries) => {
            // Round-101: map-pattern values bind (`#{ "k": v }` binds
            // `v`); keys are string literals, never binders.
            for (_, p) in entries {
                find_ident_in_pattern(p, cursor, best);
            }
        }
        _ => {}
    }
}

fn find_ident_in_expr(expr: &Expr, cursor: usize, best: &mut Option<Symbol>) {
    if let ExprKind::Ident(name) = &expr.kind {
        let start = token_start(&expr.span);
        let name_len = crate::intern::resolve(*name).len();
        if cursor >= start && cursor < start + name_len {
            *best = Some(*name);
        }
    }
    // Match-arm patterns and lambda params bind names that are visible in
    // the arm/body. Visit them so the cursor on the binder resolves.
    if let ExprKind::Match { arms, .. } = &expr.kind {
        for arm in arms {
            find_ident_in_pattern(&arm.pattern, cursor, best);
        }
    }
    if let ExprKind::Lambda { params, .. } = &expr.kind {
        for p in params {
            find_ident_in_pattern(&p.pattern, cursor, best);
            // A typed closure parameter's annotation (`{ p: Point -> ... }`)
            // is a type-position reference, like a fn param's.
            if let Some(ty) = &p.ty {
                find_ident_in_type_expr(ty, cursor, best);
            }
        }
    }
    // Round-101: ascription types (`expr: Point`) are type-position
    // references; `visit_expr_children` only walks the value side.
    if let ExprKind::Ascription(_, te) = &expr.kind {
        find_ident_in_type_expr(te, cursor, best);
    }
    // Round-101: record-construction HEAD (`Point { x: 3 }`,
    // `util.Pt { .. }`) is a type-name reference.
    if let ExprKind::RecordCreate {
        name, name_span, ..
    } = &expr.kind
    {
        check_span_match(*name, *name_span, cursor, best);
    }
    if let ExprKind::Block(stmts) = &expr.kind {
        for stmt in stmts {
            match stmt {
                Stmt::Let { pattern, ty, .. } => {
                    find_ident_in_pattern(pattern, cursor, best);
                    // Round-101: `let p: Point = ...` annotation.
                    if let Some(t) = ty {
                        find_ident_in_type_expr(t, cursor, best);
                    }
                }
                Stmt::When { pattern, .. } => {
                    find_ident_in_pattern(pattern, cursor, best);
                }
                _ => {}
            }
        }
    }
    visit_expr_children(expr, |child| find_ident_in_expr(child, cursor, best));
}

/// Visit all child expressions of an AST node.
pub(super) fn visit_expr_children(expr: &Expr, mut f: impl FnMut(&Expr)) {
    match &expr.kind {
        ExprKind::Binary(lhs, _, rhs) | ExprKind::Pipe(lhs, rhs) | ExprKind::Range(lhs, rhs) => {
            f(lhs);
            f(rhs);
        }
        ExprKind::Unary(_, e)
        | ExprKind::QuestionMark(e)
        | ExprKind::Ascription(e, _)
        | ExprKind::Return(Some(e))
        | ExprKind::FieldAccess(e, _, _) => f(e),
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
        ExprKind::AnonRecord { spread, fields } => {
            // Round-62 B10: anonymous-record literal field values must be
            // walked so identifier searches (find references / hover etc.)
            // descend into them. Mirrors the `RecordCreate` arm above.
            if let Some(s) = spread {
                f(s);
            }
            for (_, v) in fields {
                f(v);
            }
        }
        ExprKind::Loop { bindings, body } => {
            for (_, _, init) in bindings {
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
        // ── Leaf variants: no child `Expr` to recurse into. ────────
        // Listed exhaustively (rather than collapsed under a `_`
        // wildcard) so that the Rust compiler enforces parity
        // whenever a new `ExprKind` variant is added — the omission
        // of a walker arm becomes a compile error rather than a
        // silent functional regression in LSP find-references /
        // hover / inlay-hints / selection-range / folding. Round-85
        // G3 fix: removed the prior wildcard catch-all arm; the
        // source-grep parity-lock in
        // `tests/meta/round85_visit_expr_children_parity_lock_tests.rs`
        // is now belt-and-suspenders behind compiler exhaustiveness.
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::StringLit(_, _)
        | ExprKind::Ident(_)
        | ExprKind::Unit
        | ExprKind::Return(None) => {}
    }
}

/// Walk the entire AST to find the type of a variable by name.
/// Returns the most deeply nested (most specific) type found for the identifier.
pub(super) fn find_ident_type_by_name(program: &Program, name: &str) -> Option<Type> {
    let sym = intern(name);
    let mut result: Option<Type> = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => find_ident_type_in_expr(&f.body, sym, &mut result),
            Decl::Let { value, .. } => find_ident_type_in_expr(value, sym, &mut result),
            Decl::TraitImpl(ti) => {
                if ti.is_auto_derived {
                    continue;
                }
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

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::testing::checked_program;

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
            crate::types::TypeRef::test("Foo"),
            vec![(crate::intern::intern("x"), Type::Var(0))]
        )));
        assert!(!has_unresolved_vars(&Type::Record(
            crate::types::TypeRef::test("Foo"),
            vec![(crate::intern::intern("x"), Type::Int)]
        )));
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

    // ── find_type_at_offset ──────────────────────────────────────

    #[test]
    fn test_find_type_at_offset_typed() {
        let source = "fn main() { 42 }";
        let program = crate::lsp::testing::checked_program(source);

        // The literal 42 should have type Int
        let ty = find_type_at_offset(&program, 13); // offset of "42"
        assert_eq!(ty, Some(Type::Int));
    }

    // ── find_type_at_offset: richer expressions ──────────────────

    #[test]
    fn test_find_type_at_offset_string() {
        let source = r#"fn main() { "hello" }"#;
        let program = crate::lsp::testing::checked_program(source);

        let ty = find_type_at_offset(&program, 13);
        assert_eq!(ty, Some(Type::String));
    }

    #[test]
    fn test_find_type_at_offset_bool() {
        let source = "fn main() { true }";
        let program = crate::lsp::testing::checked_program(source);

        let ty = find_type_at_offset(&program, 13);
        assert_eq!(ty, Some(Type::Bool));
    }

    #[test]
    fn test_find_type_at_offset_binary_expr() {
        let source = "fn main() { 1 + 2 }";
        let program = crate::lsp::testing::checked_program(source);

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
        let program = crate::lsp::testing::checked_program(source);

        let ty = find_type_at_offset(&program, 12);
        assert_eq!(ty, Some(Type::List(Box::new(Type::Int))));
    }

    // ── find_ident_at_offset ─────────────────────────────────────

    #[test]
    fn test_find_ident_at_offset_param() {
        let source = "fn add(x, y) { x + y }";
        let program = checked_program(source);

        // 'x' at offset 15 (inside the body)
        let name = find_ident_at_offset(&program, 15);
        assert_eq!(name, Some(intern("x")));
    }

    #[test]
    fn test_find_ident_at_offset_second_param() {
        let source = "fn add(x, y) { x + y }";
        let program = checked_program(source);

        // 'y' at offset 19
        let name = find_ident_at_offset(&program, 19);
        assert_eq!(name, Some(intern("y")));
    }

    #[test]
    fn test_find_ident_at_offset_none() {
        let source = "fn main() { 42 }";
        let program = checked_program(source);

        // offset 13 is the literal 42, not an ident
        let name = find_ident_at_offset(&program, 13);
        assert_eq!(name, None);
    }

    // ── find_type_at_offset: let bindings ────────────────────────

    #[test]
    fn test_find_type_at_offset_in_let() {
        let source = "fn main() {\n  let x = 42\n  x\n}";
        let program = checked_program(source);

        // 'x' in the last expression (offset 27)
        let ty = find_type_at_offset(&program, 27);
        assert_eq!(ty, Some(Type::Int));
    }

    // ── find_type_at_offset: call callee identifiers (round 101) ──

    #[test]
    fn test_find_type_at_offset_callee_ident_is_fn_signature() {
        // The cursor on a call's CALLEE identifier must surface the
        // fn's signature, not the call's RESULT type. The typechecker's
        // named-callee shortcut in the `ExprKind::Call` arm used to
        // skip stashing `callee.ty`, so this walk fell back to the
        // enclosing Call node's result type (`Int` here).
        let source = "fn add(a: Int, b: Int) -> Int { a + b }\nfn main() { add(1, 2) }\n";
        let program = checked_program(source);

        let callee_offset = source.rfind("add(").unwrap();
        let ty = find_type_at_offset(&program, callee_offset);
        assert_eq!(
            ty,
            Some(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
            "callee ident must carry the fn signature, not the call result"
        );
    }

    #[test]
    fn test_find_type_at_offset_piped_callee_ident_is_fn_signature() {
        // Same lock for the Pipe arm's named-callee shortcut:
        // `1 |> add(2)` desugars to `add(1, 2)` and the callee ident
        // must carry the fn signature.
        let source = "fn add(a: Int, b: Int) -> Int { a + b }\nfn main() { 1 |> add(2) }\n";
        let program = checked_program(source);

        let callee_offset = source.rfind("add(").unwrap();
        let ty = find_type_at_offset(&program, callee_offset);
        assert_eq!(
            ty,
            Some(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
            "piped callee ident must carry the fn signature"
        );
    }
}
