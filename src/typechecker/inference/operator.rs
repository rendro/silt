//! The type of an operator's application (arms of `infer_expr_kind`).

use super::*;

impl TypeChecker {
    /// `infer_expr_kind` for a binary operator.
    pub(super) fn infer_binary(&mut self, expr: &mut Expr, env: &mut TypeEnv, span: Span) -> Type {
        let ExprKind::Binary(lhs, op, rhs) = &mut expr.kind else {
            unreachable!("infer_binary of another form");
        };
        let op = *op;
        let lhs_span = lhs.span;
        let rhs_span = rhs.span;
        let lt = self.infer_expr(lhs, env);
        let rt = self.infer_expr(rhs, env);

        match op {
            // An operator is its trait: the operands have one
            // type, which owes it (`Number`, `Equal`, `Compare`).
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Mod | BinOp::Div => {
                let op_str = match op {
                    BinOp::Add => "'+'",
                    BinOp::Sub => "'-'",
                    BinOp::Mul => "'*'",
                    BinOp::Mod => "'%'",
                    _ => "'/'",
                };
                // (`Type::Error` on a reported operand keeps an
                // ascribed `let` from reporting the result again.)
                match self.check_operator("Number", op_str, &lt, &rt, lhs_span, rhs_span, span) {
                    true => lt,
                    false => Type::Error,
                }
            }
            BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq => {
                let (tr, op_str) = match op {
                    BinOp::Eq => ("Equal", "'=='"),
                    BinOp::Neq => ("Equal", "'!='"),
                    BinOp::Lt => ("Compare", "'<'"),
                    BinOp::Gt => ("Compare", "'>'"),
                    BinOp::Leq => ("Compare", "'<='"),
                    _ => ("Compare", "'>='"),
                };
                self.check_operator(tr, op_str, &lt, &rt, lhs_span, rhs_span, span);
                Type::Bool
            }
            BinOp::And | BinOp::Or => {
                self.unify(&lt, &Type::Bool, lhs_span);
                self.unify(&rt, &Type::Bool, rhs_span);
                Type::Bool
            }
        }
    }

    /// `infer_expr_kind` for a unary operator.
    pub(super) fn infer_unary(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        let ExprKind::Unary(op, operand) = &mut expr.kind else {
            unreachable!("infer_unary of another form");
        };
        let op = *op;
        let operand_span = operand.span;
        let t = self.infer_expr(operand, env);
        match op {
            UnaryOp::Neg => {
                let reported = self.errors.len();
                self.want_operand("Number", &t, "unary '-'", operand_span);
                match self.errors.len() == reported {
                    true => t,
                    false => Type::Error,
                }
            }
            UnaryOp::Not => {
                self.unify(&t, &Type::Bool, operand_span);
                Type::Bool
            }
        }
    }
}
