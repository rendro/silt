//! The type of the forms that decide where control goes: `?`, a closure,
//! `match` and `return` (arms of `infer_expr_kind`).

use super::*;

impl TypeChecker {
    /// `infer_expr_kind` for `?`.
    pub(super) fn infer_question_mark(
        &mut self,
        expr: &mut Expr,
        env: &mut TypeEnv,
        span: Span,
    ) -> Type {
        let ExprKind::QuestionMark(inner) = &mut expr.kind else {
            unreachable!("infer_question_mark of another form");
        };
        let inner_ty = self.infer_expr(inner, env);
        let inner_ty = self.apply(&inner_ty);

        // Remember the `?` site so a later body/return
        // unify failure can point back here (see
        // `note_qmark_requirement_on_ret_mismatch`).
        self.current_qmark_spans.push(span);

        // A `?` returns from the function as `return` does: not
        // from one that never returns. (A closure's `?` ends the
        // closure: its body has a return type of its own.) A `?`
        // of what never is never runs (below).
        if let (Some(Type::Never), Some(name)) = (&self.current_return_type, self.never_fn)
            && !matches!(inner_ty, Type::Never)
        {
            self.error(
                Code::InvalidControlFlow,
                format!(
                    "'?' in '{name}', which is declared to return `Never`: `?` returns \
                     the `Err` or `None` it meets, and such a function never returns"
                ),
                span,
            );
        }

        // ? operator on Result(a,e) returns a, propagates Err(e)
        // ? operator on Option(a) returns a, propagates None
        // (The one check, also of an operand known only later:
        // `try_operand`.)
        match &inner_ty {
            Type::Generic(name, args)
                if (name.is_builtin("Result") && args.len() == 2)
                    || (name.is_builtin("Option") && args.len() == 1) =>
            {
                let ok = args[0].clone();
                let ret = self.current_return_type.clone();
                self.try_operand(&inner_ty, &ok, ret, span);
                ok
            }
            // An operand already reported (or from a module that
            // failed to load): nothing more to say.
            Type::Error => Type::Error,
            // An operand that never is (`todo()?`, `v.x?` of such
            // a `v`): the `?` never runs, and gives what never is,
            // as a call or a field read of it does, whatever the
            // function returns.
            Type::Never => Type::Never,
            Type::Var(_) => {
                // The operand's type is unknown: the `?` waits
                // for it (`Goal::Try`).
                let result_ty = self.fresh_var();
                self.want_goal(
                    Goal::Try {
                        operand: inner_ty.clone(),
                        ok: result_ty.clone(),
                        ret: self.current_return_type.clone(),
                    },
                    Origin {
                        span,
                        callee: None,
                        op: None,
                        header: None,
                    },
                );
                result_ty
            }
            _ => {
                self.question_misfit(&inner_ty, span);
                self.fresh_var()
            }
        }
    }

