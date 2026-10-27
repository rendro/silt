use super::*;

/// Each record type `ty` writes over a row variable it writes more than
/// once, with the fields it lists there.
fn row_listings(ty: &Type) -> Vec<(TyVar, std::collections::BTreeMap<Symbol, Type>)> {
    fn walk(ty: &Type, out: &mut Vec<(TyVar, std::collections::BTreeMap<Symbol, Type>)>) {
        match ty {
            Type::AnonRecord { fields, tail } => {
                if let RowTail::Var(v) = tail {
                    out.push((*v, fields.clone()));
                }
                fields.values().for_each(|t| walk(t, out));
            }
            Type::Fun(params, ret) => {
                params.iter().for_each(|t| walk(t, out));
                walk(ret, out);
            }
            Type::List(t) | Type::Set(t) | Type::Channel(t) => walk(t, out),
            Type::Map(k, v) => {
                walk(k, out);
                walk(v, out);
            }
            Type::Tuple(ts) | Type::Generic(_, ts) => ts.iter().for_each(|t| walk(t, out)),
            _ => {}
        }
    }
    let mut all = Vec::new();
    walk(ty, &mut all);
    let once: Vec<TyVar> = all
        .iter()
        .map(|(v, _)| *v)
        .filter(|v| all.iter().filter(|(w, _)| w == v).count() == 1)
        .collect();
    all.retain(|(v, fields)| !once.contains(v) && !fields.is_empty());
    all
}

impl TypeChecker {
    // ── Generalization / Instantiation ──────────────────────────────

    /// Generalize a type into a scheme: quantify its unresolved variables
    /// that nothing outside the scope `exit_level` just left mentions
    /// (see [`TyVarSupply`]). The predicates the scope's uses still owe
    /// for a quantified variable (`let f = constrained_fn`,
    /// `fn wrap(x) { constrained_fn(x) }`) are the scheme's: each use of
    /// it owes them again.
    pub(super) fn generalize(&mut self, ty: &Type) -> Scheme {
        // What the scope owes for a subject it has decided since is
        // checked first: what that leaves owed is on variables.
        self.decide_closed();
        self.generalize_decided(ty)
    }

    /// The scheme of a `let` inside a function. A `?`, an update or a
    /// call of a method several traits declare that still waits for its
    /// type is not decided here: the function goes on, and a use of the
    /// `let` may decide it. So the `let` is not general in what such a
    /// goal mentions.
    pub(super) fn generalize_local(&mut self, ty: &Type) -> Scheme {
        self.reopen_level();
        self.solve_wanted(self.closed_mark);
        self.close_level();
        // (A call of a method one trait declares, or none, is decided as
        // anywhere: the `let` is general in a receiver bounded by the
        // trait.)
        self.decide_tries();
        self.default_rebuilds();
        self.default_selects();
        for i in self.closed_mark..self.wanted.len() {
            if self.wanted[i].solved {
                continue;
            }
            let mentioned: Vec<Type> = match &self.wanted[i].goal {
                // A use whose trait arguments are still undecided: what
                // decides them may come later in the definition, so the
                // `let` is not general in them.
                Goal::Pred(Pred::Trait {
                    tr, args, subject, ..
                }) if !matches!(self.apply(subject), Type::Var(_) | Type::Rigid(_))
                    && self.waits_for_trait_args(*tr, subject, args) =>
                {
                    args.clone()
                }
                Goal::Pred(_) => continue,
                Goal::Select {
                    recv, args, result, ..
                } => args.iter().chain([recv, result]).cloned().collect(),
                Goal::Update { base, value, .. } => vec![base.clone(), value.clone()],
                Goal::Try { operand, ok, ret } => {
                    ret.iter().chain([operand, ok]).cloned().collect()
                }
                Goal::Listed { .. } => continue,
                // (One that waits is over a record of an outer scope.)
                Goal::Rebuild {
                    base, with, result, ..
                } => with
                    .iter()
                    .map(|(_, ty)| ty)
                    .chain([base, result])
                    .cloned()
                    .collect(),
            };
            for ty in mentioned {
                self.keep_monomorphic(&ty);
            }
        }
        self.generalize_decided(ty)
    }

