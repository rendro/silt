//! Collect local bindings (let / params / match / when / loop) with their
//! source positions, used to power hover / goto-def on locally-bound
//! identifiers. Every binder's position is its own span in the AST.

use std::collections::HashMap;

use crate::ast::*;
use crate::intern::{Symbol, resolve};
use crate::types::Type;

use super::ast_walk::visit_expr_children;
use super::definitions::find_param_type;
use super::fields::{RecordFields, record_field_types};
use super::state::LocalBinding;

// ── Local binding collection (for hover/goto on locals) ──────────────

/// Walk the program and collect every local binding (let, parameter, match)
/// with its approximate source position. Binding offsets are recovered by
/// scanning the source text between the enclosing scope start and a known
/// reference offset (`(value.span.start as usize)` for lets, `(f.span.start as usize)` for
/// params), which covers the common `let x = e` and `let x: T = e` cases.
///
/// A top-level function's parameters have the types of the checker's
/// type of the function (`top_level`), when there is one.
pub(super) fn collect_local_bindings(
    program: &Program,
    source: &str,
    top_level: Option<&HashMap<Symbol, Type>>,
    records: &RecordFields,
) -> Vec<LocalBinding> {
    let mut bindings: Vec<LocalBinding> = Vec::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                let body_start = f.body.span.start as usize;
                let body_end = f.body.span.end as usize;
                let param_types = match top_level.and_then(|types| types.get(&f.name)) {
                    Some(Type::Fun(params, _)) if params.len() == f.params.len() => {
                        Some(params.as_slice())
                    }
                    _ => None,
                };
                // Function parameters, at their own spans.
                for (i, param) in f.params.iter().enumerate() {
                    if let PatternKind::Ident(name) = &param.pattern.kind {
                        let ty = match param_types {
                            Some(types) => Some(types[i].clone()),
                            None => find_param_type(&f.body, *name),
                        };
                        bindings.push(LocalBinding {
                            name: *name,
                            binding_offset: param.pattern.span.start as usize,
                            binding_len: resolve(*name).len(),
                            scope_start: body_start,
                            scope_end: body_end,
                            ty,
                        });
                    }
                }
                collect_local_bindings_in_expr(&f.body, body_end, &mut bindings, records);
            }
            Decl::Let { value, .. } => {
                collect_local_bindings_in_expr(value, source.len(), &mut bindings, records);
            }
            Decl::TraitImpl(ti) => {
                // Skip auto-derived (synthesized) impls — see ast_walk.rs.
                if ti.is_auto_derived {
                    continue;
                }
                for method in &ti.methods {
                    let body_start = method.body.span.start as usize;
                    let body_end = method.body.span.end as usize;
                    for param in &method.params {
                        if let PatternKind::Ident(name) = &param.pattern.kind {
                            let ty = find_param_type(&method.body, *name);
                            bindings.push(LocalBinding {
                                name: *name,
                                binding_offset: param.pattern.span.start as usize,
                                binding_len: resolve(*name).len(),
                                scope_start: body_start,
                                scope_end: body_end,
                                ty,
                            });
                        }
                    }
                    collect_local_bindings_in_expr(&method.body, body_end, &mut bindings, records);
                }
            }
            _ => {}
        }
    }
    bindings
}

