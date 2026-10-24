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

/// Whether a value of a type with the trait `tr` is passed with the
/// trait's dictionary (`crate::evidence`): a trait a program declares.
/// A builtin trait is found by the value (Equal, Compare, Hash, Display,
/// json.Encode, json.Decode, Error, Number).
pub(super) fn has_dictionary(tr: TraitKey) -> bool {
    !crate::defs::is_builtin_trait(tr.id)
}

/// Whether the predicate `pred` passes a dictionary: a trait predicate
/// of a trait that has them.
pub(super) fn passes_dictionary(pred: &Pred) -> bool {
    matches!(pred, Pred::Trait { tr, .. } if has_dictionary(*tr))
}

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
    /// Each name a `let` inside a body binds with a pattern of several
    /// names, generalised over a trait predicate, by the span of the
    /// name: the place of each of its bounds among the `let`'s
    /// (`Program::pattern_lets`).
    pub(super) pattern_lets: HashMap<Span, Vec<u16>>,
    /// The bounds of the body of each method of the module's impls, by
    /// the span of the body: the predicates of the impl's header, the
    /// method's, and for an impl for every function or tuple the trait
    /// of `self`.
    pub(super) methods: HashMap<Span, Vec<BoundKey>>,
    /// The predicates of earlier REPL inputs this one solved: each with
    /// the node of its evidence here.
    pub(super) late: Vec<(crate::evidence::Late, EvId)>,
    /// The bounds of the module's top-level definitions, by name, when
    /// the evidence is written.
    pub(super) tops: HashMap<Symbol, Vec<BoundKey>>,
}

