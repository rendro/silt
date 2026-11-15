//! Post-inference type resolution and unresolved type variable detection.
//!
//! After all inference passes complete, this module walks the AST to:
//! - Detect let-bindings with unresolved (ambiguous) types
//! - Apply the final substitution to all type annotations

use super::*;

impl TypeChecker {
    // ── Unresolved type variable detection ──────────────────────────────

    /// Check whether a fully-applied type is a bare unresolved type variable.
    fn is_bare_type_var(&self, ty: &Type) -> bool {
        // (The type a parameter holds, `t` of `t.zero()`: known where the
        // code runs, as a `type t` parameter's is. A variable of the type
        // of a parameter with no annotation, `let v = c` in `fn keep(c)`:
        // decided at each call, as the parameter's is.)
        matches!(self.apply(ty), Type::Var(v)
            if !self.held_by_param(v) && !self.of_unannotated_param(v))
    }

    /// Whether `v` is in the type of a parameter with no annotation.
    fn of_unannotated_param(&self, v: TyVar) -> bool {
        (self.unannotated_params.iter()).any(|ty| free_vars_in(&self.apply(ty)).contains(&v))
    }

    /// Check whether any already-emitted error has a span that points at
    /// some sub-expression within `value`. This is used to suppress the
    /// "cannot infer the type of `x`" cascade when the value itself failed
    /// to typecheck — e.g. `let x = nonExistent` should only report the
    /// `undefined variable 'nonExistent'` diagnostic, not also the
    /// inference-failure follow-up that wouldn't be fixed by adding an
    /// annotation.
    ///
    /// Spans are single points (line/col/offset) rather than ranges in this
    /// codebase, so "containment" is implemented by collecting every
    /// sub-expression's span offset within `value` and checking whether any
    /// existing error span matches one of them. This is reliable because
    /// type errors emitted from `infer_expr` always attach to the
    /// expression node currently being inferred.
    fn value_already_errored(&self, value: &Expr) -> bool {
        if self.errors.is_empty() {
            return false;
        }
        let mut offsets: std::collections::HashSet<u32> = std::collections::HashSet::new();
        Self::collect_sub_spans(value, &mut offsets);
        self.errors
            .iter()
            .any(|e| e.severity == Severity::Error && offsets.contains(&e.span.start))
    }

    /// Collect the start offset of every sub-expression within `expr`
    /// (including `expr` itself) into `out`. Used by `value_already_errored`
    /// to determine whether an existing diagnostic was emitted somewhere
    /// inside the value expression.
    fn collect_sub_spans(expr: &Expr, out: &mut std::collections::HashSet<u32>) {
        expr.for_each_expr(&mut |e| {
            out.insert(e.span.start);
        });
    }

