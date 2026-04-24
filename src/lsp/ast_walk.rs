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

pub(super) fn token_start(span: &crate::lexer::Span) -> usize {
    span.offset
}

/// Find the inferred type of the deepest expression at the cursor byte offset.
pub(super) fn find_type_at_offset(program: &Program, cursor: usize) -> Option<Type> {
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

/// Find the identifier name at the cursor byte offset.
///
/// Visits `ExprKind::Ident` use-sites AND binding sites:
///   * `let xvar = ...` / `match` arm pattern binders / `fn foo(x)` params,
///   * `fn foo(...)` declaration name (recovered from the source between the
///     `fn` keyword and the opening `(`).
///
/// Without the binding-site visits, `prepareRename`/`rename`/`hover` on the
/// LHS of a let, on a `fn` parameter, or on a `fn` declaration name would
/// silently no-op (round-60 B8 + G4).
pub(super) fn find_ident_at_offset(program: &Program, cursor: usize) -> Option<Symbol> {
    find_ident_at_offset_with_source(program, cursor, None)
}

/// Source-aware variant. Pass `Some(source)` so binding-site lookups on
/// `fn foo(...)` declaration names can recover the name's offset (the
/// `FnDecl::span` sits at the `fn` keyword, not at the name). Without
/// `source`, the fn-name binding site is not matchable but everything
/// else works.
pub(super) fn find_ident_at_offset_with_source(
    program: &Program,
    cursor: usize,
    source: Option<&str>,
) -> Option<Symbol> {
    let mut best: Option<Symbol> = None;
    for decl in &program.decls {
        find_ident_in_decl(decl, cursor, source, &mut best);
    }
    best
}

fn find_ident_in_decl(
    decl: &Decl,
    cursor: usize,
    source: Option<&str>,
    best: &mut Option<Symbol>,
) {
    match decl {
        Decl::Fn(f) => {
            check_fn_decl_name(f, cursor, source, best);
            for param in &f.params {
                find_ident_in_pattern(&param.pattern, cursor, best);
            }
            find_ident_in_expr(&f.body, cursor, best);
        }
        Decl::Let { pattern, value, .. } => {
            find_ident_in_pattern(pattern, cursor, best);
            find_ident_in_expr(value, cursor, best);
        }
        Decl::TraitImpl(ti) => {
            for method in &ti.methods {
                check_fn_decl_name(method, cursor, source, best);
                for param in &method.params {
                    find_ident_in_pattern(&param.pattern, cursor, best);
                }
                find_ident_in_expr(&method.body, cursor, best);
            }
        }
        _ => {}
    }
}

/// Check whether the cursor sits on a `fn` declaration's name.
///
/// `FnDecl::span` points at the `fn` keyword, so we recover the name's
/// offset by scanning the source between `fn` and the next `(`. When
/// source is unavailable we skip — the use-site path still covers most
/// cases, this only affects rename/hover at the binder itself.
fn check_fn_decl_name(
    f: &crate::ast::FnDecl,
    cursor: usize,
    source: Option<&str>,
    best: &mut Option<Symbol>,
) {
    let Some(source) = source else {
        return;
    };
    let name_str = crate::intern::resolve(f.name);
    let fn_start = f.span.offset;
    if fn_start >= source.len() {
        return;
    }
    // Find the param-list `(` after `fn`. Bare `fn name = ...` (no params)
    // would lack the `(`; in that case scan to the next `=` or end of
    // line as a fallback.
    let after = &source[fn_start.min(source.len())..];
    let scan_end = after
        .find('(')
        .or_else(|| after.find('='))
        .or_else(|| after.find('\n'))
        .map(|p| fn_start + p)
        .unwrap_or(source.len());
    if let Some(off) =
        super::text_utils::find_ident_in_range(source, fn_start, scan_end, &name_str)
        && cursor >= off
        && cursor < off + name_str.len()
    {
        *best = Some(f.name);
    }
}

/// Recurse into a pattern, matching the cursor against any leaf
/// `PatternKind::Ident` binder. Constructor heads (`Some`, `Ok`, `Err`,
/// `IoNotFound`, ...) are intentionally NOT matched — those are
/// stdlib-defined and not user-renameable; they would be rejected by
/// `is_user_renameable` anyway, but we avoid even reporting them so
/// hover/prepareRename produce a clean `null` rather than a spurious
/// rejection error.
fn find_ident_in_pattern(pattern: &Pattern, cursor: usize, best: &mut Option<Symbol>) {
    match &pattern.kind {
        PatternKind::Ident(name) => {
            let start = pattern.span.offset;
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
        PatternKind::Constructor(_, fields) => {
            for p in fields {
                find_ident_in_pattern(p, cursor, best);
            }
        }
        PatternKind::Record { fields, .. } => {
            for (_, sub) in fields {
                if let Some(p) = sub {
                    find_ident_in_pattern(p, cursor, best);
                }
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
        }
    }
    if let ExprKind::Block(stmts) = &expr.kind {
        for stmt in stmts {
            match stmt {
                Stmt::Let { pattern, .. } | Stmt::When { pattern, .. } => {
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

    fn parse_and_check(source: &str) -> Program {
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        program
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

    // ── find_type_at_offset: let bindings ────────────────────────

    #[test]
    fn test_find_type_at_offset_in_let() {
        let source = "fn main() {\n  let x = 42\n  x\n}";
        let program = parse_and_check(source);

        // 'x' in the last expression (offset 27)
        let ty = find_type_at_offset(&program, 27);
        assert_eq!(ty, Some(Type::Int));
    }
}