/// Collect local bindings inside an expression, given the enclosing scope.
fn collect_local_bindings_in_expr(
    expr: &Expr,
    scope_end: usize,
    bindings: &mut Vec<LocalBinding>,
    records: &RecordFields,
) {
    match &expr.kind {
        ExprKind::Block(stmts) => {
            // Each `let x = v` in a block is visible from that point to the
            // end of the block.
            for stmt in stmts.iter() {
                match stmt {
                    Stmt::Let { pattern, value, .. } => {
                        let value_start = value.span.start as usize;
                        // Walk the pattern recursively so destructuring
                        // (`let (a, b) = ...`, `let P { x, y } = ...`, etc.)
                        // also registers each leaf ident as a binding. The
                        // binding scope starts at `value_start` so the
                        // binding ident on the LHS is still found by
                        // `find_local_binding_at_offset` via its own
                        // `binding_offset`/`binding_len`.
                        collect_pattern_bindings(
                            pattern,
                            value_start,
                            value.ty.as_ref(),
                            scope_end,
                            bindings,
                            records,
                        );
                        collect_local_bindings_in_expr(value, scope_end, bindings, records);
                    }
                    Stmt::When {
                        pattern,
                        expr,
                        else_body,
                    } => {
                        // Pattern idents are bound in the rest of the block.
                        collect_pattern_bindings(
                            pattern,
                            expr.span.start as usize,
                            expr.ty.as_ref(),
                            scope_end,
                            bindings,
                            records,
                        );
                        collect_local_bindings_in_expr(expr, scope_end, bindings, records);
                        collect_local_bindings_in_expr(else_body, scope_end, bindings, records);
                    }
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => {
                        collect_local_bindings_in_expr(condition, scope_end, bindings, records);
                        collect_local_bindings_in_expr(else_body, scope_end, bindings, records);
                    }
                    Stmt::Expr(e) => {
                        collect_local_bindings_in_expr(e, scope_end, bindings, records);
                    }
                }
            }
        }
        ExprKind::Lambda { params, body, .. } => {
            let body_start = body.span.start as usize;
            let body_end = body.span.end as usize;
            for p in params {
                if let PatternKind::Ident(name) = &p.pattern.kind {
                    bindings.push(LocalBinding {
                        name: *name,
                        binding_offset: p.pattern.span.start as usize,
                        binding_len: resolve(*name).len(),
                        scope_start: body_start,
                        scope_end: body_end,
                        ty: find_param_type(body, *name),
                    });
                }
            }
            collect_local_bindings_in_expr(body, body_end, bindings, records);
        }
        ExprKind::Match { expr, arms } => {
            if let Some(e) = expr {
                collect_local_bindings_in_expr(e, scope_end, bindings, records);
            }
            for arm in arms {
                let arm_start = arm.body.span.start as usize;
                let arm_end = arm.body.span.end as usize;
                collect_pattern_bindings(
                    &arm.pattern,
                    arm_start,
                    expr.as_ref().and_then(|e| e.ty.as_ref()),
                    arm_end,
                    bindings,
                    records,
                );
                if let Some(ref g) = arm.guard {
                    collect_local_bindings_in_expr(g, arm_end, bindings, records);
                }
                collect_local_bindings_in_expr(&arm.body, arm_end, bindings, records);
            }
        }
        ExprKind::Loop {
            bindings: loop_bindings,
            body,
        } => {
            let body_start = body.span.start as usize;
            let body_end = body.span.end as usize;
            for (name, name_span, init) in loop_bindings {
                bindings.push(LocalBinding {
                    name: *name,
                    binding_offset: name_span.start as usize,
                    binding_len: resolve(*name).len(),
                    scope_start: body_start,
                    scope_end: body_end,
                    ty: init.ty.clone(),
                });
                collect_local_bindings_in_expr(init, scope_end, bindings, records);
            }
            collect_local_bindings_in_expr(body, body_end, bindings, records);
        }
        _ => {
            visit_expr_children(expr, |child| {
                collect_local_bindings_in_expr(child, scope_end, bindings, records);
            });
        }
    }
}

