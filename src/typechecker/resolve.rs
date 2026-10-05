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
        matches!(self.apply(ty), Type::Var(_))
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
        out.insert(expr.span.start);
        match &expr.kind {
            ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
                Self::collect_sub_spans(l, out);
                Self::collect_sub_spans(r, out);
            }
            ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Return(Some(e))
            | ExprKind::FieldAccess(e, _, _)
            | ExprKind::Ascription(e, _) => {
                Self::collect_sub_spans(e, out);
            }
            ExprKind::Call(callee, args) => {
                Self::collect_sub_spans(callee, out);
                for a in args {
                    Self::collect_sub_spans(a, out);
                }
            }
            ExprKind::Block(stmts) => {
                for s in stmts {
                    Self::collect_stmt_sub_spans(s, out);
                }
            }
            ExprKind::Lambda { body, .. } => {
                Self::collect_sub_spans(body, out);
            }
            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                if let Some(s) = scrutinee {
                    Self::collect_sub_spans(s, out);
                }
                for arm in arms {
                    if let Some(ref guard) = arm.guard {
                        Self::collect_sub_spans(guard, out);
                    }
                    Self::collect_sub_spans(&arm.body, out);
                }
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) | ListElem::Spread(e) => {
                            Self::collect_sub_spans(e, out);
                        }
                    }
                }
            }
            ExprKind::Tuple(elems) | ExprKind::SetLit(elems) => {
                for e in elems {
                    Self::collect_sub_spans(e, out);
                }
            }
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    Self::collect_sub_spans(k, out);
                    Self::collect_sub_spans(v, out);
                }
            }
            ExprKind::RecordCreate { fields, .. } => {
                for (_, e) in fields {
                    Self::collect_sub_spans(e, out);
                }
            }
            ExprKind::RecordUpdate { expr, fields } => {
                Self::collect_sub_spans(expr, out);
                for (_, e) in fields {
                    Self::collect_sub_spans(e, out);
                }
            }
            ExprKind::AnonRecord { spread, fields } => {
                if let Some(s) = spread {
                    Self::collect_sub_spans(s, out);
                }
                for (_, e) in fields {
                    Self::collect_sub_spans(e, out);
                }
            }
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        Self::collect_sub_spans(e, out);
                    }
                }
            }
            ExprKind::Loop { bindings, body } => {
                for (_, _, e) in bindings {
                    Self::collect_sub_spans(e, out);
                }
                Self::collect_sub_spans(body, out);
            }
            ExprKind::Recur(args) => {
                for a in args {
                    Self::collect_sub_spans(a, out);
                }
            }
            _ => {} // Int, Float, Bool, StringLit, Ident, Unit, Return(None)
        }
    }

    fn collect_stmt_sub_spans(stmt: &Stmt, out: &mut std::collections::HashSet<u32>) {
        match stmt {
            Stmt::Let { value, .. } => Self::collect_sub_spans(value, out),
            Stmt::When {
                expr, else_body, ..
            } => {
                Self::collect_sub_spans(expr, out);
                Self::collect_sub_spans(else_body, out);
            }
            Stmt::WhenBool {
                condition,
                else_body,
            } => {
                Self::collect_sub_spans(condition, out);
                Self::collect_sub_spans(else_body, out);
            }
            Stmt::Expr(e) => Self::collect_sub_spans(e, out),
        }
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
        match &expr.kind {
            ExprKind::Block(stmts) => {
                self.check_unresolved_in_block(stmts);
                // Also recurse into sub-expressions of each statement
                for stmt in stmts {
                    match stmt {
                        Stmt::Let { value, .. } => self.check_unresolved_in_expr(value),
                        Stmt::When {
                            expr, else_body, ..
                        } => {
                            self.check_unresolved_in_expr(expr);
                            self.check_unresolved_in_expr(else_body);
                        }
                        Stmt::WhenBool {
                            condition,
                            else_body,
                        } => {
                            self.check_unresolved_in_expr(condition);
                            self.check_unresolved_in_expr(else_body);
                        }
                        Stmt::Expr(e) => self.check_unresolved_in_expr(e),
                    }
                }
            }
            ExprKind::Lambda { body, .. } => {
                self.check_unresolved_in_expr(body);
            }
            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                if let Some(s) = scrutinee {
                    self.check_unresolved_in_expr(s);
                }
                for arm in arms {
                    if let Some(ref guard) = arm.guard {
                        self.check_unresolved_in_expr(guard);
                    }
                    self.check_unresolved_in_expr(&arm.body);
                }
            }
            ExprKind::Call(callee, args) => {
                self.check_unresolved_in_expr(callee);
                for a in args {
                    self.check_unresolved_in_expr(a);
                }
            }
            ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
                self.check_unresolved_in_expr(l);
                self.check_unresolved_in_expr(r);
            }
            ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Return(Some(e))
            | ExprKind::FieldAccess(e, _, _)
            | ExprKind::Ascription(e, _) => {
                self.check_unresolved_in_expr(e);
            }
            ExprKind::Loop { bindings, body } => {
                for (_, _, e) in bindings {
                    self.check_unresolved_in_expr(e);
                }
                self.check_unresolved_in_expr(body);
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) | ListElem::Spread(e) => {
                            self.check_unresolved_in_expr(e);
                        }
                    }
                }
            }
            ExprKind::Tuple(elems) | ExprKind::SetLit(elems) => {
                for e in elems {
                    self.check_unresolved_in_expr(e);
                }
            }
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.check_unresolved_in_expr(k);
                    self.check_unresolved_in_expr(v);
                }
            }
            ExprKind::RecordCreate { fields, .. } => {
                for (_, e) in fields {
                    self.check_unresolved_in_expr(e);
                }
            }
            ExprKind::RecordUpdate { expr, fields } => {
                self.check_unresolved_in_expr(expr);
                for (_, e) in fields {
                    self.check_unresolved_in_expr(e);
                }
            }
            ExprKind::AnonRecord { spread, fields } => {
                if let Some(s) = spread {
                    self.check_unresolved_in_expr(s);
                }
                for (_, e) in fields {
                    self.check_unresolved_in_expr(e);
                }
            }
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.check_unresolved_in_expr(e);
                    }
                }
            }
            ExprKind::Recur(args) => {
                for a in args {
                    self.check_unresolved_in_expr(a);
                }
            }
            _ => {} // Int, Float, Bool, StringLit, Ident, Unit, Return(None)
        }
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
                // follow-up is misleading (round 62 G4).
                if self.value_already_errored(value) {
                    continue;
                }

                // Collect names bound by this let pattern
                let bound_names = collect_pattern_vars(pattern);
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
        match &expr.kind {
            ExprKind::Ident(n) => *n == name,
            ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
                Self::expr_references_name(l, name) || Self::expr_references_name(r, name)
            }
            ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Return(Some(e))
            | ExprKind::FieldAccess(e, _, _)
            | ExprKind::Ascription(e, _) => Self::expr_references_name(e, name),
            ExprKind::Call(callee, args) => {
                Self::expr_references_name(callee, name)
                    || args.iter().any(|a| Self::expr_references_name(a, name))
            }
            ExprKind::Block(stmts) => stmts.iter().any(|s| Self::stmt_references_name(s, name)),
            ExprKind::Lambda { body, .. } => Self::expr_references_name(body, name),
            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                scrutinee
                    .as_ref()
                    .map(|s| Self::expr_references_name(s, name))
                    .unwrap_or(false)
                    || arms.iter().any(|arm| {
                        arm.guard
                            .as_ref()
                            .map(|g| Self::expr_references_name(g, name))
                            .unwrap_or(false)
                            || Self::expr_references_name(&arm.body, name)
                    })
            }
            ExprKind::List(elems) => elems.iter().any(|elem| match elem {
                ListElem::Single(e) | ListElem::Spread(e) => Self::expr_references_name(e, name),
            }),
            ExprKind::Tuple(elems) | ExprKind::SetLit(elems) => {
                elems.iter().any(|e| Self::expr_references_name(e, name))
            }
            ExprKind::Map(pairs) => pairs.iter().any(|(k, v)| {
                Self::expr_references_name(k, name) || Self::expr_references_name(v, name)
            }),
            ExprKind::RecordCreate { fields, .. } => fields
                .iter()
                .any(|(_, e)| Self::expr_references_name(e, name)),
            ExprKind::RecordUpdate { expr, fields } => {
                Self::expr_references_name(expr, name)
                    || fields
                        .iter()
                        .any(|(_, e)| Self::expr_references_name(e, name))
            }
            ExprKind::AnonRecord { spread, fields } => {
                spread
                    .as_ref()
                    .map(|s| Self::expr_references_name(s, name))
                    .unwrap_or(false)
                    || fields
                        .iter()
                        .any(|(_, e)| Self::expr_references_name(e, name))
            }
            ExprKind::StringInterp(parts) => parts.iter().any(|part| match part {
                StringPart::Expr(e) => Self::expr_references_name(e, name),
                _ => false,
            }),
            ExprKind::Loop { bindings, body } => {
                bindings
                    .iter()
                    .any(|(_, _, e)| Self::expr_references_name(e, name))
                    || Self::expr_references_name(body, name)
            }
            ExprKind::Recur(args) => args.iter().any(|a| Self::expr_references_name(a, name)),
            ExprKind::Return(None) => false,
            _ => false, // Int, Float, Bool, StringLit, Unit
        }
    }

    // ── Post-inference type resolution ─────────────────────────────────

    /// After all passes, walk the AST and resolve any remaining type variables
    /// in the `expr.ty` annotations using the final substitution.
    pub(super) fn resolve_all_types(&self, program: &mut Program) {
        for decl in &mut program.decls {
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
        each_expr_mut(expr, &mut |expr| {
            if let Some(ty) = &expr.ty {
                expr.ty = Some(self.apply(ty));
            }
            // A method call resolved in the deferred pass: its trait.
            if matches!(expr.kind, ExprKind::FieldAccess(..))
                && let Some(t) = self.deferred_method_traits.get(&expr.span)
            {
                expr.res = Some(crate::defs::Res::Def(t.id.0));
            }
        });
    }
}