    fn generalize_decided(&mut self, ty: &Type) -> Scheme {
        let fixed = self.fix_statement_calls(false);
        self.recheck_fixed(&fixed, |checker| {
            checker.reopen_level();
            checker.solve_wanted(checker.closed_mark);
            checker.close_level();
        });
        let ty = self.apply(ty);
        let mut vars: Vec<TyVar> = free_vars_in(&ty)
            .into_iter()
            .filter(|v| self.tables.vars.is_generalizable(*v))
            .collect();
        let mut preds: Vec<Pred> = Vec::new();
        // The module of each predicate, if another module's definition
        // owes it (`let_owners`).
        let mut owners: Vec<Option<crate::session::ModuleId>> = Vec::new();
        // The evidence of each trait predicate the scheme takes: a bound
        // of the definition.
        let mut taken = Vec::new();
        // The variables of a function's parameters: a type one of them
        // gives a builtin, the function passes on.
        let passed: Vec<TyVar> = match (&ty, self.passing_on) {
            (Type::Fun(params, _), Some(_)) => params.iter().flat_map(free_vars_in).collect(),
            _ => Vec::new(),
        };
        let passer_of = |owner: Option<Symbol>, subject: TyVar| match passed.contains(&subject) {
            true => self.passing_on,
            false => owner,
        };
        // A trait method's call: the function passes on what its
        // parameters give the method beyond the receiver (a function that
        // returns a closure making the call passes nothing on).
        let via_passer = |owner: Option<Symbol>, receiver: &Type, method_ty: &Type| {
            let receiver = free_vars_in(receiver);
            let given = free_vars_in(method_ty)
                .into_iter()
                .filter(|v| !receiver.contains(v));
            match given.into_iter().any(|v| passed.contains(&v)) {
                true => self.passing_on,
                false => owner,
            }
        };
        if !vars.is_empty() {
            for i in self.closed_mark..self.wanted.len() {
                if self.wanted[i].solved {
                    continue;
                }
                let Goal::Pred(pred) = &self.wanted[i].goal.clone() else {
                    continue;
                };
                // A trait method called on an annotation variable of the
                // enclosing function, by a closure general in the type it
                // gives (`{ t -> l.load(t) }`): each use of the closure
                // owes it at the type that use gives.
                if let Pred::Via {
                    receiver,
                    method_ty,
                    ..
                } = pred
                    && !matches!(self.apply(receiver), Type::Var(_))
                {
                    let method_ty = self.apply(method_ty);
                    let receiver_ty = self.apply(receiver);
                    let mentioned = free_vars_in(&method_ty)
                        .into_iter()
                        .chain(free_vars_in(&receiver_ty));
                    if mentioned.into_iter().any(|v| vars.contains(&v)) {
                        self.wanted[i].solved = true;
                        if let Pred::Via {
                            tr, method, passer, ..
                        } = pred
                        {
                            let passer = via_passer(*passer, &receiver_ty, &method_ty);
                            let at = given_at(&method_ty, self.wanted[i].origin.span);
                            let pred = Pred::Via {
                                tr: *tr,
                                method: *method,
                                receiver: receiver_ty,
                                method_ty,
                                passer,
                                at,
                            };
                            take_pred(&mut preds, &mut owners, pred, None);
                        }
                    }
                    continue;
                }
                let Type::Var(subject) = self.apply(pred.subject()) else {
                    continue;
                };
                if !vars.contains(&subject) {
                    continue;
                }
                self.wanted[i].in_scheme = true;
                if let Pred::Trait { tr, args, .. } = pred {
                    let node = super::evidence::EvNode::Bound {
                        var: subject,
                        tr: *tr,
                        args: args.iter().map(|t| self.apply(t)).collect(),
                    };
                    taken.push((self.wanted[i].ev, node));
                }
                let pred = match pred {
                    // (The definition passes a `json.Decode` it owes of
                    // a parameter's type on.)
                    Pred::Trait {
                        tr, args, asker, ..
                    } => Pred::Trait {
                        tr: *tr,
                        args: args.iter().map(|t| self.apply(t)).collect(),
                        subject: Type::Var(subject),
                        asker: asker.map(|asker| Asker {
                            passer: passer_of(asker.passer, subject),
                            ..asker
                        }),
                    },
                    Pred::Anon { given, .. } => Pred::Anon {
                        row: Type::Var(subject),
                        given: *given,
                    },
                    Pred::Lacks { field, .. } => Pred::Lacks {
                        row: Type::Var(subject),
                        field: *field,
                    },
                    // The definition passes the type on to the builtin;
                    // each use names its own type (`written`).
                    Pred::Builtin { check, passer, .. } => Pred::Builtin {
                        check: check.unwritten(),
                        subject: Type::Var(subject),
                        passer: passer_of(*passer, subject),
                    },
                    Pred::Via {
                        tr,
                        method,
                        method_ty,
                        passer,
                        ..
                    } => Pred::Via {
                        tr: *tr,
                        method: *method,
                        receiver: Type::Var(subject),
                        method_ty: self.apply(method_ty),
                        passer: via_passer(*passer, &Type::Var(subject), &self.apply(method_ty)),
                        at: given_at(&self.apply(method_ty), self.wanted[i].origin.span),
                    },
                    Pred::Given {
                        given,
                        passer,
                        body,
                        group,
                        ..
                    } => Pred::Given {
                        subject: Type::Var(subject),
                        given: *given,
                        written: None,
                        passer: passer_of(Some(*passer), subject).unwrap_or(*passer),
                        body: *body,
                        group: *group,
                    },
                };
                let owner = self.owed_for.get(&self.wanted[i].ev).copied();
                take_pred(&mut preds, &mut owners, pred, owner);
            }
            // A type the scope gives a function's annotation variable,
            // if the definition is general in it.
            // (The types one use gives belong together: a use the
            // definition is general in goes into the scheme whole, with
            // the types it gives that are known already.)
            let mut general: HashMap<usize, Option<Symbol>> = HashMap::new();
            for k in self.given_closed..self.given_waiting.len() {
                let use_ = &self.given_waiting[k];
                let mentioned = free_vars_in(&self.apply(&use_.subject));
                if mentioned.iter().any(|v| vars.contains(v)) {
                    let passes = mentioned.iter().any(|v| passed.contains(v));
                    let passer = general.entry(use_.use_id).or_insert(None);
                    if passes {
                        *passer = self.passing_on;
                    }
                }
            }
            for k in self.given_closed..self.given_waiting.len() {
                let use_ = &self.given_waiting[k];
                let Some(passer) = general.get(&use_.use_id) else {
                    continue;
                };
                let pred = Pred::Given {
                    subject: self.apply(&use_.subject),
                    given: use_.given,
                    written: None,
                    passer: passer.unwrap_or(use_.passer),
                    body: use_.body,
                    group: Some(use_.use_id),
                };
                let owner = use_.owner;
                take_pred(&mut preds, &mut owners, pred, owner);
                // (Each use of the definition owes it now.)
                self.given_waiting[k].taken = true;
            }
            // A variable only a predicate's trait arguments mention is
            // the scheme's as well.
            for pred in &preds {
                for v in pred.args().iter().flat_map(free_vars_in) {
                    if self.tables.vars.is_generalizable(v) && !vars.contains(&v) {
                        vars.push(v);
                    }
                }
            }
        }
        for (ev, node) in taken {
            self.ev_set(Some(ev), node);
        }
        self.generalized_owners = owners;
        Scheme { vars, preds, ty }
    }

