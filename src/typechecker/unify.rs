use super::*;

/// One session's type-variable supply: the substitution and the next
/// variable. Every module's check allocates from it, so a scheme one
/// module exports is valid in the next as it is.
///
/// Each variable has a level: how deep in generalisation scopes it was
/// made. The check of a `let`'s value, or of a group of top-level
/// definitions, runs one level deeper than what surrounds it, and the
/// variables still at the deeper level when it ends are the ones no
/// outer binding mentions: those are generalised. Binding a variable to
/// a type lowers the variables of the type to its own level, so a
/// variable an outer binding has reached is never generalised.
#[derive(Clone, Default)]
pub struct TyVarSupply {
    /// The substitution: maps type variables to their resolved types.
    pub(super) subst: Vec<Option<Type>>,
    /// The level of each variable.
    levels: Vec<u32>,
    /// The level new variables are made at.
    level: u32,
    /// Counter for generating fresh type variables.
    pub(super) next: TyVar,
    /// The first variable of the module being checked.
    base: TyVar,
    /// The variables below `base` the module's check bound: an earlier
    /// module's, which a later module may resolve (`pub let xs = []`).
    pub(super) trail: Vec<TyVar>,
    /// The variables each checked module allocated, in the order of the
    /// checks, and whether the module was forgotten since.
    ranges: Vec<(crate::session::ModuleId, TyVar, bool)>,
}

impl TyVarSupply {
    /// Resolve the variable `v` to `t`.
    pub(super) fn bind(&mut self, v: TyVar, t: Type) {
        if v < self.base {
            self.trail.push(v);
        }
        self.lower(&t, self.levels[v]);
        self.subst[v] = Some(t);
    }

    /// A new variable, at the current level.
    fn fresh(&mut self) -> TyVar {
        let v = self.next;
        self.next += 1;
        self.subst.push(None);
        self.levels.push(self.level);
        v
    }

    /// Lower the unresolved variables of `t` to `level`, those above it.
    pub(super) fn lower(&mut self, t: &Type, level: u32) {
        for v in free_vars_in(t) {
            match self.subst[v].take() {
                Some(resolved) => {
                    self.lower(&resolved, level);
                    self.subst[v] = Some(resolved);
                }
                None => self.levels[v] = self.levels[v].min(level),
            }
        }
    }

    /// Whether the unresolved variable `v` is outside every declaration:
    /// a declaration and its body are checked at least one level deep.
    pub(super) fn is_outermost(&self, v: TyVar) -> bool {
        self.levels[v] == 0
    }

    /// Whether the unresolved variable `v` is above the current level:
    /// nothing outside the scope that just ended mentions it.
    pub(super) fn is_generalizable(&self, v: TyVar) -> bool {
        self.levels[v] > self.level
    }

    /// Start the check of `module`: its variables come next.
    pub(super) fn begin(&mut self, module: crate::session::ModuleId) {
        self.level = 0;
        self.base = self.next;
        self.trail.clear();
        self.ranges.push((module, self.next, false));
    }

    /// Forget the check of `module`: undo what it bound of the earlier
    /// modules' variables (`trail`), and give back the variables of the
    /// checks at the end of the supply that are all forgotten, so a long
    /// session (an editor, a REPL) does not grow with each check again.
    pub(super) fn forget(&mut self, module: crate::session::ModuleId, trail: &[TyVar]) {
        for v in trail {
            if let Some(slot) = self.subst.get_mut(*v) {
                *slot = None;
            }
        }
        for range in self.ranges.iter_mut().filter(|r| r.0 == module) {
            range.2 = true;
        }
        while let Some(&(_, start, true)) = self.ranges.last() {
            self.ranges.pop();
            self.subst.truncate(start);
            self.levels.truncate(start);
            self.next = start;
        }
    }
}

/// Why two types do not unify: one fault for each part that does not.
/// `TypeChecker::unify` reports it; a caller with a better message for
/// its place (a list element, an operand) writes its own.
pub(super) struct Mismatch(Vec<Fault>);

/// One part of two types that does not unify.
pub(super) struct Fault {
    code: Code,
    message: String,
    help: Option<String>,
    /// The two types (got, expected) when their heads differ: the report
    /// offers to wrap a value in `Ok(...)` where a `Result` is expected.
    apart: Option<(Type, Type)>,
}

impl Fault {
    fn new(code: Code, message: impl Into<String>) -> Fault {
        Fault {
            code,
            message: message.into(),
            help: None,
            apart: None,
        }
    }
}

impl TypeChecker {
    // ── Fresh variables ─────────────────────────────────────────────

    pub(super) fn fresh_var(&mut self) -> Type {
        Type::Var(self.tables.vars.fresh())
    }

    /// Enter a generalisation scope: the variables made in it, and not
    /// reached from outside it by the time `exit_level` is called, are
    /// what `generalize` quantifies.
    pub(super) fn enter_level(&mut self) {
        self.tables.vars.level += 1;
        self.bound_marks.push(self.bound_log.len());
    }

    /// Leave the scope `enter_level` entered.
    pub(super) fn exit_level(&mut self) {
        self.tables.vars.level -= 1;
        self.closed_mark = self.bound_marks.pop().expect("a level is open");
    }

