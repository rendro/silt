//! The local bindings of a document: every name a parameter, a `let` in
//! a body, a `when let`, a match arm, a closure or a `loop` binds, with
//! the span of the name where it is bound and the range of the source it
//! is visible in. Hover, go-to-definition, references and rename find a
//! local by them: a use is the binding of its name with the smallest
//! range around it.

use std::collections::HashMap;

use crate::ast::*;
use crate::intern::{Symbol, resolve};
use crate::types::Type;

use crate::defs::Res;
use crate::typechecker::Tables;

use super::ast_walk::{visit_expr_children, written_general};
use super::definitions::find_param_type;
use super::fields::{RecordFields, record_field_types};
use super::state::{LocalBinding, PatternConstructor};

// ── Local binding collection (for hover/goto on locals) ──────────────

/// What the walk of a module's bindings reads, and what it finds.
struct Collect<'a> {
    records: &'a RecordFields,
    /// The checks of the module's session, if it has them: the variants
    /// of each enum.
    tables: Option<&'a Tables>,
    bindings: Vec<LocalBinding>,
    constructors: Vec<PatternConstructor>,
}

/// Every local binding of `program`, and every constructor its patterns
/// name.
///
/// A top-level function's parameters have the types of the checker's
/// type of the function (`top_level`), when there is one.
pub(super) fn collect_local_bindings(
    program: &Program,
    source: &str,
    top_level: Option<&HashMap<Symbol, Type>>,
    records: &RecordFields,
    tables: Option<&Tables>,
) -> (Vec<LocalBinding>, Vec<PatternConstructor>) {
    let mut out = Collect {
        records,
        tables,
        bindings: Vec::new(),
        constructors: Vec::new(),
    };
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
                // The parameters, each name at its own span.
                for (i, param) in f.params.iter().enumerate() {
                    let ty = match (&param.pattern.kind, param_types) {
                        (_, Some(types)) => Some(types[i].clone()),
                        (PatternKind::Ident(name), None) => find_param_type(&f.body, *name),
                        _ => None,
                    };
                    collect_pattern_bindings(
                        &param.pattern,
                        body_start,
                        ty.as_ref(),
                        body_end,
                        &mut out,
                    );
                }
                collect_local_bindings_in_expr(&f.body, body_end, &mut out);
            }
            Decl::Let { value, .. } => {
                collect_local_bindings_in_expr(value, source.len(), &mut out);
            }
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    collect_method_bindings(method, &mut out);
                }
            }
            // A trait's methods: the parameters of each, and what a
            // default body binds.
            Decl::Trait(t) => {
                for method in &t.methods {
                    collect_method_bindings(method, &mut out);
                }
            }
            _ => {}
        }
    }
    (out.bindings, out.constructors)
}

/// The parameters of a trait's or an impl's method, and what its body
/// binds.
fn collect_method_bindings(method: &FnDecl, out: &mut Collect) {
    let body_end = method.body.span.end as usize;
    param_bindings(&method.params, &method.body, out);
    collect_local_bindings_in_expr(&method.body, body_end, out);
}

/// The names the parameters `params` of a function or closure with the
/// body `body` bind, visible in the body. A parameter that is one name
/// has the type the body uses it at.
fn param_bindings(params: &[Param], body: &Expr, out: &mut Collect) {
    let body_start = body.span.start as usize;
    let body_end = body.span.end as usize;
    for param in params {
        let ty = match &param.pattern.kind {
            PatternKind::Ident(name) => find_param_type(body, *name),
            _ => None,
        };
        collect_pattern_bindings(&param.pattern, body_start, ty.as_ref(), body_end, out);
    }
}

