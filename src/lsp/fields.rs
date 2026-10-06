//! Record field access helpers: looking up field types and resolving
//! chained access. A named record's fields are the checker's
//! ([`RecordFields`], from the session's analysis of the module).

use std::collections::HashMap;

use crate::ast::*;
use crate::intern::{Symbol, resolve};
use crate::types::Type;

/// The fields of each record type a module sees, by the name it is
/// written with (`ModuleAnalysis::record_fields`).
pub(super) type RecordFields = HashMap<crate::types::TypeRef, Vec<(Symbol, Type)>>;

/// Check if the cursor is on the field name of a `FieldAccess` expression.
/// If so, return the field's type by looking it up in the receiver's record type.
pub(super) fn find_field_type_at_offset(
    program: &Program,
    records: &RecordFields,
    cursor: usize,
) -> Option<(String, Type)> {
    let mut result: Option<(String, Type)> = None;
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => find_field_in_expr(&f.body, cursor, records, &mut result),
            Decl::Let { value, .. } => find_field_in_expr(value, cursor, records, &mut result),
            Decl::TraitImpl(ti) => {
                if ti.is_auto_derived {
                    continue;
                }
                for method in &ti.methods {
                    find_field_in_expr(&method.body, cursor, records, &mut result);
                }
            }
            _ => {}
        }
    }
    result
}

pub(super) fn find_field_in_expr(
    expr: &Expr,
    cursor: usize,
    records: &RecordFields,
    result: &mut Option<(String, Type)>,
) {
    if let ExprKind::FieldAccess(receiver, field, field_span) = &expr.kind {
        // The cursor is on the field name: look up the field type.
        if (field_span.start as usize..field_span.end as usize).contains(&cursor)
            && let Some(receiver_ty) = &receiver.ty
            && let Some(field_ty) = get_field_type_resolved(receiver_ty, *field, records)
        {
            *result = Some((resolve(*field), field_ty));
            return;
        }
        find_field_in_expr(receiver, cursor, records, result);
    } else {
        // Recurse into children
        match &expr.kind {
            ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
                find_field_in_expr(l, cursor, records, result);
                find_field_in_expr(r, cursor, records, result);
            }
            ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Ascription(e, _)
            | ExprKind::Return(Some(e)) => {
                find_field_in_expr(e, cursor, records, result);
            }
            ExprKind::Call(callee, args) => {
                find_field_in_expr(callee, cursor, records, result);
                for a in args {
                    find_field_in_expr(a, cursor, records, result);
                }
            }
            ExprKind::Lambda { body, .. } => find_field_in_expr(body, cursor, records, result),
            ExprKind::Match { expr, arms } => {
                if let Some(e) = expr {
                    find_field_in_expr(e, cursor, records, result);
                }
                for arm in arms {
                    if let Some(ref g) = arm.guard {
                        find_field_in_expr(g, cursor, records, result);
                    }
                    find_field_in_expr(&arm.body, cursor, records, result);
                }
            }
            ExprKind::Block(stmts) => {
                for stmt in stmts {
                    match stmt {
                        Stmt::Let { value, .. } => {
                            find_field_in_expr(value, cursor, records, result)
                        }
                        Stmt::Expr(e) => find_field_in_expr(e, cursor, records, result),
                        Stmt::When {
                            expr, else_body, ..
                        } => {
                            find_field_in_expr(expr, cursor, records, result);
                            find_field_in_expr(else_body, cursor, records, result);
                        }
                        Stmt::WhenBool {
                            condition,
                            else_body,
                        } => {
                            find_field_in_expr(condition, cursor, records, result);
                            find_field_in_expr(else_body, cursor, records, result);
                        }
                    }
                }
            }
            ExprKind::RecordCreate { fields, .. } => {
                for (_, v) in fields {
                    find_field_in_expr(v, cursor, records, result);
                }
            }
            ExprKind::RecordUpdate { expr, fields, .. } => {
                find_field_in_expr(expr, cursor, records, result);
                for (_, v) in fields {
                    find_field_in_expr(v, cursor, records, result);
                }
            }
            ExprKind::Loop { bindings, body } => {
                for (_, _, init) in bindings {
                    find_field_in_expr(init, cursor, records, result);
                }
                find_field_in_expr(body, cursor, records, result);
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) | ListElem::Spread(e) => {
                            find_field_in_expr(e, cursor, records, result)
                        }
                    }
                }
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    find_field_in_expr(k, cursor, records, result);
                    find_field_in_expr(v, cursor, records, result);
                }
            }
            ExprKind::SetLit(elems) | ExprKind::Tuple(elems) => {
                for e in elems {
                    find_field_in_expr(e, cursor, records, result);
                }
            }
            ExprKind::Recur(args) => {
                for a in args {
                    find_field_in_expr(a, cursor, records, result);
                }
            }
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        find_field_in_expr(e, cursor, records, result);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Look up a field's type within a record type.
pub(super) fn get_field_type(ty: &Type, field_name: Symbol) -> Option<Type> {
    match ty {
        // Anonymous structural record (`{ x: Int, y: Int }`). The
        // typechecker assigns this to bindings whose annotation refers to
        // a user-declared record type when the literal/initializer is
        // structurally inferred — without this arm, hover/completion
        // never see the fields. Tail status doesn't affect lookup of an
        // explicitly listed field; if the field isn't in `fields` we
        // simply return `None`, matching the `Type::Record` path.
        Type::AnonRecord { fields, .. } => fields.get(&field_name).cloned(),
        Type::Tuple(elems) => resolve(field_name)
            .parse::<usize>()
            .ok()
            .and_then(|i| elems.get(i).cloned()),
        _ => None,
    }
}