    /// The scheme of a top-level function whose body is checked: `ty`,
    /// its type as its body sees it, with the annotation variables
    /// quantified again and bounded as its `where` clauses say (`bounds`).
    pub(super) fn generalize_fn(&mut self, ty: &Type, bounds: &[Pred]) -> Scheme {
        let (ty, rigid_vars) = unrigidify(&self.apply(ty));
        let mut scheme = self.generalize(&ty);
        // A trait method's call on the function's own annotation variable
        // that a closure it returns makes: on the variable the scheme
        // quantifies.
        let own = |ty: &Type| {
            map_rigid(ty, &mut |r| match rigid_vars.contains(&r.var) {
                true => Type::Var(r.var),
                false => Type::Rigid(r),
            })
        };
        for pred in &mut scheme.preds {
            if let Pred::Via {
                receiver,
                method_ty,
                ..
            } = pred
            {
                *receiver = own(receiver);
                *method_ty = own(method_ty);
            }
        }
        for v in rigid_vars {
            if !scheme.vars.contains(&v) {
                scheme.vars.push(v);
            }
        }
        for bound in bounds {
            if matches!(bound.subject(), Type::Var(v) if scheme.vars.contains(v))
                && !scheme.preds.contains(bound)
            {
                scheme.preds.push(bound.clone());
            }
        }
        self.keep_owners(&scheme);
        scheme
    }

