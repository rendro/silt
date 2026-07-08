//! Per-cursor local variable collection used by completion.
//!
//! Given a byte offset into the source, walk the function containing
//! that offset and produce the set of locally-bound names (function
//! params, `let`s, pattern bindings) that are in scope there, with
//! inferred types when we can recover them.

use crate::ast::*;
use crate::intern::resolve;
use crate::types::Type;

use super::ast_walk::visit_expr_children;
use super::state::LocalVar;

// ── Local variable collection ─────────────────────────────────────

/// Collect local variables in scope at the given byte offset.
pub(super) fn locals_at_offset(program: &Program, cursor: usize) -> Vec<LocalVar> {
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
        PatternKind::Constructor { args: fields, .. } => {
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
        PatternKind::AnonRecord { fields, rest } => {
            // Round-62 B9: anon-record destructure (`let { x, y } = p`)
            // also binds shorthand fields. Round-101: the named rest
            // binder (`{ x, ...rest }`) binds too — mirror the
            // typechecker's `collect_pattern_vars`.
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
            if let Some(r) = rest {
                locals.push(LocalVar {
                    name: r.to_string(),
                    ty: None,
                });
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
        PatternKind::Or(alts) => {
            // Round-101: all alternatives are validated to bind the same
            // names; mirror `collect_pattern_vars` and take the first.
            if let Some(p) = alts.first() {
                collect_pattern_names(p, locals);
            }
        }
        PatternKind::Map(entries) => {
            // Round-101: map-pattern values bind (`#{ "k": v }` binds
            // `v`); keys are string literals, never binders.
            for (_, p) in entries {
                collect_pattern_names(p, locals);
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
                            // For `when let Ok(x) = expr`, if expr has type Result(T, E),
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

/// For `when let Ok(x) = expr` where expr has type Result(T, E), set x's type to T.
fn resolve_when_pattern_types(pattern: &Pattern, expr_ty: Option<&Type>, locals: &mut [LocalVar]) {
    if let (
        PatternKind::Constructor {
            name: ctor,
            args: fields,
            ..
        },
        Some(Type::Generic(_, args)),
    ) = (&pattern.kind, expr_ty)
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
    fn test_locals_when_let_anon_record_rest_binder() {
        // Round-101 GAP lock: `when let {x, ...rest} = p` binds BOTH the
        // shorthand field `x` and the named rest binder `rest`. Pre-fix,
        // the AnonRecord arm destructured `{ fields, .. }` and dropped
        // the rest symbol, so completion never offered `rest`.
        let source = "fn f(p) {\n  when let {x, ...rest} = p else { return 0 }\n  0\n}";
        let program = parse_and_check(source);

        let locals = locals_at_offset(&program, source.len() - 2);
        let names: Vec<&str> = locals.iter().map(|l| l.name.as_str()).collect();
        assert!(
            names.contains(&"x"),
            "should contain shorthand 'x': {names:?}"
        );
        assert!(
            names.contains(&"rest"),
            "should contain rest binder 'rest': {names:?}"
        );
    }

    #[test]
    fn test_locals_match_map_and_or_binders() {
        // Round-101 GAP lock: match-arm Map patterns (`#{\"k\": v}`) and
        // Or patterns (`A(v) | B(v)`) bind `v`; both previously fell to
        // the `_ => {}` catch-all so completion missed the binder.
        let source = "type T { A(Int), B(Int) }\nfn g(m, x) {\n  let a = match m { #{\"k\": v} -> v, _ -> 0 }\n  let b = match x { A(v2) | B(v2) -> v2 }\n  a + b\n}";
        let program = parse_and_check(source);

        let locals = locals_at_offset(&program, source.len() - 2);
        let names: Vec<&str> = locals.iter().map(|l| l.name.as_str()).collect();
        assert!(
            names.contains(&"v"),
            "should contain map binder 'v': {names:?}"
        );
        assert!(
            names.contains(&"v2"),
            "should contain or-pattern binder 'v2': {names:?}"
        );
    }

    #[test]
    fn test_locals_at_offset_empty_outside_fn() {
        let source = "let x = 42\nfn main() { 0 }";
        let program = parse_and_check(source);

        // Outside any function (offset 0)
        let locals = locals_at_offset(&program, 0);
        assert!(locals.is_empty(), "no locals outside functions");
    }
}