    /// Keep the unresolved variables of `ty` out of every later
    /// generalisation at this level: `ty` is the type of a binding that
    /// is not generalised.
    pub(super) fn keep_monomorphic(&mut self, ty: &Type) {
        let level = self.tables.vars.level;
        self.tables.vars.lower(ty, level);
    }

    /// Allocate a fresh `TyVar` id without wrapping it in `Type::Var`.
    /// Used by row polymorphism for `RowTail::Var(id)` where the id is
    /// the binding target rather than a type position.
    pub(super) fn fresh_tyvar_id(&mut self) -> TyVar {
        self.tables.vars.fresh()
    }

    // ── Substitution / apply ────────────────────────────────────────

    /// Walk the substitution chain to find the most resolved type.
    pub(super) fn apply(&self, ty: &Type) -> Type {
        match ty {
            Type::Var(v) => {
                if let Some(Some(resolved)) = self.tables.vars.subst.get(*v) {
                    self.apply(resolved)
                } else {
                    ty.clone()
                }
            }
            Type::Fun(params, ret) => {
                let params = params.iter().map(|p| self.apply(p)).collect();
                let ret = Box::new(self.apply(ret));
                Type::Fun(params, ret)
            }
            Type::List(inner) => Type::List(Box::new(self.apply(inner))),
            Type::Range(inner) => Type::Range(Box::new(self.apply(inner))),
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.apply(e)).collect()),
            Type::Record(name, fields) => {
                let fields = fields.iter().map(|(n, t)| (*n, self.apply(t))).collect();
                Type::Record(*name, fields)
            }
            Type::Generic(name, args) => {
                let args = args.iter().map(|a| self.apply(a)).collect();
                Type::Generic(*name, args)
            }
            Type::Map(k, v) => Type::Map(Box::new(self.apply(k)), Box::new(self.apply(v))),
            Type::Set(inner) => Type::Set(Box::new(self.apply(inner))),
            Type::Channel(inner) => Type::Channel(Box::new(self.apply(inner))),
            // Walk into AssocProj receivers so a once-fresh tyvar that
            // later unified with a concrete type gets propagated, which
            // unblocks `canonicalize`'s impl-binding lookup.
            Type::AssocProj {
                receiver,
                trait_name,
                assoc_name,
            } => Type::AssocProj {
                receiver: Box::new(self.apply(receiver)),
                trait_name: *trait_name,
                assoc_name: *assoc_name,
            },
            // Anon records: apply through each field; if the row tail
            // is a Var that resolved to another AnonRecord, merge its
            // fields and propagate its tail.
            Type::AnonRecord { fields, tail } => {
                use std::collections::BTreeMap;
                let mut new_fields: BTreeMap<Symbol, Type> =
                    fields.iter().map(|(n, t)| (*n, self.apply(t))).collect();
                let new_tail = match tail {
                    RowTail::Closed => RowTail::Closed,
                    RowTail::Var(v) => {
                        if let Some(Some(resolved)) = self.tables.vars.subst.get(*v) {
                            let resolved = self.apply(resolved);
                            if let Type::AnonRecord {
                                fields: rfields,
                                tail: rtail,
                            } = resolved
                            {
                                for (n, t) in rfields {
                                    new_fields.entry(n).or_insert(t);
                                }
                                rtail
                            } else if let Type::Var(w) = resolved {
                                // Tail var transitively resolved to another
                                // tyvar — re-tail on the fresh var so the
                                // freshening propagates. Mirrors
                                // substitute_vars' `Some(Type::Var(w))` arm.
                                RowTail::Var(w)
                            } else {
                                // Round 79 LATENT TS-L2: matches
                                // substitute_vars sibling — keeps the two
                                // row-tail walks aligned per L2. The
                                // unifier should never bind a row tail var
                                // to a non-record, non-Var concrete type;
                                // catch genuine drift loudly in debug
                                // builds, release falls through to the
                                // pre-existing safe fallback.
                                let _other = &resolved;
                                debug_assert!(
                                    false,
                                    "row tail var bound to non-record concrete type {:?}",
                                    _other
                                );
                                RowTail::Var(*v)
                            }
                        } else {
                            RowTail::Var(*v)
                        }
                    }
                };
                Type::AnonRecord {
                    fields: new_fields,
                    tail: new_tail,
                }
            }
            _ => ty.clone(),
        }
    }

    // ── Unification ─────────────────────────────────────────────────

    /// Helper: substitute record param vars with the call-site type
    /// args and return the field list. Used by anon×nominal-via-Generic
    /// unification.
    fn instantiate_record_fields_with_args(
        &mut self,
        name: TypeRef,
        args: &[Type],
    ) -> Vec<(Symbol, Type)> {
        if let Some(rec_info) = self.tables.records.get(&name).cloned() {
            if let Some(param_var_ids) = self.tables.record_param_var_ids.get(&name).cloned() {
                let mapping: HashMap<TyVar, Type> = if args.len() == param_var_ids.len() {
                    param_var_ids
                        .iter()
                        .zip(args.iter())
                        .map(|(&v, t)| (v, t.clone()))
                        .collect()
                } else {
                    param_var_ids
                        .iter()
                        .map(|&v| (v, self.fresh_var()))
                        .collect()
                };
                rec_info
                    .fields
                    .iter()
                    .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                    .collect()
            } else {
                rec_info.fields
            }
        } else {
            Vec::new()
        }
    }

    /// Round 74 Fix #5: canonical wording for the occurs-check
    /// diagnostic emitted from every unification site (main `Var(v) ↔ t`
    /// arm at line ~1270, plus the five row-unif arms in
    /// `unify_anon_anon` / `unify_anon_nominal`). Pre-fix: the row-unif
    /// arms emitted the terse `"infinite type"` while the main arm
    /// emitted `"infinite type: the type variable appears inside {t}"`.
    /// Routing all six sites through this helper keeps the diagnostic
    /// shape uniform — "collapse equivalent dual shapes to one unified
    /// form" applies to diagnostic wording too. A future user
    /// debugging an occurs-check failure shouldn't see two different
    /// messages depending on which arm tripped.
    pub fn infinite_type_message(t: &Type) -> String {
        format!("infinite type: the type variable appears inside {t}")
    }

    /// Resolve the variable `v` to `t`, unless that would take an
    /// annotation variable out of its declaration: a variable of
    /// something defined outside every declaration being checked (a
    /// top-level `let` that is not generalised, an earlier module's
    /// value) cannot stand for a type only one declaration's body knows.
    fn bind(&mut self, v: TyVar, t: Type, out: &mut Vec<Fault>) {
        if self.tables.vars.is_outermost(v)
            && let Some(r) = rigid_in(&self.apply(&t))
        {
            out.push(Fault::new(
                Code::TypeMismatch,
                format!(
                    "the type variable `{0}` would escape its declaration: inside it `{0}` \
                     stands for any type, but here it would become the type of a value \
                     defined outside",
                    r.name
                ),
            ));
            return;
        }
        self.tables.vars.bind(v, t);
    }

    /// Unify two anon records. See module-level row-poly notes.
    fn unify_anon_anon(
        &mut self,
        f1: std::collections::BTreeMap<Symbol, Type>,
        tail1: RowTail,
        f2: std::collections::BTreeMap<Symbol, Type>,
        tail2: RowTail,
        out: &mut Vec<Fault>,
    ) {
        use std::collections::BTreeMap;
        // Skip work if both tails point to the same row var AND fields
        // are identical — already unified.
        if let (RowTail::Var(v1), RowTail::Var(v2)) = (&tail1, &tail2)
            && v1 == v2
            && f1 == f2
        {
            return;
        }
        // Pairwise unify overlapping fields.
        let common_keys: Vec<Symbol> = f1.keys().filter(|k| f2.contains_key(k)).copied().collect();
        for k in &common_keys {
            let a = f1.get(k).cloned().unwrap();
            let b = f2.get(k).cloned().unwrap();
            self.unify_into(&a, &b, out);
        }
        // Determine non-overlapping parts.
        let only_in_1: BTreeMap<Symbol, Type> = f1
            .iter()
            .filter(|(k, _)| !f2.contains_key(*k))
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        let only_in_2: BTreeMap<Symbol, Type> = f2
            .iter()
            .filter(|(k, _)| !f1.contains_key(*k))
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        match (tail1, tail2) {
            (RowTail::Closed, RowTail::Closed) => {
                // Both must have the same field set.
                if !only_in_1.is_empty() {
                    let names: Vec<String> = only_in_1
                        .keys()
                        .map(|s| crate::intern::resolve(*s))
                        .collect();
                    out.push(Fault::new(
                        Code::NoSuchField,
                        format!(
                            "record literal has unexpected field{} not declared in target type: {}",
                            if names.len() == 1 { "" } else { "s" },
                            names.join(", ")
                        ),
                    ));
                }
                if !only_in_2.is_empty() {
                    let names: Vec<String> = only_in_2
                        .keys()
                        .map(|s| crate::intern::resolve(*s))
                        .collect();
                    out.push(Fault::new(
                        Code::MissingField,
                        format!(
                            "record literal is missing required field{}: {}",
                            if names.len() == 1 { "" } else { "s" },
                            names.join(", ")
                        ),
                    ));
                }
            }
            (RowTail::Var(v), RowTail::Closed) => {
                // Open × Closed: open must be a subset of closed; bind
                // v to a closed record carrying the leftover closed fields.
                // Round 75 DEAD-5: shared with the symmetric
                // Closed×Open arm via `unify_open_closed`.
                self.unify_open_closed(&only_in_1, only_in_2, v, out);
            }
            (RowTail::Closed, RowTail::Var(v)) => {
                // Closed × Open: symmetric — open side (only_in_2) must
                // not have extras; bind v to leftover from the closed
                // side (only_in_1). Round 75 DEAD-5.
                self.unify_open_closed(&only_in_2, only_in_1, v, out);
            }
            (RowTail::Var(v1), RowTail::Var(v2)) if v1 == v2 => {
                // Same row var, but field disagreement: impossible to
                // satisfy.
                if !only_in_1.is_empty() || !only_in_2.is_empty() {
                    out.push(Fault::new(
                        Code::TypeMismatch,
                        "row variable shared between two records with mismatched field sets"
                            .to_string(),
                    ));
                }
            }
            (RowTail::Var(v1), RowTail::Var(v2)) => {
                // Open × Open: bind v1 to a record carrying only_in_2 with
                // a fresh shared row tail; bind v2 symmetrically.
                let new_tail_id = self.fresh_tyvar_id();
                let new_tail = RowTail::Var(new_tail_id);
                let to_v1 = Type::AnonRecord {
                    fields: only_in_2,
                    tail: new_tail.clone(),
                };
                let to_v2 = Type::AnonRecord {
                    fields: only_in_1,
                    tail: new_tail,
                };
                if !occurs_in(v1, &to_v1) {
                    self.bind(v1, to_v1, out);
                } else {
                    let msg = Self::infinite_type_message(&to_v1);
                    out.push(Fault::new(Code::InfiniteType, msg));
                }
                if !occurs_in(v2, &to_v2) {
                    self.bind(v2, to_v2, out);
                } else {
                    let msg = Self::infinite_type_message(&to_v2);
                    out.push(Fault::new(Code::InfiniteType, msg));
                }
            }
        }
    }

    /// Round 75 DEAD-5: dedupe of the byte-symmetric Open×Closed and
    /// Closed×Open arms in `unify_anon_anon`. Bind a row-tail var
    /// `v` from the open side to a closed-tailed leftover record made
    /// of the closed side's extra fields. Reject if the open side has
    /// fields the closed side does not declare. The occurs-check
    /// barrier is preserved (round-73f Fix #2 lock); the canonical
    /// `Self::infinite_type_message` wording stays in lockstep with
    /// the other 4 occurs-check sites in this module.
    ///
    /// `open_extras` are fields present on the open side but missing
    /// from the closed side (rejected — they cannot be added to a
    /// closed shape). `closed_extras` are fields present on the
    /// closed side but missing from the open side (these become the
    /// row-tail leftover the open's row var binds to).
    fn unify_open_closed(
        &mut self,
        open_extras: &std::collections::BTreeMap<Symbol, Type>,
        closed_extras: std::collections::BTreeMap<Symbol, Type>,
        v: TyVar,
        out: &mut Vec<Fault>,
    ) {
        if !open_extras.is_empty() {
            let names: Vec<String> = open_extras
                .keys()
                .map(|s| crate::intern::resolve(*s))
                .collect();
            out.push(Fault::new(
                Code::TypeMismatch,
                format!(
                    "record open side has fields not present in closed target: {}",
                    names.join(", ")
                ),
            ));
            return;
        }
        let leftover = Type::AnonRecord {
            fields: closed_extras,
            tail: RowTail::Closed,
        };
        if !occurs_in(v, &leftover) {
            self.bind(v, leftover, out);
        } else {
            out.push(Fault::new(
                Code::InfiniteType,
                Self::infinite_type_message(&leftover),
            ));
        }
    }

    /// Unify an anon record with a nominal record's field list.
    /// `nominal_fields` is the resolved (instantiated) field list of the
    /// nominal record. The nominal record is treated as a closed shape.
    fn unify_anon_nominal(
        &mut self,
        anon_fields: std::collections::BTreeMap<Symbol, Type>,
        anon_tail: RowTail,
        nominal_fields: &[(Symbol, Type)],
        out: &mut Vec<Fault>,
    ) {
        use std::collections::BTreeMap;
        let mut nf_map: BTreeMap<Symbol, Type> = BTreeMap::new();
        for (n, t) in nominal_fields {
            nf_map.insert(*n, t.clone());
        }
        // Unify pairwise on overlapping fields.
        for (k, av) in anon_fields.iter() {
            if let Some(nv) = nf_map.get(k) {
                self.unify_into(av, nv, out);
            }
        }
        let only_in_anon: BTreeMap<Symbol, Type> = anon_fields
            .iter()
            .filter(|(k, _)| !nf_map.contains_key(*k))
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        let only_in_nom: BTreeMap<Symbol, Type> = nf_map
            .iter()
            .filter(|(k, _)| !anon_fields.contains_key(*k))
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        if !only_in_anon.is_empty() {
            let names: Vec<String> = only_in_anon
                .keys()
                .map(|s| crate::intern::resolve(*s))
                .collect();
            out.push(Fault::new(
                Code::NoSuchField,
                format!(
                    "anon record has fields not declared on the nominal record: {}",
                    names.join(", ")
                ),
            ));
            return;
        }
        match anon_tail {
            RowTail::Closed => {
                if !only_in_nom.is_empty() {
                    let names: Vec<String> = only_in_nom
                        .keys()
                        .map(|s| crate::intern::resolve(*s))
                        .collect();
                    out.push(Fault::new(
                        Code::MissingField,
                        format!(
                            "anon record is missing fields the nominal record requires: {}",
                            names.join(", ")
                        ),
                    ));
                }
            }
            RowTail::Var(v) => {
                let leftover = Type::AnonRecord {
                    fields: only_in_nom,
                    tail: RowTail::Closed,
                };
                if !occurs_in(v, &leftover) {
                    self.bind(v, leftover, out);
                } else {
                    out.push(Fault::new(
                        Code::InfiniteType,
                        Self::infinite_type_message(&leftover),
                    ));
                }
            }
        }
    }

    /// Unify `t1` (the type got) with `t2` (the type expected) and report
    /// at `span` why they do not unify.
    pub(super) fn unify(&mut self, t1: &Type, t2: &Type, span: Span) {
        if let Err(mismatch) = self.unify_types(t1, t2) {
            self.report_mismatch(mismatch, span);
        }
    }

    /// Unify `t1` (the type got) with `t2` (the type expected). Every
    /// part that does unify is unified, whatever part does not; the
    /// error says why each of those does not, and nothing is reported.
    pub(super) fn unify_types(&mut self, t1: &Type, t2: &Type) -> Result<(), Mismatch> {
        let mut faults = Vec::new();
        self.unify_into(t1, t2, &mut faults);
        if faults.is_empty() {
            Ok(())
        } else {
            Err(Mismatch(faults))
        }
    }

    /// Report a failed unification at `span`.
    pub(super) fn report_mismatch(&mut self, mismatch: Mismatch, span: Span) {
        for fault in mismatch.0 {
            let mut d = Diagnostic::error(fault.code, span, fault.message);
            d.help.extend(fault.help);
            if let Some((got, expected)) = &fault.apart {
                Self::add_ok_wrap_fix(&mut d, got, expected);
            }
            self.errors.push(d);
        }
    }

    /// The fault of two types whose heads differ.
    fn type_mismatch(&self, got: &Type, expected: &Type) -> Fault {
        let (got_shown, expected_shown) = self.show_apart(got, expected);
        Fault {
            code: Code::TypeMismatch,
            message: format!("type mismatch: expected {expected_shown}, got {got_shown}"),
            help: Self::chain_hint(got, expected),
            apart: Some((got.clone(), expected.clone())),
        }
    }

    fn unify_into(&mut self, t1: &Type, t2: &Type, out: &mut Vec<Fault>) {
        let t1 = self.apply(t1);
        let t2 = self.apply(t2);

        // Associated-type projections: canonicalise both sides first so
        // concrete-receiver projections reduce to their impl bindings
        // before structural matching. The canonicaliser is a fixed-point
        // function (idempotent), so calling it on already-concrete shapes
        // is a no-op. Only enter this fast-path when at least one side
        // is an AssocProj — keeps the cost out of the hot unification
        // path for normal types.
        let t1 = if matches!(&t1, Type::AssocProj { .. }) {
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(&t1))
        } else {
            t1
        };
        let t2 = if matches!(&t2, Type::AssocProj { .. }) {
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(&t2))
        } else {
            t2
        };

        match (&t1, &t2) {
            (Type::Error, _) | (_, Type::Error) | (Type::Never, _) | (_, Type::Never) => {}
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::Bool, Type::Bool)
            | (Type::String, Type::String)
            | (Type::Unit, Type::Unit) => {}

            (Type::Var(v1), Type::Var(v2)) if v1 == v2 => {}

            // An annotation variable is itself only.
            (Type::Rigid(r1), Type::Rigid(r2)) if r1 == r2 => {}

            (Type::Var(v), t) | (t, Type::Var(v)) => {
                if occurs_in(*v, t) {
                    // Special case: trying to unify a type var `a` with
                    // `TypeOf(a)` means the user returned a `type a`
                    // parameter's value where they promised a value of
                    // type `a`. The descriptor is the *type*, not an
                    // instance of it.
                    if let Type::Generic(name, args) = t
                        && name.is_builtin(crate::defs::TYPE_OF)
                        && args.len() == 1
                        && matches!(&args[0], Type::Var(inner) if inner == v)
                    {
                        out.push(Fault::new(
                            Code::TypeMismatch,
                            "cannot return a `type a` parameter as a value of type `a` — \
                             the parameter is a type descriptor, not an instance. \
                             Construct an `a` in the body instead."
                                .to_string(),
                        ));
                    } else {
                        out.push(Fault::new(
                            Code::InfiniteType,
                            Self::infinite_type_message(t),
                        ));
                    }
                } else {
                    self.bind(*v, t.clone(), out);
                }
            }

            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                if p1.len() != p2.len() {
                    // Directional convention: t1 is the "got" side, t2 is
                    // the "expected" side (see the Record arm below and
                    // every unify() call site — e.g. `unify(body_ty,
                    // ret_ty, span)` passes got then expected). Earlier
                    // rounds formatted `p1.len()` as "expected", which
                    // reversed the diagnostic.
                    let (exp, got) = (p2.len(), p1.len());
                    out.push(Fault::new(
                        Code::ArityMismatch,
                        format!(
                            "function expects {exp} {arg_word}, got {got}",
                            arg_word = if exp == 1 { "argument" } else { "arguments" }
                        ),
                    ));
                } else {
                    for (a, b) in p1.iter().zip(p2.iter()) {
                        self.unify_into(a, b, out);
                    }
                    self.unify_into(r1, r2, out);
                }
            }

            // `trait T for Fn { ... }` registers a self_type of
            // `Generic("Fn", [])` because `Fn` is variadic — the parser
            // accepts only `Named`/`Generic` as impl targets, with no
            // surface form to express "any function type". A receiver
            // dispatched into this impl arrives as `Type::Fun(_, _)`,
            // which has no element-level constraints to match against
            // the empty Generic args. Treat the bare-`Fn` Generic as a
            // wildcard for any function shape so user impls dispatch.
            // `canonical_head` collapses `Fun → Fn` at
            // registration time, so the deprecated surface alias is
            // covered by the same arm.
            (Type::Fun(_, _), Type::Generic(name, args))
            | (Type::Generic(name, args), Type::Fun(_, _))
                if args.is_empty() && name.is_builtin("Fn") => {}

            // `trait T for Tuple { ... }` likewise registers a self_type
            // of `Generic("Tuple", [])` — tuples are variadic, so unlike
            // List/Map/Set/Channel there is no fresh-var element shape
            // `register_trait_impl` could synthesize for the bare target.
            // Treat the bare-`Tuple` Generic as a wildcard for any tuple
            // shape so direct receiver dispatch (`(1, 2).pretty()`)
            // matches the where-bound path, which already dispatched via
            // the head-keyed obligation. Bare `Tuple` is rejected as a
            // type annotation (`resolve_type_expr`'s uppercase fallback
            // errors "unknown type"), so this arm is reachable only via
            // trait-impl self-types, mirroring the `Fn` arm above.
            (Type::Tuple(_), Type::Generic(name, args))
            | (Type::Generic(name, args), Type::Tuple(_))
                if args.is_empty() && name.is_builtin("Tuple") => {}

            (Type::List(a), Type::List(b)) => {
                self.unify_into(a, b, out);
            }

            (Type::Range(a), Type::Range(b)) => {
                self.unify_into(a, b, out);
            }

            // Range(T) is a nominal zero-cost alias for List(T): they
            // unify bidirectionally at the element level. `1..10` infers
            // as `Range(Int)` so annotations `let r: Range(Int) = 1..10`
            // typecheck, but existing `list.*` call sites still accept
            // ranges and `let r: List(Int) = 1..10` still typechecks.
            // Runtime representation is unchanged (Vec<Value>).
            (Type::Range(a), Type::List(b)) | (Type::List(b), Type::Range(a)) => {
                self.unify_into(a, b, out);
            }

            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                self.unify_into(k1, k2, out);
                self.unify_into(v1, v2, out);
            }

            (Type::Set(a), Type::Set(b)) => {
                self.unify_into(a, b, out);
            }

            (Type::Channel(a), Type::Channel(b)) => {
                self.unify_into(a, b, out);
            }

            (Type::Tuple(a), Type::Tuple(b)) => {
                if a.len() != b.len() {
                    // Directional convention: t1 (=a) is the "got" side,
                    // t2 (=b) is the "expected" side. Earlier wording
                    // "expected {a.len()}, got {b.len()}" reversed this.
                    out.push(Fault::new(
                        Code::TypeMismatch,
                        format!(
                            "tuple length mismatch: expected {}, got {}",
                            b.len(),
                            a.len()
                        ),
                    ));
                } else {
                    for (x, y) in a.iter().zip(b.iter()) {
                        self.unify_into(x, y, out);
                    }
                }
            }

            (Type::Record(n1, f1), Type::Record(n2, f2)) => {
                if n1 != n2 {
                    let (got, expected) = self.show_apart(&t1, &t2);
                    out.push(Fault::new(
                        Code::TypeMismatch,
                        format!("record type mismatch: expected {expected}, got {got}"),
                    ));
                } else {
                    // Unify fields by name. Messages are directional:
                    // `t1` is the got side, `t2` is the expected side
                    // (see the tuple/Generic arms above — `unify(t1, t2)`
                    // treats `t2` as expected, `t1` as got). The symmetric
                    // "record is missing field" wording was ambiguous
                    // about which side was at fault; split into distinct
                    // "unexpected field" (got has a surplus) and
                    // "missing field" (got is short) diagnostics so the
                    // caret + message unambiguously identifies the fault.
                    for (name, t1_inner) in f1 {
                        if let Some((_, t2_inner)) = f2.iter().find(|(n, _)| n == name) {
                            self.unify_into(t1_inner, t2_inner, out);
                        } else {
                            out.push(Fault::new(Code::NoSuchField,
                                format!(
                                    "unexpected field '{name}' in record; type '{n1}' has no such field"
                                ),
                            ));
                        }
                    }
                    for (name, _t2_inner) in f2 {
                        if !f1.iter().any(|(n, _)| n == name) {
                            out.push(Fault::new(
                                Code::MissingField,
                                format!(
                                    "missing field '{name}' in record; type '{n1}' requires it"
                                ),
                            ));
                        }
                    }
                }
            }

            // Record(name, fields) is compatible with Generic(name, args)
            (Type::Record(n1, f1), Type::Generic(n2, a2)) if n1 == n2 && !a2.is_empty() => {
                // B2 (round 60): a parameterless record carries Generic args
                // here only when the user wrote `Point(Bool)` against a
                // `type Point { ... }` with no params — `record_param_var_ids`
                // is absent for parameterless records, so the silent no-op
                // path swallowed the arity violation. Reject explicitly.
                if !self.tables.record_param_var_ids.contains_key(n1)
                    && self.tables.records.contains_key(n1)
                {
                    out.push(Fault::new(
                        Code::ArityMismatch,
                        format!(
                            "type argument count mismatch for {n1}: expected 0, got {}",
                            a2.len()
                        ),
                    ));
                    return;
                }
                if let (Some(rec_info), Some(param_var_ids)) = (
                    self.tables.records.get(n1).cloned(),
                    self.tables.record_param_var_ids.get(n1).cloned(),
                ) && param_var_ids.len() == a2.len()
                {
                    for (field_name, field_template_ty) in &rec_info.fields {
                        let substituted =
                            substitute_enum_params(field_template_ty, &param_var_ids, a2);
                        if let Some((_, concrete_ty)) = f1.iter().find(|(n, _)| n == field_name) {
                            self.unify_into(concrete_ty, &substituted, out);
                        }
                    }
                }
            }
            (Type::Record(n1, _), Type::Generic(n2, a2)) if n1 == n2 && a2.is_empty() => {
                // Only allow bare `Generic(name, [])` to match a Record if
                // the record is actually parameterless. For parameterized
                // records the Generic side must carry type args — otherwise
                // silently accepting it would let distinct uses pollute
                // the shared template TyVars (T1 audit fix).
                let expected = self
                    .tables
                    .record_param_var_ids
                    .get(n1)
                    .map(|v| v.len())
                    .unwrap_or(0);
                if expected != 0 {
                    out.push(Fault::new(
                        Code::ArityMismatch,
                        format!(
                            "type argument count mismatch for {n1}: expected {expected}, got 0"
                        ),
                    ));
                }
            }
            (Type::Generic(n1, a1), Type::Record(n2, f2)) if n1 == n2 && !a1.is_empty() => {
                // B2 (round 60) mirror: parameterless record with Generic args.
                if !self.tables.record_param_var_ids.contains_key(n2)
                    && self.tables.records.contains_key(n2)
                {
                    out.push(Fault::new(
                        Code::ArityMismatch,
                        format!(
                            "type argument count mismatch for {n2}: expected 0, got {}",
                            a1.len()
                        ),
                    ));
                    return;
                }
                if let (Some(rec_info), Some(param_var_ids)) = (
                    self.tables.records.get(n2).cloned(),
                    self.tables.record_param_var_ids.get(n2).cloned(),
                ) && param_var_ids.len() == a1.len()
                {
                    for (field_name, field_template_ty) in &rec_info.fields {
                        let substituted =
                            substitute_enum_params(field_template_ty, &param_var_ids, a1);
                        if let Some((_, concrete_ty)) = f2.iter().find(|(n, _)| n == field_name) {
                            self.unify_into(concrete_ty, &substituted, out);
                        }
                    }
                }
            }
            (Type::Generic(n1, a1), Type::Record(n2, _)) if n1 == n2 && a1.is_empty() => {
                // Mirror image of the Record/Generic arm above.
                let expected = self
                    .tables
                    .record_param_var_ids
                    .get(n2)
                    .map(|v| v.len())
                    .unwrap_or(0);
                if expected != 0 {
                    out.push(Fault::new(
                        Code::ArityMismatch,
                        format!(
                            "type argument count mismatch for {n2}: expected {expected}, got 0"
                        ),
                    ));
                }
            }

            // ── Anon record × Anon record ─────────────────────────────
            (
                Type::AnonRecord {
                    fields: f1,
                    tail: tail1,
                },
                Type::AnonRecord {
                    fields: f2,
                    tail: tail2,
                },
            ) => {
                self.unify_anon_anon(f1.clone(), tail1.clone(), f2.clone(), tail2.clone(), out);
            }

            // ── Anon record × Nominal record (widening) ───────────────
            (
                Type::AnonRecord {
                    fields: af,
                    tail: at,
                },
                Type::Record(_, nf),
            ) => {
                self.unify_anon_nominal(af.clone(), at.clone(), nf, out);
            }
            (
                Type::Record(_, nf),
                Type::AnonRecord {
                    fields: af,
                    tail: at,
                },
            ) => {
                self.unify_anon_nominal(af.clone(), at.clone(), nf, out);
            }
            (
                Type::AnonRecord {
                    fields: af,
                    tail: at,
                },
                Type::Generic(name, args),
            ) if self.tables.records.contains_key(name) => {
                let nf_inst = self.instantiate_record_fields_with_args(*name, args);
                self.unify_anon_nominal(af.clone(), at.clone(), &nf_inst, out);
            }
            (
                Type::Generic(name, args),
                Type::AnonRecord {
                    fields: af,
                    tail: at,
                },
            ) if self.tables.records.contains_key(name) => {
                let nf_inst = self.instantiate_record_fields_with_args(*name, args);
                self.unify_anon_nominal(af.clone(), at.clone(), &nf_inst, out);
            }

            (Type::Generic(n1, a1), Type::Generic(n2, a2)) => {
                if n1 != n2 {
                    // Round 80 BROKEN B1+B2: this arm formatted the bare
                    // `Symbol` heads, which dropped type args (`Bag(String)`
                    // → `Bag`) and leaked the internal `TypeOf` head
                    // (`type Person` → `TypeOf`). Format via the parent
                    // `Type` values so `Type::Display`'s args rendering
                    // and TypeOf special-casing apply (mirrors the
                    // catch-all arm at the bottom of `unify`).
                    out.push(self.type_mismatch(&t1, &t2));
                } else if a1.len() != a2.len() {
                    // Directional convention: t1 (=a1) is the "got" side,
                    // t2 (=a2) is the "expected" side (see the Record arm
                    // above and the unify() callsite convention). Earlier
                    // wording had a1/a2 reversed.
                    out.push(Fault::new(
                        Code::ArityMismatch,
                        format!(
                            "type argument count mismatch for {n1}: expected {}, got {}",
                            a2.len(),
                            a1.len()
                        ),
                    ));
                } else {
                    for (x, y) in a1.iter().zip(a2.iter()) {
                        self.unify_into(x, y, out);
                    }
                }
            }

            // ── AssocProj × AssocProj (abstract receivers) ────────────
            // The canonicalise fast-path at the top of `unify` already
            // reduced any projection whose receiver has a registered
            // impl, so reaching this arm means both sides are still
            // abstract — the receiver is a type variable under a
            // `where` bound (or another unreduced projection). Per the
            // documented contract on `Type::AssocProj`
            // (src/types/mod.rs): two abstract projections unify iff
            // they have the same receiver, trait_name, and assoc_name.
            // "Same receiver" means *unifiable* receivers — two
            // distinct type variables can still unify (binding one to
            // the other), so recurse rather than compare structurally.
            (
                Type::AssocProj {
                    receiver: r1,
                    trait_name: tn1,
                    assoc_name: an1,
                },
                Type::AssocProj {
                    receiver: r2,
                    trait_name: tn2,
                    assoc_name: an2,
                },
            ) => {
                if tn1 == tn2 && an1 == an2 {
                    self.unify_into(r1, r2, out);
                } else {
                    // Different trait or different member: genuinely
                    // distinct abstract types. Directional convention:
                    // t1 is the "got" side, t2 the "expected" side.
                    out.push(Fault::new(
                        Code::TypeMismatch,
                        format!("type mismatch: expected {t2}, got {t1}"),
                    ));
                }
            }

            _ => {
                // Suppress cascade errors where either side is already in
                // error state — a previous diagnostic explained the root
                // cause and further mismatch reports would confuse.
                if matches!(&t1, Type::Error) || matches!(&t2, Type::Error) {
                    return;
                }
                // When either side is an unresolved type variable, the
                // user doesn't have a user-facing name for it yet
                // (`?17` is internal). Report as "cannot determine" and
                // nudge toward an annotation.
                match (&t1, &t2) {
                    (Type::Var(_), other) | (other, Type::Var(_)) => {
                        out.push(Fault::new(
                            Code::AmbiguousType,
                            format!(
                                "cannot determine a consistent type here; \
                                 one side resolved to `{other}` but the other \
                                 is still unspecified — add a type annotation"
                            ),
                        ));
                    }
                    _ => {
                        out.push(self.type_mismatch(&t1, &t2));
                    }
                }
            }
        }
    }

    /// Helper: create a fresh type variable and return both the Type::Var and
    /// its TyVar id.
    pub(super) fn fresh_tv(&mut self) -> (Type, TyVar) {
        let t = self.fresh_var();
        let v = match &t {
            Type::Var(v) => *v,
            _ => unreachable!(),
        };
        (t, v)
    }
}

