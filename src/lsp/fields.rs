//! Record field access helpers: looking up field types, resolving chained
//! access, and converting AST type expressions to the typechecker's
//! `Type` for display.

use crate::ast::*;
use crate::intern::{Symbol, intern, resolve};
use crate::types::Type;

/// Check if the cursor is on the field name of a `FieldAccess` expression.
/// If so, return the field's type by looking it up in the receiver's record type.
pub(super) fn find_field_type_at_offset(
    program: &Program,
    source: &str,
    cursor: usize,
) -> Option<(String, Type)> {
    let mut result: Option<(String, Type)> = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => find_field_in_expr(&f.body, source, cursor, program, &mut result),
            Decl::Let { value, .. } => {
                find_field_in_expr(value, source, cursor, program, &mut result)
            }
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    find_field_in_expr(&method.body, source, cursor, program, &mut result);
                }
            }
            _ => {}
        }
    }
    result
}

pub(super) fn find_field_in_expr(
    expr: &Expr,
    source: &str,
    cursor: usize,
    program: &Program,
    result: &mut Option<(String, Type)>,
) {
    if let ExprKind::FieldAccess(receiver, field) = &expr.kind {
        // Find where the field name starts in the source.
        // The FieldAccess span covers the receiver. The field name is after the dot.
        //
        // For chained access like `d.response.status`, the AST nests as:
        //   FieldAccess(FieldAccess(d, "response"), "status")
        // and all nodes share the same `span.offset` (the leftmost receiver).
        // A naive `find('.')` would always locate the FIRST dot, mis-identifying
        // the field position for deeper chains.  Instead we search backwards
        // (rfind) for the needle `.{field_name}`, bounded to the region that
        // can contain the cursor, so we match the correct dot.
        let field_str = resolve(*field);
        let expr_start = expr.span.offset;
        if cursor >= expr_start {
            let needle = format!(".{field_str}");
            // Upper-bound: the field text must end at or after the cursor,
            // so the needle cannot start later than `cursor`.  Clamp to
            // source length for safety.
            let search_end = source.len().min(cursor + field_str.len());
            if let Some(dot_rel) = source[expr_start..search_end].rfind(&needle) {
                let field_start = expr_start + dot_rel + 1; // skip the '.'
                let field_end = field_start + field_str.len();
                if cursor >= field_start && cursor < field_end {
                    // Cursor is on the field name — look up the field type
                    if let Some(receiver_ty) = &receiver.ty
                        && let Some(field_ty) =
                            get_field_type_resolved(receiver_ty, *field, program)
                    {
                        *result = Some((field_str, field_ty));
                        return;
                    }
                }
            }
        }
        find_field_in_expr(receiver, source, cursor, program, result);
    } else {
        // Recurse into children
        match &expr.kind {
            ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
                find_field_in_expr(l, source, cursor, program, result);
                find_field_in_expr(r, source, cursor, program, result);
            }
            ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Ascription(e, _)
            | ExprKind::Return(Some(e)) => {
                find_field_in_expr(e, source, cursor, program, result);
            }
            ExprKind::Call(callee, args) => {
                find_field_in_expr(callee, source, cursor, program, result);
                for a in args {
                    find_field_in_expr(a, source, cursor, program, result);
                }
            }
            ExprKind::Lambda { body, .. } => {
                find_field_in_expr(body, source, cursor, program, result)
            }
            ExprKind::Match { expr, arms } => {
                if let Some(e) = expr {
                    find_field_in_expr(e, source, cursor, program, result);
                }
                for arm in arms {
                    if let Some(ref g) = arm.guard {
                        find_field_in_expr(g, source, cursor, program, result);
                    }
                    find_field_in_expr(&arm.body, source, cursor, program, result);
                }
            }
            ExprKind::Block(stmts) => {
                for stmt in stmts {
                    match stmt {
                        Stmt::Let { value, .. } => {
                            find_field_in_expr(value, source, cursor, program, result)
                        }
                        Stmt::Expr(e) => find_field_in_expr(e, source, cursor, program, result),
                        Stmt::When {
                            expr, else_body, ..
                        } => {
                            find_field_in_expr(expr, source, cursor, program, result);
                            find_field_in_expr(else_body, source, cursor, program, result);
                        }
                        Stmt::WhenBool {
                            condition,
                            else_body,
                        } => {
                            find_field_in_expr(condition, source, cursor, program, result);
                            find_field_in_expr(else_body, source, cursor, program, result);
                        }
                    }
                }
            }
            ExprKind::RecordCreate { fields, .. } => {
                for (_, v) in fields {
                    find_field_in_expr(v, source, cursor, program, result);
                }
            }
            ExprKind::RecordUpdate { expr, fields, .. } => {
                find_field_in_expr(expr, source, cursor, program, result);
                for (_, v) in fields {
                    find_field_in_expr(v, source, cursor, program, result);
                }
            }
            ExprKind::Loop { bindings, body } => {
                for (_, init) in bindings {
                    find_field_in_expr(init, source, cursor, program, result);
                }
                find_field_in_expr(body, source, cursor, program, result);
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) | ListElem::Spread(e) => {
                            find_field_in_expr(e, source, cursor, program, result)
                        }
                    }
                }
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    find_field_in_expr(k, source, cursor, program, result);
                    find_field_in_expr(v, source, cursor, program, result);
                }
            }
            ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
                for e in elems {
                    find_field_in_expr(e, source, cursor, program, result);
                }
            }
            ExprKind::Recur(args) => {
                for a in args {
                    find_field_in_expr(a, source, cursor, program, result);
                }
            }
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        find_field_in_expr(e, source, cursor, program, result);
                    }
                }
            }
            ExprKind::FloatElse(expr, fallback) => {
                find_field_in_expr(expr, source, cursor, program, result);
                find_field_in_expr(fallback, source, cursor, program, result);
            }
            _ => {}
        }
    }
}

