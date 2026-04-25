//! Collect local bindings (let / params / match / when) with their
//! approximate source positions, used to power hover / goto-def on
//! locally-bound identifiers.
//!
//! Binding offsets are recovered heuristically by scanning the source
//! text between an enclosing scope and a known reference offset; the
//! actual typed AST doesn't carry separate spans for pattern idents.

use crate::ast::*;
use crate::intern::{Symbol, resolve};
use crate::types::Type;

use super::ast_walk::visit_expr_children;
use super::definitions::find_param_type;
use super::state::LocalBinding;
use super::text_utils::{expr_extent, find_ident_in_range};

// ── Local binding collection (for hover/goto on locals) ──────────────

/// Walk the program and collect every local binding (let, parameter, match)
/// with its approximate source position. Binding offsets are recovered by
/// scanning the source text between the enclosing scope start and a known
/// reference offset (`value.span.offset` for lets, `f.span.offset` for
/// params), which covers the common `let x = e` and `let x: T = e` cases.
pub(super) fn collect_local_bindings(program: &Program, source: &str) -> Vec<LocalBinding> {
    let mut bindings: Vec<LocalBinding> = Vec::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                let body_start = f.body.span.offset;
                let (body_end, _) = expr_extent(&f.body, source);
                // Function parameters: find each in the param-list region
                // before the body start.
                let params_search_end = body_start;
                for param in &f.params {
                    if let PatternKind::Ident(name) = &param.pattern.kind {
                        let name_str = resolve(*name);
                        if let Some(off) =
                            find_ident_in_range(source, f.span.offset, params_search_end, &name_str)
                        {
                            // Look up the param type from the typed body.
                            let ty = find_param_type(&f.body, *name);
                            bindings.push(LocalBinding {
                                name: *name,
                                binding_offset: off,
                                binding_len: name_str.len(),
                                scope_start: body_start,
                                scope_end: body_end,
                                ty,
                            });
                        }
                    }
                }
                collect_local_bindings_in_expr(
                    &f.body,
                    source,
                    body_start,
                    body_end,
                    &mut bindings,
                );
            }
            Decl::Let { value, .. } => {
                collect_local_bindings_in_expr(value, source, 0, source.len(), &mut bindings);
            }
            Decl::TraitImpl(ti) => {
                // Skip auto-derived (synthesized) impls — see ast_walk.rs.
                if ti.is_auto_derived {
                    continue;
                }
                for method in &ti.methods {
                    let body_start = method.body.span.offset;
                    let (body_end, _) = expr_extent(&method.body, source);
                    for param in &method.params {
                        if let PatternKind::Ident(name) = &param.pattern.kind {
                            let name_str = resolve(*name);
                            if let Some(off) = find_ident_in_range(
                                source,
                                method.span.offset,
                                body_start,
                                &name_str,
                            ) {
                                let ty = find_param_type(&method.body, *name);
                                bindings.push(LocalBinding {
                                    name: *name,
                                    binding_offset: off,
                                    binding_len: name_str.len(),
                                    scope_start: body_start,
                                    scope_end: body_end,
                                    ty,
                                });
                            }
                        }
                    }
                    collect_local_bindings_in_expr(
                        &method.body,
                        source,
                        body_start,
                        body_end,
                        &mut bindings,
                    );
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
    source: &str,
    scope_start: usize,
    scope_end: usize,
    bindings: &mut Vec<LocalBinding>,
) {
    match &expr.kind {
        ExprKind::Block(stmts) => {
            // Each `let x = v` in a block is visible from that point to the
            // end of the block.
            for stmt in stmts.iter() {
                match stmt {
                    Stmt::Let { pattern, value, .. } => {
                        let value_start = value.span.offset;
                        // Walk the pattern recursively so destructuring
                        // (`let (a, b) = ...`, `let P { x, y } = ...`, etc.)
                        // also registers each leaf ident as a binding. The
                        // binding scope starts at `value_start` so the
                        // binding ident on the LHS is still found by
                        // `find_local_binding_at_offset` via its own
                        // `binding_offset`/`binding_len`.
                        collect_pattern_bindings(
                            pattern,
                            source,
                            scope_start,
                            value_start,
                            value.ty.as_ref(),
                            scope_end,
                            bindings,
                        );
                        collect_local_bindings_in_expr(
                            value,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                    }
                    Stmt::When {
                        pattern,
                        expr,
                        else_body,
                    } => {
                        // Pattern idents are bound in the rest of the block.
                        collect_pattern_bindings(
                            pattern,
                            source,
                            scope_start,
                            expr.span.offset,
                            expr.ty.as_ref(),
                            scope_end,
                            bindings,
                        );
                        collect_local_bindings_in_expr(
                            expr,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                        collect_local_bindings_in_expr(
                            else_body,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                    }
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => {
                        collect_local_bindings_in_expr(
                            condition,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                        collect_local_bindings_in_expr(
                            else_body,
                            source,
                            scope_start,
                            scope_end,
                            bindings,
                        );
                    }
                    Stmt::Expr(e) => {
                        collect_local_bindings_in_expr(e, source, scope_start, scope_end, bindings);
                    }
                }
            }
        }
        ExprKind::Lambda { params, body } => {
            let body_start = body.span.offset;
            let (body_end, _) = expr_extent(body, source);
            for p in params {
                if let PatternKind::Ident(name) = &p.pattern.kind {
                    let name_str = resolve(*name);
                    if let Some(off) =
                        find_ident_in_range(source, scope_start, body_start, &name_str)
                    {
                        bindings.push(LocalBinding {
                            name: *name,
                            binding_offset: off,
                            binding_len: name_str.len(),
                            scope_start: body_start,
                            scope_end: body_end,
                            ty: find_param_type(body, *name),
                        });
                    }
                }
            }
            collect_local_bindings_in_expr(body, source, body_start, body_end, bindings);
        }
        ExprKind::Match { expr, arms } => {
            if let Some(e) = expr {
                collect_local_bindings_in_expr(e, source, scope_start, scope_end, bindings);
            }
            for arm in arms {
                let arm_start = arm.body.span.offset;
                let (arm_end, _) = expr_extent(&arm.body, source);
                collect_pattern_bindings(
                    &arm.pattern,
                    source,
                    scope_start,
                    arm_start,
                    expr.as_ref().and_then(|e| e.ty.as_ref()),
                    arm_end,
                    bindings,
                );
                if let Some(ref g) = arm.guard {
                    collect_local_bindings_in_expr(g, source, arm_start, arm_end, bindings);
                }
                collect_local_bindings_in_expr(&arm.body, source, arm_start, arm_end, bindings);
            }
        }
        ExprKind::Loop {
            bindings: loop_bindings,
            body,
        } => {
            let body_start = body.span.offset;
            let (body_end, _) = expr_extent(body, source);
            for (name, init) in loop_bindings {
                let name_str = resolve(*name);
                if let Some(off) =
                    find_ident_in_range(source, scope_start, init.span.offset, &name_str)
                {
                    bindings.push(LocalBinding {
                        name: *name,
                        binding_offset: off,
                        binding_len: name_str.len(),
                        scope_start: body_start,
                        scope_end: body_end,
                        ty: init.ty.clone(),
                    });
                }
                collect_local_bindings_in_expr(init, source, scope_start, scope_end, bindings);
            }
            collect_local_bindings_in_expr(body, source, body_start, body_end, bindings);
        }
        _ => {
            visit_expr_children(expr, |child| {
                collect_local_bindings_in_expr(child, source, scope_start, scope_end, bindings);
            });
        }
    }
}

/// Collect the identifiers introduced by a (match/when) pattern.
/// We don't try to recover precise offsets for constructor sub-patterns;
/// instead, we scan the `(search_start..search_end)` window for each bound name.
fn collect_pattern_bindings(
    pattern: &Pattern,
    source: &str,
    search_start: usize,
    search_end: usize,
    expr_ty: Option<&Type>,
    scope_end: usize,
    bindings: &mut Vec<LocalBinding>,
) {
    match &pattern.kind {
        PatternKind::Ident(name) if resolve(*name) != "_" => {
            let name_str = resolve(*name);
            if let Some(off) = find_ident_in_range(source, search_start, search_end, &name_str) {
                bindings.push(LocalBinding {
                    name: *name,
                    binding_offset: off,
                    binding_len: name_str.len(),
                    scope_start: search_end,
                    scope_end,
                    ty: expr_ty.cloned(),
                });
            }
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
                collect_pattern_bindings(
                    p,
                    source,
                    search_start,
                    search_end,
                    inner,
                    scope_end,
                    bindings,
                );
            }
        }
        PatternKind::Or(pats) => {
            for p in pats {
                collect_pattern_bindings(
                    p,
                    source,
                    search_start,
                    search_end,
                    expr_ty,
                    scope_end,
                    bindings,
                );
            }
        }
        PatternKind::Constructor(ctor, fields) => {
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
                    source,
                    search_start,
                    search_end,
                    inner_ty.as_ref(),
                    scope_end,
                    bindings,
                );
            }
        }
        PatternKind::Record { fields, .. } => {
            // Propagate each declared field's type when the value's type
            // is a nominal record, so hover on a destructured field shows
            // the right type.
            let field_tys: Option<Vec<(Symbol, Type)>> = match expr_ty {
                Some(Type::Record(_, fs)) => Some(fs.clone()),
                _ => None,
            };
            let lookup_field_ty = |fname: Symbol| -> Option<Type> {
                field_tys
                    .as_ref()
                    .and_then(|fs| fs.iter().find(|(n, _)| *n == fname).map(|(_, t)| t.clone()))
            };
            for (name, sub) in fields {
                if let Some(p) = sub {
                    let ty = lookup_field_ty(*name);
                    collect_pattern_bindings(
                        p,
                        source,
                        search_start,
                        search_end,
                        ty.as_ref(),
                        scope_end,
                        bindings,
                    );
                } else {
                    let name_str = resolve(*name);
                    if let Some(off) =
                        find_ident_in_range(source, search_start, search_end, &name_str)
                    {
                        bindings.push(LocalBinding {
                            name: *name,
                            binding_offset: off,
                            binding_len: name_str.len(),
                            scope_start: search_end,
                            scope_end,
                            ty: lookup_field_ty(*name),
                        });
                    }
                }
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
                    source,
                    search_start,
                    search_end,
                    elem_ty.as_ref(),
                    scope_end,
                    bindings,
                );
            }
            if let Some(r) = rest {
                collect_pattern_bindings(
                    r,
                    source,
                    search_start,
                    search_end,
                    list_ty.as_ref(),
                    scope_end,
                    bindings,
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