/// A rigid variable of `ty`, if it has one.
pub(super) fn rigid_in(ty: &Type) -> Option<RigidId> {
    match ty {
        Type::Rigid(r) => Some(*r),
        Type::Fun(params, ret) => params.iter().find_map(rigid_in).or_else(|| rigid_in(ret)),
        Type::List(inner) | Type::Range(inner) | Type::Set(inner) | Type::Channel(inner) => {
            rigid_in(inner)
        }
        Type::Tuple(elems) => elems.iter().find_map(rigid_in),
        Type::Record(_, fields) => fields.iter().find_map(|(_, t)| rigid_in(t)),
        Type::Generic(_, args) => args.iter().find_map(rigid_in),
        Type::Map(k, v) => rigid_in(k).or_else(|| rigid_in(v)),
        Type::AssocProj { receiver, .. } => rigid_in(receiver),
        Type::AnonRecord { fields, .. } => fields.values().find_map(rigid_in),
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Var(_)
        | Type::Error
        | Type::Never => None,
    }
}

/// Check if a type variable occurs in a type (occurs check for unification).
fn occurs_in(var: TyVar, ty: &Type) -> bool {
    match ty {
        Type::Var(v) => *v == var,
        Type::Fun(params, ret) => params.iter().any(|p| occurs_in(var, p)) || occurs_in(var, ret),
        Type::List(inner) => occurs_in(var, inner),
        Type::Range(inner) => occurs_in(var, inner),
        Type::Tuple(elems) => elems.iter().any(|e| occurs_in(var, e)),
        Type::Record(_, fields) => fields.iter().any(|(_, t)| occurs_in(var, t)),
        Type::Generic(_, args) => args.iter().any(|a| occurs_in(var, a)),
        Type::Map(k, v) => occurs_in(var, k) || occurs_in(var, v),
        Type::Set(inner) => occurs_in(var, inner),
        Type::Channel(inner) => occurs_in(var, inner),
        Type::AssocProj { receiver, .. } => occurs_in(var, receiver),
        Type::AnonRecord { fields, tail } => {
            fields.values().any(|t| occurs_in(var, t))
                || matches!(tail, RowTail::Var(v) if *v == var)
        }
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Rigid(_)
        | Type::Error
        | Type::Never => false,
    }
}