/// Look up a field's type within a record type.
pub(super) fn get_field_type(ty: &Type, field_name: Symbol) -> Option<Type> {
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

/// Look up a field's type, resolving `Type::Generic(record_name, _)` against
/// the program's type declarations.  In practice, the typechecker annotates
/// intermediate nodes of a chained field access like `o.inner.val` with
/// `Type::Generic(<record_name>, [])` rather than `Type::Record(...)`
/// (see `typechecker::inference::type_from_name`), so the bare
/// `get_field_type` cannot resolve anything past the leftmost dot.  This
/// wrapper falls back to `lookup_record_fields` for named records.
pub(super) fn get_field_type_resolved(
    ty: &Type,
    field_name: Symbol,
    program: &Program,
) -> Option<Type> {
    if let Some(t) = get_field_type(ty, field_name) {
        return Some(t);
    }
    if let Type::Generic(name, _) = ty {
        let field_str = resolve(field_name);
        if let Some(fields) = lookup_record_fields(program, *name) {
            for (n, ft) in fields {
                if n == field_str {
                    return Some(ft);
                }
            }
        }
    }
    None
}

/// Given a type, return the record fields if it is (or wraps) a record type.
/// Looks up type declarations in the program if the type references a named record.
pub(super) fn record_fields_from_type(ty: &Type, program: &Program) -> Option<Vec<(String, Type)>> {
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
pub(super) fn lookup_record_fields(
    program: &Program,
    type_name: Symbol,
) -> Option<Vec<(String, Type)>> {
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
pub(super) fn type_expr_to_type(te: &TypeExpr) -> Type {
    match &te.kind {
        TypeExprKind::Named(n) => {
            let s = resolve(*n);
            match s.as_str() {
                "Int" => Type::Int,
                "Float" => Type::Float,
                "String" => Type::String,
                "Bool" => Type::Bool,
                _ => Type::Generic(*n, vec![]),
            }
        }
        TypeExprKind::Generic(name, args) => {
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
        TypeExprKind::SelfType => Type::Generic(intern("Self"), vec![]),
        _ => Type::String, // fallback
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Span;

    fn parse_and_check(source: &str) -> Program {
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let (mut program, _) = crate::parser::Parser::new(tokens).parse_program_recovering();
        let _ = crate::typechecker::check(&mut program);
        program
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

    // ── find_field_type_at_offset: chained field access ─────────────

    #[test]
    fn test_find_field_single_dot_access() {
        //            0         1         2         3         4         5         6
        //            0123456789012345678901234567890123456789012345678901234567890123456
        let source = "type Pt { x: Int, y: Int }\nfn main() { let p = Pt { x: 1, y: 2 }\np.x }";
        let program = parse_and_check(source);

        // "p.x" — the 'x' field starts after the dot.  Find where "p.x" is
        // in the source and place the cursor on 'x'.
        let px_offset = source.rfind("p.x").unwrap();
        let cursor_on_x = px_offset + 2; // the 'x' in "p.x"

        let result = find_field_type_at_offset(&program, source, cursor_on_x);
        assert!(result.is_some(), "should find field for single-dot access");
        let (name, ty) = result.unwrap();
        assert_eq!(name, "x");
        assert_eq!(ty, Type::Int);
    }

    #[test]
    fn test_find_field_chained_access_rightmost() {
        // Manually construct a chained field access AST: `d.inner.value`
        // where the source text is "d.inner.value" starting at offset 0.
        //
        // AST structure:
        //   FieldAccess(FieldAccess(d, "inner"), "value")
        // Both FieldAccess nodes share span.offset = 0 (the leftmost
        // receiver), which used to cause `find('.')` to locate the FIRST
        // dot instead of the correct one for "value".
        let source = "d.inner.value";
        let span = Span {
            line: 1,
            col: 1,
            offset: 0,
        };

        let inner_sym = crate::intern::intern("inner");
        let value_sym = crate::intern::intern("value");

        // The innermost receiver `d` — type doesn't matter here.
        let d_expr = Expr {
            kind: ExprKind::Ident(crate::intern::intern("d")),
            span,
            ty: Some(Type::Record(
                crate::intern::intern("Outer"),
                vec![(
                    inner_sym,
                    Type::Record(crate::intern::intern("Inner"), vec![(value_sym, Type::Int)]),
                )],
            )),
        };

        // Middle node: `d.inner` with type Record("Inner", [("value", Int)])
        let inner_access = Expr {
            kind: ExprKind::FieldAccess(Box::new(d_expr), inner_sym),
            span,
            ty: Some(Type::Record(
                crate::intern::intern("Inner"),
                vec![(value_sym, Type::Int)],
            )),
        };

        // Outermost node: `d.inner.value` with type Int
        // The receiver is `inner_access` whose type is Record("Inner", ...)
        let outer_access = Expr {
            kind: ExprKind::FieldAccess(Box::new(inner_access), value_sym),
            span,
            ty: Some(Type::Int),
        };

        // Cursor on 'v' of "value" — offset 8 in "d.inner.value"
        let cursor_on_value = 8;
        let mut result = None;
        let program = Program { decls: vec![] };
        find_field_in_expr(
            &outer_access,
            source,
            cursor_on_value,
            &program,
            &mut result,
        );

        assert!(
            result.is_some(),
            "should find field-specific hover for rightmost field in chain"
        );
        let (name, ty) = result.unwrap();
        assert_eq!(name, "value");
        assert_eq!(ty, Type::Int);
    }

    #[test]
    fn test_find_field_chained_access_middle() {
        // Same chain `d.inner.value`, but cursor on 'i' of "inner" (offset 2).
        let source = "d.inner.value";
        let span = Span {
            line: 1,
            col: 1,
            offset: 0,
        };

        let inner_sym = crate::intern::intern("inner");
        let value_sym = crate::intern::intern("value");

        let d_expr = Expr {
            kind: ExprKind::Ident(crate::intern::intern("d")),
            span,
            ty: Some(Type::Record(
                crate::intern::intern("Outer"),
                vec![(
                    inner_sym,
                    Type::Record(crate::intern::intern("Inner"), vec![(value_sym, Type::Int)]),
                )],
            )),
        };

        let inner_access = Expr {
            kind: ExprKind::FieldAccess(Box::new(d_expr), inner_sym),
            span,
            ty: Some(Type::Record(
                crate::intern::intern("Inner"),
                vec![(value_sym, Type::Int)],
            )),
        };

        let outer_access = Expr {
            kind: ExprKind::FieldAccess(Box::new(inner_access), value_sym),
            span,
            ty: Some(Type::Int),
        };

        // Cursor on 'i' of "inner" — offset 2 in "d.inner.value"
        let cursor_on_inner = 2;
        let mut result = None;
        let program = Program { decls: vec![] };
        find_field_in_expr(
            &outer_access,
            source,
            cursor_on_inner,
            &program,
            &mut result,
        );

        assert!(
            result.is_some(),
            "should find field-specific hover for middle field in chain"
        );
        let (name, ty) = result.unwrap();
        assert_eq!(name, "inner");
        // `inner` field type is Record("Inner", ...)
        if let Type::Record(sym, _) = &ty {
            assert_eq!(crate::intern::resolve(*sym), "Inner");
        } else {
            panic!("expected Record type for 'inner' field, got {:?}", ty);
        }
    }
}