/// The evidence node of a predicate a use owes: of the module that owes
/// it, which for a REPL input may be an earlier input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EvRef {
    pub(super) module: crate::session::ModuleId,
    pub(super) id: EvId,
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
    /// (The predicate of an earlier REPL input gets a node here, which
    /// the input's evidence names: `Ledger::late`.)
    pub(super) fn ev_set(&mut self, at: Option<EvRef>, node: EvNode) {
        let Some(at) = at else {
            return;
        };
        if at.module != self.module {
            // (Only a program trait's evidence is code to an input.)
            let tr = match &node {
                EvNode::Impl { tr, .. } | EvNode::Bound { tr, .. } | EvNode::Structural(tr) => *tr,
                EvNode::Pending => return,
            };
            if !has_dictionary(tr) {
                return;
            }
            let id = self.ev_new(node);
            let late = crate::evidence::Late {
                cell: at.module,
                id: at.id,
            };
            self.ev.late.push((late, id));
            return;
        }
        if let Some(slot) = self.ev.nodes.get_mut(at.id as usize) {
            *slot = node;
        }
    }

    /// The node `id` of the module checked, as a predicate owes it.
    pub(super) fn ev_here(&self, id: EvId) -> EvRef {
        EvRef {
            module: self.module,
            id,
        }
    }

    /// How many predicates the header of the impl of `tr` for `head`
    /// asks, and how many bounds `tr` declares for its method `method`:
    /// the first parts of what a use of the impl's method owes.
    /// (With `dictionaries`, only those that pass a dictionary.)
    fn method_parts(
        &self,
        tr: TraitKey,
        head: TypeRef,
        method: Symbol,
        dictionaries: bool,
    ) -> (usize, usize) {
        let count = |preds: &Vec<Pred>| match dictionaries {
            true => preds.iter().filter(|pred| passes_dictionary(pred)).count(),
            false => preds.len(),
        };
        let header = (self.tables.impl_preds.get(&(tr, head))).map_or(0, count);
        let declared = (self.tables.traits.get(&tr))
            .and_then(|info| info.method_bounds.get(&method))
            .map_or(0, count);
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
        let (header, declared) = self.method_parts(tr, head, method, false);
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
        let (header, declared) = self.method_parts(tr, head, method, true);
        let mut own = owed.split_off(header.min(owed.len()));
        own.truncate(declared);
        let recv = self.ev_new(EvNode::Impl {
            tr,
            ty: head,
            subs: owed,
        });
        (recv, own)
    }

    /// The bounds of a scheme's predicates that pass a dictionary, in
    /// order.
    /// The bounds of `scheme`, the scheme of a name a `let` binds with a
    /// pattern of several names, each as the place it has among those of
    /// the `let`'s value, `all`, which get the ones they do not have yet.
    pub(super) fn pattern_places(&self, scheme: &Scheme, all: &mut Vec<BoundKey>) -> Vec<u16> {
        (self.trait_bounds(&scheme.preds).into_iter())
            .map(|bound| {
                let at = match all.iter().position(|b| *b == bound) {
                    Some(at) => at,
                    None => {
                        all.push(bound);
                        all.len() - 1
                    }
                };
                u16::try_from(at).unwrap_or(u16::MAX)
            })
            .collect()
    }

    pub(super) fn trait_bounds(&self, preds: &[Pred]) -> Vec<BoundKey> {
        preds
            .iter()
            .filter(|pred| passes_dictionary(pred))
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
            // (A REPL input's predicate a later input decides.)
            Some(EvNode::Pending) if self.is_cell => Evidence::Pending(crate::evidence::Late {
                cell: self.module,
                id,
            }),
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
                    return (path.into_iter()).fold(param(k), |of, (to, index)| Evidence::Super {
                        of: Box::new(of),
                        to: to.id,
                        index,
                    });
                }
            }
        }
        Evidence::Missing
    }

    /// The supertraits from `from(from_args)` to `to(to_args)`, one step
    /// at a time, if `to` is one of `from`'s: each with its place among
    /// the supertraits of the trait before it.
    fn super_path(
        &self,
        from: TraitKey,
        from_args: &[Type],
        to: TraitKey,
        to_args: &[Type],
    ) -> Option<Vec<(TraitKey, u16)>> {
        let mut seen: Vec<TraitKey> = vec![from];
        let mut next: Vec<(TraitKey, Vec<Type>, Vec<(TraitKey, u16)>)> =
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
                    path.push((*sup, u16::try_from(i).unwrap_or(u16::MAX)));
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
            .filter(|(_, bound)| has_dictionary(*bound))
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
        .filter(|pred| passes_dictionary(pred))
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
    pub(super) fn write_evidence(&mut self, program: &mut Program, env: &TypeEnv) {
        // (A use of a definition of the module inside the group it is
        // checked in passes the evidence of its bounds from the user.)
        let tops: HashMap<Symbol, Vec<BoundKey>> = (program.decls.iter())
            .filter(|decl| matches!(decl, Decl::Fn(_) | Decl::Let { .. }))
            .flat_map(crate::parser::top_level_binders)
            .filter_map(|(name, _, _)| {
                let bounds = self.trait_bounds(&env.lookup(name)?.preds);
                (!bounds.is_empty()).then_some((name, bounds))
            })
            .collect();
        self.ev.tops = tops;
        self.write_evidence_of(program, env);
    }

    /// `write_evidence`, the module's top-level bounds being known.
    fn write_evidence_of(&self, program: &mut Program, env: &TypeEnv) {
        use crate::evidence::{Layout, Shape};
        program.late_evidence = (self.ev.late.iter())
            .map(|(late, id)| (*late, self.ev_final(*id, &[], 0)))
            .collect();
        let count = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        let scheme = |n: usize| Shape {
            layout: Layout::Scheme,
            header: 0,
            own: count(n),
        };
        // (A `let` inside a body that the checker generalises over a
        // bound.)
        let mut owners: Vec<(Span, Shape)> = (self.ev.lets.iter())
            .map(|(tag, bounds)| (*tag, scheme(bounds.len())))
            .collect();
        let mut pattern_lets: Vec<(Span, Vec<u16>)> = Vec::new();
        for decl in &mut program.decls {
            match decl {
                Decl::Fn(f) => {
                    let bounds = env
                        .lookup(f.name)
                        .map(|scheme| self.trait_bounds(&scheme.preds))
                        .unwrap_or_default();
                    let tag = f.body.span;
                    if !bounds.is_empty() {
                        owners.push((tag, scheme(bounds.len())));
                    }
                    self.evidence_in(&mut f.body, &mut Vec::new(), tag, bounds);
                }
                Decl::Let { pattern, value, .. } => {
                    let bounds = match &pattern.kind {
                        PatternKind::Ident(name) => env
                            .lookup(*name)
                            .map(|scheme| self.trait_bounds(&scheme.preds))
                            .unwrap_or_default(),
                        // (Each name of a pattern is general as its part
                        // is: the value's bounds are all the names'.)
                        _ => {
                            let mut bounds = Vec::new();
                            for (name, at) in super::names::binders(pattern) {
                                if let Some(scheme) = env.lookup(name) {
                                    let places = self.pattern_places(scheme, &mut bounds);
                                    if !places.is_empty() {
                                        pattern_lets.push((at, places));
                                    }
                                }
                            }
                            bounds
                        }
                    };
                    let tag = value.span;
                    if !bounds.is_empty() {
                        owners.push((tag, scheme(bounds.len())));
                    }
                    self.evidence_in(value, &mut Vec::new(), tag, bounds);
                }
                Decl::TraitImpl(ti) => {
                    if let (Some(target), Some(tr)) = (self.impl_target(ti), self.impl_trait(ti)) {
                        ti.evidence = self.impl_evidence(tr, target);
                        // A method of a program trait's impl takes the
                        // impl's dictionary, then those of the bounds the
                        // trait declares for it.
                        if has_dictionary(tr) {
                            for method in &ti.methods {
                                let (header, own) =
                                    self.method_parts(tr, target, method.name, true);
                                let shape = Shape {
                                    layout: Layout::ImplMethod,
                                    header: count(header),
                                    own: count(own),
                                };
                                owners.push((method.body.span, shape));
                            }
                        }
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
                        let own = (info.method_bounds.get(&method.name))
                            .map(|preds| self.trait_bounds(preds))
                            .unwrap_or_default();
                        let shape = Shape {
                            layout: Layout::Default,
                            header: 0,
                            own: count(own.len()),
                        };
                        owners.push((method.body.span, shape));
                        bounds.extend(own);
                        bounds.extend(wheres.iter().cloned());
                        let tag = method.body.span;
                        self.evidence_in(&mut method.body, &mut Vec::new(), tag, bounds);
                    }
                }
                _ => {}
            }
        }
        program.dict_owners = owners;
        pattern_lets.extend(
            self.ev
                .pattern_lets
                .iter()
                .map(|(at, places)| (*at, places.clone())),
        );
        program.pattern_lets = pattern_lets;
    }

    /// The evidence of the use `expr` of the name `name`, a definition
    /// with bounds that owes none where it is used: a top-level
    /// definition of the module used in the group it is checked with (in
    /// its own body too), where it has one type. Each bound is the
    /// user's, as the definitions of a group share their variables (the
    /// bounds of the group are on each). (A `let`'s closure used inside
    /// itself is the closure, which has its dictionaries.)
    fn unowed_bounds(&self, expr: &Expr, name: Symbol, owners: &[Owner]) -> Option<Vec<Evidence>> {
        let Some(crate::defs::Res::Def(id)) = expr.res else {
            return None;
        };
        let def = self.def(id)?;
        if def.module != self.module
            || !matches!(
                def.kind,
                crate::defs::DefKind::Fn | crate::defs::DefKind::Let
            )
        {
            return None;
        }
        let bounds = self.ev.tops.get(&name)?;
        Some(
            (bounds.iter())
                .map(|(var, tr, args)| self.bound_evidence(*var, *tr, args, owners))
                .collect(),
        )
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
        } else if let ExprKind::Ident(name) = expr.kind
            && let Some(bounds) = self.unowed_bounds(expr, name, owners)
        {
            expr.ev = Some(Box::new(Site::Ref(bounds)));
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