    /// Keep, for `scheme`, generalised last, the module of each variable
    /// and predicate that came from a use of another module's definition
    /// (`let_owners`): each use of the scheme is a use of that
    /// definition (`let g = mail.name_any`, `fn wrap(r) { mail.name_any(r) }`).
    pub(super) fn keep_owners(&mut self, scheme: &Scheme) {
        let owners = std::mem::take(&mut self.generalized_owners);
        if let Some(&first) = scheme.vars.first()
            && (owners.iter().any(Option::is_some)
                || scheme.vars.iter().any(|v| self.use_owners.contains_key(v)))
        {
            self.let_owners.insert(first, owners);
        }
    }

    /// Settle what the scope `exit_level` just left still owes or waits
    /// to decide, once everything the scope defines is generalised. What
    /// is on a variable that is still unresolved and belongs to an outer
    /// binding (a top-level `let` that is not generalised) waits on: it
    /// is decided when a later definition decides the variable. What
    /// nothing can decide any more is an error, or, for a builtin
    /// structural trait, fixes its subject to `()`.
    pub(super) fn settle_bounds(&mut self) {
        self.decide_closed();
        let fixed = self.fix_statement_calls(false);
        self.recheck_fixed(&fixed, Self::decide_closed);
        let mut recorded = self.wanted.split_off(self.closed_mark);
        // A use whose trait arguments nothing decided, of a type that has
        // impls of the trait at several: which impl it means is unknown
        // (before anything else is decided for its lack). The definition
        // the use is in decides them, a top-level `let` as a function: no
        // later one does.
        for wanted in &mut recorded {
            let Goal::Pred(Pred::Trait {
                tr, args, subject, ..
            }) = &wanted.goal
            else {
                continue;
            };
            if wanted.solved
                || matches!(self.apply(subject), Type::Var(_) | Type::Rigid(_))
                || !self.waits_for_trait_args(*tr, subject, args)
            {
                continue;
            }
            wanted.solved = true;
            for arg in args {
                (self.reported_trait_args).extend(free_vars_in(&self.apply(arg)));
            }
            let error = self.undecided_trait_args(*tr, args, subject, wanted.origin);
            self.errors.push(error);
        }
        // (What stood at the places the scope's goals leave is gone.)
        let kept = self.closed_mark;
        self.waiting_decoders.retain(|&k| k < kept);
        self.waiting_vias.retain(|&k| k < kept);
        for wanted in recorded {
            if wanted.solved {
                continue;
            }
            let waits_on = match &wanted.goal {
                Goal::Rebuild { base, .. } => self.rebuild_waits(base).map(Type::Var),
                goal => Some(goal.waits_on().clone()),
            };
            if waits_on.is_some_and(|ty| self.waits_for_outer(&ty)) {
                self.wanted.push(wanted);
                continue;
            }
            let (tr, args, subject) = match &wanted.goal {
                Goal::Pred(Pred::Trait {
                    tr, args, subject, ..
                }) => (tr, args, subject),
                // (A row nothing gives fields to has none to clash, and
                // is no declared record.)
                Goal::Listed { .. }
                | Goal::Rebuild { .. }
                | Goal::Pred(Pred::Anon { .. } | Pred::Lacks { .. }) => continue,
                // A type nothing decides is no type a builtin is given:
                // nothing to check.
                Goal::Pred(Pred::Builtin { .. } | Pred::Given { .. }) => continue,
                // A trait method called on an annotation variable of the
                // scope's function: a need of its body.
                Goal::Pred(Pred::Via {
                    tr,
                    method,
                    receiver,
                    method_ty,
                    passer,
                    ..
                }) => {
                    if !wanted.in_scheme && !matches!(self.apply(receiver), Type::Var(_)) {
                        let need = super::builtin_needs::ViaNeed {
                            tr: *tr,
                            method: *method,
                            receiver: receiver.clone(),
                            method_ty: method_ty.clone(),
                        };
                        let in_body = self.body_owner;
                        let places = (wanted.places.clone(), wanted.arguments.clone());
                        self.via_owed(need, *passer, wanted.origin, places, in_body);
                    }
                    continue;
                }
                Goal::Try { .. } => {
                    self.errors.push(
                        Diagnostic::error(
                            Code::AmbiguousType,
                            wanted.origin.span,
                            "cannot infer the type `?` is applied to: a Result or an Option",
                        )
                        .with_help(
                            "annotate the value, or give the function it is in a return type",
                        ),
                    );
                    continue;
                }
                // (Each is decided when the scope is generalised.)
                Goal::Select { .. } => continue,
                Goal::Update { field, .. } => {
                    self.errors.push(
                        Diagnostic::error(
                            Code::AmbiguousType,
                            wanted.origin.span,
                            format!(
                                "cannot infer the record type whose field '{field}' this update sets"
                            ),
                        )
                        .with_help("annotate the record: `r: SomeRecord`"),
                    );
                    continue;
                }
            };
            if wanted.in_scheme || !matches!(self.apply(subject), Type::Var(_)) {
                continue;
            }
            // Nothing decides the subject and no definition is general
            // in it. For a trait every type implements by its structure
            // it does not matter which type it is: it is `()`
            // (`println([])`, `json.stringify(Ok(1))`). A type nothing
            // decides is no type a decoder is given (`let _ =
            // json.parse`): `json.Decode` of it asks nothing.
            if tr.is_builtin("Decode") {
                continue;
            }
            if STRUCTURAL_METHODS
                .iter()
                .any(|(name, _)| tr.is_builtin(name))
            {
                let _ = self.unify_types(subject, &Type::Unit);
                self.ev_set(Some(wanted.ev), super::evidence::EvNode::Structural(*tr));
                continue;
            }
            {
                // Nothing decides the subject, and no definition is
                // general in it: which impl the use means is unknown.
                let bound = self.show_bound(*tr, args);
                let what = match wanted.origin.callee {
                    Some(callee) => format!("this use of '{callee}'"),
                    None => "this".to_string(),
                };
                self.errors.push(
                    Diagnostic::error(
                        Code::AmbiguousType,
                        wanted.origin.span,
                        format!("cannot infer the type {what} needs to implement trait '{bound}'"),
                    )
                    .with_help("annotate the value it is given"),
                );
            }
        }
    }