/// Call `f` with `expr` and then with each expression inside it.
pub(super) fn each_expr_mut(expr: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    f(expr);
    match &mut expr.kind {
        ExprKind::Binary(l, _, r) => {
            each_expr_mut(l, f);
            each_expr_mut(r, f);
        }
        ExprKind::Unary(_, e)
        | ExprKind::QuestionMark(e)
        | ExprKind::Ascription(e, _)
        | ExprKind::Return(Some(e)) => {
            each_expr_mut(e, f);
        }
        ExprKind::Call(callee, args) => {
            each_expr_mut(callee, f);
            for a in args {
                each_expr_mut(a, f);
            }
        }
        ExprKind::List(elems) => {
            for elem in elems {
                match elem {
                    ListElem::Single(e) => each_expr_mut(e, f),
                    ListElem::Spread(e) => each_expr_mut(e, f),
                }
            }
        }
        ExprKind::Tuple(elems) => {
            for e in elems {
                each_expr_mut(e, f);
            }
        }
        ExprKind::Map(pairs) => {
            for (k, v) in pairs {
                each_expr_mut(k, f);
                each_expr_mut(v, f);
            }
        }
        ExprKind::SetLit(elems) => {
            for e in elems {
                each_expr_mut(e, f);
            }
        }
        ExprKind::Lambda { body, .. } => {
            each_expr_mut(body, f);
        }
        ExprKind::Match {
            expr: scrutinee,
            arms,
        } => {
            if let Some(s) = scrutinee {
                each_expr_mut(s, f);
            }
            for arm in arms {
                if let Some(ref mut guard) = arm.guard {
                    each_expr_mut(guard, f);
                }
                each_expr_mut(&mut arm.body, f);
            }
        }
        ExprKind::Block(stmts) => {
            for stmt in stmts {
                match stmt {
                    Stmt::Let { value, .. } => each_expr_mut(value, f),
                    Stmt::When {
                        expr, else_body, ..
                    } => {
                        each_expr_mut(expr, f);
                        each_expr_mut(else_body, f);
                    }
                    Stmt::WhenBool {
                        condition,
                        else_body,
                    } => {
                        each_expr_mut(condition, f);
                        each_expr_mut(else_body, f);
                    }
                    Stmt::Expr(e) => each_expr_mut(e, f),
                }
            }
        }
        ExprKind::Pipe(l, r) => {
            each_expr_mut(l, f);
            each_expr_mut(r, f);
        }
        ExprKind::Range(l, r) => {
            each_expr_mut(l, f);
            each_expr_mut(r, f);
        }
        ExprKind::FieldAccess(e, _, _) => each_expr_mut(e, f),
        ExprKind::RecordCreate { fields, .. } => {
            for (_, e) in fields {
                each_expr_mut(e, f);
            }
        }
        ExprKind::RecordUpdate { expr, fields } => {
            each_expr_mut(expr, f);
            for (_, e) in fields {
                each_expr_mut(e, f);
            }
        }
        ExprKind::AnonRecord { spread, fields } => {
            if let Some(s) = spread {
                each_expr_mut(s, f);
            }
            for (_, e) in fields {
                each_expr_mut(e, f);
            }
        }
        ExprKind::StringInterp(parts) => {
            for part in parts {
                if let StringPart::Expr(e) = part {
                    each_expr_mut(e, f);
                }
            }
        }
        ExprKind::Loop { bindings, body } => {
            for (_, _, e) in bindings {
                each_expr_mut(e, f);
            }
            each_expr_mut(body, f);
        }
        ExprKind::Recur(args) => {
            for a in args {
                each_expr_mut(a, f);
            }
        }
        _ => {} // Int, Float, Bool, StringLit, Ident, Unit, Return(None)
    }
}

#[cfg(test)]
mod tests;
