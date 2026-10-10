use super::inference::*;
use super::*;

/// A predicate a use owes (`TypeChecker::want`), until it is solved:
/// checked against the impls once its subject is known, or against the
/// declared bounds of an annotation variable; or taken into the scheme
/// of the definition that is generalised over its subject.
#[derive(Debug, Clone)]
pub(crate) struct Wanted {
    pub(super) goal: Goal,
    pub(super) origin: Origin,
    /// Whether it is checked already.
    pub(super) solved: bool,
    /// Whether a scheme has it: its subject is a variable a definition
    /// is general in (`generalize`), so each use of that owes it.
    pub(super) in_scheme: bool,
}

/// What waits for a type to be known.
#[derive(Debug, Clone)]
pub(crate) enum Goal {
    /// A predicate a use owes.
    Pred(Pred),
    /// `recv.name(args)`, with the result `result`, where the receiver's
    /// type was unknown at the call: which method or function-typed
    /// field it calls is decided when the receiver's type is (`select`),
    /// and when the definition is generalised if it still is not.
    Select {
        recv: Type,
        name: Symbol,
        args: Vec<Type>,
        result: Type,
    },
    /// `base.{ field: value }` where the type of `base` was unknown at
    /// the update: the field is checked when it is known. It never
    /// enters a scheme: an update of a record nothing decides needs an
    /// annotation.
    Update {
        base: Type,
        field: Symbol,
        value: Type,
    },
    /// `operand?`, with the result `ok`, in a function that returns `ret`
    /// (none outside any), where the operand's type was unknown. Like an
    /// update, it never enters a scheme.
    Try {
        operand: Type,
        ok: Type,
        ret: Option<Type>,
    },
    /// `result` is the anonymous record made from the record `base`:
    /// its fields but those named `without`, and the fields `with`
    /// (a spread `{...base, f: e}`; the rest binder of a record pattern,
    /// `{f, ...rest}`). What it is waits until the type of `base` is
    /// decided: a declared record gives its fields, an anonymous one
    /// its own. A base that is still unknown, or an open row, when its
    /// scope closes is a record over a row that no declared record may
    /// stand for (`Pred::Anon`).
    Rebuild {
        base: Type,
        without: Vec<Symbol>,
        with: Vec<(Symbol, Type)>,
        result: Type,
        /// Whether it is a pattern's rest (the pattern has reported a
        /// scrutinee that is no record).
        rest: bool,
    },
    /// A use of a scheme writes the record type `{fields, ...row}` and
    /// another with the same row: if a declared record stands for the
    /// row, each of `fields` is that record's field, at its type.
    Listed {
        fields: std::collections::BTreeMap<Symbol, Type>,
        row: Type,
    },
}

impl Goal {
    /// The type the goal waits for.
    pub(super) fn waits_on(&self) -> &Type {
        match self {
            Goal::Pred(pred) => pred.subject(),
            Goal::Select { recv, .. } => recv,
            Goal::Update { base, .. } => base,
            Goal::Try { operand, .. } => operand,
            Goal::Listed { row, .. } => row,
            // (Or the row of an open record: `rebuild_waits`.)
            Goal::Rebuild { base, .. } => base,
        }
    }
}

/// How many types deep the structural judgement looks (see
/// `structure_gap`).
const STRUCTURE_DEPTH: usize = 100;

const TOO_DEEP: &str = "it is nested too deeply, or its definition recurs at ever larger types";

/// What one structural judgement has gathered so far.
#[derive(Default)]
pub(super) struct Walk {
    /// The nominal types looked at, each at its arguments: a type that
    /// comes up again is not looked at twice.
    seen: Vec<(TypeRef, Vec<Type>)>,
    /// The parts whose type is unknown, or an annotation variable.
    pub(super) open: Vec<Type>,
    /// The parts that have a written `Display` impl.
    pub(super) written: Vec<Type>,
}

/// The part of a type that lacks a structural trait.
pub(super) struct Gap {
    /// The way to the part from the type: `field 'f'`, `element`.
    path: Vec<String>,
    /// The head of the part's type (`Fn`), and the type.
    head: String,
    full: String,
    /// Why, when it is not simply that the part's type has no such
    /// trait.
    why: Option<&'static str>,
}

impl Gap {
    /// Whether it is the type itself that lacks the trait, not a part
    /// of it.
    pub(super) fn is_whole(&self) -> bool {
        self.path.is_empty() && self.why.is_none()
    }
}

/// Where a predicate is owed, and what asked for it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Origin {
    /// The use: a call, a name used as a value.
    pub(super) span: Span,
    /// The name of what is used, for a message (`hello`, `list.sort`).
    pub(super) callee: Option<Symbol>,
    /// The operator that owes it, as a message names it (`'+'`), when
    /// the use is an operator's operand.
    pub(super) op: Option<&'static str>,
}

impl TypeChecker {
    /// A use owes `pred`. A subject that is known is checked now; an
    /// annotation variable must have the bound declared. A subject still
    /// unknown waits (`solve_wanted`): by the time the definitions being
    /// checked are done it is known, or it is generalised and the
    /// predicate is the scheme's (`generalize`), or it belongs to an
    /// outer binding and waits on.
    pub(super) fn want(&mut self, pred: Pred, origin: Origin) {
        self.want_goal(Goal::Pred(pred), origin);
    }

    /// `goal` waits for its type, or is decided now if the type is known.
    pub(super) fn want_goal(&mut self, goal: Goal, origin: Origin) {
        self.wanted.push(Wanted {
            goal,
            origin,
            solved: false,
            in_scheme: false,
        });
        self.solve_wanted(self.wanted.len() - 1);
    }

    /// Check each predicate owed since `from` whose subject is known by
    /// now. What the impl that answers one asks of the subject's parts
    /// (`trait Greet for Box(a) where a: Greet`) is owed in turn, by the
    /// same use: checked here if the part is known, waiting like any
    /// predicate if it is not.
    pub(super) fn solve_wanted(&mut self, from: usize) {
        let mut i = from;
        while i < self.wanted.len() {
            if !self.wanted[i].solved {
                let origin = self.wanted[i].origin;
                match self.wanted[i].goal.clone() {
                    Goal::Pred(Pred::Trait { tr, args, subject }) => {
                        let subject = self.apply(&subject);
                        if !matches!(subject, Type::Var(_)) {
                            self.wanted[i].solved = true;
                            // One error for one type at one place: a use
                            // that owes two traits its type has neither
                            // of (`test.assert_eq(f, f)`) says the first.
                            let reported = self.errors.len();
                            self.verify_trait_obligation(tr, &args, &subject, origin);
                            if self.errors.len() > reported {
                                let key = (origin.span, self.show_type(&subject));
                                if !self.lacking.insert(key) {
                                    self.errors.truncate(reported);
                                }
                            }
                        }
                    }
                    Goal::Pred(Pred::Anon { row, given }) => match self.apply(&row) {
                        Type::Var(_) => {}
                        // (The row of an open record is the rest of
                        // the row; the fields it has by now are looked
                        // at when the signature's body is known.)
                        Type::AnonRecord {
                            fields,
                            tail: RowTail::Var(rest),
                        } => {
                            if let Some(given) = given {
                                let seen = Type::AnonRecord {
                                    fields,
                                    tail: RowTail::Closed,
                                };
                                self.anon_waiting.push((given, seen, origin));
                            }
                            self.wanted[i].goal = Goal::Pred(Pred::Anon {
                                row: Type::Var(rest),
                                given,
                            });
                        }
                        row => {
                            self.wanted[i].solved = true;
                            match given {
                                Some(given) => self.anon_waiting.push((given, row, origin)),
                                None => self.row_is_anon(&row, origin),
                            }
                        }
                    },
                    Goal::Rebuild {
                        base,
                        without,
                        with,
                        result,
                        rest,
                    } => {
                        if self.rebuild_waits(&base).is_none() {
                            self.wanted[i].solved = true;
                            self.rebuild(&base, &without, &with, &result, rest, origin);
                        }
                    }
                    Goal::Listed { fields, row } => match self.apply(&row) {
                        Type::Var(_) => {}
                        Type::AnonRecord {
                            tail: RowTail::Var(rest),
                            ..
                        } => {
                            self.wanted[i].goal = Goal::Listed {
                                fields,
                                row: Type::Var(rest),
                            };
                        }
                        row => {
                            self.wanted[i].solved = true;
                            self.listed_fields_are(&fields, &row, origin);
                        }
                    },
                    Goal::Select {
                        recv,
                        name,
                        args,
                        result,
                    } => {
                        if !matches!(self.apply(&recv), Type::Var(_)) {
                            self.wanted[i].solved = true;
                            self.select_call(&recv, name, args, result, origin.span);
                        }
                    }
                    Goal::Try { operand, ok, ret } => {
                        let operand = self.apply(&operand);
                        if !matches!(operand, Type::Var(_)) {
                            self.wanted[i].solved = true;
                            self.try_operand(&operand, &ok, ret, origin.span);
                        }
                    }
                    Goal::Pred(Pred::Lacks { row, field }) => {
                        let row = self.apply(&row);
                        if !matches!(row, Type::Var(_)) {
                            self.wanted[i].solved = true;
                            self.row_lacks(&row, field, origin);
                        }
                    }
                    Goal::Update { base, field, value } => {
                        let base = self.apply(&base);
                        if !matches!(base, Type::Var(_)) {
                            self.wanted[i].solved = true;
                            self.update_field(&base, field, &value, origin.span);
                        }
                    }
                }
            }
            i += 1;
        }
    }