    /// The error of a use whose trait arguments nothing decides: which
    /// impl of `tr` for `subject` it means is not known (`origin`).
    fn undecided_trait_args(
        &self,
        tr: TraitKey,
        args: &[Type],
        subject: &Type,
        origin: Origin,
    ) -> Diagnostic {
        let what = match origin.callee {
            Some(callee) => format!("this use of '{callee}'"),
            None => "this".to_string(),
        };
        let message = format!(
            "cannot decide which impl of '{}' for '{}' {what} means: its trait argument is not \
             decided here",
            self.show_bound(tr, args),
            self.show_type(&self.apply(subject))
        );
        let info = self.tables.traits.get(&tr);
        // The example: the trait arguments of the first impl the type has
        // at written types (a placeholder where it has only one for every
        // argument), and the method's type at them.
        let subject_ty = self.apply(subject);
        let at_impl: Option<Vec<Type>> = (self.type_name_for_impl(&subject_ty)).and_then(|head| {
            (self.tables.impl_args_of(tr, head).iter())
                .filter_map(|at| self.tables.impl_trait_args.get(&(tr, head, *at)))
                .find(|args| args.iter().all(|arg| free_vars_in(arg).is_empty()))
                .cloned()
        });
        let example = match &at_impl {
            Some(impl_args) => self.show_bound(tr, impl_args),
            None => {
                let name = self.show_trait(tr);
                let places: Vec<&str> = (args.iter().zip(["T", "U", "V", "W"].iter().cycle()))
                    .map(|(_, letter)| *letter)
                    .collect();
                format!("{name}({})", places.join(", "))
            }
        };
        let declared = origin.callee.and_then(|method| {
            let (_, ty) = info?.methods.iter().find(|(name, _)| *name == method)?;
            Some((method, ty))
        });
        // The method's type at the example's trait arguments.
        let at_example = |ty: &Type| -> Option<Type> {
            let (info, impl_args) = (info?, at_impl.as_ref()?);
            let mut mapping: HashMap<TyVar, Type> = HashMap::new();
            mapping.insert(info.self_var, subject_ty.clone());
            mapping.extend(info.param_var_ids.iter().copied().zip(impl_args.clone()));
            Some(substitute_vars(ty, &mapping))
        };
        let mentions_param = |ty: &Type| {
            info.is_some_and(|info| {
                (free_vars_in(ty).iter()).any(|v| info.param_var_ids.contains(v))
            })
        };
        // (A method whose type mentions no parameter of its trait: no
        // annotation of what a call gives decides one.)
        let undecidable = declared.filter(|(_, ty)| !mentions_param(ty));
        // What a call gives, at the example, where that decides it.
        let given = declared.and_then(|(_, ty)| match ty {
            Type::Fun(_, ret) if mentions_param(ret) => at_example(ret),
            _ => None,
        });
        let given_shown =
            (given.as_ref()).map_or_else(|| "Int".to_string(), |ty| self.show_type(ty));
        // Else what a call is given, where that decides it (`A.from(x)`).
        let passed = declared.and_then(|(_, ty)| match ty {
            Type::Fun(params, _) => (params.iter()).find(|param| mentions_param(param)),
            _ => None,
        });
        let passed = passed.and_then(&at_example);
        let statement = (args.iter()).any(
            |arg| matches!(self.apply(arg), Type::Var(v) if self.statement_trait_args.contains(&v)),
        );
        // A method taken as a value: the whole value of a `let`, whose
        // type says which.
        let value_let =
            (self.method_value_lets.get(&origin.span)).zip(self.method_values.get(&origin.span));
        let help = match (undecidable, value_let) {
            (Some((method, _)), _) => format!(
                "nothing a call of '{method}' gives decides the trait argument: call it where a \
                 bound decides it (`where t: {example}`)"
            ),
            (None, Some((name, written))) => {
                let shown = declared.and_then(|(_, ty)| at_example(ty));
                match shown {
                    Some(ty) => format!(
                        "annotate the function's type: `let {name}: {} = {written}`",
                        self.show_type(&ty)
                    ),
                    None => "annotate the function's type".to_string(),
                }
            }
            // (A statement's value: the `let` to write for it.)
            (None, None) if statement => {
                format!("annotate the value, `let s: {given_shown} = ...`")
            }
            (None, None) => match (&given, &passed) {
                (None, Some(passed)) => format!(
                    "annotate the value you pass (`let x: {} = ...`), or call it where a bound \
                     decides it (`where t: {example}`)",
                    self.show_type(passed)
                ),
                _ => format!(
                    "annotate the type the call gives (`let n: {given_shown} = ...`), or call it \
                     where a bound decides it (`where t: {example}`)"
                ),
            },
        };
        Diagnostic::error(Code::AmbiguousType, origin.span, message).with_help(help)
    }

