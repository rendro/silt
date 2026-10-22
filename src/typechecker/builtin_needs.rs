//! What a function or a method passes on to builtins (`Pred::Given`,
//! `Pred::Via`): a body that gives an annotation variable of its
//! signature to a decoder, a postgres connection or a statement's
//! parameters, or calls a trait method on a value whose type is one,
//! asks of the type each use gives the variable what the builtin, or the
//! method of that type's impl, asks. What a body asks is known once the
//! bodies of its module are checked: the uses wait until then
//! (`settle_builtin_needs`).

use super::solve::{Goal, Origin};
use super::*;
use std::rc::Rc;

/// A use of a function or a method that gives a type to an annotation
/// variable of its signature (`Pred::Given`), waiting for the bodies of
/// the module.
#[derive(Clone, Debug)]
pub(crate) struct GivenUse {
    pub(super) given: TyVar,
    pub(super) subject: Type,
    /// The alias the use names the type by, if it names it by one.
    pub(super) written: Option<Symbol>,
    /// The function or method a note names.
    pub(super) passer: Symbol,
    /// The method body whose variable `given` is (`Pred::Given`).
    pub(super) body: Option<Symbol>,
    /// The method body the use is in: what its subject asks, if the
    /// subject is an annotation variable, is that body's.
    pub(super) in_body: Option<Symbol>,
    pub(super) origin: Origin,
    /// Whether `origin` is the argument of the call that gives the type:
    /// no later call is its place.
    pub(super) placed: bool,
    /// The instantiation that owes it: the uses of one call share it.
    pub(super) use_id: usize,
    /// Whether a definition took it into its scheme: each use of that
    /// owes it, not this.
    pub(super) taken: bool,
}

/// What a body asks of the type an annotation variable of its signature
/// stands for: a call of the method `method` of the trait `tr` on a
/// value of the type `receiver`, with the method's type `method_ty` at
/// the call (a `Pred::Via` whose receiver is the variable).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViaNeed {
    pub(super) tr: TraitKey,
    pub(super) method: Symbol,
    pub(super) receiver: Type,
    pub(super) method_ty: Type,
}

/// Each argument of a call: its place, the alias it names a type by,
/// and its type.
pub(super) type CallArguments = [(Span, Option<Symbol>, Type)];

/// The types a use gives, each with its place and the alias it names
/// the type by.
pub(super) type Places = Vec<(Type, Origin, Option<Symbol>)>;

/// A call of a trait method on a value of a known type, waiting for the
/// bodies of the module: in the body `in_body`, at `origin`, a note
/// naming `passer`.
#[derive(Clone, Debug)]
pub(crate) struct ViaUse {
    pub(super) need: ViaNeed,
    pub(super) passer: Option<Symbol>,
    pub(super) origin: Origin,
    /// The types the call gives the method, each at its argument.
    pub(super) places: Places,
    pub(super) in_body: Option<Symbol>,
}

/// What a trait method of a type asks (`TypeChecker::leaves`): a check of
/// a type it gives a builtin, or a call of a trait method on a value
/// whose type is an annotation variable of a caller.
#[derive(Clone, Debug)]
pub(crate) enum Leaf {
    Check(BuiltinCheck, Type),
    Via(ViaNeed),
}

/// Where a use's checks go: the types it gives, at their places, and the
/// function its caller called.
struct Placing {
    passer: Option<Symbol>,
    origin: Origin,
    places: Places,
}

/// The keys `TypeChecker::leaves` keeps what it found by.
pub(crate) type LeafMemo = HashMap<(Symbol, u64), Vec<(TraitKey, Type, Type, Rc<Vec<Leaf>>)>>;

/// The method `method` of the impl of `tr` for `owner` (for a trait's
/// default, `owner` is empty), as the needs of its body are kept: an
/// impl's methods share its parameters, a trait's defaults its `Self`.
pub(super) fn body_key(tr: TraitKey, owner: &str, method: Symbol) -> Symbol {
    intern(&format!("{tr:?}/{owner}.{method}"))
}

impl TypeChecker {
    /// The name a note gives the function or method `passer` at a use
    /// whose callee is written `origin.callee`: as the call writes it
    /// (`store.load`), if that is the function.
    pub(super) fn passer_at(&self, passer: Symbol, origin: &Origin) -> Symbol {
        match origin.callee {
            Some(label) if label == passer || resolve(label).ends_with(&format!(".{passer}")) => {
                label
            }
            _ => passer,
        }
    }

    /// What the body of its function (or the method body `body`) asks of
    /// the annotation variable `var` for the builtins it gives it to.
    pub(super) fn builtin_needs_of(&self, var: TyVar, body: Option<Symbol>) -> Vec<BuiltinCheck> {
        (self.builtin_needs.get(&(self.rigid_rep_var(var), body)))
            .cloned()
            .unwrap_or_default()
    }

