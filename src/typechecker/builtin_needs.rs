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
use std::collections::HashSet;

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

    /// A call of a trait method owed (`Pred::Via`), its receiver known
    /// now: on an annotation variable, a need of its function's body; on
    /// a type, what that type's method asks, at `origin` (or at the
    /// place in `places` that gave a type). `in_body`: the method body
    /// the call is in.
    pub(super) fn via_owed(
        &mut self,
        need: ViaNeed,
        passer: Option<Symbol>,
        origin: Origin,
        places: &[(Type, Origin, Option<Symbol>)],
        in_body: Option<Symbol>,
    ) -> bool {
        match self.apply(&need.receiver) {
            Type::Var(_) | Type::Error | Type::Never => false,
            Type::Rigid(r) => {
                let key = (self.rigid_rep_var(r.var), in_body);
                let needs = self.via_needs.entry(key).or_default();
                if needs.contains(&need) {
                    return false;
                }
                needs.push(need);
                true
            }
            receiver => {
                self.resolve_via(&need, &receiver, passer, origin, places);
                true
            }
        }
    }

    /// What the method `need.method` of the impl for `receiver` (or the
    /// trait's default it takes) asks of the types the call gives it: its
    /// uses are owed as a call of it would owe them, each where the call
    /// gave its type, and a note names `passer`, the function the caller
    /// called.
    fn resolve_via(
        &mut self,
        need: &ViaNeed,
        receiver: &Type,
        passer: Option<Symbol>,
        origin: Origin,
        places: &[(Type, Origin, Option<Symbol>)],
    ) {
        let Some(head) = self.type_name_for_impl(receiver) else {
            return;
        };
        // A method that calls itself, or a chain of them, on the same
        // types for the same use is looked at once.
        let key = format!(
            "{:?} {} {} {} {:?}",
            need.tr,
            need.method,
            self.show_type(receiver),
            self.show_type(&self.apply(&need.method_ty)),
            origin.span
        );
        if !self.resolved_vias.insert(key) {
            return;
        }
        let Some(entry) = self.tables.impl_methods.get(head, need.method, need.tr) else {
            return;
        };
        if entry.structural {
            return;
        }
        let scheme = self.method_scheme(&entry.clone());
        let (owed_before, given_before) = (self.wanted.len(), self.given_waiting.len());
        self.named_use = Some(origin);
        let ty = self.instantiate(&scheme);
        let _ = self.unify_types(&ty, &need.method_ty);
        let place = |checker: &Self, subject: &Type| {
            let subject = checker.apply(subject);
            (places.iter())
                .find(|(ty, ..)| checker.apply(ty) == subject)
                .map_or((origin, None), |(_, origin, written)| (*origin, *written))
        };
        for k in given_before..self.given_waiting.len() {
            let (at, written) = place(self, &self.given_waiting[k].subject);
            let given = &mut self.given_waiting[k];
            given.origin = at;
            given.placed = true;
            given.written = written.or(given.written);
            if let Some(passer) = passer {
                given.passer = passer;
            }
        }
        for k in owed_before..self.wanted.len() {
            let subject = self.wanted[k].goal.waits_on().clone();
            let (at, alias) = place(self, &subject);
            let wanted = &mut self.wanted[k];
            wanted.origin = at;
            match &mut wanted.goal {
                Goal::Pred(Pred::Builtin {
                    check,
                    passer: owner,
                    ..
                }) => {
                    if let BuiltinCheck::Decodes { written, .. } = check {
                        *written = alias.or(*written);
                    }
                    *owner = passer.or(*owner);
                }
                Goal::Pred(Pred::Via { passer: owner, .. }) => *owner = passer.or(*owner),
                _ => {}
            }
        }
        self.solve_wanted(owed_before);
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
        for k in scope..self.wanted.len() {
            if self.wanted[k].solved {
                continue;
            }
            let Goal::Pred(Pred::Builtin {
                check: BuiltinCheck::Decodes { .. },
                subject,
                ..
            }) = &self.wanted[k].goal
            else {
                continue;
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
            if let Goal::Pred(Pred::Builtin {
                check: BuiltinCheck::Decodes { written, .. },
                passer: owner,
                ..
            }) = &mut wanted.goal
            {
                *written = alias;
                *owner = passed_on.or(*owner);
            }
            // Decided here: a later call that gives a decoder the same
            // type is no place of this one.
            if !matches!(subject, Type::Var(_))
                && let Goal::Pred(Pred::Builtin {
                    check,
                    subject,
                    passer,
                }) = self.wanted[k].goal.clone()
            {
                self.wanted[k].solved = true;
                let origin = self.wanted[k].origin;
                self.check_builtin_argument(check, &subject, passer, origin);
            }
        }
        // The same for a type given to an annotation variable of a
        // function the scope holds.
        let scope = self.given_marks.last().copied().unwrap_or(0);
        for k in scope..self.given_waiting.len() {
            if self.given_waiting[k].placed {
                continue;
            }
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

    /// Decide what waited for the bodies of the module's functions: what
    /// each type given to an annotation variable of a signature must be.
    /// A body that gives its own annotation variable to another function
    /// asks of it what that function asks; a trait method called on a
    /// value of such a variable's type asks, at each use, what the method
    /// of the type the use gives asks.
    pub(super) fn settle_builtin_needs(&mut self) {
        let mut applied: HashSet<(usize, TyVar, usize)> = HashSet::new();
        let mut by_use: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut indexed = 0;
        // The uses to look at: all of them while what a body asks grows,
        // else only those the last round made.
        let mut from = 0;
        loop {
            let known = self.given_waiting.len();
            for k in indexed..known {
                by_use
                    .entry(self.given_waiting[k].use_id)
                    .or_default()
                    .push(k);
            }
            indexed = known;
            let mut grown = false;
            for k in from..known {
                let use_ = self.given_waiting[k].clone();
                let subject = self.apply(&use_.subject);
                if let Type::Rigid(r) = &subject {
                    let to = self.rigid_rep_var(r.var);
                    for check in self.builtin_needs_of(use_.given, use_.body) {
                        let needs = self.builtin_needs.entry((to, use_.in_body)).or_default();
                        if !needs.contains(&check) {
                            needs.push(check);
                            grown = true;
                        }
                    }
                }
                let vias = self.via_needs_of(use_.given, use_.body);
                if vias.is_empty() {
                    continue;
                }
                // The types this use gives the signature's variables, and
                // where it gives each.
                let mut mapping: HashMap<TyVar, Type> = HashMap::new();
                let mut places = Vec::new();
                for &s in &by_use[&use_.use_id] {
                    let sibling = &self.given_waiting[s];
                    let ty = self.apply(&sibling.subject);
                    mapping.insert(self.rigid_rep_var(sibling.given), ty.clone());
                    places.push((ty, sibling.origin, sibling.written));
                }
                let given = self.rigid_rep_var(use_.given);
                for (t, via) in vias.into_iter().enumerate() {
                    if !applied.insert((use_.use_id, given, t)) {
                        continue;
                    }
                    // (What the call leaves general is new at each use.)
                    let mut mapping = mapping.clone();
                    for v in free_vars_in(&via.receiver)
                        .into_iter()
                        .chain(free_vars_in(&via.method_ty))
                    {
                        if let std::collections::hash_map::Entry::Vacant(slot) = mapping.entry(v) {
                            slot.insert(self.fresh_var());
                        }
                    }
                    let need = ViaNeed {
                        receiver: substitute_vars(&via.receiver, &mapping),
                        method_ty: substitute_vars(&via.method_ty, &mapping),
                        ..via
                    };
                    let on_variable = matches!(self.apply(&need.receiver), Type::Rigid(_));
                    let owed =
                        self.via_owed(need, Some(use_.passer), use_.origin, &places, use_.in_body);
                    grown |= owed && on_variable;
                }
            }
            from = match (grown, self.given_waiting.len() > known) {
                (true, _) => 0,
                (false, true) => known,
                (false, false) => break,
            };
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
        for use_ in std::mem::take(&mut self.given_waiting) {
            if matches!(self.apply(&use_.subject), Type::Rigid(_)) {
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
                        });
                    }
                }
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