    /// A method call whose receiver was a type variable when it was
    /// inferred and is the type `type_name` now: `true` when the type has
    /// the method. The call's trait is recorded by its span
    /// (`deferred_method_traits`), which `resolve_all_types` writes on
    /// the access; a call that sees the method in two traits is
    /// ambiguous, as anywhere.
    pub(super) fn deferred_method_call(
        &mut self,
        type_name: TypeRef,
        field: Symbol,
        obj_ty: &Type,
        result_ty: &Type,
        span: Span,
    ) -> bool {
        let Some(entry) = self.method_entry(type_name, field) else {
            return false;
        };
        let instantiated = self.dispatch_method_entry(&entry, field, obj_ty, span);
        if let Some(t) = self.method_trait.take() {
            self.deferred_method_traits.insert(span, t);
        }
        let method_ty = self.apply(&instantiated);
        self.unify_deferred_method(result_ty, &method_ty, span);
        true
    }

    /// Unify the type a method call on an unknown receiver was given
    /// (`result_ty`) with the method it turned out to call.
    ///
    /// Method types include `self` as the first param. When the call
    /// site originally saw this field access as an unknown Var, it
    /// unified the var with a function type built from the *explicit*
    /// args only (no receiver). Strip `self` when adapting.
    pub(super) fn unify_deferred_method(&mut self, result_ty: &Type, method_ty: &Type, span: Span) {
        let result_resolved = self.apply(result_ty);
        match (&result_resolved, method_ty) {
            (Type::Fun(call_params, call_ret), Type::Fun(method_params, method_ret))
                if method_params.len() == call_params.len() + 1 =>
            {
                for (cp, mp) in call_params.iter().zip(method_params.iter().skip(1)) {
                    self.unify(cp, mp, span);
                }
                self.unify(call_ret, method_ret, span);
            }
            _ => {
                self.unify(result_ty, method_ty, span);
            }
        }
    }