    /// The trait methods the body of its function calls on a value of
    /// the type of the annotation variable `var`, over the variables of
    /// the signature (each as the variable of its representative).
    pub(super) fn via_needs_of(&self, var: TyVar, body: Option<Symbol>) -> Vec<ViaNeed> {
        let rep = |checker: &Self, ty: &Type| {
            map_rigid(&checker.apply(ty), &mut |r| {
                Type::Var(checker.rigid_rep_var(r.var))
            })
        };
        (self
            .via_needs
            .get(&(self.rigid_rep_var(var), body))
            .into_iter()
            .flatten())
        .map(|need| ViaNeed {
            tr: need.tr,
            method: need.method,
            receiver: rep(self, &need.receiver),
            method_ty: rep(self, &need.method_ty),
        })
        .collect()
    }

    /// `via_needs_of`, kept while the needs do not grow (`via_cache`).
    fn vias_of(&mut self, var: TyVar, body: Option<Symbol>) -> Rc<Vec<ViaNeed>> {
        let key = (self.rigid_rep_var(var), body);
        if let Some(found) = self.via_cache.get(&key) {
            return found.clone();
        }
        let found = Rc::new(self.via_needs_of(var, body));
        self.via_cache.insert(key, found.clone());
        found
    }

    /// A call of a trait method owed (`Pred::Via`), its receiver known
    /// now: on an annotation variable, a need of the body the call is in
    /// (`in_body`); on a type, decided with the module's bodies
    /// (`via_waiting`), at `origin` (each type the call gives at its
    /// place in `places`), naming `passer`. Whether a need grew.
    pub(super) fn via_owed(
        &mut self,
        need: ViaNeed,
        passer: Option<Symbol>,
        origin: Origin,
        places: Places,
        in_body: Option<Symbol>,
    ) -> bool {
        match self.apply(&need.receiver) {
            Type::Var(_) | Type::Error | Type::Never => false,
            Type::Rigid(r) => self.add_via_need(r, need, in_body),
            _ => {
                self.via_waiting.push(ViaUse {
                    need,
                    passer,
                    origin,
                    places,
                    in_body,
                });
                false
            }
        }
    }

    /// The body `in_body` calls a trait method on a value of the type of
    /// its annotation variable `r`. Whether that is news.
    fn add_via_need(&mut self, r: RigidId, need: ViaNeed, in_body: Option<Symbol>) -> bool {
        let key = (self.rigid_rep_var(r.var), in_body);
        let needs = self.via_needs.entry(key).or_default();
        if needs.contains(&need) {
            return false;
        }
        needs.push(need);
        self.via_cache.clear();
        self.grown_needs.push(key);
        true
    }

    /// The body `in_body` gives its annotation variable `r` to a builtin
    /// that asks `check` of it. Whether that is news.
    fn add_builtin_need(
        &mut self,
        r: RigidId,
        check: BuiltinCheck,
        in_body: Option<Symbol>,
    ) -> bool {
        let key = (self.rigid_rep_var(r.var), in_body);
        let needs = self.builtin_needs.entry(key).or_default();
        if needs.contains(&check.unwritten()) {
            return false;
        }
        needs.push(check.unwritten());
        self.grown_needs.push(key);
        true
    }

    /// Decide the call `need` of a trait method, in the body `in_body`:
    /// on an annotation variable, a need of that body; on a type, what
    /// the method of the type's impl (or the trait's default) asks of the
    /// types the call gives it (`leaves`). With `placing`, the checks of
    /// types are made, each where the use gave its type; without, only
    /// what the body's annotation variables owe is gathered. Whether a
    /// need grew.
    fn apply_via(
        &mut self,
        need: &ViaNeed,
        in_body: Option<Symbol>,
        placing: Option<&Placing>,
    ) -> bool {
        match self.apply(&need.receiver) {
            Type::Var(_) | Type::Error | Type::Never => false,
            Type::Rigid(r) => self.add_via_need(r, need.clone(), in_body),
            receiver => {
                let leaves = self.leaves(need.tr, need.method, &receiver, &need.method_ty);
                let mut grown = false;
                for leaf in leaves.iter() {
                    match leaf {
                        Leaf::Check(check, ty) => match self.apply(ty) {
                            Type::Rigid(r) => grown |= self.add_builtin_need(r, *check, in_body),
                            Type::Var(_) | Type::Error | Type::Never => {}
                            ty => {
                                if let Some(placing) = placing {
                                    self.check_placed(*check, &ty, placing);
                                }
                            }
                        },
                        Leaf::Via(via) => {
                            if let Type::Rigid(r) = self.apply(&via.receiver) {
                                grown |= self.add_via_need(r, via.clone(), in_body);
                            }
                        }
                    }
                }
                grown
            }
        }
    }