/// Look up a field's type, resolving `Type::Generic(record_name, _)`
/// through the checker's record fields. The typechecker annotates
/// intermediate nodes of a chained field access like `o.inner.val` with
/// `Type::Generic(<record_name>, [])` rather than `Type::Record(...)`, so
/// the bare `get_field_type` cannot resolve anything past the leftmost
/// dot.
pub(super) fn get_field_type_resolved(
    ty: &Type,
    field_name: Symbol,
    records: &RecordFields,
) -> Option<Type> {
    if let Some(t) = get_field_type(ty, field_name) {
        return Some(t);
    }
    match ty {
        Type::Generic(name, _) => records
            .get(name)?
            .iter()
            .find(|(n, _)| *n == field_name)
            .map(|(_, t)| t.clone()),
        _ => None,
    }
}

/// Given a type, return the record fields if it is (or wraps) a record type.
/// A named record's fields are the checker's.
pub(super) fn record_fields_from_type(
    ty: &Type,
    records: &RecordFields,
) -> Option<Vec<(String, Type)>> {
    let fields = match ty {
        // Anonymous structural record (`{ x: Int, y: Int }`). The
        // typechecker assigns this shape to bindings whose initializer is
        // a record literal — even when the binder has an explicit named
        // type annotation like `let p: Point = { x: 1, y: 2 }`. Listed
        // fields are surfaced regardless of tail (`Closed` vs row
        // variable): a row tail only signals "more fields possible",
        // which the editor can't enumerate, so we report what is known.
        Type::AnonRecord { fields, .. } => fields.iter().map(|(n, t)| (*n, t.clone())).collect(),
        Type::Generic(name, _) => records.get(name)?.clone(),
        _ => return None,
    };
    Some(fields.into_iter().map(|(n, t)| (resolve(n), t)).collect())
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::testing::checked_program;
    use crate::source::Span;

    /// A closed record type of these fields.
    fn anon(fields: Vec<(Symbol, Type)>) -> Type {
        Type::AnonRecord {
            fields: fields.into_iter().collect(),
            tail: crate::types::RowTail::Closed,
        }
    }

    // ── get_field_type ────────────────────────────────────────────

    #[test]
    fn test_get_field_type_record() {
        let ty = anon(vec![
            (crate::intern::intern("name"), Type::String),
            (crate::intern::intern("age"), Type::Int),
        ]);
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
        let ty = anon(vec![
            (crate::intern::intern("x"), Type::Float),
            (crate::intern::intern("y"), Type::Float),
        ]);
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
        let source = "fn main() { let p = { x: 1, y: 2 }\np.x }";
        let program = checked_program(source);

        // "p.x" — the 'x' field starts after the dot.  Find where "p.x" is
        // in the source and place the cursor on 'x'.
        let px_offset = source.rfind("p.x").unwrap();
        let cursor_on_x = px_offset + 2; // the 'x' in "p.x"

        let result = find_field_type_at_offset(&program, &RecordFields::new(), cursor_on_x);
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
        // Both FieldAccess nodes start at 0 (the leftmost receiver); each
        // field name has its own span.
        // The text is "d.inner.value".
        let at = |start, end| Span {
            file: crate::source::FileId::default(),
            start,
            end,
        };

        let inner_sym = crate::intern::intern("inner");
        let value_sym = crate::intern::intern("value");

        // The innermost receiver `d` — type doesn't matter here.
        let d_expr = Expr {
            kind: ExprKind::Ident(crate::intern::intern("d")),
            span: at(0, 1),
            ty: Some(anon(vec![(inner_sym, anon(vec![(value_sym, Type::Int)]))])),
            res: None,
        };

        // Middle node: `d.inner` with type Record("Inner", [("value", Int)])
        let inner_access = Expr {
            kind: ExprKind::FieldAccess(Box::new(d_expr), inner_sym, at(2, 7)),
            span: at(0, 7),
            ty: Some(anon(vec![(value_sym, Type::Int)])),
            res: None,
        };

        // Outermost node: `d.inner.value` with type Int
        // The receiver is `inner_access` whose type is Record("Inner", ...)
        let outer_access = Expr {
            kind: ExprKind::FieldAccess(Box::new(inner_access), value_sym, at(8, 13)),
            span: at(0, 13),
            ty: Some(Type::Int),
            res: None,
        };

        // Cursor on 'v' of "value" — offset 8 in "d.inner.value"
        let cursor_on_value = 8;
        let mut result = None;
        find_field_in_expr(
            &outer_access,
            cursor_on_value,
            &RecordFields::new(),
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
        // The text is "d.inner.value".
        let at = |start, end| Span {
            file: crate::source::FileId::default(),
            start,
            end,
        };

        let inner_sym = crate::intern::intern("inner");
        let value_sym = crate::intern::intern("value");

        let d_expr = Expr {
            kind: ExprKind::Ident(crate::intern::intern("d")),
            span: at(0, 1),
            ty: Some(anon(vec![(inner_sym, anon(vec![(value_sym, Type::Int)]))])),
            res: None,
        };

        let inner_access = Expr {
            kind: ExprKind::FieldAccess(Box::new(d_expr), inner_sym, at(2, 7)),
            span: at(0, 7),
            ty: Some(anon(vec![(value_sym, Type::Int)])),
            res: None,
        };

        let outer_access = Expr {
            kind: ExprKind::FieldAccess(Box::new(inner_access), value_sym, at(8, 13)),
            span: at(0, 13),
            ty: Some(Type::Int),
            res: None,
        };

        // Cursor on 'i' of "inner" — offset 2 in "d.inner.value"
        let cursor_on_inner = 2;
        let mut result = None;
        find_field_in_expr(
            &outer_access,
            cursor_on_inner,
            &RecordFields::new(),
            &mut result,
        );

        assert!(
            result.is_some(),
            "should find field-specific hover for middle field in chain"
        );
        let (name, ty) = result.unwrap();
        assert_eq!(name, "inner");
        // `inner` field type is Record("Inner", ...)
        // `inner` field type is the record of `value`.
        assert_eq!(ty, anon(vec![(value_sym, Type::Int)]));
    }
}