/// Collect the identifiers introduced by a (let/match/when) pattern, each
/// at its own span, visible from `visible_from` to `scope_end`.
fn collect_pattern_bindings(
    pattern: &Pattern,
    visible_from: usize,
    expr_ty: Option<&Type>,
    scope_end: usize,
    bindings: &mut Vec<LocalBinding>,
    records: &RecordFields,
) {
    match &pattern.kind {
        PatternKind::Ident(name) if resolve(*name) != "_" => {
            bindings.push(LocalBinding {
                name: *name,
                binding_offset: pattern.span.start as usize,
                binding_len: resolve(*name).len(),
                scope_start: visible_from,
                scope_end,
                ty: expr_ty.cloned(),
            });
        }
        PatternKind::Tuple(pats) => {
            // Propagate element types when the value's type is a tuple of
            // the same arity, so `let (a, b) = (1, 2)` gives `a: Int, b: Int`.
            let elem_tys: Option<Vec<Type>> = match expr_ty {
                Some(Type::Tuple(tys)) if tys.len() == pats.len() => Some(tys.clone()),
                _ => None,
            };
            for (i, p) in pats.iter().enumerate() {
                let inner = elem_tys.as_ref().and_then(|tys| tys.get(i));
                collect_pattern_bindings(p, visible_from, inner, scope_end, bindings, records);
            }
        }
        PatternKind::Or(pats) => {
            for p in pats {
                collect_pattern_bindings(p, visible_from, expr_ty, scope_end, bindings, records);
            }
        }
        PatternKind::Constructor {
            name: ctor,
            args: fields,
            ..
        } => {
            // For Ok/Err/Some, try to propagate the inner type.
            let inner_ty: Option<Type> = match (resolve(*ctor).as_str(), expr_ty) {
                ("Ok", Some(Type::Generic(_, args))) => args.first().cloned(),
                ("Err", Some(Type::Generic(_, args))) => args.get(1).cloned(),
                ("Some", Some(Type::Generic(_, args))) => args.first().cloned(),
                _ => None,
            };
            for p in fields {
                collect_pattern_bindings(
                    p,
                    visible_from,
                    inner_ty.as_ref(),
                    scope_end,
                    bindings,
                    records,
                );
            }
        }
        PatternKind::Record { fields, .. } => {
            // Propagate each declared field's type when the value's type
            // is a nominal record, so hover on a destructured field shows
            // the right type.
            let field_tys = expr_ty.and_then(|ty| record_field_types(ty, records));
            let lookup_field_ty = |fname: Symbol| -> Option<Type> {
                field_tys
                    .as_ref()
                    .and_then(|fs| fs.iter().find(|(n, _)| *n == fname).map(|(_, t)| t.clone()))
            };
            for (name, name_span, sub) in fields {
                if let Some(p) = sub {
                    let ty = lookup_field_ty(*name);
                    collect_pattern_bindings(
                        p,
                        visible_from,
                        ty.as_ref(),
                        scope_end,
                        bindings,
                        records,
                    );
                } else {
                    bindings.push(LocalBinding {
                        name: *name,
                        binding_offset: name_span.start as usize,
                        binding_len: resolve(*name).len(),
                        scope_start: visible_from,
                        scope_end,
                        ty: lookup_field_ty(*name),
                    });
                }
            }
        }
        PatternKind::AnonRecord { fields, rest } => {
            // Round-62 B9: row-polymorphism destructure
            // (`let { x, y } = anon_record`). Mirror the `Record` arm.
            // The fields come from the type of the expression: an
            // anonymous record type's own, or a declared record's
            // (`record_field_types`). A field the type does not list
            // binds without a type.
            let field_tys = expr_ty.and_then(|ty| record_field_types(ty, records));
            let lookup_field_ty = |fname: Symbol| -> Option<Type> {
                field_tys
                    .as_ref()
                    .and_then(|fs| fs.iter().find(|(n, _)| *n == fname).map(|(_, t)| t.clone()))
            };
            for (name, name_span, sub) in fields {
                if let Some(p) = sub {
                    let ty = lookup_field_ty(*name);
                    collect_pattern_bindings(
                        p,
                        visible_from,
                        ty.as_ref(),
                        scope_end,
                        bindings,
                        records,
                    );
                } else {
                    bindings.push(LocalBinding {
                        name: *name,
                        binding_offset: name_span.start as usize,
                        binding_len: resolve(*name).len(),
                        scope_start: visible_from,
                        scope_end,
                        ty: lookup_field_ty(*name),
                    });
                }
            }
            // The named rest binder (`{ x, ...rest }`) binds `rest` to
            // an anonymous record of the fields the pattern does not
            // name, whatever the record is (over an open row, with the
            // row).
            if let Some((r, r_span)) = rest {
                let tail = match expr_ty {
                    Some(Type::AnonRecord { tail, .. }) => tail.clone(),
                    _ => crate::types::RowTail::Closed,
                };
                let ty = field_tys.as_ref().map(|fs| Type::AnonRecord {
                    fields: fs
                        .iter()
                        .filter(|(n, _)| !fields.iter().any(|(named, _, _)| named == n))
                        .cloned()
                        .collect(),
                    tail,
                });
                bindings.push(LocalBinding {
                    name: *r,
                    binding_offset: r_span.start as usize,
                    binding_len: resolve(*r).len(),
                    scope_start: visible_from,
                    scope_end,
                    ty,
                });
            }
        }
        PatternKind::Map(entries) => {
            // Round-101: map-pattern values bind (`#{ "k": v }` binds
            // `v`); keys are string literals, never binders.
            for (_, p) in entries {
                collect_pattern_bindings(p, visible_from, None, scope_end, bindings, records);
            }
        }
        PatternKind::List(pats, rest) => {
            // A list destructure binds each head element to the list's
            // element type and the tail to the full list type.
            let (elem_ty, list_ty): (Option<Type>, Option<Type>) = match expr_ty {
                Some(t @ Type::List(inner)) => (Some((**inner).clone()), Some(t.clone())),
                _ => (None, None),
            };
            for p in pats {
                collect_pattern_bindings(
                    p,
                    visible_from,
                    elem_ty.as_ref(),
                    scope_end,
                    bindings,
                    records,
                );
            }
            if let Some(r) = rest {
                collect_pattern_bindings(
                    r,
                    visible_from,
                    list_ty.as_ref(),
                    scope_end,
                    bindings,
                    records,
                );
            }
        }
        _ => {}
    }
}

/// Find the binding whose identifier span contains the given cursor offset.
pub(super) fn find_local_binding_at_offset(
    locals: &[LocalBinding],
    cursor: usize,
) -> Option<&LocalBinding> {
    locals
        .iter()
        .find(|b| cursor >= b.binding_offset && cursor < b.binding_offset + b.binding_len)
}

/// Find the nearest (by scope) local binding with the given name visible at the cursor.
pub(super) fn nearest_local_binding_for(
    locals: &[LocalBinding],
    name: Symbol,
    cursor: usize,
) -> Option<&LocalBinding> {
    // Prefer the innermost scope that contains the cursor (smallest scope
    // width), breaking ties by picking the later binding offset so shadowed
    // bindings resolve to the most recent one.
    locals
        .iter()
        .filter(|b| b.name == name)
        .filter(|b| cursor >= b.scope_start && cursor <= b.scope_end)
        .min_by(|a, b| {
            let wa = a.scope_end.saturating_sub(a.scope_start);
            let wb = b.scope_end.saturating_sub(b.scope_start);
            wa.cmp(&wb)
                .then_with(|| b.binding_offset.cmp(&a.binding_offset))
        })
}