    /// Check `check` of the type `ty`, where the use `placing` gave it,
    /// naming the function the caller called.
    fn check_placed(&mut self, check: BuiltinCheck, ty: &Type, placing: &Placing) {
        let (origin, written) = (placing.places.iter())
            .find(|(given, ..)| self.apply(given) == *ty)
            .map_or((placing.origin, None), |(_, origin, written)| {
                (*origin, *written)
            });
        let check = match check {
            BuiltinCheck::Decodes {
                builtin, decoding, ..
            } => BuiltinCheck::Decodes {
                builtin,
                decoding,
                written,
            },
            other => other,
        };
        self.check_builtin_argument(check, ty, placing.passer, origin);
    }

    /// What the method `method` of the impl of `tr` for `receiver` (or
    /// the trait's default it takes), called with the type `method_ty`,
    /// asks: the checks of the types it gives builtins, and the trait
    /// methods it calls on an annotation variable of a caller, through
    /// the methods it calls in turn. Kept by the method and the types
    /// (`leaf_memo`): each call of the method on them reads it.
    fn leaves(
        &mut self,
        tr: TraitKey,
        method: Symbol,
        receiver: &Type,
        method_ty: &Type,
    ) -> Rc<Vec<Leaf>> {
        let receiver = self.apply(receiver);
        let method_ty = self.apply(method_ty);
        let key = (method, shape_hash(&[&receiver, &method_ty]));
        let same = |at: &(TraitKey, Type, Type, Rc<Vec<Leaf>>)| {
            at.0 == tr && same_shape(&[&at.1, &at.2], &[&receiver, &method_ty])
        };
        if let Some(found) = self
            .leaf_memo
            .get(&key)
            .and_then(|at| at.iter().find(|at| same(at)))
        {
            return found.3.clone();
        }
        // A method that calls itself on the same types: what it asks is
        // what the outer call gathers.
        if (self.leaf_busy.iter()).any(|(t, m, r, ty)| {
            *t == tr && *m == method && same_shape(&[r, ty], &[&receiver, &method_ty])
        }) {
            return Rc::new(Vec::new());
        }
        let mut out = Vec::new();
        let entry = self
            .type_name_for_impl(&receiver)
            .and_then(|head| self.tables.impl_methods.get(head, method, tr))
            .filter(|entry| !entry.structural)
            .cloned();
        if let Some(entry) = entry {
            self.leaf_busy
                .push((tr, method, receiver.clone(), method_ty.clone()));
            let scheme = self.method_scheme(&entry);
            // (The method's type read against the call's: no variable of
            // the session is bound.)
            let mut mapping: HashMap<TyVar, Type> = HashMap::new();
            match_type(&scheme.ty, &method_ty, &scheme.vars, &mut mapping);
            let preds: Vec<Pred> = (scheme.preds.iter())
                .map(|pred| pred.substitute(&mapping))
                .collect();
            let given: HashMap<TyVar, Type> = (preds.iter())
                .filter_map(|pred| match pred {
                    Pred::Given { subject, given, .. } => {
                        Some((self.rigid_rep_var(*given), self.apply(subject)))
                    }
                    _ => None,
                })
                .collect();
            for pred in &preds {
                match pred {
                    Pred::Builtin { check, subject, .. } => {
                        out.push(Leaf::Check(*check, self.apply(subject)));
                    }
                    Pred::Given {
                        subject,
                        given: var,
                        body,
                        ..
                    } => {
                        for check in self.builtin_needs_of(*var, *body) {
                            out.push(Leaf::Check(check, self.apply(subject)));
                        }
                        for via in self.vias_of(*var, *body).iter() {
                            let via = self.at_use(via, &given);
                            self.expand(via, &mut out);
                        }
                    }
                    Pred::Via {
                        tr,
                        method,
                        receiver,
                        method_ty,
                        ..
                    } => {
                        let via = ViaNeed {
                            tr: *tr,
                            method: *method,
                            receiver: self.apply(receiver),
                            method_ty: self.apply(method_ty),
                        };
                        self.expand(via, &mut out);
                    }
                    _ => {}
                }
            }
            self.leaf_busy.pop();
        }
        // (Each once: methods that call each other give the same checks
        // many times over.)
        let mut leaves: Vec<Leaf> = Vec::with_capacity(out.len());
        for leaf in out {
            let known = leaves.iter().any(|seen| match (seen, &leaf) {
                (Leaf::Check(a, x), Leaf::Check(b, y)) => a == b && x == y,
                (Leaf::Via(a), Leaf::Via(b)) => a == b,
                _ => false,
            });
            if !known {
                leaves.push(leaf);
            }
        }
        let out = Rc::new(leaves);
        (self.leaf_memo.entry(key).or_default()).push((tr, receiver, method_ty, out.clone()));
        out
    }