    /// Whether `ty` is a variable still unresolved that the scope
    /// `exit_level` just left does not own.
    fn waits_for_outer(&self, ty: &Type) -> bool {
        matches!(self.apply(ty), Type::Var(v) if !self.tables.vars.is_generalizable(v))
    }

    /// Instantiate a scheme: its type with a fresh variable for each
    /// quantified one. The use owes the scheme's predicates at those
    /// variables (`want`), whatever the use is: a call, a name passed as
    /// a value, a pipe, a pattern.
    pub(super) fn instantiate(&mut self, scheme: &Scheme) -> Type {
        // (The types this use gives a signature's variables are one use.)
        self.uses += 1;
        let use_id = self.uses;
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        // A `let` of the body that holds a use of another module's
        // definition: each use of the `let` is a use of that definition.
        let owners = (scheme.vars.first()).and_then(|v| self.let_owners.get(v).cloned());
        for &v in &scheme.vars {
            let fresh = self.fresh_var();
            if owners.is_some()
                && let (Some(&module), Type::Var(w)) = (self.use_owners.get(&v), &fresh)
            {
                self.use_owners.insert(*w, module);
            }
            // (A type constructor variable keeps its name: `m(Int)`.)
            if let (Some(name), Type::Var(w)) = (self.tables.constructor_names.get(&v), &fresh) {
                let name = *name;
                self.tables.constructor_names.insert(*w, name);
            }
            mapping.insert(v, fresh);
        }
        self.ev.use_ev = None;
        if !scheme.preds.is_empty() {
            let origin = self.use_origin();
            // (The uses a definition took from its body stay apart.)
            let mut groups: HashMap<usize, usize> = HashMap::new();
            // The evidence of each trait the use owes, for the use.
            let mut owed = Vec::new();
            for (k, pred) in scheme.preds.iter().enumerate() {
                let around = (owners.as_ref()).map(|owners| {
                    let owner = owners.get(k).copied().flatten();
                    std::mem::replace(&mut self.instantiating, owner)
                });
                // (A predicate decided now may make a use of its own.)
                self.use_id = match pred {
                    Pred::Given {
                        group: Some(group), ..
                    } => *groups.entry(*group).or_insert_with(|| {
                        self.uses += 1;
                        self.uses
                    }),
                    _ => use_id,
                };
                let ev = self.want(pred.substitute(&mapping), origin);
                if let Some(around) = around {
                    self.instantiating = around;
                }
                if let (true, Some(ev)) = (super::evidence::passes_dictionary(pred), ev) {
                    owed.push(ev);
                }
            }
            if !owed.is_empty() {
                self.ev.use_ev = Some(owed);
            }
        }
        // A row variable the type writes more than once: what a declared
        // record that stands for it says of each field holds for each
        // writing.
        for (row, fields) in row_listings(&scheme.ty) {
            if let Some(row) = mapping.get(&row).cloned() {
                let origin = self.use_origin();
                let fields = fields
                    .iter()
                    .map(|(name, ty)| (*name, substitute_vars(ty, &mapping)))
                    .collect();
                self.want_goal(Goal::Listed { fields, row }, origin);
            }
        }
        self.named_use = None;
        substitute_vars(&scheme.ty, &mapping)
    }

