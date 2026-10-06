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
}

impl Goal {
    /// The type the goal waits for.
    pub(super) fn waits_on(&self) -> &Type {
        match self {
            Goal::Pred(Pred::Trait { subject, .. }) => subject,
            Goal::Select { recv, .. } => recv,
            Goal::Update { base, .. } => base,
            Goal::Try { operand, .. } => operand,
        }
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
                            self.verify_trait_obligation(tr, &args, &subject, origin);
                        }
                    }
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
        let Some(entry) = self.tables.method_table.get(&(type_name, field)).cloned() else {
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
                    self.deferred_method_traits.insert(span, *trait_name);
                    self.unify_deferred_method(&result_ty, &method_ty, span);
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
            Type::Record(rec_name, rec_fields) => {
                if let Some((_, field_ty)) = rec_fields.iter().find(|(n, _)| *n == field) {
                    let ft = field_ty.clone();
                    self.unify(&result_ty, &ft, span);
                } else if !self.deferred_method_call(*rec_name, field, &obj_ty, &result_ty, span) {
                    // GAP (round 35 F7): thread did-you-mean suggestion
                    // through the deferred-field-access path so typos
                    // on Record-shaped receivers get the same hint.
                    let base = format!("unknown field '{field}' on type {resolved}");
                    self.error_help(
                        Code::UnknownField,
                        format_record_field_suggestion(base, field, rec_fields),
                        span,
                    );
                }
            }
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
                // Round 93: the field-aware auto-derive gate removed
                // this type's provisional `.equal()`/`.compare()`/
                // `.hash()` entry — name the offending field instead
                // of a generic "unknown method".
                if let Some(msg) = self.method_auto_derive_violation(type_name, field) {
                    self.error(Code::NotDerivable, msg, span);
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
            Type::Record(_, fields) => fields.clone(),
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

    /// Decide each call `x.m(args)` of the scope just left whose
    /// receiver is still unknown and is the scope's to generalise. When
    /// exactly one trait the module sees declares a method `m`, the call
    /// is that method's and `x` is bounded by the trait
    /// (`fn g(x) { x.greet() }` is `a -> String where a: Greet`). When
    /// none does, `x` is a record with a function in its field `m`. When
    /// several do, the receiver needs an annotation.
    pub(super) fn default_selects(&mut self) {
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
            if matches!(self.apply(&recv), Type::Var(_)) {
                self.wanted[i].solved = true;
                let call_ty = Type::Fun(args, Box::new(result));
                self.default_select(&recv, name, call_ty, origin.span);
            }
        }
        self.solve_wanted(self.closed_mark);
        self.close_level();
    }

    fn default_select(&mut self, recv: &Type, name: Symbol, call_ty: Type, span: Span) {
        let mut traits: Vec<TraitKey> = self
            .tables
            .traits
            .iter()
            .filter(|(_, info)| info.methods.iter().any(|(n, _)| *n == name))
            .map(|(t, _)| *t)
            .filter(|t| self.sees_trait(*t))
            .collect();
        traits.sort_by_key(|t| self.show_trait(*t));
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
                let shown: Vec<String> = several.iter().map(|t| self.show_trait(*t)).collect();
                self.errors.push(
                    Diagnostic::error(
                        Code::AmbiguousMethod,
                        span,
                        format!(
                            "ambiguous method '{name}' on a value whose type is not known: provided by traits {}",
                            shown.join(", ")
                        ),
                    )
                    .with_help("annotate the receiver's type, or bound it with a `where` clause"),
                );
            }
        }
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
            // `trait_impl_set.contains(...)` check fire the correct
            // "type '<anon>' does not implement trait 'X'" diagnostic;
            // no program can name that type, so no impl targets it.
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
            let mut open = Vec::new();
            let mut written = Vec::new();
            let mut assumed = Vec::new();
            match self.structure_gap(trait_name, &resolved, &mut assumed, &mut open, &mut written) {
                Some(gap) => {
                    // An operator's own operand of a type the operator
                    // is not for: the operator's message.
                    let message = match origin.op {
                        Some(op)
                            if !is_valid_compare_operand(
                                &resolved,
                                trait_name.is_builtin("Equal"),
                            ) && !trait_name.is_builtin("Display") =>
                        {
                            let domain = if trait_name.is_builtin("Equal") {
                                "a comparable type"
                            } else {
                                "Int, Float, String, Bool, List, Tuple, Record, or Variant"
                            };
                            self.error(
                                Code::UnsupportedOperation,
                                format!("operator {op} requires {domain}, got '{resolved}'"),
                                span,
                            );
                            return;
                        }
                        _ => gap,
                    };
                    self.error(Code::MissingTraitImpl, message, span);
                }
                None => {
                    // A part still unknown, or an annotation variable,
                    // owes the trait in turn; a part with a written
                    // `Display` impl is that impl's to answer.
                    for part in open {
                        self.want(
                            Pred::Trait {
                                tr: trait_name,
                                args: Vec::new(),
                                subject: part,
                            },
                            origin,
                        );
                    }
                    for part in written {
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
        trait_name.is_builtin("Display")
            && self
                .tables
                .method_table
                .get(&(head, intern("display")))
                .is_none_or(|entry| entry.is_auto_derived)
    }

    /// Why `ty` does not have the structural trait `tr`, if it does not:
    /// a function has none of them, a channel only `Equal`; a container,
    /// a tuple, a record or an enum has one when its head does (`Option`
    /// has no `Compare`) and each of its parts does. The parts whose
    /// type is still unknown, or an annotation variable, are added to
    /// `open`: the trait holds if it holds of them. A recursive type
    /// holds if its other parts do (`assumed`).
    fn structure_gap(
        &self,
        tr: TraitKey,
        ty: &Type,
        assumed: &mut Vec<(TypeRef, Vec<Type>)>,
        open: &mut Vec<Type>,
        written: &mut Vec<Type>,
    ) -> Option<String> {
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        let lacks = |this: &Self, head: &str| {
            Some(format!(
                "type '{head}' does not implement trait '{}'",
                this.show_trait(tr)
            ))
        };
        let stamped = |this: &Self, head: TypeRef| {
            let canon = canonical_head(&this.tables.resolver, head);
            this.tables.trait_impl_set.contains(&(tr, canon))
        };
        match &ty {
            Type::Error | Type::Never | Type::AssocProj { .. } => None,
            Type::Var(_) | Type::Rigid(_) => {
                if !open.contains(&ty) {
                    open.push(ty.clone());
                }
                None
            }
            Type::Fun(..) => lacks(self, "Fn"),
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
                    return lacks(self, &resolve(head.name));
                }
                let parts: Vec<&Type> = match &ty {
                    Type::List(t) | Type::Range(t) | Type::Set(t) => vec![&**t],
                    Type::Map(k, v) => vec![&**k, &**v],
                    Type::Tuple(ts) => ts.iter().collect(),
                    _ => Vec::new(),
                };
                parts
                    .into_iter()
                    .find_map(|part| self.structure_gap(tr, part, assumed, open, written))
            }
            Type::AnonRecord { fields, tail } => {
                if tr.is_builtin("Compare") {
                    return Some(format!(
                        "type '{}' does not implement trait '{}'",
                        self.show_type(&ty),
                        self.show_trait(tr)
                    ));
                }
                if !matches!(tail, RowTail::Closed) {
                    return Some(format!(
                        "type '{}' does not implement trait '{}': its other fields are not known",
                        self.show_type(&ty),
                        self.show_trait(tr)
                    ));
                }
                fields
                    .values()
                    .find_map(|part| self.structure_gap(tr, part, assumed, open, written))
            }
            Type::Record(name, _) | Type::Generic(name, _) => {
                let head = canonical_head(&self.tables.resolver, *name);
                // A written `Display` impl answers for its type.
                if !self.by_structure(tr, head) {
                    written.push(ty.clone());
                    return None;
                }
                // A type with no fields or variants to look at (a
                // handle, `Bytes`) has what the builtins say it has.
                let opaque = !matches!(ty, Type::Record(..))
                    && !self.tables.records.contains_key(name)
                    && !self.tables.enums.contains_key(name);
                if opaque {
                    return match stamped(self, head) {
                        true => None,
                        false => lacks(self, &self.show_type(&Type::Generic(*name, vec![]))),
                    };
                }
                let args = self.type_args_of(&ty);
                // A recursive type holds if its other parts do. (A type
                // that recurs at ever larger arguments,
                // `type N(a) { Z, S(N(List(a))) }`, is taken to hold
                // past a depth no value of a program reaches.)
                if assumed.len() > 64 || assumed.iter().any(|(h, a)| *h == head && *a == args) {
                    return None;
                }
                assumed.push((head, args.clone()));
                // The type's parts, at its arguments.
                let parts: Vec<(String, Type)> = match &ty {
                    Type::Record(_, fields) => fields
                        .iter()
                        .map(|(n, t)| (format!("field '{n}'"), t.clone()))
                        .collect(),
                    _ => {
                        if let Some(info) = self.tables.records.get(name) {
                            let mapping: HashMap<TyVar, Type> = self
                                .tables
                                .record_param_var_ids
                                .get(name)
                                .filter(|ids| ids.len() == args.len())
                                .map(|ids| ids.iter().copied().zip(args.iter().cloned()).collect())
                                .unwrap_or_default();
                            info.fields
                                .iter()
                                .map(|(n, t)| {
                                    (format!("field '{n}'"), substitute_vars(t, &mapping))
                                })
                                .collect()
                        } else if let Some(info) = self.tables.enums.get(name) {
                            let mapping: HashMap<TyVar, Type> =
                                if info.param_var_ids.len() == args.len() {
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
                        }
                    }
                };
                parts.into_iter().find_map(|(what, part)| {
                    self.structure_gap(tr, &part, assumed, open, written)
                        .map(|_| {
                            format!(
                                "type '{}' cannot derive '{}': {what} has type '{}', which is not {}",
                                self.show_type(&ty),
                                self.show_trait(tr),
                                self.show_type(&self.apply(&part)),
                                derive_gates::builtin_trait_adjective(tr),
                            )
                        })
                })
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
                let message = if trait_name.is_builtin("Number") {
                    arith_operand_message(op, &resolved)
                } else {
                    let domain = if trait_name.is_builtin("Equal") {
                        "a comparable type"
                    } else {
                        "Int, Float, String, Bool, List, Tuple, Record, or Variant"
                    };
                    format!("operator {op} requires {domain}, got '{resolved}'")
                };
                self.error(Code::UnsupportedOperation, message, span);
                return;
            }
            self.error(
                Code::MissingTraitImpl,
                format!(
                    "type '{}' does not implement trait '{}'",
                    self.show_type(&Type::Generic(type_name, vec![])),
                    self.show_bound(trait_name, bound_trait_args)
                ),
                span,
            );
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
        for Pred::Trait { args, subject, .. } in &header {
            own.extend(args.iter().chain([subject]).flat_map(free_vars_in));
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
        // self type (builtin pre-stamps, auto-derive synthesis) skip the
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
            (Type::Record(n1, _), Type::Record(n2, _)) => n1 == n2,
            (Type::Record(n1, _), Type::Generic(n2, _))
            | (Type::Generic(n1, _), Type::Record(n2, _)) => n1 == n2,
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

    /// The entry of the method `method` of the trait `t` for the type
    /// `ty`, when the impls of two or more traits provide the method.
    pub(super) fn trait_method_entry(
        &self,
        ty: TypeRef,
        method: Symbol,
        t: TraitKey,
    ) -> Option<MethodEntry> {
        self.tables.trait_methods.get(&(ty, method, t)).cloned()
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
        let shown: Vec<String> = traits.iter().map(|t| self.show_trait(*t)).collect();
        self.error(
            Code::AmbiguousMethod,
            format!(
                "ambiguous method '{method}' on type '{}': provided by traits {}",
                self.show_type(&Type::Generic(ty, vec![])),
                shown.join(", ")
            ),
            span,
        );
        true
    }
}