    /// The leaves of the trait-method call `via`, gathered into `out`.
    fn expand(&mut self, via: ViaNeed, out: &mut Vec<Leaf>) {
        match self.apply(&via.receiver) {
            Type::Rigid(_) => out.push(Leaf::Via(via)),
            Type::Var(_) | Type::Error | Type::Never => {}
            receiver => {
                let leaves = self.leaves(via.tr, via.method, &receiver, &via.method_ty);
                out.extend(leaves.iter().cloned());
            }
        }
    }

    /// Whether the call `via` names an annotation variable.
    fn names_a_variable(&self, via: &ViaNeed) -> bool {
        let mut found = false;
        for ty in [&via.receiver, &via.method_ty] {
            map_rigid(&self.apply(ty), &mut |r| {
                found = true;
                Type::Rigid(r)
            });
        }
        found
    }

    /// The call `via` of a body's need at a use that gives the body's
    /// variables the types `given`.
    fn at_use(&self, via: &ViaNeed, given: &HashMap<TyVar, Type>) -> ViaNeed {
        ViaNeed {
            receiver: substitute_vars(&via.receiver, given),
            method_ty: substitute_vars(&via.method_ty, given),
            ..via.clone()
        }
    }

    /// The types each waiting use gives the signature's variables.
    fn given_by_use(&self) -> HashMap<usize, HashMap<TyVar, Type>> {
        let mut given_by_use: HashMap<usize, HashMap<TyVar, Type>> = HashMap::new();
        for use_ in &self.given_waiting {
            (given_by_use.entry(use_.use_id).or_default())
                .insert(self.rigid_rep_var(use_.given), self.apply(&use_.subject));
        }
        given_by_use
    }

    /// Each waiting use whose variable a body calls trait methods on,
    /// with those calls at the types the use gives the signature's
    /// variables, and the places it gives them at.
    fn uses_with_vias(&mut self) -> Vec<(GivenUse, Vec<ViaNeed>, Places)> {
        let mut given_by_use: HashMap<usize, HashMap<TyVar, Type>> = HashMap::new();
        let mut places_by_use: HashMap<usize, Places> = HashMap::new();
        for use_ in &self.given_waiting {
            let ty = self.apply(&use_.subject);
            (given_by_use.entry(use_.use_id).or_default())
                .insert(self.rigid_rep_var(use_.given), ty.clone());
            (places_by_use.entry(use_.use_id).or_default()).push((ty, use_.origin, use_.written));
        }
        let mut out = Vec::new();
        for k in 0..self.given_waiting.len() {
            let use_ = self.given_waiting[k].clone();
            if use_.taken {
                continue;
            }
            let vias = self.vias_of(use_.given, use_.body);
            if vias.is_empty() {
                continue;
            }
            let given = &given_by_use[&use_.use_id];
            let vias = vias.iter().map(|via| self.at_use(via, given)).collect();
            let places = places_by_use[&use_.use_id].clone();
            out.push((use_, vias, places));
        }
        out
    }