    /// `infer_expr_kind` for a closure.
    pub(super) fn infer_lambda(&mut self, expr: &mut Expr, env: &mut TypeEnv, span: Span) -> Type {
        let ExprKind::Lambda { params, body } = &mut expr.kind else {
            unreachable!("infer_lambda of another form");
        };
        self.closure_spans.push((span, self.written_vars.clone()));
        let owner = self.frame_owner.take();
        env.push();
        // Soundness: lambda param lists are a single conjunctive
        // scope too — `|a, a| ...` must be rejected the same way
        // `fn f(a, a)` is.
        self.check_fn_params_duplicate_bindings(params);
        // The parameter types the place the closure stands in
        // expects (an argument of a call whose callee is known):
        // an unannotated parameter has that type before the body
        // is checked.
        let (expected, expected_result) = match self.expected_closure.take() {
            Some((expected, result)) if expected.len() == params.len() => {
                (Some(expected), Some(result))
            }
            _ => (None, None),
        };
        let param_types: Vec<Type> = params
            .iter_mut()
            .enumerate()
            .map(|(i, p)| {
                let ty = if let Some(te) = &p.ty {
                    // B2: annotation arity errors carry the
                    // annotation's own span.
                    let prev_type_span = self.current_type_anno_span.replace(te.span);
                    let resolved = self.resolve_type_expr(te, &mut self.sig_names.clone());
                    self.current_type_anno_span = prev_type_span;
                    resolved
                } else if let Some(expected) = &expected {
                    expected[i].clone()
                } else {
                    self.fresh_var()
                };
                self.note_held_param(p, &ty);
                self.check_pattern(
                    &mut p.pattern,
                    &ty,
                    env,
                    span,
                    PatternMode::Binding(BindingSite::ClosureParam),
                );
                ty
            })
            .collect();
        for (p, ty) in params.iter().zip(&param_types) {
            if let (PatternKind::Ident(name), None) = (&p.pattern.kind, &p.ty) {
                self.closure_params.push((span, *name, ty.clone()));
            }
        }

        // A lambda is its own `?`/`return`
        // boundary — the VM's Op::QuestionMark pops exactly ONE
        // frame (the lambda's), so `?` inside a lambda must
        // validate against the LAMBDA's return type, not the
        // enclosing named fn's. Establish a fresh return-type
        // context for the body, mirroring check_body: checked
        // against the outer fn's return type, `{ x -> x? + 1 }`
        // would let a Variant escape into a List(Int) at runtime.
        let lambda_ret = self.fresh_var();
        let prev_return_type = self.current_return_type.replace(lambda_ret.clone());
        let prev_qmark_spans = std::mem::take(&mut self.current_qmark_spans);

        let names: Vec<Option<Symbol>> = (params.iter())
            .map(|p| match p.pattern.kind {
                PatternKind::Ident(name) => Some(name),
                _ => None,
            })
            .collect();
        let read_fields = read_fields(body, &names);
        self.fn_frames.push(unused::FnFrame {
            owner,
            params: param_types.clone(),
            names,
            read_fields,
        });
        self.closure_returns.push(Vec::new());
        let in_body = self.errors.len();
        let body_type = self.infer_expr(body, env);
        self.fn_frames.pop();
        env.pop();
        let before = self.errors.len();
        // (A `?` that ends the closure with a type its body does
        // not give is reported at the `?`: the closure's result is
        // then of no type, which asks nothing more of its place,
        // `Ok(list.map(xs) { s -> int.parse(s)? + 1 })`.)
        let question_misfit = self.unify_body_result(body, &body_type, &lambda_ret);
        let returns = self.closure_returns.pop().unwrap_or_default();
        if let Some((first, returned)) = returns.first() {
            let misfits = (self.errors[before..].iter())
                .filter(|e| e.code == Code::TypeMismatch)
                .map(|e| e.span)
                .collect();
            let returned = self.show_type(&self.apply(returned));
            self.closure_return_misfits
                .push((span, *first, returned, misfits));
        }

        self.current_return_type = prev_return_type;
        self.current_qmark_spans = prev_qmark_spans;

        // A result of a type the place does not take is reported at
        // the value that gives it, not at the closure.
        let result_misfit = match expected_result {
            Some(expected) if !question_misfit => {
                self.closure_result_misfit(body, &expected, in_body, span)
            }
            _ => false,
        };

        // `lambda_ret` rather than `body_type`: identical when the
        // unify above succeeded, and on failure it carries any
        // `?`-imposed Result/Option constraint, which is the type
        // the VM actually returns.
        match question_misfit || result_misfit {
            true => Type::Fun(param_types, Box::new(Type::Error)),
            false => Type::Fun(param_types, Box::new(lambda_ret)),
        }
    }

    /// Whether a value the closure `closure` gives as its result, at the
    /// end of `body` or of an arm of a `match` there, is of a type other
    /// than `expected`, the result its place takes: each such value is
    /// reported where it is, with the closure as a label. A result its
    /// place leaves open (a type variable), or `()` (a callback called
    /// as a statement), is left to the check of the whole closure. A
    /// value already reported in the body (an arm unlike the others) is
    /// not reported again. `in_body` is where the body's errors start.
    fn closure_result_misfit(
        &mut self,
        body: &Expr,
        expected: &Type,
        in_body: usize,
        closure: Span,
    ) -> bool {
        let expected = self.apply(expected);
        if matches!(
            expected,
            Type::Var(_) | Type::Unit | Type::Error | Type::Never
        ) {
            return false;
        }
        let mut sites = Vec::new();
        result_sites(body, &mut sites);
        // (A value whose type was not kept is left to the check of the
        // whole closure, with the others.)
        let Some(sites) = (sites.into_iter())
            .map(|(at, ty)| ty.map(|ty| (at, ty)))
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        let shown = self.show_type(&expected);
        let mut misfit = false;
        for (at, ty) in sites {
            let ty = self.apply(&ty);
            if matches!(ty, Type::Never | Type::Error)
                || (self.errors[in_body..].iter()).any(|e| e.span == at)
            {
                continue;
            }
            let from = self.errors.len();
            self.unify(&ty, &expected, at);
            for d in self.errors.iter_mut().skip(from) {
                d.labels.push((
                    closure,
                    format!("the result of this closure is to be {shown}"),
                ));
            }
            misfit |= self.errors.len() > from;
        }
        misfit
    }

