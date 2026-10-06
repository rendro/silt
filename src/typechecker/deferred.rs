use super::inference::*;
use super::*;

impl TypeChecker {
    // ── Deferred check finalization ─────────────────────────────────

    /// Resolve any deferred field-access and numeric-op checks that were
    /// recorded against type variables during inference. Called after all
    /// function bodies have been processed so we can see the final
    /// substitution.
    ///
    /// Important architectural note: Silt uses Algorithm W with
    /// let-polymorphism, so the body of a polymorphic function is
    /// inferred once using fresh instantiated vars that are NEVER unified
    /// with call-site concrete types (each call instantiates *another*
    /// set of fresh vars). This means that if a polymorphic function's
    /// body uses an ambiguous field access or arithmetic op, the body-
    /// inference-time vars stay unresolved at finalization. We cannot
    /// emit errors on those, because that would reject legitimate
    /// polymorphic definitions like `fn add(a, b) { a + b }` or
    /// `fn get_x(obj) { obj.x }`. Instead, the deferred-check pass ONLY
    /// fires when the operand / receiver has resolved to a concrete,
    /// non-conforming type (e.g. a monomorphic `let s = "hi"; -s`).
    /// A method call whose receiver was a type variable when it was
    /// inferred and is the type `type_name` now: `true` when the type has
    /// the method. The call's trait is recorded by its span
    /// (`deferred_method_traits`), which `resolve_all_types` writes on
    /// the access; a call that sees the method in two traits is
    /// ambiguous, as anywhere.
    fn deferred_method_call(
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
    fn unify_deferred_method(&mut self, result_ty: &Type, method_ty: &Type, span: Span) {
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

    pub(super) fn finalize_deferred_checks(&mut self) {
        // B4: pending field accesses on type variables. Only flag when
        // the receiver resolved to a concrete type.
        let pending_fields = std::mem::take(&mut self.pending_field_accesses);
        for (obj_ty, field, result_ty, span) in pending_fields {
            let resolved = self.apply(&obj_ty);
            match &resolved {
                Type::Error | Type::Never => {}
                // Still unknown: it waits. `settle_bounds` drops it once
                // the variable is generalised (see above); on a variable
                // of an outer binding it is checked when that is decided.
                Type::Var(_) => {
                    self.pending_field_accesses
                        .push((obj_ty, field, result_ty, span));
                }
                // The receiver became an annotation variable: it has the
                // methods of its bounds, and nothing else.
                Type::Rigid(r) => match self.bound_methods(*r, field).as_slice() {
                    [(trait_name, method_ty, bounds)] => {
                        let method_ty =
                            self.instantiate_bound_method(method_ty, bounds, field, span);
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
                    } else if !self
                        .deferred_method_call(*rec_name, field, &obj_ty, &result_ty, span)
                    {
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
                Type::AnonRecord { fields: af, .. } => {
                    if let Some(field_ty) = af.get(&field) {
                        let ft = field_ty.clone();
                        self.unify(&result_ty, &ft, span);
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
                        continue;
                    }
                    // Also check the method table for trait methods.
                    if self.deferred_method_call(type_name, field, &obj_ty, &result_ty, span) {
                        continue;
                    }
                    // Round 93: the field-aware auto-derive gate removed
                    // this type's provisional `.equal()`/`.compare()`/
                    // `.hash()` entry — name the offending field instead
                    // of a generic "unknown method".
                    if let Some(msg) = self.method_auto_derive_violation(type_name, field) {
                        self.error(Code::NotDerivable, msg, span);
                        continue;
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
                        continue;
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

        // B5 / B2 / B3: pending numeric / comparison checks on type variables.
        let pending_numeric = std::mem::take(&mut self.pending_numeric_checks);
        for (ty, op_desc, span) in pending_numeric {
            let resolved = self.apply(&ty);
            // Early-exit for types that never participate in operator errors.
            if matches!(resolved, Type::Error | Type::Never) {
                continue;
            }
            // If the operand is still a type variable at the end of inference,
            // it's either (a) a function parameter that's genuinely polymorphic
            // (e.g. `fn add(a, b) { a + b }` that's never called) — in which
            // case the fn was never monomorphized so we can't validate, or
            // (b) a body inference var from a polymorphic fn template whose
            // call sites were processed using fresh instantiated vars (so the
            // template var never got constrained). We skip both rather than
            // reject legitimate polymorphic definitions. Note the constraint
            // is NOT re-checked at the call site (each call instantiates
            // fresh vars that never flow back into this template var), so a
            // call passing a non-conforming operand is not caught statically
            // — the VM catches it at runtime with a clean operator-domain
            // diagnostic.
            if matches!(resolved, Type::Var(_)) {
                self.pending_numeric_checks.push((ty, op_desc, span));
                continue;
            }
            if matches!(resolved, Type::Rigid(_)) {
                continue;
            }
            // Classify the op based on its recorded tag (string literals set
            // at the binary-op or unary-op site).
            let valid = match op_desc {
                // Numeric-only arithmetic.
                "'+'" | "'-'" | "'*'" | "'/'" | "'%'" | "unary '-'" => {
                    is_valid_arith_operand(&resolved)
                }
                // Equality: anything comparable.
                "'=='/'!='" => is_valid_compare_operand(&resolved, true),
                // Ordering comparison: stricter domain.
                "ordering comparison" => is_valid_compare_operand(&resolved, false),
                _ => true,
            };
            if !valid {
                if matches!(op_desc, "'+'" | "'-'" | "'*'" | "'/'" | "'%'" | "unary '-'") {
                    self.error(
                        Code::UnsupportedOperation,
                        arith_operand_message(op_desc, &resolved),
                        span,
                    );
                    continue;
                }
                let domain = match op_desc {
                    "'=='/'!='" => "a comparable type",
                    "ordering comparison" => "Int, Float, String, List, Range, Record, or Variant",
                    _ => "a valid operand",
                };
                self.error(
                    Code::UnsupportedOperation,
                    format!("operator {op_desc} requires {domain}, got '{resolved}'"),
                    span,
                );
            } else if matches!(op_desc, "'=='/'!='" | "ordering comparison")
                && let Some(msg) =
                    self.operand_builtin_trait_violation(&resolved, op_desc == "'=='/'!='")
            {
                // Round 93: deferred mirror of the concrete comparison
                // arm — a late-resolved nominal operand may wrap fields
                // that cannot support the Value-level operation.
                self.error(Code::NotDerivable, msg, span);
            }
        }

        // Round 93: deferred `?` checks recorded when the ?-ed expression's
        // type was still an unresolved Var at inference time (e.g. the
        // unannotated param of an inline lambda, resolved later by the
        // call-site unification). Validate now with the final substitution:
        // mirror the concrete inline arms of ExprKind::QuestionMark —
        // unwrap into the surrounding expression AND constrain the
        // enclosing fn/lambda return type. Still-Var inners stay lenient
        // (polymorphic templates whose body vars never unify with call
        // sites — same rationale as `pending_numeric_checks` above).
        let pending_qmarks = std::mem::take(&mut self.pending_question_marks);
        for (inner_ty, result_ty, expected_ret, span) in pending_qmarks {
            let resolved = self.apply(&inner_ty);
            let (head, args) = match &resolved {
                Type::Error | Type::Never => continue,
                Type::Var(_) => {
                    self.pending_question_marks
                        .push((inner_ty, result_ty, expected_ret, span));
                    continue;
                }
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
                    continue;
                }
            };
            // The unwrapped Ok/Some payload is what flowed into the
            // surrounding expression (t1 = got, t2 = expected).
            self.unify(&args[0], &result_ty, span);
            let Some(ret) = expected_ret else {
                self.error(
                    Code::InvalidQuestion,
                    "? operator can only be used inside a function that returns Result or Option"
                        .to_string(),
                    span,
                );
                continue;
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

        // The bounds owed for a variable that was unknown where they
        // were owed (`owe_bound`). A variable known now is checked. An
        // annotation variable must have the bound declared. One still
        // unknown is generalised with the definition being checked, and
        // the bound is its scheme's (`generalize`), or it belongs to a
        // binding checked later.
        let pending_where = std::mem::take(&mut self.pending_where_constraints);
        for pending in pending_where {
            let PendingWhereConstraint {
                tyvar,
                trait_name,
                callee_fn_name,
                span,
                bound_trait_args,
            } = pending;
            let resolved = self.apply(&Type::Var(tyvar));
            match &resolved {
                Type::Error | Type::Never => {}
                Type::Var(v) => self.pending_where_constraints.push(PendingWhereConstraint {
                    tyvar: *v,
                    trait_name,
                    callee_fn_name,
                    span,
                    bound_trait_args,
                }),
                Type::Rigid(r) => self.require_declared_bound(*r, trait_name, callee_fn_name, span),
                _ => self.verify_trait_obligation(trait_name, &bound_trait_args, &resolved, span),
            }
        }
    }
}