    /// Point what a decoder asks of its type argument, still owed in the
    /// scope, at the argument of this call that gives the type (`given`):
    /// a decoder held in a list, a record or an Option, returned by a
    /// function, or given to a function the call passes the type to
    /// (`apply(json.parse, s, Blob)`). Where the call gives the callee a
    /// value that holds the decoder as well, the callee passes the type
    /// on: a note names it (`passer`, a function of the program).
    pub(super) fn point_at_type_arguments(
        &mut self,
        given: &CallArguments,
        passer: Option<Symbol>,
        callee_owes: std::ops::Range<usize>,
    ) {
        let type_of = |checker: &Self, ty: &Type| match checker.apply(ty) {
            Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
                args.first().map(|arg| checker.apply(arg))
            }
            _ => None,
        };
        let types: Vec<(usize, Span, Option<Symbol>, Type)> = (given.iter().enumerate())
            .filter_map(|(i, (span, alias, ty))| Some((i, *span, *alias, type_of(self, ty)?)))
            .collect();
        if types.is_empty() {
            return;
        }
        // Whether an argument other than the one at `at` holds a decoder
        // of the type `subject`.
        let holds_decoder = |checker: &Self, at: usize, subject: &Type| {
            (given.iter().enumerate()).any(|(i, (_, _, ty))| {
                i != at && mentions_type_of(&checker.apply(ty), subject, &|t| checker.apply(t))
            })
        };
        let scope = self.wanted_marks.last().copied().unwrap_or(0);
        // (Only the decoders whose type was unknown when they were owed:
        // `waiting_decoders`, kept small as they are decided.)
        let len = self.wanted.len();
        self.waiting_decoders
            .retain(|&k| k < len && !self.wanted[k].solved);
        let candidates: Vec<usize> = (self.waiting_decoders.iter().copied())
            .filter(|&k| k >= scope)
            .collect();
        for k in candidates {
            if self.wanted[k].solved {
                continue;
            }
            let subject = match &self.wanted[k].goal {
                Goal::Pred(Pred::Builtin {
                    check: BuiltinCheck::Decodes { .. },
                    subject,
                    ..
                }) => subject,
                Goal::Pred(Pred::Trait { tr, subject, .. }) if Self::is_decode(*tr) => subject,
                _ => continue,
            };
            let subject = self.apply(subject);
            if matches!(subject, Type::Error) {
                continue;
            }
            let Some(&(at, span, alias, _)) = types.iter().find(|(.., ty)| *ty == subject) else {
                continue;
            };
            if self.wanted[k].origin.span == span {
                continue;
            }
            let passed_on = passer.filter(|_| holds_decoder(self, at, &subject));
            let wanted = &mut self.wanted[k];
            wanted.origin.span = span;
            match &mut wanted.goal {
                Goal::Pred(Pred::Builtin {
                    check: BuiltinCheck::Decodes { written, .. },
                    passer: owner,
                    ..
                }) => {
                    *written = alias;
                    *owner = passed_on.or(*owner);
                }
                // (`json.Decode`.)
                Goal::Pred(Pred::Trait { asker, .. }) => {
                    if let (Some(asker), Some(passer)) = (asker, passed_on) {
                        asker.passer = Some(passer);
                    }
                    if let Some(alias) = alias {
                        self.decode_aliases.insert(span, alias);
                    }
                }
                _ => {}
            }
            // Decided here: a later call that gives a decoder the same
            // type is no place of this one.
            if !matches!(subject, Type::Var(_)) {
                let origin = self.wanted[k].origin;
                match self.wanted[k].goal.clone() {
                    Goal::Pred(Pred::Builtin {
                        check,
                        subject,
                        passer,
                    }) => {
                        self.wanted[k].solved = true;
                        self.check_builtin_argument(check, &subject, passer, origin);
                    }
                    Goal::Pred(Pred::Trait {
                        tr,
                        args,
                        subject,
                        asker,
                    }) => {
                        self.wanted[k].solved = true;
                        self.verify_trait_obligation(tr, &args, &subject, origin, asker);
                    }
                    _ => {}
                }
            }
        }
        // The same for a type given to an annotation variable of a
        // function the scope holds.
        let scope = self.given_marks.last().copied().unwrap_or(0);
        self.unplaced.retain(|&k| !self.given_waiting[k].placed);
        // (Not the uses this call owes: they are at its arguments.)
        let candidates: Vec<usize> = (self.unplaced.iter().copied())
            .filter(|&k| k >= scope && !callee_owes.contains(&k))
            .collect();
        for k in candidates {
            let subject = self.apply(&self.given_waiting[k].subject);
            if matches!(subject, Type::Error) {
                continue;
            }
            let Some(&(at, span, alias, _)) = types.iter().find(|(.., ty)| *ty == subject) else {
                continue;
            };
            if self.given_waiting[k].origin.span == span {
                continue;
            }
            let passed_on = passer.filter(|_| holds_decoder(self, at, &subject));
            let use_ = &mut self.given_waiting[k];
            use_.origin.span = span;
            use_.placed = !matches!(subject, Type::Var(_));
            use_.written = alias;
            if let Some(passer) = passed_on {
                use_.passer = passer;
            }
        }
    }

    /// The calls of trait methods owed in the scope whose type argument
    /// is still unknown.
    pub(super) fn open_vias(&mut self) -> Vec<usize> {
        let len = self.wanted.len();
        self.waiting_vias
            .retain(|&k| k < len && !self.wanted[k].solved);
        let scope = self.wanted_marks.last().copied().unwrap_or(0);
        (self.waiting_vias.iter().copied())
            .filter(|&k| k >= scope)
            .filter(|&k| match &self.wanted[k].goal {
                Goal::Pred(Pred::Via { method_ty, .. }) => {
                    super::inference::type_of_var(&self.apply(method_ty)).is_some()
                }
                _ => false,
            })
            .collect()
    }

    /// Point each of the calls `open` of trait methods whose type argument
    /// this call gave at the argument that gave it (`given`).
    pub(super) fn point_vias_at_type_arguments(&mut self, given: &CallArguments, open: &[usize]) {
        for &k in open {
            let Goal::Pred(Pred::Via {
                method_ty,
                at: None,
                ..
            }) = &self.wanted[k].goal
            else {
                continue;
            };
            // The first type argument the method takes, as the call has
            // it now.
            let Some(taken) = type_of_first(&self.apply(method_ty)) else {
                continue;
            };
            let taken = self.apply(&taken);
            if matches!(taken, Type::Var(_)) {
                continue;
            }
            let place = given.iter().find(|(_, _, ty)| match self.apply(ty) {
                Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
                    args.first().is_some_and(|arg| self.apply(arg) == taken)
                }
                _ => false,
            });
            if let Some((span, ..)) = place {
                self.wanted[k].origin.span = *span;
            }
        }
    }

    /// Decide what waited for the bodies of the module's functions: what
    /// each type given to an annotation variable of a signature must be.
    /// A body that gives its own annotation variable to another function
    /// asks of it what that function asks; a trait method called on a
    /// value of such a variable's type asks, at each use, what the method
    /// of the type the use gives asks.
    pub(super) fn settle_builtin_needs(&mut self) {
        // What each body's annotation variables owe, through the
        // functions and trait methods they are given to, until it stops
        // growing; then each use's checks.
        // The uses of each variable, by the variable's representative and
        // its body: a use is looked at again when what its variable owes
        // grows (`grown_needs`).
        let mut by_given: HashMap<(TyVar, Option<Symbol>), Vec<usize>> = HashMap::new();
        for (k, use_) in self.given_waiting.iter().enumerate() {
            let key = (self.rigid_rep_var(use_.given), use_.body);
            by_given.entry(key).or_default().push(k);
        }
        let given_by_use = self.given_by_use();
        self.grown_needs.clear();
        let mut queue: std::collections::VecDeque<usize> = (0..self.given_waiting.len()).collect();
        let mut queued = vec![true; self.given_waiting.len()];
        loop {
            while let Some(k) = queue.pop_front() {
                queued[k] = false;
                let use_ = self.given_waiting[k].clone();
                if use_.taken {
                    continue;
                }
                if let Type::Rigid(r) = self.apply(&use_.subject) {
                    for check in self.builtin_needs_of(use_.given, use_.body) {
                        self.add_builtin_need(r, check, use_.in_body);
                    }
                }
                // (A call on types that name no annotation variable owes
                // none of a body's: its checks are made below.)
                let vias = self.vias_of(use_.given, use_.body);
                for via in vias.iter() {
                    let via = self.at_use(via, &given_by_use[&use_.use_id]);
                    if self.names_a_variable(&via) {
                        self.apply_via(&via, use_.in_body, None);
                    }
                }
                for key in std::mem::take(&mut self.grown_needs) {
                    for &u in by_given.get(&key).into_iter().flatten() {
                        if !queued[u] {
                            queued[u] = true;
                            queue.push_back(u);
                        }
                    }
                }
            }
            let mut grown = false;
            for k in 0..self.via_waiting.len() {
                let use_ = self.via_waiting[k].clone();
                if self.names_a_variable(&use_.need) {
                    grown |= self.apply_via(&use_.need, use_.in_body, None);
                }
            }
            for key in std::mem::take(&mut self.grown_needs) {
                for &u in by_given.get(&key).into_iter().flatten() {
                    if !queued[u] {
                        queued[u] = true;
                        queue.push_back(u);
                    }
                }
            }
            if !grown && queue.is_empty() {
                break;
            }
        }
        // (What a method asks may have grown with what its body's
        // variables owe.)
        self.leaf_memo.clear();
        for (use_, vias, places) in self.uses_with_vias() {
            let placing = Placing {
                passer: Some(use_.passer),
                origin: use_.origin,
                places,
            };
            for via in vias {
                self.apply_via(&via, use_.in_body, Some(&placing));
            }
        }
        for use_ in std::mem::take(&mut self.via_waiting) {
            // (A type the calling function writes as the method's type
            // argument itself, `x.one(List(Int))`, is none the call
            // gives: no place names it.)
            let mut written = Vec::new();
            written_type_arguments(&use_.need.method_ty, &mut written);
            let written: Vec<Type> = written.iter().map(|ty| self.apply(ty)).collect();
            let places = (use_.places.into_iter())
                .filter(|(given, ..)| !written.contains(&self.apply(given)))
                .collect();
            let placing = Placing {
                passer: use_.passer,
                origin: use_.origin,
                places,
            };
            self.apply_via(&use_.need, use_.in_body, Some(&placing));
        }
        // What another module, or a later REPL entry, reads of this
        // module's impls and traits: the needs, decided.
        for key in self.tables.impl_methods.own_keys() {
            let Some(entry) = self.tables.impl_methods.get(key.0, key.1, key.2) else {
                continue;
            };
            let settled = self.settled_preds(&entry.preds);
            if let Some(entry) = self.tables.impl_methods.own_mut(&key) {
                entry.preds = settled;
            }
        }
        let own_traits: Vec<TraitKey> = (self.tables.traits.keys())
            .filter(|key| self.tables.traits.is_own(key))
            .copied()
            .collect();
        for key in own_traits {
            let Some(info) = self.tables.traits.get(&key) else {
                continue;
            };
            // Each default's variables: `Self`, the trait's parameters and
            // the method's own.
            let methods: Vec<(Symbol, Vec<TyVar>)> = (info.methods.iter())
                .filter(|(name, _)| info.default_method_bodies.contains_key(name))
                .map(|(name, ty)| {
                    let mut vars = free_vars_in(ty);
                    vars.push(info.self_var);
                    vars.extend(info.param_var_ids.iter().copied());
                    (*name, vars)
                })
                .collect();
            let mut needs: HashMap<(TyVar, Symbol), Vec<BuiltinCheck>> = HashMap::new();
            let mut vias: HashMap<(TyVar, Symbol), Vec<ViaNeed>> = HashMap::new();
            for (method, vars) in methods {
                let body = Some(body_key(key, "", method));
                for var in &vars {
                    let of_var = self.builtin_needs_of(*var, body);
                    if !of_var.is_empty() {
                        needs.insert((*var, method), of_var);
                    }
                    let of_var = self.via_needs_of(*var, body);
                    if !of_var.is_empty() {
                        vias.insert((*var, method), of_var);
                    }
                }
            }
            if let Some(info) = self.tables.traits.get_mut(&key) {
                info.builtin_needs = needs;
                info.via_needs = vias;
            }
        }
        // (`unplaced` holds places in `given_waiting`.)
        self.unplaced.clear();
        for use_ in std::mem::take(&mut self.given_waiting) {
            if use_.taken || matches!(self.apply(&use_.subject), Type::Rigid(_)) {
                continue;
            }
            for check in self.builtin_needs_of(use_.given, use_.body) {
                let check = match check {
                    BuiltinCheck::Decodes {
                        builtin, decoding, ..
                    } => BuiltinCheck::Decodes {
                        builtin,
                        decoding,
                        written: use_.written,
                    },
                    other => other,
                };
                self.check_builtin_argument(check, &use_.subject, Some(use_.passer), use_.origin);
            }
        }
    }

    /// The predicates of a scheme as another module reads them: what a
    /// body does to a row variable of its signature, and what it asks of
    /// an annotation variable of it, is decided.
    pub(super) fn settled_preds(&self, preds: &[Pred]) -> Vec<Pred> {
        // The types the scheme gives its signature's variables.
        let given: HashMap<TyVar, Type> = (preds.iter())
            .filter_map(|pred| match pred {
                Pred::Given { subject, given, .. } => {
                    Some((self.rigid_rep_var(*given), subject.clone()))
                }
                _ => None,
            })
            .collect();
        let mut settled = Vec::new();
        for pred in preds {
            match pred {
                Pred::Anon {
                    row,
                    given: Some(given),
                } => {
                    if self.is_anon_row(*given) {
                        settled.push(Pred::Anon {
                            row: row.clone(),
                            given: None,
                        });
                        for field in self.adds_of(*given) {
                            settled.push(Pred::Lacks {
                                row: row.clone(),
                                field,
                            });
                        }
                    }
                }
                Pred::Given {
                    subject,
                    given: var,
                    passer,
                    body,
                    ..
                } => {
                    for check in self.builtin_needs_of(*var, *body) {
                        settled.push(Pred::Builtin {
                            check,
                            subject: subject.clone(),
                            passer: Some(*passer),
                        });
                    }
                    for via in self.via_needs_of(*var, *body) {
                        settled.push(Pred::Via {
                            tr: via.tr,
                            method: via.method,
                            receiver: substitute_vars(&via.receiver, &given),
                            method_ty: substitute_vars(&via.method_ty, &given),
                            passer: Some(*passer),
                            at: None,
                        });
                    }
                }
                // What a declared `json.Decode` bound asks for, decided.
                Pred::Trait {
                    tr,
                    args,
                    subject,
                    asker: Some(asker),
                } => settled.push(Pred::Trait {
                    tr: *tr,
                    args: args.clone(),
                    subject: subject.clone(),
                    asker: self.settled_asker(Some(*asker)),
                }),
                other => settled.push(other.clone()),
            }
        }
        settled
    }
}