    /// `infer_expr_kind` for `match`.
    pub(super) fn infer_match(&mut self, expr: &mut Expr, env: &mut TypeEnv, span: Span) -> Type {
        let ExprKind::Match {
            expr: scrutinee,
            arms,
        } = &mut expr.kind
        else {
            unreachable!("infer_match of another form");
        };
        match scrutinee {
            Some(scrutinee) => {
                let scrutinee_span = scrutinee.span;
                let scrutinee_ty = self.infer_expr(scrutinee, env);
                let result_ty = self.fresh_var();

                // Every arm checks its pattern against the SAME
                // scrutinee. A mismatch at a pattern that repeats
                // an earlier arm's is left out by `unify_pattern`
                // (`arm_mismatches`); what an arm's pattern reports
                // at the scrutinee itself (the key type of a map
                // pattern) would repeat at the same place: identical
                // (message, span, severity) diagnostics are reported
                // once across the arm loop.
                let mut seen_arm_diags: std::collections::HashSet<(
                    std::string::String,
                    crate::source::FileId,
                    u32,
                    bool,
                )> = std::collections::HashSet::new();
                let mut any_pattern_mismatch = false;
                // The texts of the mismatches the arms' patterns
                // reported (see `unify_pattern`).
                let mut arm_mismatches = std::collections::HashSet::new();
                // What each arm's pattern tests, for the
                // exhaustiveness check.
                let mut arm_pats = Vec::with_capacity(arms.len());
                // A match diverges when it has arms and every
                // arm diverges. `unify` leaves `result_ty`
                // unbound against `Never`, so the arms are
                // tracked here to type such a match `Never`.
                let mut every_arm_diverges = !arms.is_empty();
                for arm in arms.iter_mut() {
                    env.push();
                    // Soundness: `match e { (x, x) -> x }` used to
                    // typecheck silently, binding the second `x` on
                    // top of the first. Reject duplicate binders
                    // in the arm pattern before check_pattern walks
                    // it and defines them in the arm's frame.
                    self.check_pattern_duplicate_bindings(&arm.pattern);
                    // A name in the pattern the resolver reported:
                    // what the arm covers is not known.
                    if names_unresolved(&arm.pattern) {
                        any_pattern_mismatch = true;
                    }
                    let pat_err_count = self.errors.len();
                    self.arm_mismatches = Some(std::mem::take(&mut arm_mismatches));
                    arm_pats.push(self.check_pattern(
                        &mut arm.pattern,
                        &scrutinee_ty,
                        env,
                        scrutinee_span,
                        PatternMode::Arm,
                    ));
                    arm_mismatches = self.arm_mismatches.take().unwrap_or_default();
                    if self.errors.len() > pat_err_count {
                        let new_diags: Vec<Diagnostic> =
                            self.errors.drain(pat_err_count..).collect();
                        for d in new_diags {
                            if matches!(d.severity, Severity::Error) {
                                any_pattern_mismatch = true;
                            }
                            let key = (
                                d.message.clone(),
                                d.span.file,
                                d.span.start,
                                matches!(d.severity, Severity::Error),
                            );
                            if seen_arm_diags.insert(key) {
                                // Re-inserting a pre-built Diagnostic drained
                                // above for dedup — not a new emission site,
                                // so `self.error(...)` does not apply.
                                self.errors.push(d);
                            }
                        }
                    }

                    if let Some(ref mut guard) = arm.guard {
                        let guard_span = guard.span;
                        let guard_ty = self.infer_expr(guard, env);
                        self.unify(&guard_ty, &Type::Bool, guard_span);
                    }

                    let body_span = arm.body.span;
                    let arm_ty = self.infer_expr(&mut arm.body, env);
                    env.pop();
                    every_arm_diverges &= matches!(arm_ty, Type::Never);
                    self.unify(&arm_ty, &result_ty, body_span);
                }

                // Check exhaustiveness after pattern checking, so the
                // scrutinee type is fully resolved through unification.
                // Skipped when the scrutinee type already failed to
                // match the arms — exhaustiveness over a broken match
                // is a cascade, not new information.
                // So is a scrutinee of a type already reported.
                let resolved_scrutinee_ty = self.apply(&scrutinee_ty);
                if !any_pattern_mismatch && resolved_scrutinee_ty != Type::Error {
                    self.check_exhaustiveness(
                        arms,
                        &arm_pats,
                        &resolved_scrutinee_ty,
                        scrutinee_span,
                    );
                }

                if every_arm_diverges {
                    Type::Never
                } else {
                    result_ty
                }
            }
            None => {
                // Guardless match: each arm's guard is a boolean
                // condition. When no condition holds there is no
                // value, so the last arm must be the `_` default,
                // and there is exactly one: arms after a `_` never
                // run.
                let defaults: Vec<usize> = arms
                    .iter()
                    .enumerate()
                    .filter(|(_, arm)| arm.guard.is_none())
                    .map(|(i, _)| i)
                    .collect();
                match defaults.first() {
                    None => self.error(
                        Code::NonExhaustive,
                        "a `match` without a scrutinee must end with a `_ -> ...` arm \
                         for when no condition is true"
                            .to_string(),
                        span,
                    ),
                    Some(&first) => {
                        if arms[first + 1..].iter().any(|arm| arm.guard.is_some()) {
                            self.error(
                                Code::UnreachablePattern,
                                "the `_` arm must be last: it always matches, so the \
                                 arms after it never run"
                                    .to_string(),
                                arms[first].pattern.span,
                            );
                        }
                        for &i in &defaults[1..] {
                            self.error(
                                Code::UnreachablePattern,
                                "unreachable `_` arm: an earlier `_` arm always matches"
                                    .to_string(),
                                arms[i].pattern.span,
                            );
                        }
                    }
                }
                let result_ty = self.fresh_var();
                // Same rule as the scrutinee form: arms that all
                // diverge make the match diverge.
                let mut every_arm_diverges = !arms.is_empty();

                for arm in arms.iter_mut() {
                    env.push();

                    if let Some(ref mut guard) = arm.guard {
                        let guard_span = guard.span;
                        let guard_ty = self.infer_expr(guard, env);
                        self.unify(&guard_ty, &Type::Bool, guard_span);
                    }

                    let body_span = arm.body.span;
                    let arm_ty = self.infer_expr(&mut arm.body, env);
                    env.pop();
                    every_arm_diverges &= matches!(arm_ty, Type::Never);
                    self.unify(&arm_ty, &result_ty, body_span);
                }

                if every_arm_diverges {
                    Type::Never
                } else {
                    result_ty
                }
            }
        }
    }