    /// Enter the scheme of each definition the module declares in the
    /// session's tables, from the module's scope `env`: what an importer
    /// of the module reads.
    pub(super) fn enter_schemes(&mut self, env: &TypeEnv) {
        let Some(defs) = self.defs.clone() else {
            return;
        };
        for id in defs.of_module(self.module) {
            let def = defs.get(*id);
            // A variant's scheme is entered when its type is.
            if matches!(
                def.kind,
                crate::defs::DefKind::Variant { .. }
                    | crate::defs::DefKind::Trait(_)
                    | crate::defs::DefKind::TypeAlias
            ) {
                continue;
            }
            if let Some(scheme) = env.lookup(def.name) {
                let mut scheme = scheme.clone();
                // A `pub let` whose type is partly unknown is reported at
                // its declaration (`report_unknown_let_types`); its
                // importers see the unknown part as an error type, so no
                // importer fixes it.
                if def.kind == crate::defs::DefKind::Let
                    && def.vis == crate::defs::Vis::Pub
                    && !self.is_cell
                {
                    let ty = self.apply(&scheme.ty);
                    let mut rows = Vec::new();
                    row_tail_vars(&ty, &mut rows);
                    let unknown: HashMap<TyVar, Type> = free_vars_in(&ty)
                        .into_iter()
                        .filter(|v| !scheme.vars.contains(v) && !rows.contains(v))
                        .map(|v| (v, Type::Error))
                        .collect();
                    if !unknown.is_empty() {
                        scheme.ty = substitute_vars(&ty, &unknown);
                    }
                }
                scheme.preds = self.settled_preds(&scheme.preds);
                self.tables.schemes.insert(*id, scheme);
            }
        }
    }