    /// After all inference passes, walk let-bindings (both top-level and inside
    /// function bodies) and emit an error when the value expression's type could
    /// not be determined (still a bare `Type::Var` with no user annotation).
    ///
    /// To avoid false positives from the register-before-check architecture
    /// (where many function call return types are technically bare type variables
    /// but get constrained by later usage), we only flag a let-binding when the
    /// bound name is NOT referenced in any subsequent statement within the same
    /// block.  If the binding IS used later, the polymorphic type is acceptable
    /// because the use site will instantiate it concretely.
    pub(super) fn check_unresolved_let_types(&mut self, program: &Program) {
        // (A top-level `let` whose type stays unknown is reported by
        // `report_unknown_let_types`.)
        // Function bodies and trait impl method bodies
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => self.check_unresolved_in_expr(&f.body),
                Decl::TraitImpl(ti) => {
                    for m in &ti.methods {
                        self.check_unresolved_in_expr(&m.body);
                    }
                }
                Decl::Trait(t) => {
                    for m in t.methods.iter().filter(|m| !m.is_signature_only) {
                        self.check_unresolved_in_expr(&m.body);
                    }
                }
                _ => {}
            }
        }
    }

    /// Recursively walk an expression tree looking for blocks that contain
    /// let-bindings with unresolved bare type variables.
    fn check_unresolved_in_expr(&mut self, expr: &Expr) {
        if let ExprKind::Block(stmts) = &expr.kind {
            self.check_unresolved_in_block(stmts);
        }
        expr.for_each_child(|child| self.check_unresolved_in_expr(child));
    }

    /// Check let-bindings in a block of statements. For each `Stmt::Let` where
    /// the type annotation is absent and the value's resolved type is a bare
    /// `Type::Var`, emit an error only when:
    ///
    /// 1. The bound name does not appear in any subsequent statement in the
    ///    same block (meaning nothing constrains the type later).
    /// 2. The value expression is NOT a call with arguments — calls to functions
    ///    with parameters commonly produce bare type variables due to the
    ///    register-before-check architecture, even when the return type would
    ///    theoretically be deterministic. Only nullary calls (zero arguments)
    ///    or non-call expressions are flagged.
    fn check_unresolved_in_block(&mut self, stmts: &[Stmt]) {
        for (i, stmt) in stmts.iter().enumerate() {
            if let Stmt::Let { pattern, ty, value } = stmt {
                // Only check when there's no user annotation
                if ty.is_some() {
                    continue;
                }

                // Only check when the value type is a bare unresolved Type::Var
                let is_bare_var = value
                    .ty
                    .as_ref()
                    .map(|t| self.is_bare_type_var(t))
                    .unwrap_or(false);
                if !is_bare_var {
                    continue;
                }

                // Skip calls with arguments — they often have bare Var returns
                // due to register-before-check but the type is usually fine.
                if let ExprKind::Call(_, args) = &value.kind
                    && !args.is_empty()
                {
                    continue;
                }
                // Pipe expressions are also calls; skip them.
                if matches!(&value.kind, ExprKind::Pipe(..)) {
                    continue;
                }

                // Skip the cascade if the value expression itself already
                // produced an error — fixing that root cause would also
                // resolve the inference failure, so the "cannot infer"
                // follow-up is misleading.
                if self.value_already_errored(value) {
                    continue;
                }

                // Collect names bound by this let pattern
                let bound_names = pattern.binder_names();
                if bound_names.is_empty() {
                    continue;
                }

                // Check whether any bound name is referenced in subsequent
                // statements (the remaining slice of the block).
                let used_later = bound_names.iter().any(|name| {
                    stmts[i + 1..]
                        .iter()
                        .any(|s| Self::stmt_references_name(s, *name))
                });

                if !used_later {
                    let first = resolve(bound_names[0]);
                    self.error(
                        Code::AmbiguousType,
                        format!(
                            "cannot infer the type of `{first}` — \
                             add an annotation, e.g. `let {first}: SomeType = ...`"
                        ),
                        value.span,
                    );
                }
            }
        }
    }

    /// Check if a statement contains any reference to the given name.
    fn stmt_references_name(stmt: &Stmt, name: Symbol) -> bool {
        match stmt {
            Stmt::Let { value, .. } => Self::expr_references_name(value, name),
            Stmt::When {
                expr, else_body, ..
            } => {
                Self::expr_references_name(expr, name)
                    || Self::expr_references_name(else_body, name)
            }
            Stmt::WhenBool {
                condition,
                else_body,
            } => {
                Self::expr_references_name(condition, name)
                    || Self::expr_references_name(else_body, name)
            }
            Stmt::Expr(e) => Self::expr_references_name(e, name),
        }
    }

    /// Check if an expression tree contains any `Ident` reference to the given name.
    fn expr_references_name(expr: &Expr, name: Symbol) -> bool {
        expr.any_expr(&mut |e| matches!(e.kind, ExprKind::Ident(n) if n == name))
    }

    // ── Post-inference type resolution ─────────────────────────────────

    /// After all passes, walk the AST and resolve any remaining type variables
    /// in the `expr.ty` annotations using the final substitution.
    pub(super) fn resolve_all_types(&self, program: &mut Program) {
        self.resolve_decl_types(&mut program.decls);
    }

    /// `resolve_all_types` for the declarations `decls`.
    pub(super) fn resolve_decl_types(&self, decls: &mut [Decl]) {
        for decl in decls {
            match decl {
                Decl::Fn(f) => self.resolve_expr_types(&mut f.body),
                Decl::TraitImpl(ti) => {
                    for m in &mut ti.methods {
                        self.resolve_expr_types(&mut m.body);
                    }
                }
                Decl::Trait(t) => {
                    for m in t.methods.iter_mut().filter(|m| !m.is_signature_only) {
                        self.resolve_expr_types(&mut m.body);
                    }
                }
                Decl::Let { value, .. } => self.resolve_expr_types(value),
                _ => {}
            }
        }
    }

    fn resolve_expr_types(&self, expr: &mut Expr) {
        expr.for_each_expr_mut(&mut |expr| {
            if let Some(ty) = &expr.ty {
                expr.ty = Some(self.apply(ty));
            }
            self.select(expr);
        });
    }

    /// Write what the field accesses of `expr` (itself, or the callee of
    /// the call it is) mean, the types being final: the compiler's
    /// `Selection`.
    fn select(&self, expr: &mut Expr) {
        use crate::ast::Selection;
        match &mut expr.kind {
            // (The callee is visited after its call: a field that is
            // called is marked here, and decided there.)
            ExprKind::Call(callee, _) => {
                if matches!(callee.kind, ExprKind::FieldAccess(..)) {
                    callee.sel = Some(Selection::FieldCall);
                }
            }
            ExprKind::Pipe(_, right) => {
                if matches!(right.kind, ExprKind::FieldAccess(..)) {
                    right.sel = Some(Selection::FieldCall);
                }
            }
            ExprKind::FieldAccess(recv, field, _) => {
                let called = expr.sel.take().is_some();
                // `Tr.m`: the method of the impl for the type the call
                // gives `Self`.
                if let Some(self_ty) = self.trait_call_self.get(&expr.span)
                    && let Some(crate::defs::Res::Def(id)) = expr.res
                    && let Some(tr) = self.trait_key(id)
                {
                    // (The receiver is the argument of type `Self`.)
                    let at = self.self_param(tr, *field).unwrap_or(0);
                    let at = u8::try_from(at).unwrap_or(u8::MAX);
                    expr.sel = Some(self.method_selection(tr, self_ty, at));
                    return;
                }
                // A receiver the checker gave no type is a name: a
                // module, a type (`m.f`, `Shape.Circle`, `Pt.make`).
                let Some(recv_ty) = &recv.ty else {
                    return;
                };
                let tr = match expr.res {
                    Some(crate::defs::Res::Def(id)) => self.trait_key(id),
                    _ => None,
                };
                expr.sel = Some(match tr {
                    Some(tr) => self.method_selection(tr, recv_ty, 0),
                    None if called => Selection::FieldCall,
                    None => Selection::Field,
                });
            }
            _ => {}
        }
    }

    /// What a call of a method of the trait `tr` on a receiver of the
    /// type `recv`, the argument `at` of the call, runs.
    fn method_selection(&self, tr: TraitKey, recv: &Type, at: u8) -> crate::ast::Selection {
        use crate::ast::Selection;
        let recv = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(recv));
        match &recv {
            Type::Var(_) | Type::Rigid(_) | Type::Error | Type::Never => {
                Selection::Dynamic { tr: tr.id, at }
            }
            // A type passed as a value: the method of the type it is.
            Type::Generic(name, _) if name.is_builtin(crate::defs::TYPE_OF) => {
                Selection::Dynamic { tr: tr.id, at }
            }
            // A trait the type has by its structure (the sealed three,
            // `Display` with no written impl) is the VM's own method;
            // any other is the method of the impl written for the type.
            _ => match self.type_name_for_impl(&recv) {
                Some(ty)
                    if !self.by_structure(tr, ty)
                        && self.tables.trait_impl_set.contains(&(tr, ty)) =>
                {
                    Selection::Impl {
                        tr: tr.id,
                        ty: ty.id,
                    }
                }
                _ => Selection::Native { tr: tr.id },
            },
        }
    }
}

#[cfg(test)]
mod tests;
