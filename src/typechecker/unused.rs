//! The unused-value error.
//!
//! A statement that is not the last of its block has no one to give its
//! value to, so it must have none: its type is `()` or `Never`.
//! Anything else is an error, and `let _ = ...` is how a value is
//! discarded on purpose.
//!
//! A call whose type nothing decides (`fail()` with
//! `fn fail() { panic("...") }`, `g()` with `g` a parameter of unknown
//! type) is a statement, so its type is `()`: the checker fixes it,
//! before the definition the statement is in is generalised
//! (`fix_statement_calls`). A value that is not a call (an operator
//! expression, a literal, a name, a field, a constructor) is unused
//! whatever its type.

use super::*;

impl TypeChecker {
    /// Note the statement `stmt` of type `ty`, which is not the last of
    /// its block: a call whose type is still unknown waits for
    /// `fix_statement_calls`.
    pub(super) fn note_statement(&mut self, stmt: &Stmt, ty: &Type) {
        if let Stmt::Expr(e) = stmt
            && matches!(self.apply(ty), Type::Var(_))
            && self.is_call_like(e)
        {
            self.statement_calls.push(ty.clone());
        }
    }

    /// Give `()` to the calls that stand as statements and whose type is
    /// a variable of the scope `exit_level` just left (with `all`, at
    /// the end of the module: whatever variable it still is). A variable
    /// of an outer binding waits: a later definition may decide it.
    /// Returns whether a type was decided: what the scope owes for it
    /// is to be checked again.
    pub(super) fn fix_statement_calls(&mut self, all: bool) -> bool {
        let mut fixed = false;
        for ty in std::mem::take(&mut self.statement_calls) {
            match self.apply(&ty) {
                Type::Var(v) if all || self.tables.vars.is_generalizable(v) => {
                    fixed |= self.unify_types(&ty, &Type::Unit).is_ok();
                }
                Type::Var(_) => self.statement_calls.push(ty),
                _ => {}
            }
        }
        fixed
    }

    /// Whether the value of `e` comes from calls alone: a call, a pipe,
    /// a `?`, or a `match`, block or loop whose every result does. A
    /// variant applied to values is a value, not a call.
    fn is_call_like(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Call(callee, _) => !self.names_variant(callee),
            ExprKind::Pipe(_, right) => match &right.kind {
                ExprKind::Call(callee, _) => !self.names_variant(callee),
                _ => !self.names_variant(right),
            },
            ExprKind::QuestionMark(inner) | ExprKind::Ascription(inner, _) => {
                self.is_call_like(inner)
            }
            ExprKind::Match { arms, .. } => arms.iter().all(|arm| self.is_call_like(&arm.body)),
            ExprKind::Block(stmts) => match stmts.last() {
                Some(Stmt::Expr(last)) => self.is_call_like(last),
                _ => true,
            },
            ExprKind::Loop { body, .. } => self.is_call_like(body),
            ExprKind::Return(_) | ExprKind::Recur(_) => true,
            _ => false,
        }
    }

    fn names_variant(&self, callee: &Expr) -> bool {
        matches!(
            callee.res,
            Some(crate::defs::Res::Def(id))
                if self.def(id).is_some_and(|def| matches!(def.kind, crate::defs::DefKind::Variant { .. }))
        )
    }

    /// Report every statement that leaves a value unused, the types
    /// being final.
    pub(super) fn check_unused_values(&mut self, program: &mut Program) {
        let mut found: Vec<Diagnostic> = Vec::new();
        for decl in &mut program.decls {
            match decl {
                Decl::Fn(f) => self.unused_in(&mut f.body, &mut found),
                Decl::TraitImpl(ti) => {
                    for m in &mut ti.methods {
                        self.unused_in(&mut m.body, &mut found);
                    }
                }
                Decl::Trait(t) => {
                    for m in t.methods.iter_mut().filter(|m| !m.is_signature_only) {
                        self.unused_in(&mut m.body, &mut found);
                    }
                }
                Decl::Let { value, .. } => self.unused_in(value, &mut found),
                _ => {}
            }
        }
        self.errors.extend(found);
    }

    /// The type of a statement as the rule reads it: an associated type
    /// of a known type is the type its impl binds (`<Printer as
    /// Sink>::Out` is `()` where the impl says `type Out = ()`).
    fn statement_type(&self, ty: &Type) -> Type {
        match self.apply(ty) {
            ty @ Type::AssocProj { .. } => {
                crate::types::canonical::canonicalize(&self.tables.resolver, &ty)
            }
            ty => ty,
        }
    }

    fn unused_in(&self, expr: &mut Expr, found: &mut Vec<Diagnostic>) {
        resolve::each_expr_mut(expr, &mut |expr| {
            let ExprKind::Block(stmts) = &expr.kind else {
                return;
            };
            let Some((_, rest)) = stmts.split_last() else {
                return;
            };
            for (i, stmt) in rest.iter().enumerate() {
                let Stmt::Expr(e) = stmt else { continue };
                // (A statement the checker gave no type is in a body it
                // did not check.)
                let Some(ty) = &e.ty else { continue };
                let message = match self.statement_type(ty) {
                    Type::Unit | Type::Never | Type::Error => continue,
                    Type::Var(_) => {
                        "this value is unused; write `let _ = ...` to discard it".to_string()
                    }
                    // An associated type of a type that is not known
                    // here (`s.next()` with `s: a where a: Source`): the
                    // caller's type decides it, so it is a value.
                    ty @ Type::AssocProj { .. } if !free_vars_in(&ty).is_empty() => {
                        "this value is unused; write `let _ = ...` to discard it".to_string()
                    }
                    ty if ty.is_builtin("Result") => {
                        "this `Result` is unused: an error in it would go unseen; handle it, \
                         return it with `?`, or write `let _ = ...`"
                            .to_string()
                    }
                    ty => format!(
                        "this `{}` value is unused; write `let _ = ...` to discard it",
                        self.show_type(&ty)
                    ),
                };
                let at = Span::point(e.span.file, e.span.start);
                let mut d = Diagnostic::error(Code::UnusedValue, e.span, message)
                    .with_fix("Discard with `let _ =`", vec![(at, "let _ = ".to_string())]);
                if i > 0 && starts_with_minus(e) {
                    d = d.with_help(
                        "a line that starts with `-` is a statement of its own; to subtract \
                         from the line above, end that line with the `-`",
                    );
                }
                found.push(d);
            }
        });
    }
}

/// Whether the text of `e` starts with a unary minus.
fn starts_with_minus(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Unary(UnaryOp::Neg, _) => true,
        ExprKind::Binary(left, _, _)
        | ExprKind::Pipe(left, _)
        | ExprKind::Range(left, _)
        | ExprKind::Call(left, _)
        | ExprKind::FieldAccess(left, _, _)
        | ExprKind::QuestionMark(left)
        | ExprKind::Ascription(left, _) => starts_with_minus(left),
        _ => false,
    }
}