/// Collect local bindings inside an expression, given the enclosing scope.
fn collect_local_bindings_in_expr(expr: &Expr, scope_end: usize, out: &mut Collect) {
    match &expr.kind {
        ExprKind::Block(stmts) => {
            // Each `let x = v` in a block is visible from that point to the
            // end of the block: its own closing brace, wherever the
            // block stands.
            let scope_end = scope_end.min(expr.span.end as usize);
            for stmt in stmts.iter() {
                match stmt {
                    Stmt::Let { pattern, value, .. } => {
                        // The names are visible behind the value:
                        // `let x = x + 1` uses the `x` before it. A
                        // closure is its own name inside it:
                        // `let f = { n -> f(n - 1) }`.
                        let visible = match (&pattern.kind, &value.kind) {
                            (PatternKind::Ident(_), ExprKind::Lambda { .. }) => value.span.start,
                            _ => value.span.end,
                        };
                        collect_pattern_bindings(
                            pattern,
                            visible as usize,
                            value.ty.as_ref(),
                            scope_end,
                            out,
                        );
                        collect_local_bindings_in_expr(value, scope_end, out);
                    }
                    Stmt::When {
                        pattern,
                        expr,
                        else_body,
                    } => {
                        // The names are bound in the rest of the block,
                        // behind the `else` body.
                        collect_pattern_bindings(
                            pattern,
                            else_body.span.end as usize,
                            expr.ty.as_ref(),
                            scope_end,
                            out,
                        );
                        collect_local_bindings_in_expr(expr, scope_end, out);
                        collect_local_bindings_in_expr(else_body, scope_end, out);
                    }
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => {
                        collect_local_bindings_in_expr(condition, scope_end, out);
                        collect_local_bindings_in_expr(else_body, scope_end, out);
                    }
                    Stmt::Expr(e) => {
                        collect_local_bindings_in_expr(e, scope_end, out);
                    }
                }
            }
        }
        ExprKind::Lambda { params, body, .. } => {
            let body_end = body.span.end as usize;
            param_bindings(params, body, out);
            collect_local_bindings_in_expr(body, body_end, out);
        }
        ExprKind::Match { expr, arms } => {
            if let Some(e) = expr {
                collect_local_bindings_in_expr(e, scope_end, out);
            }
            for arm in arms {
                // An arm's names are visible in its guard and its body.
                let arm_start = match &arm.guard {
                    Some(guard) => guard.span.start as usize,
                    None => arm.body.span.start as usize,
                };
                let arm_end = arm.body.span.end as usize;
                collect_pattern_bindings(
                    &arm.pattern,
                    arm_start,
                    expr.as_ref().and_then(|e| e.ty.as_ref()),
                    arm_end,
                    out,
                );
                // (The head of an arm's pattern has the scrutinee's
                // type: see `find_type_at_offset`.)
                if let PatternKind::Constructor { name_span, .. } = &arm.pattern.kind {
                    out.constructors.retain(|c| c.span != *name_span);
                }
                if let Some(ref g) = arm.guard {
                    collect_local_bindings_in_expr(g, arm_end, out);
                }
                collect_local_bindings_in_expr(&arm.body, arm_end, out);
            }
        }
        ExprKind::Loop {
            bindings: loop_bindings,
            body,
        } => {
            // The initial values are of the enclosing scope, all of
            // them: a binder is visible in the body only.
            let body_end = body.span.end as usize;
            for (_, _, init) in loop_bindings {
                collect_local_bindings_in_expr(init, scope_end, out);
            }
            for (name, name_span, init) in loop_bindings {
                out.bindings.push(LocalBinding {
                    name: *name,
                    binding_offset: name_span.start as usize,
                    binding_len: resolve(*name).len(),
                    scope_start: body.span.start as usize,
                    scope_end: body_end,
                    ty: init.ty.clone(),
                    same_as: None,
                });
            }
            collect_local_bindings_in_expr(body, body_end, out);
        }
        _ => {
            visit_expr_children(expr, |child| {
                collect_local_bindings_in_expr(child, scope_end, out);
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
    out: &mut Collect,
) {
    match &pattern.kind {
        PatternKind::Ident(name) if resolve(*name) != "_" => {
            out.bindings.push(LocalBinding {
                name: *name,
                binding_offset: pattern.span.start as usize,
                binding_len: resolve(*name).len(),
                scope_start: visible_from,
                scope_end,
                ty: expr_ty.cloned(),
                same_as: None,
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
                collect_pattern_bindings(p, visible_from, inner, scope_end, out);
            }
        }
        // The alternatives bind the same names: the binders of the first
        // are the bindings, those of the others are further sites of them.
        PatternKind::Or(pats) => {
            let before = out.bindings.len();
            for (i, p) in pats.iter().enumerate() {
                let first_of_this = out.bindings.len();
                collect_pattern_bindings(p, visible_from, expr_ty, scope_end, out);
                if i == 0 {
                    continue;
                }
                for k in first_of_this..out.bindings.len() {
                    let canonical = out.bindings[before..first_of_this]
                        .iter()
                        .find(|b| b.name == out.bindings[k].name)
                        .map(LocalBinding::id);
                    if out.bindings[k].same_as.is_none() {
                        out.bindings[k].same_as = canonical;
                    }
                }
            }
        }
        PatternKind::Constructor {
            name,
            name_span,
            args,
            ..
        } => {
            // The types of the variant's fields, at the type of the
            // value it matches (`Circle` of `Shape`: `[Float]`, `Some`
            // of `Option(Int)`: `[Int]`).
            let fields = match (expr_ty, out.tables) {
                (Some(Type::Generic(ty, ty_args)), Some(tables)) => {
                    tables.variant_fields_at(*ty, ty_args, *name)
                }
                _ => None,
            };
            // Without the type it matches, the variant's own, general.
            let ty = match (&fields, expr_ty) {
                (Some(fields), Some(ty)) if fields.is_empty() => Some(ty.clone()),
                (Some(fields), Some(ty)) => Some(Type::Fun(fields.clone(), Box::new(ty.clone()))),
                _ => match (pattern.res, out.tables) {
                    (Some(Res::Def(id)), Some(tables)) => {
                        tables.scheme_of(id).map(|scheme| written_general(&scheme))
                    }
                    _ => None,
                },
            };
            out.constructors.push(PatternConstructor {
                span: *name_span,
                ty,
            });
            for (i, p) in args.iter().enumerate() {
                let field = fields.as_ref().and_then(|fields| fields.get(i));
                collect_pattern_bindings(p, visible_from, field, scope_end, out);
            }
        }
        PatternKind::Record { fields, .. } => {
            // Propagate each declared field's type when the value's type
            // is a nominal record, so hover on a destructured field shows
            // the right type.
            let field_tys = expr_ty.and_then(|ty| record_field_types(ty, out.records));
            let lookup_field_ty = |fname: Symbol| -> Option<Type> {
                field_tys
                    .as_ref()
                    .and_then(|fs| fs.iter().find(|(n, _)| *n == fname).map(|(_, t)| t.clone()))
            };
            for (name, name_span, sub) in fields {
                if let Some(p) = sub {
                    let ty = lookup_field_ty(*name);
                    collect_pattern_bindings(p, visible_from, ty.as_ref(), scope_end, out);
                } else {
                    out.bindings.push(LocalBinding {
                        name: *name,
                        binding_offset: name_span.start as usize,
                        binding_len: resolve(*name).len(),
                        scope_start: visible_from,
                        scope_end,
                        ty: lookup_field_ty(*name),
                        same_as: None,
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
            let field_tys = expr_ty.and_then(|ty| record_field_types(ty, out.records));
            let lookup_field_ty = |fname: Symbol| -> Option<Type> {
                field_tys
                    .as_ref()
                    .and_then(|fs| fs.iter().find(|(n, _)| *n == fname).map(|(_, t)| t.clone()))
            };
            for (name, name_span, sub) in fields {
                if let Some(p) = sub {
                    let ty = lookup_field_ty(*name);
                    collect_pattern_bindings(p, visible_from, ty.as_ref(), scope_end, out);
                } else {
                    out.bindings.push(LocalBinding {
                        name: *name,
                        binding_offset: name_span.start as usize,
                        binding_len: resolve(*name).len(),
                        scope_start: visible_from,
                        scope_end,
                        ty: lookup_field_ty(*name),
                        same_as: None,
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
                out.bindings.push(LocalBinding {
                    name: *r,
                    binding_offset: r_span.start as usize,
                    binding_len: resolve(*r).len(),
                    scope_start: visible_from,
                    scope_end,
                    ty,
                    same_as: None,
                });
            }
        }
        PatternKind::Map(entries) => {
            // Round-101: map-pattern values bind (`#{ "k": v }` binds
            // `v`); keys are string literals, never binders.
            for (_, p) in entries {
                collect_pattern_bindings(p, visible_from, None, scope_end, out);
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
                collect_pattern_bindings(p, visible_from, elem_ty.as_ref(), scope_end, out);
            }
            if let Some(r) = rest {
                collect_pattern_bindings(r, visible_from, list_ty.as_ref(), scope_end, out);
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
