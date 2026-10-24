//! The evidence of the trait predicates the checker solves
//! (`crate::evidence`). While a module is checked, each predicate a use
//! owes (`Wanted`) has a node in the module's [`Ledger`], which says,
//! once the predicate is solved, what solved it. When the module's types
//! are final, `write_evidence` turns the nodes into the
//! [`crate::evidence::Site`] of each place that passes evidence on: the
//! uses of definitions with bounds and the calls of trait methods.

use super::*;
use crate::evidence::{Evidence, Site};

/// A node of the [`Ledger`].
pub(super) type EvId = u32;

/// What the checker knows of one predicate's proof.
#[derive(Debug, Clone)]
pub(super) enum EvNode {
    /// Not solved (yet): the predicate is still owed, or failed.
    Pending,
    /// The impl of `tr` for `ty`, and the nodes of what its header asks.
    Impl {
        tr: TraitKey,
        ty: TypeRef,
        subs: Vec<EvId>,
    },
    /// A bound of the variable `var` of an enclosing definition, by a
    /// `where` clause or taken into its scheme: which bound (and through
    /// which supertraits) is found when the definitions are known.
    Bound {
        var: TyVar,
        tr: TraitKey,
        args: Vec<Type>,
    },
    /// The trait holds by the type's structure.
    Structural(TraitKey),
}

/// A bound of a definition: its subject variable, trait and trait
/// arguments.
pub(super) type BoundKey = (TyVar, TraitKey, Vec<Type>);

/// The evidence of the module being checked.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ledger {
    pub(super) nodes: Vec<EvNode>,
    /// The uses of definitions with bounds, by the span of the use: the
    /// node of each bound the use owes.
    pub(super) refs: HashMap<Span, Vec<EvId>>,
    /// The calls through a trait (`Tr.m`), by the span of the access:
    /// the receiver's node and those of the method's own bounds.
    pub(super) calls: HashMap<Span, (EvId, Vec<EvId>)>,
    /// What the scheme instantiated last owes of traits, for the use
    /// that instantiated it (`instantiate`).
    pub(super) use_ev: Option<Vec<EvId>>,
    /// The bounds of each `let` inside a body that the checker
    /// generalises over a trait predicate, by the span of its value.
    pub(super) lets: HashMap<Span, Vec<BoundKey>>,
    /// The bounds of the body of each method of the module's impls, by
    /// the span of the body: the predicates of the impl's header, the
    /// method's, and for an impl for every function or tuple the trait
    /// of `self`.
    pub(super) methods: HashMap<Span, Vec<BoundKey>>,
}

/// A definition whose body is being walked: the span that names it
/// (`Evidence::Param`), with its bounds.
struct Owner {
    tag: Span,
    bounds: Vec<BoundKey>,
}

impl TypeChecker {
    /// A new node of the ledger.
    pub(super) fn ev_new(&mut self, node: EvNode) -> EvId {
        let id = EvId::try_from(self.ev.nodes.len()).unwrap_or(EvId::MAX);
        self.ev.nodes.push(node);
        id
    }

    /// Say what solved the predicate of the node `id`.
    pub(super) fn ev_set(&mut self, id: Option<EvId>, node: EvNode) {
        if let Some(slot) = id.and_then(|id| self.ev.nodes.get_mut(id as usize)) {
            *slot = node;
        }
    }

    /// How many predicates the header of the impl of `tr` for `head`
    /// asks, and how many bounds `tr` declares for its method `method`:
    /// the first parts of what a use of the impl's method owes.
    fn method_parts(&self, tr: TraitKey, head: TypeRef, method: Symbol) -> (usize, usize) {
        let header = (self.tables.impl_preds.get(&(tr, head))).map_or(0, Vec::len);
        let declared = (self.tables.traits.get(&tr))
            .and_then(|info| info.method_bounds.get(&method))
            .map_or(0, Vec::len);
        (header, declared)
    }