    /// Decide what `recv.field(args)` calls, now that the receiver's type
    /// is known: a method of the type (its impls, or the bounds of an
    /// annotation variable), or the function a field of the record holds.
    fn select_call(
        &mut self,
        recv: &Type,
        field: Symbol,
        args: Vec<Type>,
        result: Type,
        span: Span,
    ) {
        let obj_ty = recv.clone();
        let result_ty = Type::Fun(args, Box::new(result));
        let resolved =
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(recv));
        match &resolved {
            Type::Error | Type::Never => {}
            Type::Var(_) => unreachable!("a selection waits while its receiver is unknown"),
            // The receiver became an annotation variable: it has the
            // methods of its bounds, and nothing else.
            Type::Rigid(r) => match self.bound_methods(*r, field).as_slice() {
                [(trait_name, scheme)] => {
                    let method_ty = self.instantiate_method(scheme, field, span);
                    let on_type = on_type_parameter(r.name, field);
                    if !self.takes_no_self(&method_ty, field, &on_type, span) {
                        self.deferred_method_traits.insert(span, *trait_name);
                        self.unify_deferred_method(&result_ty, &method_ty, span);
                    }
                }
                _ => self.error(
                    Code::UnknownMethod,
                    format!(
                        "no field or method '{field}' on a value of type `{}`: the \
                         bounds of the type variable provide none, or more than one",
                        r.name
                    ),
                    span,
                ),
            },
            Type::AnonRecord { fields: af, tail } => {
                if let Some(field_ty) = af.get(&field) {
                    let ft = field_ty.clone();
                    self.unify(&result_ty, &ft, span);
                } else if let RowTail::Var(_) = tail {
                    let extended = Type::AnonRecord {
                        fields: std::collections::BTreeMap::from([(field, result_ty.clone())]),
                        tail: RowTail::Var(self.fresh_tyvar_id()),
                    };
                    self.unify(&resolved, &extended, span);
                } else {
                    // ERR-GAP (round 81 F2): match the sibling Record /
                    // Generic deferred sites and append a did-you-mean
                    // hint when a near-edit-distance field exists.
                    let candidates: Vec<(Symbol, Type)> =
                        af.iter().map(|(k, v)| (*k, v.clone())).collect();
                    let base = format!("anon record has no field '{field}'");
                    self.error_help(
                        Code::NoSuchField,
                        format_record_field_suggestion(base, field, &candidates),
                        span,
                    );
                }
            }
            Type::Generic(type_name, type_args) => {
                // User-declared records with or without type parameters
                // are represented as Type::Generic(name, args). Look up
                // the record definition and validate the field.
                let type_name = *type_name;
                let type_args = type_args.clone();
                if let Some(rec_info) = self.tables.records.get(&type_name).cloned()
                    && let Some((_, ft)) = rec_info.fields.iter().find(|(n, _)| *n == field)
                {
                    // Same fresh-var fallback as in infer_expr (T1 audit fix):
                    // never return the template TyVar; if the caller's
                    // type_args are missing/mismatched, use fresh vars.
                    let field_ty = if let Some(param_var_ids) =
                        self.tables.record_param_var_ids.get(&type_name).cloned()
                    {
                        let mapping: HashMap<TyVar, Type> =
                            if type_args.len() == param_var_ids.len() {
                                param_var_ids
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(&v, t)| (v, t.clone()))
                                    .collect()
                            } else {
                                param_var_ids
                                    .iter()
                                    .map(|&v| (v, self.fresh_var()))
                                    .collect()
                            };
                        let substituted = substitute_vars(ft, &mapping);
                        self.apply(&substituted)
                    } else {
                        self.apply(ft)
                    };
                    self.unify(&result_ty, &field_ty, span);
                    return;
                }
                // Also check the method table for trait methods.
                if self.deferred_method_call(type_name, field, &obj_ty, &result_ty, span) {
                    return;
                }
                // A method of a structural trait the type has no entry
                // for.
                if let Some(method_ty) = self.structural_method(&obj_ty, field, span) {
                    self.last_field_access_was_method = false;
                    if let Some(t) = self.method_trait.take() {
                        self.deferred_method_traits.insert(span, t);
                    }
                    self.unify_deferred_method(&result_ty, &method_ty, span);
                    return;
                }
                // GAP (round 35 F7): thread did-you-mean suggestion
                // through the Generic/named-record deferred path.
                let shown = self.show_type(&Type::Generic(type_name, vec![]));
                let base = format!("unknown field or method '{field}' on type {shown}");
                let msg = if let Some(rec_info) = self.tables.records.get(&type_name) {
                    format_record_field_suggestion(base, field, &rec_info.fields)
                } else {
                    (base, None)
                };
                self.error_help(Code::UnknownField, msg, span);
            }
            // A builtin type (`Int`, `List`, ...): its trait methods.
            _ => {
                if let Some(type_name) = self.type_name_for_impl(&resolved)
                    && self.deferred_method_call(type_name, field, &obj_ty, &result_ty, span)
                {
                    return;
                }
                self.error(
                    Code::UnknownField,
                    format!(
                        "unknown field or method '{field}' on type {}",
                        self.show_type(&resolved)
                    ),
                    span,
                );
            }
        }
    }

    /// Check `operand?` now that the operand's type is known: it is a
    /// Result or an Option, whose payload is `result_ty`, and the
    /// function it is in returns the same wrapper.
    fn try_operand(
        &mut self,
        resolved: &Type,
        result_ty: &Type,
        expected_ret: Option<Type>,
        span: Span,
    ) {
        let (head, args) = match &resolved {
            Type::Error | Type::Never => return,
            Type::Generic(name, args) if name.is_builtin("Result") && args.len() == 2 => {
                ("Result", args.clone())
            }
            Type::Generic(name, args) if name.is_builtin("Option") && args.len() == 1 => {
                ("Option", args.clone())
            }
            other => {
                self.error(
                    Code::InvalidQuestion,
                    format!("'?' operator requires Result or Option type, got '{other}'"),
                    span,
                );
                return;
            }
        };
        // The unwrapped Ok/Some payload is what flowed into the
        // surrounding expression (t1 = got, t2 = expected).
        self.unify(&args[0], result_ty, span);
        let Some(ret) = expected_ret else {
            self.error(
                Code::InvalidQuestion,
                "? operator can only be used inside a function that returns Result or Option"
                    .to_string(),
                span,
            );
            return;
        };
        let ret_resolved = self.apply(&ret);
        let expected_wrapper = if head == "Result" {
            let fresh_ok = self.fresh_var();
            Type::builtin("Result", vec![fresh_ok, args[1].clone()])
        } else {
            let fresh_inner = self.fresh_var();
            Type::option(fresh_inner)
        };
        match &ret_resolved {
            Type::Error | Type::Never => {}
            // Return type still open (or already the right wrapper):
            // constrain it, exactly like the inline concrete path.
            Type::Var(_) => {
                self.unify(&ret_resolved, &expected_wrapper, span);
            }
            Type::Generic(n, _) if n.is_builtin(head) => {
                self.unify(&ret_resolved, &expected_wrapper, span);
            }
            // Concrete non-matching return type: this is the unsound
            // lambda repro (`{ x -> x? + 1 }` whose return type
            // resolved to Int). Curated message, same class as the
            // inline no-context error.
            other => {
                self.errors.push(
                    Diagnostic::error(
                        Code::InvalidQuestion,
                        span,
                        "? operator can only be used inside a function that returns Result or Option",
                    )
                    .with_note(format!(
                        "the ?-ed expression is {resolved}, but the enclosing function returns {other}"
                    )),
                );
            }
        }
    }

    /// Report that the record a spread extends with `field` has the
    /// field already: the one wording.
    pub(super) fn extends_existing(&mut self, field: Symbol, span: Span) {
        if !self.lacking.insert((span, format!("field {field}"))) {
            return;
        }
        self.errors.push(
            Diagnostic::error(
                Code::TypeMismatch,
                span,
                format!("cannot extend the record with field '{field}': it has one already"),
            )
            .with_help(format!(
                "a record is extended, never overwritten: update the field with `r.{{ {field}: ... }}`"
            )),
        );
    }

    /// Report that the declared record `record` is given for a row a
    /// definition makes an anonymous record from: once for one record at
    /// one place.
    fn spread_through(&mut self, record: &Type, origin: Origin) {
        let shown = self.show_type(record);
        if !self.lacking.insert((origin.span, shown.clone())) {
            return;
        }
        let through = match origin.callee {
            Some(callee) => format!("'{callee}'"),
            None => "this function".to_string(),
        };
        self.errors.push(Diagnostic::error(
            Code::TypeMismatch,
            origin.span,
            format!(
                "cannot spread a `{shown}` through {through}: convert it where its type is known, `{{...p}}`"
            ),
        ));
    }

    /// Check that no declared record stands for `row`, known now
    /// (`Pred::Anon`).
    fn row_is_anon(&mut self, row: &Type, origin: Origin) {
        match row {
            Type::AnonRecord {
                tail: RowTail::Rigid(r),
                ..
            }
            | Type::Rigid(r) => self.anon_rigid(*r, origin),
            Type::Generic(name, _) if self.tables.records.contains_key(name) => {
                self.spread_through(row, origin)
            }
            _ => {}
        }
    }

    /// Whether a body makes an anonymous record from the row variable
    /// `var` of its function's signature.
    fn is_anon_row(&self, var: TyVar) -> bool {
        self.anon_rows.contains(&var) || self.anon_rows.contains(&self.rigid_rep_var(var))
    }

    /// Whether `var` is a row variable of a function's signature.
    fn is_sig_row(&self, var: TyVar) -> bool {
        self.sig_rows.contains(&var) || self.sig_rows.contains(&self.rigid_rep_var(var))
    }

    /// The fields a body adds over the row variable `var` of its
    /// function's signature.
    fn adds_of(&self, var: TyVar) -> Vec<Symbol> {
        let mut adds = self.row_adds.get(&var).cloned().unwrap_or_default();
        if let Some(more) = self.row_adds.get(&self.rigid_rep_var(var)) {
            adds.extend(
                more.iter()
                    .filter(|f| !adds.contains(f))
                    .copied()
                    .collect::<Vec<_>>(),
            );
        }
        adds
    }

    /// A body makes an anonymous record from a record whose row is the
    /// annotation variable `r`, with the fields `adds` added. For a row
    /// variable of a function's signature, the function's uses owe that
    /// no declared record stands for it and that what does has none of
    /// the fields; no other annotation can ask that. Whether it is news.
    fn mark_sig_row(&mut self, var: TyVar, adds: &[Symbol]) -> bool {
        let rep = self.rigid_rep_var(var);
        let mut news = self.anon_rows.insert(var);
        news |= self.anon_rows.insert(rep);
        for key in [var, rep] {
            let known = self.row_adds.entry(key).or_default();
            for field in adds {
                if !known.contains(field) {
                    known.push(*field);
                    news = true;
                }
            }
        }
        news
    }

    /// See `mark_sig_row`: reports the row of another annotation.
    pub(super) fn anon_rigid(&mut self, r: RigidId, origin: Origin) {
        if self.is_sig_row(r.var) {
            self.mark_sig_row(r.var, &[]);
            return;
        }
        if !self
            .lacking
            .insert((origin.span, format!("row {}", r.name)))
        {
            return;
        }
        self.errors.push(
            Diagnostic::error(
                Code::TypeMismatch,
                origin.span,
                format!(
                    "cannot spread a record of the row `{}` here: a declared record may stand for the row, and a spread makes an anonymous record",
                    r.name
                ),
            )
            .with_help(
                "only a function's own signature keeps declared records from a row its body spreads: do the spread in a function",
            ),
        );
    }

    /// Decide what waited for the bodies of the module's functions:
    /// whether what stands for a row variable of a signature is spread
    /// by the body, and with which fields added.
    pub(super) fn settle_rows(&mut self) {
        let waiting = std::mem::take(&mut self.anon_waiting);
        // What a body does to its row it does to the row of a signature
        // that is given for it.
        loop {
            let mut news = false;
            for (given, row, _) in &waiting {
                if !self.is_anon_row(*given) {
                    continue;
                }
                let (fields, to) = match row {
                    Type::AnonRecord {
                        fields,
                        tail: RowTail::Rigid(r),
                    } => (fields.keys().copied().collect(), r.var),
                    Type::Rigid(r) => (Vec::new(), r.var),
                    _ => continue,
                };
                if self.is_sig_row(to) {
                    let adds: Vec<Symbol> = self
                        .adds_of(*given)
                        .into_iter()
                        .filter(|f| !fields.contains(f))
                        .collect();
                    news |= self.mark_sig_row(to, &adds);
                }
            }
            if !news {
                break;
            }
        }
        for (given, row, origin) in waiting {
            if !self.is_anon_row(given) {
                continue;
            }
            self.row_is_anon(&row, origin);
            if let Type::AnonRecord { fields, .. } = &row {
                for field in self.adds_of(given) {
                    if fields.contains_key(&field) {
                        self.extends_existing(field, origin.span);
                    }
                }
            }
        }
    }

    /// The predicates of a scheme as another module reads them: what a
    /// body does to a row variable of its signature is decided.
    pub(super) fn settled_preds(&self, preds: &[Pred]) -> Vec<Pred> {
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
                other => settled.push(other.clone()),
            }
        }
        settled
    }

    /// The variable a `Goal::Rebuild` over `base` waits for: the type
    /// of the base, or the row of the open record it is.
    pub(super) fn rebuild_waits(&self, base: &Type) -> Option<TyVar> {
        match self.apply(base) {
            Type::Var(v) => Some(v),
            Type::AnonRecord {
                tail: RowTail::Var(v),
                ..
            } => Some(v),
            _ => None,
        }
    }

    /// Decide `Goal::Rebuild`: the type of `base` is decided, or its
    /// scope closes.
    fn rebuild(
        &mut self,
        base: &Type,
        without: &[Symbol],
        with: &[(Symbol, Type)],
        result: &Type,
        rest: bool,
        origin: Origin,
    ) {
        use std::collections::BTreeMap;
        let span = origin.span;
        let mut base_ty =
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(base));
        // A base nothing has decided is a record with a row of its own.
        if matches!(base_ty, Type::Var(_)) {
            let open = Type::AnonRecord {
                fields: BTreeMap::new(),
                tail: RowTail::Var(self.fresh_tyvar_id()),
            };
            self.unify(&base_ty, &open, span);
            base_ty = self.apply(&open);
        }
        let (mut fields, tail): (BTreeMap<Symbol, Type>, RowTail) = match &base_ty {
            // A declared record gives its fields: a field written after
            // the spread is added, or replaces the record's by name.
            Type::Generic(name, args) if self.tables.records.contains_key(name) => (
                self.instantiate_record_fields_with_args(*name, args)
                    .into_iter()
                    .filter(|(n, _)| !with.iter().any(|(written, _)| written == n))
                    .collect(),
                RowTail::Closed,
            ),
            Type::AnonRecord { fields, tail } => (fields.clone(), tail.clone()),
            other => {
                if !rest && !matches!(other, Type::Error | Type::Never) {
                    self.error(
                        Code::TypeMismatch,
                        format!(
                            "spread requires a record base, but '{other}' is not a record type"
                        ),
                        span,
                    );
                }
                let _ = self.unify_types(result, &Type::Error);
                return;
            }
        };
        for name in without {
            fields.remove(name);
        }
        // A record is extended, never overwritten.
        for (name, ty) in with {
            if fields.insert(*name, ty.clone()).is_some() {
                self.extends_existing(*name, span);
            }
        }
        // What is made is anonymous whatever the row turns out to be: a
        // place that wants a declared record is not given one.
        let wanted = self.apply(result);
        if let Type::Generic(name, _) = &wanted
            && self.tables.records.contains_key(name)
            && matches!(tail, RowTail::Var(_))
        {
            let made = Type::AnonRecord { fields, tail };
            let shown = self.written_type(*name);
            self.errors.push(
                Diagnostic::error(
                    Code::TypeMismatch,
                    span,
                    format!(
                        "type mismatch: expected {}, got {}",
                        self.show_type(&wanted),
                        self.show_type(&made)
                    ),
                )
                .with_help(format!(
                    "an anonymous record is not a `{shown}`: write `{shown} {{ ... }}`"
                )),
            );
            return;
        }
        match &tail {
            RowTail::Closed => {}
            RowTail::Rigid(r) => {
                self.anon_rigid(*r, origin);
                for (name, _) in with {
                    self.lacks_rigid(*r, *name);
                }
            }
            RowTail::Var(row) => {
                for (name, _) in with {
                    self.want(
                        Pred::Lacks {
                            row: Type::Var(*row),
                            field: *name,
                        },
                        origin,
                    );
                }
                self.want(
                    Pred::Anon {
                        row: Type::Var(*row),
                        given: None,
                    },
                    origin,
                );
            }
        }
        self.unify(&Type::AnonRecord { fields, tail }, result, span);
    }

    /// Give each `Goal::Rebuild` of the scope just left whose base is
    /// still undecided, and the scope's to generalise, a row of its
    /// own, in the order they were written (a rest binder before the
    /// spread over it).
    pub(super) fn default_rebuilds(&mut self) {
        let mut i = self.closed_mark;
        while i < self.wanted.len() {
            if !self.wanted[i].solved
                && let Goal::Rebuild {
                    base,
                    without,
                    with,
                    result,
                    rest,
                } = self.wanted[i].goal.clone()
                && self
                    .rebuild_waits(&base)
                    .is_none_or(|v| self.tables.vars.is_generalizable(v))
            {
                let origin = self.wanted[i].origin;
                self.wanted[i].solved = true;
                // The variables this makes are the scope's.
                self.reopen_level();
                self.rebuild(&base, &without, &with, &result, rest, origin);
                self.solve_wanted(self.closed_mark);
                self.close_level();
            }
            i += 1;
        }
    }

    /// Check a record type a scheme's use writes over a row a declared
    /// record stands for (`Goal::Listed`).
    fn listed_fields_are(
        &mut self,
        fields: &std::collections::BTreeMap<Symbol, Type>,
        row: &Type,
        origin: Origin,
    ) {
        let Type::Generic(name, args) = row else {
            return;
        };
        if !self.tables.records.contains_key(name) {
            return;
        }
        let declared = self.instantiate_record_fields_with_args(*name, args);
        for (field, ty) in fields {
            let Some((_, declared_ty)) = declared.iter().find(|(n, _)| n == field) else {
                continue;
            };
            if self.unify_types(ty, declared_ty).is_err() {
                let shown = self.show_type(row);
                // (One error for one record at one place: it may be
                // spread there as well.)
                if !self.lacking.insert((origin.span, shown.clone())) {
                    return;
                }
                self.error(
                    Code::TypeMismatch,
                    format!(
                        "a `{shown}` cannot be the record here: its field '{field}' is of type {}, and this use also has the record with a field '{field}' of type {}",
                        self.show_type(&self.apply(declared_ty)),
                        self.show_type(&self.apply(ty)),
                    ),
                    origin.span,
                );
            }
        }
    }

    /// The record over the annotation variable `r` is extended with
    /// `field`: for a row variable of a function's signature, the
    /// function's uses owe that what stands for it has no such field.
    /// (For another annotation's, the spread itself is the error:
    /// `anon_rigid`.)
    fn lacks_rigid(&mut self, r: RigidId, field: Symbol) {
        if self.is_sig_row(r.var) {
            self.mark_sig_row(r.var, &[field]);
        }
    }

    /// Check that the row `row`, known now, has no field `field`
    /// (`Pred::Lacks`). A declared record cannot stand for a row that
    /// is extended at all.
    fn row_lacks(&mut self, row: &Type, field: Symbol, origin: Origin) {
        match row {
            Type::AnonRecord { fields, tail } => {
                if fields.contains_key(&field) {
                    self.extends_existing(field, origin.span);
                    return;
                }
                match tail {
                    RowTail::Var(rest) => self.want(
                        Pred::Lacks {
                            row: Type::Var(*rest),
                            field,
                        },
                        origin,
                    ),
                    RowTail::Rigid(r) => self.lacks_rigid(*r, field),
                    RowTail::Closed => {}
                }
            }
            Type::Rigid(r) => self.lacks_rigid(*r, field),
            Type::Generic(name, _) if self.tables.records.contains_key(name) => {
                self.spread_through(row, origin);
            }
            _ => {}
        }
    }

    /// Check `base.{ field: value }` now that the type of `base` is
    /// known.
    fn update_field(&mut self, base: &Type, field: Symbol, value: &Type, span: Span) {
        let declared: Vec<(Symbol, Type)> = match base {
            Type::Error | Type::Never => return,
            Type::AnonRecord { fields, tail } => {
                if !fields.contains_key(&field) && matches!(tail, RowTail::Var(_)) {
                    let extended = Type::AnonRecord {
                        fields: std::collections::BTreeMap::from([(field, value.clone())]),
                        tail: RowTail::Var(self.fresh_tyvar_id()),
                    };
                    self.unify(base, &extended, span);
                    return;
                }
                fields.iter().map(|(k, v)| (*k, v.clone())).collect()
            }
            Type::Generic(name, args) if self.tables.records.contains_key(name) => {
                let fields = self.tables.records[name].fields.clone();
                match self.tables.record_param_var_ids.get(name).cloned() {
                    Some(params) if params.len() == args.len() => {
                        let mapping: HashMap<TyVar, Type> =
                            params.iter().copied().zip(args.iter().cloned()).collect();
                        fields
                            .iter()
                            .map(|(n, t)| (*n, substitute_vars(t, &mapping)))
                            .collect()
                    }
                    _ => fields,
                }
            }
            other => {
                self.error(
                    Code::TypeMismatch,
                    format!(
                        "record update requires a record base, but '{}' is not a record type",
                        self.show_type(other)
                    ),
                    span,
                );
                return;
            }
        };
        match declared.iter().find(|(n, _)| *n == field) {
            Some((_, declared_ty)) => self.unify(value, declared_ty, span),
            None => {
                let base = format!("unknown field '{field}' in {}", self.show_type(base));
                self.error_help(
                    Code::UnknownField,
                    format_record_field_suggestion(base, field, &declared),
                    span,
                );
            }
        }
    }

    /// Decide each `e?` of the scope just left whose operand is still
    /// unknown and is the scope's to generalise, where the return type
    /// of the function it is in is known by now: `?` unwraps a Result
    /// with the function's error type, or an Option.
    pub(super) fn decide_tries(&mut self) {
        let mine: Vec<usize> = (self.closed_mark..self.wanted.len())
            .filter(|&i| {
                !self.wanted[i].solved
                    && matches!(self.wanted[i].goal, Goal::Try { .. })
                    && matches!(
                        self.apply(self.wanted[i].goal.waits_on()),
                        Type::Var(var) if self.tables.vars.is_generalizable(var)
                    )
            })
            .collect();
        self.reopen_level();
        for i in mine {
            let Goal::Try {
                operand,
                ret: Some(ret),
                ..
            } = self.wanted[i].goal.clone()
            else {
                continue;
            };
            let wrapper = match self.apply(&ret) {
                Type::Generic(name, args) if name.is_builtin("Result") && args.len() == 2 => {
                    Type::builtin("Result", vec![self.fresh_var(), args[1].clone()])
                }
                Type::Generic(name, args) if name.is_builtin("Option") && args.len() == 1 => {
                    Type::option(self.fresh_var())
                }
                _ => continue,
            };
            let _ = self.unify_types(&operand, &wrapper);
        }
        self.solve_wanted(self.closed_mark);
        self.close_level();
    }

    /// Decide each call `x.m(args)` of the scope just left whose
    /// receiver is still unknown and is the scope's to generalise. When
    /// exactly one trait the module sees declares a method `m`, the call
    /// is that method's and `x` is bounded by the trait
    /// (`fn g(x) { x.greet() }` is `a -> String where a: Greet`). When
    /// none does, `x` is a record with a function in its field `m`. When
    /// several do, the receiver needs an annotation.
    pub(super) fn default_selects(&mut self, wait_if_several: bool) {
        let mine: Vec<usize> = (self.closed_mark..self.wanted.len())
            .filter(|&i| {
                !self.wanted[i].solved
                    && matches!(self.wanted[i].goal, Goal::Select { .. })
                    && matches!(
                        self.apply(self.wanted[i].goal.waits_on()),
                        Type::Var(var) if self.tables.vars.is_generalizable(var)
                    )
            })
            .collect();
        // The variables this makes are the scope's.
        self.reopen_level();
        for i in mine {
            let origin = self.wanted[i].origin;
            let Goal::Select {
                recv,
                name,
                args,
                result,
            } = self.wanted[i].goal.clone()
            else {
                continue;
            };
            // (An earlier one of these may have decided this receiver.)
            if matches!(self.apply(&recv), Type::Var(_))
                && !(wait_if_several && self.traits_declaring(name).len() > 1)
            {
                self.wanted[i].solved = true;
                let call_ty = Type::Fun(args, Box::new(result));
                self.default_select(&recv, name, call_ty, origin.span);
            }
        }
        self.solve_wanted(self.closed_mark);
        self.close_level();
    }

    /// The traits in sight that declare a method `name`, in the order
    /// of their names.
    fn traits_declaring(&self, name: Symbol) -> Vec<TraitKey> {
        let mut traits: Vec<TraitKey> = self
            .tables
            .traits
            .iter()
            .filter(|(_, info)| info.methods.iter().any(|(n, _)| *n == name))
            .map(|(t, _)| *t)
            .filter(|t| self.sees_trait(*t))
            .collect();
        traits.sort_by_key(|t| self.show_trait(*t));
        traits
    }

    fn default_select(&mut self, recv: &Type, name: Symbol, call_ty: Type, span: Span) {
        let traits = self.traits_declaring(name);
        match traits.as_slice() {
            [] => {
                let row = Type::AnonRecord {
                    fields: std::collections::BTreeMap::from([(name, call_ty)]),
                    tail: RowTail::Var(self.fresh_tyvar_id()),
                };
                self.unify(recv, &row, span);
            }
            [tr] => {
                let info = self.tables.traits[tr].clone();
                let (_, method_ty) = info
                    .methods
                    .iter()
                    .find(|(n, _)| *n == name)
                    .expect("the trait declares the method");
                // A method without `self` is not what a value's `.{name}`
                // calls, and says nothing of the receiver.
                let on_type = format!("`SomeType.{name}()`");
                if self.takes_no_self(method_ty, name, &on_type, span) {
                    return;
                }
                // The trait's parameters are what the receiver's impl
                // will say; the method's own variables are new.
                let mut mapping: HashMap<TyVar, Type> = HashMap::new();
                mapping.insert(info.self_var, recv.clone());
                let params: Vec<Type> = info
                    .param_var_ids
                    .iter()
                    .map(|v| {
                        let fresh = self.fresh_var();
                        mapping.insert(*v, fresh.clone());
                        fresh
                    })
                    .collect();
                let ty = substitute_vars(method_ty, &mapping);
                let preds: Vec<Pred> = info
                    .method_bounds
                    .get(&name)
                    .into_iter()
                    .flatten()
                    .map(|pred| pred.substitute(&mapping))
                    .collect();
                let own: Vec<TyVar> = free_vars_in(method_ty)
                    .into_iter()
                    .filter(|v| !mapping.contains_key(v))
                    .collect();
                let scheme = Scheme {
                    vars: own,
                    preds,
                    ty,
                    optional_last_param: false,
                };
                let origin = Origin {
                    span,
                    callee: Some(name),
                    op: None,
                };
                self.want(
                    Pred::Trait {
                        tr: *tr,
                        args: params,
                        subject: recv.clone(),
                    },
                    origin,
                );
                let method_ty = self.instantiate_method(&scheme, name, span);
                self.deferred_method_traits.insert(span, *tr);
                self.unify_deferred_method(&call_ty, &method_ty, span);
            }
            several => {
                self.ambiguous_method(name, "a value whose type is not known", several, span);
            }
        }
    }

    /// Report that `method`, called on `on` (`type 'Int'`, `a value
    /// whose type is not known`), is a method of each of `traits`: the
    /// one wording, the traits by their names in order, each with the
    /// module that declares it.
    pub(super) fn ambiguous_method(
        &mut self,
        method: Symbol,
        on: &str,
        traits: &[TraitKey],
        span: Span,
    ) {
        let mut shown: Vec<String> = traits
            .iter()
            .map(|t| self.show_trait_in_module(*t))
            .collect();
        shown.sort();
        self.errors.push(
            Diagnostic::error(
                Code::AmbiguousMethod,
                span,
                format!(
                    "ambiguous method '{method}' on {on}: provided by traits {}",
                    shown.join(", ")
                ),
            )
            .with_help(
                "say which is meant: annotate the receiver's type, or bound it with a `where` clause",
            ),
        );
    }

    /// The bound `tr(args)` as a message shows it: `Greet`, `Conv(Int)`.
    pub(super) fn show_bound(&self, tr: TraitKey, args: &[Type]) -> String {
        let name = self.show_trait(tr);
        if args.is_empty() {
            return name;
        }
        let args: Vec<String> = args
            .iter()
            .map(|t| self.show_type(&self.apply(t)))
            .collect();
        format!("{name}({})", args.join(", "))
    }

    /// Where the scheme being instantiated is used: the use that named
    /// itself (`named_use`: a call, a name), else the expression being
    /// checked.
    pub(super) fn use_origin(&self) -> Origin {
        self.named_use.unwrap_or(Origin {
            span: self.at,
            callee: None,
            op: None,
        })
    }

    // ── Type name for trait impl matching ────────────────────────────

    /// The type a resolved Type's impls are keyed by. Returns `None` if
    /// the type is unresolved (still a type variable) or has no head.
    ///
    /// Phase B: routes through `crate::types::canonical::canonicalize` so
    /// that `Range(T)` collapses to `List(T)` before name lookup. The
    /// dedicated Range arm is therefore no longer needed (it became
    /// unreachable once the input was canonicalised). This is the single
    /// source of truth for the dispatch-name oracle on the typechecker
    /// side; the VM and compiler will reach the same conclusion via
    /// `canonical_name` in phase C.
    pub(super) fn type_name_for_impl(&self, ty: &Type) -> Option<TypeRef> {
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, ty);
        match &ty {
            // Anonymous (structural) records are CONCRETE types — they
            // carry a definite shape — but no user impl can ever target
            // them (impls bind to nominal heads only). Round 73 B2:
            // returning `None` here let `verify_trait_obligation` and
            // its callers in inference.rs treat AnonRecord receivers as
            // "still polymorphic, defer", which silently bypassed
            // user-trait `where` constraints — a soundness hole.
            // Returning the builtin `<anon>` type makes the existing
            // `trait_impl_set.contains(...)` check fire the "does not
            // implement trait 'X'" diagnostic (which writes the record's
            // type); no program can name that type, so no impl targets
            // it.
            Type::AnonRecord { .. } => Some(TypeRef::builtin(crate::defs::ANON_RECORD)),
            // Function values resolve to `Fn`, and `Unit` is `Unit`, so
            // `where a: Trait` constraints route into the same impl
            // table the compiler keys impl methods by and
            // `dispatch_type_for_value` returns at runtime.
            _ => head_of(&ty),
        }
    }

    /// Return the positional type arguments of a (concrete) type. Mirrors
    /// the inverse of `register_trait_impl`'s self_type construction:
    /// `Type::Generic(_, args)` yields `args`; the parameterized builtin
    /// containers (List, Set, Channel, Map) yield their element types in
    /// declaration order; tuples yield their elements and functions their
    /// params-then-return. Anything else (Int, String, Record without type
    /// params, etc.) has no positional args. Used by `verify_trait_obligation`
    /// to walk into an impl's where-clause obligations.
    ///
    /// Phase B: canonicalise the input first so a Range receiver supplies
    /// its element type via the List arm rather than a dedicated Range arm.
    pub(super) fn type_args_of(&self, ty: &Type) -> Vec<Type> {
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, ty);
        match &ty {
            Type::Generic(_, args) => args.clone(),
            Type::List(inner) | Type::Set(inner) | Type::Channel(inner) => {
                vec![(**inner).clone()]
            }
            Type::Map(k, v) => vec![(**k).clone(), (**v).clone()],
            // Tuple-/Fn-shaped alias impls (`type P2 = (Int, Int)`;
            // `type IntOp = Fn(Int) -> Int`) register under the synthetic
            // heads `"Tuple"`/`"Fn"` with the expanded structural self
            // type. Their positional args are the element types (params
            // plus return for `Fn`) so `verify_trait_obligation`'s
            // self-type-args comparison sees them. Pre-fix both shapes
            // fell through to `Vec::new()`: obligated-vs-impl args
            // compared as empty-vs-empty and ANY tuple/function satisfied
            // a bound whose only impl targeted a concrete alias shape
            // (round-102 hole, same class as the head-key-only bug it
            // fixed). Differing arities land on the caller's equal-length
            // conservative-skip guard, so the bare `trait T for Tuple`
            // wildcard (`Generic("Tuple", [])`, zero args) keeps matching
            // every tuple, and mismatched-arity functions defer to the
            // direct-dispatch unify.
            Type::Tuple(elems) => elems.clone(),
            Type::Fun(params, ret) => {
                let mut args = params.clone();
                args.push((**ret).clone());
                args
            }
            _ => Vec::new(),
        }
    }

    /// Check that `ty`, whose head is known, implements `trait_name` at
    /// `bound_trait_args`, for the use `origin`: the one answer to "does
    /// this type implement this trait". Reports
    /// `type 'X' does not implement trait 'Y'` if not.
    ///
    /// A trait has at most one impl for a type's head, so the impl that
    /// answers is known from the head alone, and what it says decides
    /// what is still unknown of the subject and of the bound: the impl's
    /// self type and trait arguments, with new variables for the impl's
    /// own, are unified with the subject's and the bound's (`s(3, y)`
    /// for `where a: Conv(b)` and `trait Conv(String) for Int` makes `y`
    /// a `String`). What the impl's header asks of its variables is owed
    /// by the same use (`want`): a part of the subject still unknown
    /// waits, and is generalised with the definition or decided later.
    pub(super) fn verify_trait_obligation(
        &mut self,
        trait_name: TraitKey,
        bound_trait_args: &[Type],
        ty: &Type,
        origin: Origin,
    ) {
        let span = origin.span;
        let resolved = self.apply(ty);
        if matches!(resolved, Type::Error | Type::Never) {
            return;
        }
        // An annotation variable implements what its bounds say.
        if let Type::Rigid(r) = resolved {
            self.require_declared_bound(r, trait_name, bound_trait_args, origin);
            return;
        }
        let Some(type_name) = self.type_name_for_impl(&resolved) else {
            // Still unknown: the caller lets it wait (`want`).
            return;
        };
        // Equal, Compare and Hash, and Display where no impl is written,
        // are what the type's structure says.
        if self.by_structure(trait_name, type_name) {
            let mut walk = Walk::default();
            match self.structure_gap(trait_name, &resolved, &mut walk, 0) {
                Some(gap) => {
                    // An operator's own operand, of a type the operator
                    // is not for: the operator's message.
                    if let Some(op) = origin.op
                        && gap.is_whole()
                    {
                        self.error(
                            Code::UnsupportedOperation,
                            operator_message(&resolve(trait_name.name), op, &resolved),
                            span,
                        );
                        return;
                    }
                    let message = self.gap_message(trait_name, &resolved, &gap);
                    self.error(Code::MissingTraitImpl, message, span);
                }
                None => {
                    // A part still unknown, or an annotation variable,
                    // owes the trait in turn; a part with a written
                    // `Display` impl is that impl's to answer.
                    for part in walk.open {
                        self.want(
                            Pred::Trait {
                                tr: trait_name,
                                args: Vec::new(),
                                subject: part,
                            },
                            origin,
                        );
                    }
                    for part in walk.written {
                        self.verify_impl(trait_name, &[], &part, origin);
                    }
                }
            }
            return;
        }
        self.verify_impl(trait_name, bound_trait_args, &resolved, origin);
    }

    /// Whether `trait_name` holds of a type with the head `head` by the
    /// type's structure: the sealed `Equal`, `Compare` and `Hash`
    /// always; `Display` unless an impl of it is written for the head.
    pub(super) fn by_structure(&self, trait_name: TraitKey, head: TypeRef) -> bool {
        if ["Equal", "Compare", "Hash"]
            .iter()
            .any(|name| trait_name.is_builtin(name))
        {
            return true;
        }
        // A `Display` impl is written for the head: in this module
        // (`display_written`, known before its impls are entered), in
        // another one or in an earlier REPL cell (its method is in
        // `trait_methods`), or by silt itself (a builtin error enum's,
        // in the method table). A method `display` of another trait is
        // no `Display` impl.
        let display = intern("display");
        trait_name.is_builtin("Display")
            && !self.display_written.contains(&head)
            && self
                .tables
                .trait_methods
                .get(&(head, display, trait_name))
                .is_none_or(|entry| entry.structural)
            && self
                .tables
                .method_table
                .get(&(head, display))
                .is_none_or(|entry| entry.structural || entry.trait_name != Some(trait_name))
    }

    /// What a message says of a type that lacks the structural trait
    /// `tr`: the type, and the way down to the part that lacks it
    /// (`type 'Box(H)' does not implement trait 'Display': variant 'Box'
    /// payload #1 > field 'f' is of type 'Fn(Int) -> Int', which does
    /// not`).
    pub(super) fn gap_message(&self, tr: TraitKey, ty: &Type, gap: &Gap) -> String {
        let tr = self.show_trait(tr);
        if gap.why == Some(TOO_DEEP) {
            return format!(
                "type '{}' cannot be shown to implement trait '{tr}': {TOO_DEEP}",
                self.show_type(&self.apply(ty))
            );
        }
        match (gap.path.is_empty(), gap.why) {
            (true, None) => format!("type '{}' does not implement trait '{tr}'", gap.head),
            (true, Some(why)) => {
                format!("type '{}' does not implement trait '{tr}': {why}", gap.full)
            }
            (false, why) => format!(
                "type '{}' does not implement trait '{tr}': {} is of type '{}', {}",
                self.show_type(&self.apply(ty)),
                gap.path.join(" > "),
                gap.full,
                why.unwrap_or("which does not")
            ),
        }
    }

    /// Why `ty` does not have the structural trait `tr`, if it does not:
    /// a function has none of them, a channel only `Equal`; a container,
    /// a tuple, a record or an enum has one when each of its parts does
    /// (and its head has it at all: a map has no `Compare`). The parts
    /// whose type is still unknown, or an annotation variable, are added
    /// to `walk.open`: the trait holds if it holds of them. A type being
    /// looked at is taken to hold where it comes up again inside itself
    /// (`walk.seen`), so a recursive type holds if its other parts do. A
    /// type that recurs at ever larger arguments
    /// (`type N(a) { Z, S(N(List(a))) }`) cannot be decided this way:
    /// past `STRUCTURE_DEPTH` levels it is reported as not verified.
    pub(super) fn structure_gap(
        &self,
        tr: TraitKey,
        ty: &Type,
        walk: &mut Walk,
        depth: usize,
    ) -> Option<Gap> {
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        let leaf = |this: &Self, head: &str, why: Option<&'static str>| {
            Some(Gap {
                path: Vec::new(),
                head: head.to_string(),
                full: this.show_type(&ty),
                why,
            })
        };
        if depth > STRUCTURE_DEPTH {
            return leaf(self, "", Some(TOO_DEEP));
        }
        let stamped = |this: &Self, head: TypeRef| {
            let canon = canonical_head(&this.tables.resolver, head);
            this.tables.trait_impl_set.contains(&(tr, canon))
        };
        // The first part that lacks the trait, with the way to it.
        let first = |this: &Self, walk: &mut Walk, parts: Vec<(String, Type)>| {
            parts.into_iter().find_map(|(what, part)| {
                this.structure_gap(tr, &part, walk, depth + 1)
                    .map(|mut gap| {
                        if gap.why != Some(TOO_DEEP) {
                            gap.path.insert(0, what);
                        }
                        gap
                    })
            })
        };
        match &ty {
            Type::Error | Type::Never | Type::AssocProj { .. } => None,
            Type::Var(_) | Type::Rigid(_) => {
                if !walk.open.contains(&ty) {
                    walk.open.push(ty.clone());
                }
                None
            }
            Type::Fun(..) => leaf(self, "Fn", None),
            Type::Channel(_) if tr.is_builtin("Equal") => None,
            Type::List(_)
            | Type::Range(_)
            | Type::Set(_)
            | Type::Map(..)
            | Type::Tuple(_)
            | Type::Channel(_)
            | Type::Int
            | Type::Float
            | Type::Bool
            | Type::String
            | Type::Unit => {
                let head = self.type_name_for_impl(&ty).expect("the type has a head");
                if !stamped(self, head) {
                    return leaf(self, &resolve(head.name), None);
                }
                let parts: Vec<(String, Type)> = match &ty {
                    Type::List(t) | Type::Range(t) | Type::Set(t) => {
                        vec![("element".to_string(), (**t).clone())]
                    }
                    Type::Map(k, v) => vec![
                        ("key".to_string(), (**k).clone()),
                        ("value".to_string(), (**v).clone()),
                    ],
                    Type::Tuple(ts) => ts
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (format!("part #{}", i + 1), t.clone()))
                        .collect(),
                    _ => Vec::new(),
                };
                first(self, walk, parts)
            }
            Type::AnonRecord { fields, tail } => {
                if tr.is_builtin("Compare") {
                    return leaf(self, &self.show_type(&ty), None);
                }
                // The fields behind an open row have the trait when the
                // row does: a row variable owes it like a type variable
                // (and stands for the record's other fields, or for the
                // nominal record itself, at a use).
                let rest = match tail {
                    RowTail::Closed => None,
                    RowTail::Var(v) => Some(Type::Var(*v)),
                    RowTail::Rigid(r) => Some(Type::Rigid(*r)),
                };
                if let Some(rest) = rest
                    && !walk.open.contains(&rest)
                {
                    walk.open.push(rest);
                }
                let parts = fields
                    .iter()
                    .map(|(n, t)| (format!("field '{n}'"), t.clone()))
                    .collect();
                first(self, walk, parts)
            }
            Type::Generic(name, _) => {
                let head = canonical_head(&self.tables.resolver, *name);
                // A written `Display` impl answers for its type.
                if !self.by_structure(tr, head) {
                    walk.written.push(ty.clone());
                    return None;
                }
                // A type with no fields or variants to look at (a
                // handle, `Bytes`) has what the builtins say it has.
                let opaque = !self.tables.records.contains_key(name)
                    && !self.tables.enums.contains_key(name);
                if opaque {
                    return match stamped(self, head) {
                        true => None,
                        false => leaf(self, &self.show_type(&Type::Generic(*name, vec![])), None),
                    };
                }
                let args = self.type_args_of(&ty);
                if walk.seen.iter().any(|(h, a)| *h == head && *a == args) {
                    return None;
                }
                walk.seen.push((head, args.clone()));
                // The type's parts, at its arguments.
                let parts: Vec<(String, Type)> = if let Some(info) = self.tables.records.get(name) {
                    let mapping: HashMap<TyVar, Type> = self
                        .tables
                        .record_param_var_ids
                        .get(name)
                        .filter(|ids| ids.len() == args.len())
                        .map(|ids| ids.iter().copied().zip(args.iter().cloned()).collect())
                        .unwrap_or_default();
                    info.fields
                        .iter()
                        .map(|(n, t)| (format!("field '{n}'"), substitute_vars(t, &mapping)))
                        .collect()
                } else if let Some(info) = self.tables.enums.get(name) {
                    let mapping: HashMap<TyVar, Type> = if info.param_var_ids.len() == args.len() {
                        info.param_var_ids
                            .iter()
                            .copied()
                            .zip(args.iter().cloned())
                            .collect()
                    } else {
                        HashMap::new()
                    };
                    info.variants
                        .iter()
                        .flat_map(|v| {
                            v.field_types.iter().enumerate().map(|(i, t)| {
                                (
                                    format!("variant '{}' payload #{}", v.name, i + 1),
                                    substitute_vars(t, &mapping),
                                )
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                first(self, walk, parts)
            }
        }
    }

    /// Check that `ty` implements `trait_name` by an impl: the one the
    /// type's head has.
    fn verify_impl(
        &mut self,
        trait_name: TraitKey,
        bound_trait_args: &[Type],
        resolved: &Type,
        origin: Origin,
    ) {
        let span = origin.span;
        let resolved = resolved.clone();
        let Some(type_name) = self.type_name_for_impl(&resolved) else {
            return;
        };
        if !self
            .tables
            .trait_impl_set
            .contains(&(trait_name, type_name))
        {
            // An operator's own operand: the operator's message.
            if let Some(op) = origin.op {
                self.error(
                    Code::UnsupportedOperation,
                    operator_message(&resolve(trait_name.name), op, &resolved),
                    span,
                );
                return;
            }
            // (An anonymous record has no name: the message writes its
            // type, and says why it has no impl.)
            let message = match &resolved {
                Type::AnonRecord { .. } => format!(
                    "type '{}' does not implement trait '{}': an anonymous record has the traits its fields give it, and no impl is written for one",
                    self.show_type(&resolved),
                    self.show_bound(trait_name, bound_trait_args)
                ),
                _ => format!(
                    "type '{}' does not implement trait '{}'",
                    self.show_type(&Type::Generic(type_name, vec![])),
                    self.show_bound(trait_name, bound_trait_args)
                ),
            };
            self.error(Code::MissingTraitImpl, message, span);
            return;
        }
        // The impl, with new variables for its own: its self type, its
        // trait arguments and what its header asks.
        let impl_self = self
            .tables
            .impl_self_types
            .get(&(trait_name, type_name))
            .cloned();
        let impl_trait_args = self
            .tables
            .impl_trait_args
            .get(&(trait_name, type_name))
            .cloned()
            .unwrap_or_default();
        let header = self
            .tables
            .impl_preds
            .get(&(trait_name, type_name))
            .cloned()
            .unwrap_or_default();
        let mut own: Vec<TyVar> = impl_self.iter().flat_map(free_vars_in).collect();
        own.extend(impl_trait_args.iter().flat_map(free_vars_in));
        for pred in &header {
            own.extend(
                pred.args()
                    .iter()
                    .chain([pred.subject()])
                    .flat_map(free_vars_in),
            );
        }
        let mut fresh: HashMap<TyVar, Type> = HashMap::new();
        for v in own {
            fresh.entry(v).or_insert_with(|| self.fresh_var());
        }
        // Head-key membership alone is not enough: an alias-expanded impl
        // can carry CONCRETE self-type args (`type Bytes2 = List(Int)`;
        // `trait Total for Bytes2` registers under head "List" with
        // self_type `List(Int)`). The subject's positional args are
        // compared with the impl self type's: a concrete mismatch is
        // reported; a repeated binder of a NON-LINEAR impl self type
        // (`type Pair(a) = (a, a)`) must see equal types
        // (`impl_self_args_consistent`). A length mismatch means the two
        // sides describe differently shaped representations of the same
        // head (a `Record` receiver against a `Generic` impl form, the
        // bare `Tuple`/`Fn` wildcard): skipped. Impls without a stored
        // self type (the stamps of the structural traits) skip the
        // check.
        let obligated_args = self.type_args_of(&resolved);
        // Whether the impl's variables are the subject's parts by now.
        let mut linked = false;
        if let Some(impl_self) = &impl_self {
            let impl_self = substitute_vars(impl_self, &fresh);
            let impl_args = self.type_args_of(&impl_self);
            if obligated_args.len() == impl_args.len() {
                if !self.impl_self_args_consistent(&obligated_args, &impl_args) {
                    let (obligated, only) = self.show_apart(&resolved, &impl_self);
                    self.error(
                        Code::MissingTraitImpl,
                        format!(
                            "type '{}' does not implement trait '{}': the only impl is for '{}'",
                            obligated,
                            self.show_bound(trait_name, bound_trait_args),
                            only
                        ),
                        span,
                    );
                    return;
                }
                // The only impl is this one: what it says of the
                // subject's parts holds.
                for (ob, im) in obligated_args.iter().zip(&impl_args) {
                    let _ = self.unify_types(ob, im);
                }
                linked = true;
            }
        }
        // The bound's trait arguments are the impl's
        // (`where a: TryInto(Int)` against `trait TryInto(Float) for
        // String` is a mismatch). A bare bound (supertrait chains) says
        // nothing of them.
        if !bound_trait_args.is_empty() && impl_trait_args.len() == bound_trait_args.len() {
            let impl_trait_args: Vec<Type> = impl_trait_args
                .iter()
                .map(|t| substitute_vars(t, &fresh))
                .collect();
            for (bound_arg, impl_arg) in bound_trait_args.iter().zip(&impl_trait_args) {
                let b = self.apply(bound_arg);
                let i = self.apply(impl_arg);
                if !self.trait_arg_compatible(&b, &i) {
                    self.error(
                        Code::MissingTraitImpl,
                        format!(
                            "type '{}' does not implement trait '{}': the matched impl is '{}'",
                            self.show_type(&Type::Generic(type_name, vec![])),
                            self.show_bound(trait_name, bound_trait_args),
                            self.show_bound(trait_name, &impl_trait_args),
                        ),
                        span,
                    );
                    return;
                }
            }
            for (bound_arg, impl_arg) in bound_trait_args.iter().zip(&impl_trait_args) {
                let _ = self.unify_types(bound_arg, impl_arg);
            }
        }
        // What the impl's header asks of its variables, which are the
        // subject's parts now, is owed by the same use. (An impl whose
        // self type is another shape than the subject's, a bare `Tuple`
        // or `Fn` target, says nothing of the parts.)
        if linked {
            for pred in header {
                self.want(pred.substitute(&fresh), origin);
            }
        }
    }

    /// Check, where an impl is declared, that `ty` implements
    /// `trait_name` at `args`, as far as the declaration says what `ty`
    /// is: what would be owed for one of the impl's own variables is the
    /// impl's uses' to owe, not the declaration's.
    pub(super) fn verify_declared(
        &mut self,
        trait_name: TraitKey,
        args: &[Type],
        ty: &Type,
        span: Span,
    ) {
        let owed = self.wanted.len();
        self.verify_trait_obligation(
            trait_name,
            args,
            ty,
            Origin {
                span,
                callee: None,
                op: None,
            },
        );
        self.wanted.truncate(owed);
    }

    /// Consistency-tracking variant of the per-slot compatibility walk
    /// for `verify_trait_obligation`'s self-type-args check.
    /// Round 104: the stateless per-pair `trait_arg_compatible` lost the
    /// cross-slot linkage of NON-LINEAR impl self types — a repeated
    /// binder, reachable only via alias expansion (the parser rejects
    /// duplicate binders in direct impl targets): `Pair(a) = (a, a)` →
    /// `(Var a', Var a')`, ditto `Square(a) = Map(a, a)`. An obligated
    /// `(Int, Fn)` deferred each slot against `Var a'` alone, satisfied
    /// the bound, and died at runtime. Here a binding map is
    /// threaded across ALL slots: the first obligated type an impl-side
    /// `Var` meets binds it; every re-encounter must be compatible with
    /// that binding. Obligated-side `Var`s still defer (inference may
    /// resolve them later), and concrete/concrete pairs walk structurally
    /// exactly as before.
    ///
    /// Enforcing the linkage here also keeps the first-occurrence-only
    /// where-clause obligation index (the `.position(..)` over
    /// `expanded_self_args` in `register_trait_impl`) sound: once every
    /// slot sharing a binder is forced equal, checking the bound at the
    /// binder's first slot covers all of them.
    fn impl_self_args_consistent(&self, obligated: &[Type], impl_args: &[Type]) -> bool {
        let mut bindings: HashMap<TyVar, Type> = HashMap::new();
        obligated.iter().zip(impl_args.iter()).all(|(ob, im)| {
            let ob = crate::types::canonical::canonicalize(&self.tables.resolver, ob);
            let im = crate::types::canonical::canonicalize(&self.tables.resolver, im);
            Self::impl_arg_matches_canon(&ob, &im, &mut bindings)
        })
    }

    /// One-sided structural matcher threading `bindings` for
    /// `impl_self_args_consistent`. Mirrors `trait_arg_compatible_canon`'s
    /// recursive arms; both inputs are pre-canonicalised (deep), so the
    /// recursion never re-canonicalises. Leaf pairs with no impl-side
    /// binder to thread (scalars, nominal `Record`/`Generic` head-name
    /// comparisons without args, `Never`, mismatches) delegate to the
    /// existing stateless walk via the catch-all.
    fn impl_arg_matches_canon(ob: &Type, im: &Type, bindings: &mut HashMap<TyVar, Type>) -> bool {
        match (ob, im) {
            (Type::Error, _) | (_, Type::Error) => true,
            // Obligated side unresolved: defer, as before. (Deliberately
            // no binding — a caller-side tyvar may resolve after this
            // check; rejecting on it would be a false negative.)
            (Type::Var(_), _) => true,
            // Impl-side binder: bind on first encounter, require
            // compatibility with the binding on re-encounter.
            // `trait_arg_compatible_canon` is the right comparator —
            // a nested `Var` on either side keeps deferring
            // conservatively, while concrete mismatches reject.
            (_, Type::Var(tv)) => match bindings.get(tv) {
                Some(bound) => Self::trait_arg_compatible_canon(bound, ob),
                None => {
                    bindings.insert(*tv, ob.clone());
                    true
                }
            },
            (Type::List(x), Type::List(y))
            | (Type::Set(x), Type::Set(y))
            | (Type::Channel(x), Type::Channel(y)) => Self::impl_arg_matches_canon(x, y, bindings),
            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                Self::impl_arg_matches_canon(k1, k2, bindings)
                    && Self::impl_arg_matches_canon(v1, v2, bindings)
            }
            (Type::Tuple(xs), Type::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| Self::impl_arg_matches_canon(x, y, bindings))
            }
            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                p1.len() == p2.len()
                    && p1
                        .iter()
                        .zip(p2.iter())
                        .all(|(x, y)| Self::impl_arg_matches_canon(x, y, bindings))
                    && Self::impl_arg_matches_canon(r1, r2, bindings)
            }
            (Type::Generic(n1, a1), Type::Generic(n2, a2)) => {
                n1 == n2
                    && a1.len() == a2.len()
                    && a1
                        .iter()
                        .zip(a2.iter())
                        .all(|(x, y)| Self::impl_arg_matches_canon(x, y, bindings))
            }
            (
                Type::AnonRecord {
                    fields: f1,
                    tail: t1,
                },
                Type::AnonRecord {
                    fields: f2,
                    tail: t2,
                },
            ) => {
                t1 == t2
                    && f1.len() == f2.len()
                    && f1.iter().zip(f2.iter()).all(|((k1, v1), (k2, v2))| {
                        k1 == k2 && Self::impl_arg_matches_canon(v1, v2, bindings)
                    })
            }
            _ => Self::trait_arg_compatible_canon(ob, im),
        }
    }

    /// Side-effect-free compatibility check between a bound's trait-arg
    /// and an impl's trait-arg. Returns true when the pair could unify:
    /// either side is a type variable (defer), or both are concrete and
    /// structurally equal. Used by `verify_trait_obligation` to reject
    /// `where a: TryInto(Int)` when only `TryInto(Float) for ...` exists.
    ///
    /// Phase B: canonicalise both sides at entry. The recursive walk
    /// then never sees `Type::Range`; the dedicated `(Range, Range)`
    /// pair-arm is unreachable and removed.
    pub(super) fn trait_arg_compatible(&self, a: &Type, b: &Type) -> bool {
        let a = crate::types::canonical::canonicalize(&self.tables.resolver, a);
        let b = crate::types::canonical::canonicalize(&self.tables.resolver, b);
        Self::trait_arg_compatible_canon(&a, &b)
    }

    fn trait_arg_compatible_canon(a: &Type, b: &Type) -> bool {
        match (a, b) {
            (Type::Error, _) | (_, Type::Error) => true,
            (Type::Var(_), _) | (_, Type::Var(_)) => true,
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::Bool, Type::Bool)
            | (Type::String, Type::String)
            | (Type::Unit, Type::Unit) => true,
            (Type::List(x), Type::List(y))
            | (Type::Set(x), Type::Set(y))
            | (Type::Channel(x), Type::Channel(y)) => Self::trait_arg_compatible_canon(x, y),
            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                Self::trait_arg_compatible_canon(k1, k2) && Self::trait_arg_compatible_canon(v1, v2)
            }
            (Type::Tuple(xs), Type::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| Self::trait_arg_compatible_canon(x, y))
            }
            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                p1.len() == p2.len()
                    && p1
                        .iter()
                        .zip(p2.iter())
                        .all(|(x, y)| Self::trait_arg_compatible_canon(x, y))
                    && Self::trait_arg_compatible_canon(r1, r2)
            }
            (Type::Generic(n1, a1), Type::Generic(n2, a2)) => {
                n1 == n2
                    && a1.len() == a2.len()
                    && a1
                        .iter()
                        .zip(a2.iter())
                        .all(|(x, y)| Self::trait_arg_compatible_canon(x, y))
            }
            // Round 79 TS-B1: structurally compare anonymous records so
            // bounds like `where a: Convert({a: Int, b: String})` accept
            // an impl whose trait-arg is the byte-equal record. Without
            // this arm two equal `AnonRecord` values fell through to the
            // `_ => false` catch-all and the obligation was rejected.
            (
                Type::AnonRecord {
                    fields: f1,
                    tail: t1,
                },
                Type::AnonRecord {
                    fields: f2,
                    tail: t2,
                },
            ) => {
                t1 == t2
                    && f1.len() == f2.len()
                    && f1.iter().zip(f2.iter()).all(|((k1, v1), (k2, v2))| {
                        k1 == k2 && Self::trait_arg_compatible_canon(v1, v2)
                    })
            }
            // `Never` is uninhabited; only equal to itself. Symmetry arm
            // for completeness — entry-point `canonicalize` doesn't
            // collapse `Never` to anything else.
            (Type::Never, Type::Never) => true,
            // No `(AssocProj, _)` arm is needed: the entry-point
            // `canonicalize` resolves projections before this walk runs,
            // so the recursive comparator never sees `AssocProj`.
            _ => false,
        }
    }

    /// The trait the definition `id` is, if it is one.
    pub(super) fn trait_key(&self, id: crate::defs::DefId) -> Option<TraitKey> {
        let first = crate::defs::builtin_types().len();
        if let Some(k) = (id.0 as usize).checked_sub(first)
            && let Some(name) = crate::defs::BUILTIN_TRAITS.get(k)
        {
            return Some(TraitKey::builtin(name));
        }
        let def = self.defs.as_ref()?.get(id);
        match def.kind {
            crate::defs::DefKind::Trait(t) => Some(TraitKey {
                id: t,
                name: def.name,
            }),
            _ => None,
        }
    }

    /// The trait a method entry is of: its own, or for a builtin trait's
    /// method of a builtin type, which the table keeps without one, that
    /// builtin trait.
    pub(super) fn entry_trait(&self, entry: &MethodEntry, method: Symbol) -> Option<TraitKey> {
        entry.trait_name.or_else(|| {
            crate::defs::builtin_trait_of_method(&resolve(method)).and_then(|t| self.trait_key(t.0))
        })
    }

    /// Whether the module checked sees the trait `t`: a builtin trait, a
    /// trait it declares, names by an import or reaches through a module
    /// it imports. Another module's private trait it never sees.
    pub(super) fn sees_trait(&self, t: TraitKey) -> bool {
        let Some(defs) = &self.defs else {
            return true;
        };
        let def = defs.get(t.id.0);
        if def.module == self.module || def.module.is_builtin() {
            return true;
        }
        let private = self.tables.traits.get(&t).is_some_and(|info| {
            info.private_to
                .is_some_and(|(owner, _)| owner != self.module)
        });
        !private && (self.seen_traits.contains(&t.id.0) || self.seen_modules.contains(&def.module))
    }

    /// The module that declares the trait `t`, by name, when the module
    /// checked does not import it, nor a module that does, and so on: a
    /// method of the trait is not called here. (Its impls may or may not
    /// be known to the session by now; what a module means does not
    /// depend on that.)
    fn unreached(&self, t: TraitKey) -> Option<Symbol> {
        let module = self.defs.as_ref()?.get(t.id.0).module;
        let reach = self.reach.as_ref()?;
        if module == self.module || module.is_builtin() || reach.contains(&module) {
            return None;
        }
        Some(
            self.tables
                .module_names
                .get(&module)
                .copied()
                .unwrap_or_else(|| intern("?")),
        )
    }

    /// The method `method` of the type `ty`, if the module checked may
    /// call it: a method of a trait declared in a module it does not
    /// reach by its imports it has not.
    pub(super) fn method_entry(&self, ty: TypeRef, method: Symbol) -> Option<MethodEntry> {
        let entry = self.tables.method_table.get(&(ty, method))?;
        match entry.trait_name.and_then(|t| self.unreached(t)) {
            Some(_) => None,
            None => Some(entry.clone()),
        }
    }

    /// Keep that the type `ty` was reported at `span` to have no method
    /// `method`: a trait of a module that is not imported may have one,
    /// which is known once the whole program is checked.
    pub(super) fn note_unknown_method(&mut self, span: Span, ty: TypeRef, method: Symbol) {
        self.tables
            .unknown_methods
            .entry(self.module)
            .or_default()
            .push((span, ty, method));
    }

    /// Whether a call of `method` of `ty` is ambiguous here; if so, it is
    /// reported at `span`.
    pub(super) fn ambiguous_method_call(
        &mut self,
        ty: TypeRef,
        method: Symbol,
        span: Span,
    ) -> bool {
        let Some(traits) = self.ambiguous_methods.get(&(ty, method)).cloned() else {
            return false;
        };
        let on = format!("type '{}'", self.show_type(&Type::Generic(ty, vec![])));
        self.ambiguous_method(method, &on, &traits, span);
        true
    }
}