    /// The scheme of a definition a name resolves to that is not one
    /// of the module's own functions and `let`s (those are in the
    /// module's scope): a variant's constructor, a type written as a
    /// value, a member of another module, a builtin.
    pub(super) fn def_scheme(
        &self,
        res: Option<crate::defs::Res>,
        env: &TypeEnv,
    ) -> Option<Scheme> {
        let Some(crate::defs::Res::Def(id)) = res else {
            return None;
        };
        let def = self.def(id)?;
        if def.module.is_builtin() {
            return builtin_scheme(&def);
        }
        if self.names_rejected(res, def.name) {
            return Some(Scheme::mono(Type::Error));
        }
        // An enum written as a value whose name a variant of it has too
        // (`type Box(a) { Box(a), Empty }`): the name is bound to the
        // variant, and the type's value is made here.
        if let crate::defs::DefKind::Type(t) = def.kind {
            let ty = TypeRef {
                id: t,
                name: def.name,
            };
            if let Some(info) = self.tables.enums.get(&ty)
                && info.variants.iter().any(|v| v.name == def.name)
            {
                let vars = info.param_var_ids.clone();
                let args = vars.iter().map(|v| Type::Var(*v)).collect();
                return Some(Scheme {
                    vars,
                    preds: Vec::new(),
                    ty: Type::type_of(Type::Generic(ty, args)),
                });
            }
        }
        if def.module == self.module && !matches!(def.kind, crate::defs::DefKind::Variant { .. }) {
            return env.lookup(def.name).cloned();
        }
        self.tables.schemes.get(&id).cloned()
    }

    /// The scheme of an impl's method: its type, general in its type
    /// variables (the impl's and the method's own), with what the impl's
    /// header, the trait and the method ask of them.
    pub(super) fn method_scheme(&self, entry: &MethodEntry) -> Scheme {
        let ty = self.apply(&entry.method_type);
        let mut vars = free_vars_in(&ty);
        for pred in &entry.preds {
            for v in pred
                .args()
                .iter()
                .chain([pred.subject()])
                .flat_map(free_vars_in)
            {
                if !vars.contains(&v) {
                    vars.push(v);
                }
            }
        }
        Scheme {
            vars,
            preds: entry.preds.clone(),
            ty,
        }
    }
}

/// Where a trait method's call gave its type argument (`at`), if the call
/// knows the type already: each use of a definition that takes the call
/// into its scheme reports it there.
fn given_at(method_ty: &Type, at: Span) -> Option<Span> {
    super::inference::type_of_var(method_ty)
        .is_none()
        .then_some(at)
}

/// Take `pred`, owed for `owner` (`let_owners`), into a scheme's
/// predicates once. (One owed twice, once for another module's definition
/// and once for no such definition, is owed for none.)
fn take_pred(
    preds: &mut Vec<Pred>,
    owners: &mut Vec<Option<crate::session::ModuleId>>,
    pred: Pred,
    owner: Option<crate::session::ModuleId>,
) {
    match preds.iter().position(|taken| *taken == pred) {
        Some(k) if owners[k] != owner => owners[k] = None,
        Some(_) => {}
        None => {
            preds.push(pred);
            owners.push(owner);
        }
    }
}
