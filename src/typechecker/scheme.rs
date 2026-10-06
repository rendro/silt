use super::*;

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
        self.default_selects(true);
        for i in self.closed_mark..self.wanted.len() {
            if self.wanted[i].solved {
                continue;
            }
            let mentioned: Vec<Type> = match &self.wanted[i].goal {
                Goal::Pred(_) => continue,
                Goal::Select {
                    recv, args, result, ..
                } => args.iter().chain([recv, result]).cloned().collect(),
                Goal::Update { base, value, .. } => vec![base.clone(), value.clone()],
                Goal::Try { operand, ok, ret } => {
                    ret.iter().chain([operand, ok]).cloned().collect()
                }
            };
            for ty in mentioned {
                self.keep_monomorphic(&ty);
            }
        }
        self.generalize_decided(ty)
    }

    fn generalize_decided(&mut self, ty: &Type) -> Scheme {
        let ty = self.apply(ty);
        let mut vars: Vec<TyVar> = free_vars_in(&ty)
            .into_iter()
            .filter(|v| self.tables.vars.is_generalizable(*v))
            .collect();
        let mut preds: Vec<Pred> = Vec::new();
        if !vars.is_empty() {
            for i in self.closed_mark..self.wanted.len() {
                if self.wanted[i].solved {
                    continue;
                }
                let Goal::Pred(Pred::Trait { tr, args, subject }) = &self.wanted[i].goal.clone()
                else {
                    continue;
                };
                let Type::Var(subject) = self.apply(subject) else {
                    continue;
                };
                if !vars.contains(&subject) {
                    continue;
                }
                self.wanted[i].in_scheme = true;
                let pred = Pred::Trait {
                    tr: *tr,
                    args: args.iter().map(|t| self.apply(t)).collect(),
                    subject: Type::Var(subject),
                };
                if !preds.contains(&pred) {
                    preds.push(pred);
                }
            }
            // A variable only a predicate's trait arguments mention is
            // the scheme's as well.
            for Pred::Trait { args, .. } in &preds {
                for v in args.iter().flat_map(free_vars_in) {
                    if self.tables.vars.is_generalizable(v) && !vars.contains(&v) {
                        vars.push(v);
                    }
                }
            }
        }
        Scheme {
            vars,
            preds,
            ty,
            optional_last_param: false,
        }
    }

    /// The scheme of a top-level function whose body is checked: `ty`,
    /// its type as its body sees it, with the annotation variables
    /// quantified again and bounded as its `where` clauses say (`bounds`).
    pub(super) fn generalize_fn(&mut self, ty: &Type, bounds: &[Pred]) -> Scheme {
        let (ty, rigid_vars) = unrigidify(&self.apply(ty));
        let mut scheme = self.generalize(&ty);
        for v in rigid_vars {
            if !scheme.vars.contains(&v) {
                scheme.vars.push(v);
            }
        }
        for bound in bounds {
            let Pred::Trait { subject, .. } = bound;
            if matches!(subject, Type::Var(v) if scheme.vars.contains(v))
                && !scheme.preds.contains(bound)
            {
                scheme.preds.push(bound.clone());
            }
        }
        scheme
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
        let recorded = self.wanted.split_off(self.closed_mark);
        for wanted in recorded {
            if wanted.solved {
                continue;
            }
            if self.waits_for_outer(wanted.goal.waits_on()) {
                self.wanted.push(wanted);
                continue;
            }
            let (tr, args, subject) = match &wanted.goal {
                Goal::Pred(Pred::Trait { tr, args, subject }) => (tr, args, subject),
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
            // (`println([])`).
            if ["Display", "Equal", "Compare", "Hash"]
                .iter()
                .any(|name| tr.is_builtin(name))
            {
                let _ = self.unify_types(subject, &Type::Unit);
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
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for &v in &scheme.vars {
            mapping.insert(v, self.fresh_var());
        }
        if !scheme.preds.is_empty() {
            let origin = self.use_origin();
            for pred in &scheme.preds {
                self.want(pred.substitute(&mapping), origin);
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
        for Pred::Trait { args, subject, .. } in &entry.preds {
            for v in args.iter().chain([subject]).flat_map(free_vars_in) {
                if !vars.contains(&v) {
                    vars.push(v);
                }
            }
        }
        Scheme {
            vars,
            preds: entry.preds.clone(),
            ty,
            optional_last_param: false,
        }
    }
}
