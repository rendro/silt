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
        // Top-level Decl::Let
        for decl in &program.decls {
            if let Decl::Let {
                value, ty, span, ..
            } = decl
                && ty.is_none()
                && value
                    .ty
                    .as_ref()
                    .map(|t| self.is_bare_type_var(t))
                    .unwrap_or(false)
            {
                self.error(
                    "could not fully determine the type of this expression; consider adding a type annotation".to_string(),
                    *span,
                );
            }
        }

        // Function bodies and trait impl method bodies
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => self.check_unresolved_in_expr(&f.body),
                Decl::TraitImpl(ti) => {
                    for m in &ti.methods {
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
                        Stmt::When { expr, else_body, .. } => {
                            self.check_unresolved_in_expr(expr);
                            self.check_unresolved_in_expr(else_body);
                        }
                        Stmt::WhenBool { condition, else_body } => {
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
            ExprKind::Match { expr: scrutinee, arms } => {
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
            | ExprKind::FieldAccess(e, _) => {
                self.check_unresolved_in_expr(e);
            }
            ExprKind::Loop { bindings, body } => {
                for (_, e) in bindings {
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
            if let Stmt::Let {
                pattern,
                ty,
                value,
            } = stmt
            {
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
                        .any(|s| Self::stmt_references_name(s, name))
                });

                if !used_later {
                    self.error(
                        "could not fully determine the type of this expression; consider adding a type annotation".to_string(),
                        value.span,
                    );
                }
            }
        }
    }

    /// Check if a statement contains any reference to the given name.
    fn stmt_references_name(stmt: &Stmt, name: &str) -> bool {
        match stmt {
            Stmt::Let { value, .. } => Self::expr_references_name(value, name),
            Stmt::When { expr, else_body, .. } => {
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
    fn expr_references_name(expr: &Expr, name: &str) -> bool {
        match &expr.kind {
            ExprKind::Ident(n) => n == name,
            ExprKind::Binary(l, _, r) | ExprKind::Pipe(l, r) | ExprKind::Range(l, r) => {
                Self::expr_references_name(l, name) || Self::expr_references_name(r, name)
            }
            ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Return(Some(e))
            | ExprKind::FieldAccess(e, _) => Self::expr_references_name(e, name),
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
                ListElem::Single(e) | ListElem::Spread(e) => {
                    Self::expr_references_name(e, name)
                }
            }),
            ExprKind::Tuple(elems) | ExprKind::SetLit(elems) => {
                elems.iter().any(|e| Self::expr_references_name(e, name))
            }
            ExprKind::Map(pairs) => pairs.iter().any(|(k, v)| {
                Self::expr_references_name(k, name) || Self::expr_references_name(v, name)
            }),
            ExprKind::RecordCreate { fields, .. } => {
                fields.iter().any(|(_, e)| Self::expr_references_name(e, name))
            }
            ExprKind::RecordUpdate { expr, fields } => {
                Self::expr_references_name(expr, name)
                    || fields.iter().any(|(_, e)| Self::expr_references_name(e, name))
            }
            ExprKind::StringInterp(parts) => parts.iter().any(|part| match part {
                StringPart::Expr(e) => Self::expr_references_name(e, name),
                _ => false,
            }),
            ExprKind::Loop { bindings, body } => {
                bindings
                    .iter()
                    .any(|(_, e)| Self::expr_references_name(e, name))
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
                _ => {}
            }
        }
    }

    fn resolve_expr_types(&self, expr: &mut Expr) {
        if let Some(ty) = &expr.ty {
            expr.ty = Some(self.apply(ty));
        }
        match &mut expr.kind {
            ExprKind::Binary(l, _, r) => {
                self.resolve_expr_types(l);
                self.resolve_expr_types(r);
            }
            ExprKind::Unary(_, e)
            | ExprKind::QuestionMark(e)
            | ExprKind::Ascription(e, _)
            | ExprKind::Return(Some(e)) => {
                self.resolve_expr_types(e);
            }
            ExprKind::Call(callee, args) => {
                self.resolve_expr_types(callee);
                for a in args {
                    self.resolve_expr_types(a);
                }
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) => self.resolve_expr_types(e),
                        ListElem::Spread(e) => self.resolve_expr_types(e),
                    }
                }
            }
            ExprKind::Tuple(elems) => {
                for e in elems {
                    self.resolve_expr_types(e);
                }
            }
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.resolve_expr_types(k);
                    self.resolve_expr_types(v);
                }
            }
            ExprKind::SetLit(elems) => {
                for e in elems {
                    self.resolve_expr_types(e);
                }
            }
            ExprKind::Lambda { body, .. } => {
                self.resolve_expr_types(body);
            }
            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                if let Some(s) = scrutinee {
                    self.resolve_expr_types(s);
                }
                for arm in arms {
                    if let Some(ref mut guard) = arm.guard {
                        self.resolve_expr_types(guard);
                    }
                    self.resolve_expr_types(&mut arm.body);
                }
            }
            ExprKind::Block(stmts) => {
                for stmt in stmts {
                    match stmt {
                        Stmt::Let { value, .. } => self.resolve_expr_types(value),
                        Stmt::When {
                            expr, else_body, ..
                        } => {
                            self.resolve_expr_types(expr);
                            self.resolve_expr_types(else_body);
                        }
                        Stmt::WhenBool {
                            condition,
                            else_body,
                        } => {
                            self.resolve_expr_types(condition);
                            self.resolve_expr_types(else_body);
                        }
                        Stmt::Expr(e) => self.resolve_expr_types(e),
                    }
                }
            }
            ExprKind::Pipe(l, r) => {
                self.resolve_expr_types(l);
                self.resolve_expr_types(r);
            }
            ExprKind::Range(l, r) => {
                self.resolve_expr_types(l);
                self.resolve_expr_types(r);
            }
            ExprKind::FieldAccess(e, _) => self.resolve_expr_types(e),
            ExprKind::RecordCreate { fields, .. } => {
                for (_, e) in fields {
                    self.resolve_expr_types(e);
                }
            }
            ExprKind::RecordUpdate { expr, fields } => {
                self.resolve_expr_types(expr);
                for (_, e) in fields {
                    self.resolve_expr_types(e);
                }
            }
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.resolve_expr_types(e);
                    }
                }
            }
            ExprKind::Loop { bindings, body } => {
                for (_, e) in bindings {
                    self.resolve_expr_types(e);
                }
                self.resolve_expr_types(body);
            }
            ExprKind::Recur(args) => {
                for a in args {
                    self.resolve_expr_types(a);
                }
            }
            _ => {} // Int, Float, Bool, StringLit, Ident, Unit, Return(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn check_errors(input: &str) -> Vec<TypeError> {
        let tokens = crate::lexer::Lexer::new(input).tokenize().expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens).parse_program().expect("parse error");
        check(&mut program)
    }

    fn assert_no_errors(input: &str) {
        let errors = check_errors(input);
        let hard: Vec<_> = errors.iter().filter(|e| e.severity == Severity::Error).collect();
        assert!(hard.is_empty(), "expected no type errors, got:\n{}", hard.iter().map(|e| format!("  {e}")).collect::<Vec<_>>().join("\n"));
    }

    fn assert_has_error(input: &str, expected: &str) {
        let errors = check_errors(input);
        assert!(errors.iter().any(|e| e.message.contains(expected)), "expected error containing '{expected}', got: {:?}", errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    }

    // ── is_bare_type_var ────────────────────────────────────────────

    #[test]
    fn test_resolved_var_is_not_bare() {
        let mut checker = TypeChecker::new();
        let tv = checker.fresh_var();
        // Unify with Int, so it's resolved
        let span = crate::lexer::Span { line: 0, col: 0, offset: 0 };
        checker.unify(&tv, &Type::Int, span);
        assert!(!checker.is_bare_type_var(&tv));
    }

    #[test]
    fn test_unresolved_var_is_bare() {
        let mut checker = TypeChecker::new();
        let tv = checker.fresh_var();
        assert!(checker.is_bare_type_var(&tv));
    }

    // ── Unresolved type variable detection ──────────────────────────

    #[test]
    fn test_used_variable_no_false_positive() {
        // A let binding whose value type is initially unknown but resolved by usage
        assert_no_errors(r#"
fn main() {
  let x = []
  let y = list.append(x, 1)
  y
}
        "#);
    }

    #[test]
    fn test_annotated_let_not_flagged() {
        assert_no_errors(r#"
fn main() {
  let x: Int = 42
  x
}
        "#);
    }

    // ── stmt_references_name / expr_references_name ─────────────────

    #[test]
    fn test_stmt_references_name_in_let() {
        // If a later statement uses the variable, the unresolved check skips it
        assert_no_errors(r#"
fn identity(x) = x
fn main() {
  let x = identity(42)
  x + 1
}
        "#);
    }

    #[test]
    fn test_expr_references_in_nested_block() {
        assert_no_errors(r#"
fn main() {
  let f = { x -> x + 1 }
  f(42)
}
        "#);
    }

    // ── resolve_all_types applies substitution ──────────────────────

    #[test]
    fn test_type_annotations_resolved_after_check() {
        let input = r#"
fn double(x) = x * 2
fn main() { double(5) }
        "#;
        let tokens = crate::lexer::Lexer::new(input).tokenize().expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens).parse_program().expect("parse error");
        let errors = check(&mut program);
        assert!(errors.iter().filter(|e| e.severity == Severity::Error).count() == 0);
        // After checking, the function body should have resolved types (no bare Vars)
        for decl in &program.decls {
            if let Decl::Fn(f) = decl {
                if f.name == "double" {
                    if let Some(ty) = &f.body.ty {
                        assert!(!matches!(ty, Type::Var(_)), "body type should be resolved, got {ty}");
                    }
                }
            }
        }
    }

    // ── Edge case: function call return type resolved by context ────

    #[test]
    fn test_generic_call_resolved_by_context() {
        assert_no_errors(r#"
fn id(x) = x
fn main() {
  let n = id(42)
  n + 1
}
        "#);
    }
}