    /// The bounds of the body of the impl's method `method` of the impl
    /// of `tr` for `head`, from what its body is checked with (`bounds`):
    /// the predicates of the header, then the bounds the trait declares
    /// for the method, then, for an impl for every function or tuple,
    /// the trait of `self`. A bound the method's `where` restates is one
    /// of these (or a supertrait of one), not a bound of its own: a call
    /// passes the evidence of these, in this order.
    pub(super) fn method_body_bounds(
        &self,
        tr: TraitKey,
        head: TypeRef,
        method: Symbol,
        bounds: &[Pred],
    ) -> Vec<BoundKey> {
        let (header, declared) = self.method_parts(tr, head, method);
        let own = header + declared;
        let mut kept: Vec<Pred> = bounds.iter().take(own).cloned().collect();
        // (The trait of `self`, which the body is checked with last.)
        if bounds.len() > own
            && let Some(last @ Pred::Trait { subject, tr: t, .. }) = bounds.last()
            && *t == tr
            && matches!(self.apply(subject), Type::Var(v) if self.shape_vars.contains_key(&v))
        {
            kept.push(last.clone());
        }
        self.trait_bounds(&kept)
    }

    /// The evidence a use of the impl's method `method` of the impl of
    /// `tr` for `head` passes on, from what the use owes (`owed`, the
    /// nodes of the trait predicates of the method's scheme): the
    /// receiver's (the impl, with the evidence of its header) and that of
    /// each bound the trait declares for the method. (What the method's
    /// own `where` restates is owed after those, and passes nothing on.)
    pub(super) fn method_site(
        &mut self,
        tr: TraitKey,
        head: TypeRef,
        method: Symbol,
        mut owed: Vec<EvId>,
    ) -> (EvId, Vec<EvId>) {
        let (header, declared) = self.method_parts(tr, head, method);
        let mut own = owed.split_off(header.min(owed.len()));
        own.truncate(declared);
        let recv = self.ev_new(EvNode::Impl {
            tr,
            ty: head,
            subs: owed,
        });
        (recv, own)
    }