/// Whether `ty` holds a value whose type mentions `TypeOf(subject)` (a
/// decoder of that type, in a record or a list too), each part read
/// through `apply`.
fn mentions_type_of(ty: &Type, subject: &Type, apply: &dyn Fn(&Type) -> Type) -> bool {
    let within = |t: &Type| mentions_type_of(&apply(t), subject, apply);
    match ty {
        Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
            args.first().is_some_and(|arg| apply(arg) == *subject)
        }
        Type::Generic(_, args) | Type::Tuple(args) => args.iter().any(within),
        Type::Fun(params, ret) => params.iter().any(within) || within(ret),
        Type::List(t) | Type::Set(t) | Type::Channel(t) => within(t),
        Type::Map(k, v) => within(k) || within(v),
        Type::AnonRecord { fields, .. } => fields.values().any(within),
        _ => false,
    }
}

/// A hash of the shape of `types`: each type variable by the place it
/// first comes at (what a call leaves general is new at each call; two
/// variables that are one stay one).
fn shape_hash(types: &[&Type]) -> u64 {
    use std::hash::{Hash, Hasher};
    fn walk(ty: &Type, h: &mut std::collections::hash_map::DefaultHasher, vars: &mut Vec<TyVar>) {
        std::mem::discriminant(ty).hash(h);
        match ty {
            Type::Var(v) => match vars.iter().position(|seen| seen == v) {
                Some(at) => at.hash(h),
                None => {
                    vars.len().hash(h);
                    vars.push(*v);
                }
            },
            Type::Generic(name, args) => {
                name.id.hash(h);
                args.iter().for_each(|t| walk(t, h, vars));
            }
            Type::Rigid(r) => r.var.hash(h),
            Type::Fun(params, ret) => {
                params.iter().for_each(|t| walk(t, h, vars));
                walk(ret, h, vars);
            }
            Type::List(t) | Type::Set(t) | Type::Channel(t) => walk(t, h, vars),
            Type::Map(k, v) => {
                walk(k, h, vars);
                walk(v, h, vars);
            }
            Type::Tuple(ts) => ts.iter().for_each(|t| walk(t, h, vars)),
            _ => {}
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut vars = Vec::new();
    for ty in types {
        walk(ty, &mut h, &mut vars);
    }
    h.finish()
}

/// Whether `a` and `b` have one shape: equal, with their type variables
/// renamed one to one.
fn same_shape(a: &[&Type], b: &[&Type]) -> bool {
    fn walk(a: &Type, b: &Type, names: &mut Vec<(TyVar, TyVar)>) -> bool {
        let all = |xs: &[Type], ys: &[Type], names: &mut Vec<(TyVar, TyVar)>| {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| walk(x, y, names))
        };
        match (a, b) {
            (Type::Var(x), Type::Var(y)) => match names.iter().find(|(p, q)| p == x || q == y) {
                Some((p, q)) => p == x && q == y,
                None => {
                    names.push((*x, *y));
                    true
                }
            },
            (Type::Generic(x, xs), Type::Generic(y, ys)) => x == y && all(xs, ys, names),
            (Type::Fun(xs, x), Type::Fun(ys, y)) => all(xs, ys, names) && walk(x, y, names),
            (Type::List(x), Type::List(y))
            | (Type::Set(x), Type::Set(y))
            | (Type::Channel(x), Type::Channel(y)) => walk(x, y, names),
            (Type::Map(xk, xv), Type::Map(yk, yv)) => walk(xk, yk, names) && walk(xv, yv, names),
            (Type::Tuple(xs), Type::Tuple(ys)) => all(xs, ys, names),
            (x, y) => x == y,
        }
    }
    let mut names = Vec::new();
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| walk(x, y, &mut names))
}