    /// `infer_expr_kind` for `return`.
    pub(super) fn infer_return(&mut self, expr: &mut Expr, env: &mut TypeEnv, span: Span) -> Type {
        let ExprKind::Return(maybe_expr) = &mut expr.kind else {
            unreachable!("infer_return of another form");
        };
        let ret_val_ty = if let Some(e) = maybe_expr {
            self.infer_expr(e, env)
        } else {
            Type::Unit
        };
        if let Some(returns) = self.closure_returns.last_mut() {
            returns.push((span, ret_val_ty.clone()));
        }
        match (&self.current_return_type, self.never_fn) {
            // (A closure's `return` ends the closure: its body
            // has a return type of its own.)
            (Some(Type::Never), Some(name)) => self.error(
                Code::InvalidControlFlow,
                format!(
                    "'return' in '{name}', which is declared to return `Never`: such a \
                     function never returns"
                ),
                span,
            ),
            (Some(expected_ret), _) => {
                let expected_ret = expected_ret.clone();
                self.unify(&ret_val_ty, &expected_ret, span);
            }
            (None, _) => {}
        }
        Type::Never
    }
}

/// The values `body` ends in, each with its type: the last expression of
/// a block, each arm's value of a `match` there, or `body` itself.
fn result_sites(body: &Expr, sites: &mut Vec<(Span, Option<Type>)>) {
    match &body.kind {
        ExprKind::Block(stmts) => match stmts.last() {
            Some(Stmt::Expr(e)) => result_sites(e, sites),
            _ => sites.push((body.span, body.ty.clone())),
        },
        ExprKind::Match { arms, .. } if !arms.is_empty() => {
            for arm in arms {
                result_sites(&arm.body, sites);
            }
        }
        _ => sites.push((body.span, body.ty.clone())),
    }
}