    /// The trait bounds of a scheme's predicates, in order.
    pub(super) fn trait_bounds(&self, preds: &[Pred]) -> Vec<BoundKey> {
        preds
            .iter()
            .filter_map(|pred| match pred {
                Pred::Trait {
                    tr, args, subject, ..
                } => {
                    let var = self.bound_var(subject)?;
                    Some((var, *tr, args.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// The variable a bound is on, as the definitions name it.
    fn bound_var(&self, subject: &Type) -> Option<TyVar> {
        match self.apply(subject) {
            Type::Var(v) => Some(self.rigid_rep_var(v)),
            Type::Rigid(r) => Some(self.rigid_rep_var(r.var)),
            _ => None,
        }
    }

    /// A type as two bounds compare it: final, with every annotation
    /// variable written as its variable.
    fn bound_type(&self, ty: &Type) -> Type {
        let canon = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        let ty = unrigidify(&canon).0;
        let reps: HashMap<TyVar, Type> = free_vars_in(&ty)
            .into_iter()
            .map(|v| (v, Type::Var(self.rigid_rep_var(v))))
            .collect();
        substitute_vars(&ty, &reps)
    }

    /// Whether the trait arguments `a` and `b` of two bounds of one
    /// trait agree (a bare bound says nothing of them).
    fn bound_args_agree(&self, a: &[Type], b: &[Type]) -> bool {
        a.is_empty()
            || b.is_empty()
            || (a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(a, b)| self.bound_type(a) == self.bound_type(b)))
    }

    /// Whether the impl of `tr` for `ty` is one the runtime provides: no
    /// program wrote it (it has no impl block).
    fn provided(&self, tr: TraitKey, ty: TypeRef) -> bool {
        !self.tables.trait_impl_spans.contains_key(&(tr, ty))
    }

    /// The evidence of the node `id`, in a body inside `owners`
    /// (innermost last).
    fn ev_final(&self, id: EvId, owners: &[Owner], depth: usize) -> Evidence {
        if depth > 64 {
            return Evidence::Missing;
        }
        match self.ev.nodes.get(id as usize) {
            None | Some(EvNode::Pending) => Evidence::Missing,
            Some(EvNode::Structural(tr)) => Evidence::Structural { tr: tr.id },
            Some(EvNode::Impl { tr, ty, .. }) if self.provided(*tr, *ty) => Evidence::Native {
                tr: tr.id,
                ty: ty.id,
            },
            Some(EvNode::Impl { tr, ty, subs }) => Evidence::Impl {
                tr: tr.id,
                ty: ty.id,
                subs: subs
                    .iter()
                    .map(|sub| self.ev_final(*sub, owners, depth + 1))
                    .collect(),
            },
            Some(EvNode::Bound { var, tr, args }) => self.bound_evidence(*var, *tr, args, owners),
        }
    }

    /// The evidence of `var: tr(args)`, a bound of an enclosing
    /// definition: the bound itself, or one whose trait has `tr` among
    /// its supertraits, the innermost definition first.
    fn bound_evidence(
        &self,
        var: TyVar,
        tr: TraitKey,
        args: &[Type],
        owners: &[Owner],
    ) -> Evidence {
        let var = self.rigid_rep_var(var);
        for owner in owners.iter().rev() {
            let param = |k: usize| Evidence::Param {
                owner: owner.tag,
                k: u16::try_from(k).unwrap_or(u16::MAX),
            };
            let mine = || owner.bounds.iter().enumerate().filter(|(_, b)| b.0 == var);
            if let Some((k, _)) =
                mine().find(|(_, (_, t, a))| *t == tr && self.bound_args_agree(a, args))
            {
                return param(k);
            }
            for (k, (_, t, a)) in mine() {
                if let Some(path) = self.super_path(*t, a, tr, args) {
                    return path.into_iter().fold(param(k), |of, to| Evidence::Super {
                        of: Box::new(of),
                        to: to.id,
                    });
                }
            }
        }
        Evidence::Missing
    }

    /// The supertraits from `from(from_args)` to `to(to_args)`, one step
    /// at a time, if `to` is one of `from`'s.
    fn super_path(
        &self,
        from: TraitKey,
        from_args: &[Type],
        to: TraitKey,
        to_args: &[Type],
    ) -> Option<Vec<TraitKey>> {
        let mut seen: Vec<TraitKey> = vec![from];
        let mut next: Vec<(TraitKey, Vec<Type>, Vec<TraitKey>)> =
            vec![(from, from_args.to_vec(), Vec::new())];
        while !next.is_empty() {
            let mut later = Vec::new();
            for (t, args, path) in next {
                let Some(info) = self.tables.traits.get(&t) else {
                    continue;
                };
                for (i, sup) in info.supertraits.iter().enumerate() {
                    let sup_args: Vec<Type> = match info.supertrait_args.get(i) {
                        Some(exprs) if !exprs.is_empty() => (exprs.iter())
                            .map(|te| super::inference::resolve_supertrait_arg(te, info, &args))
                            .collect(),
                        _ => Vec::new(),
                    };
                    let mut path = path.clone();
                    path.push(*sup);
                    if *sup == to && self.bound_args_agree(&sup_args, to_args) {
                        return Some(path);
                    }
                    if !seen.contains(sup) {
                        seen.push(*sup);
                        later.push((*sup, sup_args, path));
                    }
                }
            }
            next = later;
        }
        None
    }

    /// What the impl of `tr` for `head` proves from the predicates of its
    /// header: that its type has each supertrait of `tr`, and that its
    /// trait arguments meet the `where` clauses `tr` declares on its
    /// parameters.
    fn impl_evidence(&self, tr: TraitKey, head: TypeRef) -> crate::evidence::ImplEvidence {
        let (Some(info), Some(self_ty)) = (
            self.tables.traits.get(&tr),
            self.tables.impl_self_types.get(&(tr, head)),
        ) else {
            return Default::default();
        };
        let header = Owner {
            tag: crate::evidence::IMPL_HEADER,
            bounds: (self.tables.impl_preds.get(&(tr, head)))
                .map(|preds| self.trait_bounds(preds))
                .unwrap_or_default(),
        };
        let impl_args = (self.tables.impl_trait_args.get(&(tr, head)))
            .cloned()
            .unwrap_or_default();
        let supers = (info.supertraits.iter().enumerate())
            .map(|(i, sup)| {
                let args: Vec<Type> = match info.supertrait_args.get(i) {
                    Some(exprs) if !exprs.is_empty() => (exprs.iter())
                        .map(|te| super::inference::resolve_supertrait_arg(te, info, &impl_args))
                        .collect(),
                    _ => Vec::new(),
                };
                (sup.id, self.evidence_of(*sup, &args, self_ty, &header, 0))
            })
            .collect();
        let wheres = (self.param_wheres(tr).into_iter())
            .map(|(i, bound)| match impl_args.get(i) {
                Some(arg) => self.evidence_of(bound, &[], arg, &header, 0),
                None => Evidence::Missing,
            })
            .collect();
        crate::evidence::ImplEvidence { supers, wheres }
    }

    /// The `where` clauses the trait `tr` declares on its parameters
    /// (`trait Pr(a) where a: Greet`), in order: the parameter's place
    /// and the trait.
    fn param_wheres(&self, tr: TraitKey) -> Vec<(usize, TraitKey)> {
        let Some(info) = self.tables.traits.get(&tr) else {
            return Vec::new();
        };
        (info.param_where_clauses.iter())
            .filter_map(|(param, bound)| {
                Some((info.params.iter().position(|p| p == param)?, *bound))
            })
            .collect()
    }

    /// The evidence that `ty` implements `tr(args)` where the bounds of
    /// `owner` are given.
    fn evidence_of(
        &self,
        tr: TraitKey,
        args: &[Type],
        ty: &Type,
        owner: &Owner,
        depth: usize,
    ) -> Evidence {
        if depth > 16 {
            return Evidence::Missing;
        }
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        if let Some(var) = self.bound_var(&ty) {
            let owners = [Owner {
                tag: owner.tag,
                bounds: owner.bounds.clone(),
            }];
            return self.bound_evidence(var, tr, args, &owners);
        }
        let Some(head) = self.type_name_for_impl(&ty) else {
            return Evidence::Missing;
        };
        if self.by_structure(tr, head) {
            return Evidence::Structural { tr: tr.id };
        }
        if !self.tables.trait_impl_set.contains(&(tr, head)) {
            return Evidence::Missing;
        }
        if self.provided(tr, head) {
            return Evidence::Native {
                tr: tr.id,
                ty: head.id,
            };
        }
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        if let Some(impl_ty) = self.tables.impl_self_types.get(&(tr, head)) {
            super::builtin_needs::match_type(impl_ty, &ty, &free_vars_in(impl_ty), &mut mapping);
        }
        let subs = (self
            .tables
            .impl_preds
            .get(&(tr, head))
            .into_iter()
            .flatten())
        .filter_map(|pred| match pred {
            Pred::Trait {
                tr, args, subject, ..
            } => {
                let args: Vec<Type> = args.iter().map(|t| substitute_vars(t, &mapping)).collect();
                let subject = substitute_vars(subject, &mapping);
                Some(self.evidence_of(*tr, &args, &subject, owner, depth + 1))
            }
            _ => None,
        })
        .collect();
        Evidence::Impl {
            tr: tr.id,
            ty: head.id,
            subs,
        }
    }

    /// Write the evidence of every place in `program` that passes some
    /// on (`Expr::ev`), the module's definitions being final in `env`.
    pub(super) fn write_evidence(&self, program: &mut Program, env: &TypeEnv) {
        for decl in &mut program.decls {
            match decl {
                Decl::Fn(f) => {
                    let bounds = env
                        .lookup(f.name)
                        .map(|scheme| self.trait_bounds(&scheme.preds))
                        .unwrap_or_default();
                    let tag = f.body.span;
                    self.evidence_in(&mut f.body, &mut Vec::new(), tag, bounds);
                }
                Decl::Let { pattern, value, .. } => {
                    let bounds = match &pattern.kind {
                        PatternKind::Ident(name) => env
                            .lookup(*name)
                            .map(|scheme| self.trait_bounds(&scheme.preds))
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    };
                    let tag = value.span;
                    self.evidence_in(value, &mut Vec::new(), tag, bounds);
                }
                Decl::TraitImpl(ti) => {
                    if let (Some(target), Some(tr)) = (self.impl_target(ti), self.impl_trait(ti)) {
                        ti.evidence = self.impl_evidence(tr, target);
                    }
                    for method in &mut ti.methods {
                        let tag = method.body.span;
                        let bounds = self.ev.methods.get(&tag).cloned().unwrap_or_default();
                        self.evidence_in(&mut method.body, &mut Vec::new(), tag, bounds);
                    }
                }
                Decl::Trait(t) => {
                    let tr = self.own_trait(t.name);
                    let Some(info) = self.tables.traits.get(&tr) else {
                        continue;
                    };
                    let own_args: Vec<Type> =
                        info.param_var_ids.iter().map(|v| Type::Var(*v)).collect();
                    let wheres: Vec<BoundKey> = (self.param_wheres(tr).into_iter())
                        .map(|(i, bound)| (info.param_var_ids[i], bound, Vec::new()))
                        .collect();
                    for method in t.methods.iter_mut().filter(|m| !m.is_signature_only) {
                        let mut bounds = vec![(info.self_var, tr, own_args.clone())];
                        if let Some(preds) = info.method_bounds.get(&method.name) {
                            bounds.extend(self.trait_bounds(preds));
                        }
                        bounds.extend(wheres.iter().cloned());
                        let tag = method.body.span;
                        self.evidence_in(&mut method.body, &mut Vec::new(), tag, bounds);
                    }
                }
                _ => {}
            }
        }
    }

    /// `write_evidence` for the body `expr` of a definition named `tag`
    /// with the bounds `bounds`, inside `owners`.
    fn evidence_in(
        &self,
        expr: &mut Expr,
        owners: &mut Vec<Owner>,
        tag: Span,
        bounds: Vec<BoundKey>,
    ) {
        let owns = !bounds.is_empty();
        if owns {
            owners.push(Owner { tag, bounds });
        }
        self.evidence_walk(expr, owners);
        if owns {
            owners.pop();
        }
    }

    fn evidence_walk(&self, expr: &mut Expr, owners: &mut Vec<Owner>) {
        let span = expr.span;
        if matches!(expr.kind, ExprKind::FieldAccess(..))
            && let Some((recv, own)) = self.ev.calls.get(&span)
        {
            expr.ev = Some(Box::new(Site::Method {
                recv: self.ev_final(*recv, owners, 0),
                own: own.iter().map(|k| self.ev_final(*k, owners, 0)).collect(),
            }));
        } else if matches!(expr.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..))
            && let Some(ids) = self.ev.refs.get(&span)
        {
            expr.ev = Some(Box::new(Site::Ref(
                ids.iter().map(|k| self.ev_final(*k, owners, 0)).collect(),
            )));
        }
        if let ExprKind::Block(stmts) = &mut expr.kind {
            for stmt in stmts {
                match stmt {
                    Stmt::Let { value, .. } => {
                        let tag = value.span;
                        let bounds = self.ev.lets.get(&tag).cloned().unwrap_or_default();
                        self.evidence_in(value, owners, tag, bounds);
                    }
                    Stmt::When {
                        expr, else_body, ..
                    } => {
                        self.evidence_walk(expr, owners);
                        self.evidence_walk(else_body, owners);
                    }
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => {
                        self.evidence_walk(condition, owners);
                        self.evidence_walk(else_body, owners);
                    }
                    Stmt::Expr(e) => self.evidence_walk(e, owners),
                }
            }
            return;
        }
        super::resolve::each_child_mut(expr, &mut |child| self.evidence_walk(child, owners));
    }
}

#[cfg(test)]
mod tests;