/// Bind each of the variables `vars` of `pattern` to the part of `ty` at
/// its place, into `mapping` (the first place a variable is at decides).
fn match_type(pattern: &Type, ty: &Type, vars: &[TyVar], mapping: &mut HashMap<TyVar, Type>) {
    let mut zip = |xs: &[Type], ys: &[Type]| {
        for (x, y) in xs.iter().zip(ys) {
            match_type(x, y, vars, mapping);
        }
    };
    match (pattern, ty) {
        (Type::Var(v), _) if vars.contains(v) => {
            mapping.entry(*v).or_insert_with(|| ty.clone());
        }
        (Type::Generic(x, xs), Type::Generic(y, ys)) if x == y => zip(xs, ys),
        (Type::Tuple(xs), Type::Tuple(ys)) => zip(xs, ys),
        (Type::Fun(xs, x), Type::Fun(ys, y)) => {
            zip(xs, ys);
            match_type(x, y, vars, mapping);
        }
        (Type::List(x), Type::List(y))
        | (Type::Set(x), Type::Set(y))
        | (Type::Channel(x), Type::Channel(y)) => match_type(x, y, vars, mapping),
        (Type::Map(xk, xv), Type::Map(yk, yv)) => {
            match_type(xk, yk, vars, mapping);
            match_type(xv, yv, vars, mapping);
        }
        _ => {}
    }
}

/// The type the first type argument (`TypeOf(t)`) of the function type
/// `ty` gives: `t`.
/// The type arguments (`TypeOf(t)`) of `ty`, a method's type at a call,
/// that name no type variable: the types the calling function writes,
/// gathered into `out`.
fn written_type_arguments(ty: &Type, out: &mut Vec<Type>) {
    match ty {
        Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => {
            out.extend(
                (args.first())
                    .filter(|arg| free_vars_in(arg).is_empty())
                    .cloned(),
            );
        }
        Type::Fun(params, ret) => {
            for part in params.iter().chain([&**ret]) {
                written_type_arguments(part, out);
            }
        }
        _ => {}
    }
}

fn type_of_first(ty: &Type) -> Option<Type> {
    match ty {
        Type::Generic(name, args) if name.is_builtin(crate::defs::TYPE_OF) => args.first().cloned(),
        Type::Fun(params, ret) => params.iter().chain([&**ret]).find_map(type_of_first),
        _ => None,
    }
}
