//! AST-to-bytecode compiler for Silt.
//!
//! Walks the AST and emits stack-based bytecode into `Function` objects.
//! Phase 1: arithmetic expressions, simple let/fn, function calls.

use crate::ast::{
    BinOp, Decl, Expr, ExprKind, Pattern, Program, Stmt, StringPart, UnaryOp,
};
use crate::bytecode::{Chunk, Function, Op};
use crate::lexer::Span;
use crate::value::Value;

// ── Compiler context ──────────────────────────────────────────────────

/// Per-function compilation state.
struct CompileContext {
    function: Function,
    locals: Vec<Local>,
    scope_depth: usize,
}

impl CompileContext {
    fn new(name: String, arity: u8) -> Self {
        Self {
            function: Function::new(name, arity),
            locals: Vec::new(),
            scope_depth: 0,
        }
    }
}

struct Local {
    name: String,
    depth: usize,
    slot: u16,
}

// ── Compiler ──────────────────────────────────────────────────────────

pub struct Compiler {
    contexts: Vec<CompileContext>,
    /// Accumulated compiled functions (one per `Decl::Fn`).
    functions: Vec<Function>,
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            contexts: Vec::new(),
            functions: Vec::new(),
        }
    }

    // ── Public entry point ────────────────────────────────────────

    /// Compile a full program, returning all functions.
    ///
    /// The first function in the returned `Vec` is the top-level `<script>`,
    /// which ends with `GetGlobal "main" ; Call 0 ; Return`.
    pub fn compile_program(&mut self, program: &Program) -> Result<Vec<Function>, String> {
        // Push a top-level script context.
        self.contexts.push(CompileContext::new("<script>".into(), 0));

        for decl in &program.decls {
            self.compile_decl(decl)?;
        }

        // Emit: GetGlobal "main", Call 0, Return
        let span = Span::new(0, 0);
        let name_idx = self.current_chunk().add_constant(Value::String("main".into()));
        self.current_chunk().emit_op(Op::GetGlobal, span);
        self.current_chunk().emit_u16(name_idx, span);
        self.current_chunk().emit_op(Op::Call, span);
        self.current_chunk().emit_u8(0, span);
        self.current_chunk().emit_op(Op::Return, span);

        let script = self.contexts.pop().unwrap().function;

        // Build the result: script first, then all compiled functions.
        let mut result = vec![script];
        result.append(&mut self.functions);
        Ok(result)
    }

    // ── Declarations ──────────────────────────────────────────────

    fn compile_decl(&mut self, decl: &Decl) -> Result<(), String> {
        match decl {
            Decl::Fn(fn_decl) => {
                let arity = fn_decl.params.len() as u8;
                let span = fn_decl.span;

                // Push a new context for the function body.
                self.contexts
                    .push(CompileContext::new(fn_decl.name.clone(), arity));

                // Add parameters as locals.
                for param in &fn_decl.params {
                    match &param.pattern {
                        Pattern::Ident(name) => {
                            self.add_local(name.clone());
                        }
                        _ => {
                            return Err(format!(
                                "unsupported parameter pattern in function '{}'",
                                fn_decl.name
                            ));
                        }
                    }
                }

                // Compile the function body.
                self.compile_expr(&fn_decl.body)?;

                // Emit Return.
                self.current_chunk().emit_op(Op::Return, span);

                // Pop the context, recovering the compiled function.
                let ctx = self.contexts.pop().unwrap();
                let func = ctx.function;
                let func_index = self.functions.len();
                self.functions.push(func);

                // In the enclosing context, emit code to register the function
                // as a global with its name.
                let func_val = Value::Int(func_index as i64); // placeholder constant encoding
                let fi = self.current_chunk().add_constant(func_val);
                self.current_chunk().emit_op(Op::Constant, span);
                self.current_chunk().emit_u16(fi, span);

                let name_idx = self
                    .current_chunk()
                    .add_constant(Value::String(fn_decl.name.clone()));
                self.current_chunk().emit_op(Op::SetGlobal, span);
                self.current_chunk().emit_u16(name_idx, span);
                self.current_chunk().emit_op(Op::Pop, span);

                Ok(())
            }

            Decl::Let {
                pattern,
                value,
                span,
                ..
            } => {
                let span = *span;
                self.compile_expr(value)?;

                match pattern {
                    Pattern::Ident(name) => {
                        let name_idx =
                            self.current_chunk().add_constant(Value::String(name.clone()));
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    _ => {
                        return Err("unsupported pattern in top-level let".into());
                    }
                }

                Ok(())
            }

            Decl::Type(_) | Decl::Trait(_) | Decl::TraitImpl(_) | Decl::Import(_) => {
                // Phase 1: skip type/trait/import declarations silently.
                Ok(())
            }
        }
    }

    // ── Statements ────────────────────────────────────────────────

    fn compile_stmt(&mut self, stmt: &Stmt, is_last: bool) -> Result<(), String> {
        match stmt {
            Stmt::Let { pattern, value, .. } => {
                self.compile_expr(value)?;

                match pattern {
                    Pattern::Ident(name) => {
                        let slot = self.add_local(name.clone());
                        let span = value.span;
                        self.current_chunk().emit_op(Op::SetLocal, span);
                        self.current_chunk().emit_u16(slot, span);
                        // Let statements produce Unit for the block value if they are last.
                        if is_last {
                            self.current_chunk().emit_op(Op::Unit, span);
                        } else {
                            self.current_chunk().emit_op(Op::Pop, span);
                        }
                    }
                    _ => {
                        return Err("unsupported pattern in let statement".into());
                    }
                }

                Ok(())
            }

            Stmt::Expr(expr) => {
                self.compile_expr(expr)?;
                if !is_last {
                    self.current_chunk().emit_op(Op::Pop, expr.span);
                }
                // If last, leave the value on the stack as the block's result.
                Ok(())
            }

            Stmt::When { .. } | Stmt::WhenBool { .. } => {
                // Phase 1: not yet supported.
                Err("when statements not yet supported in compiler".into())
            }
        }
    }

    // ── Expressions ───────────────────────────────────────────────

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), String> {
        let span = expr.span;

        match &expr.kind {
            ExprKind::Int(n) => {
                let idx = self.current_chunk().add_constant(Value::Int(*n));
                self.current_chunk().emit_op(Op::Constant, span);
                self.current_chunk().emit_u16(idx, span);
            }

            ExprKind::Float(n) => {
                let idx = self.current_chunk().add_constant(Value::Float(*n));
                self.current_chunk().emit_op(Op::Constant, span);
                self.current_chunk().emit_u16(idx, span);
            }

            ExprKind::Bool(b) => {
                if *b {
                    self.current_chunk().emit_op(Op::True, span);
                } else {
                    self.current_chunk().emit_op(Op::False, span);
                }
            }

            ExprKind::StringLit(s) => {
                let idx = self.current_chunk().add_constant(Value::String(s.clone()));
                self.current_chunk().emit_op(Op::Constant, span);
                self.current_chunk().emit_u16(idx, span);
            }

            ExprKind::Unit => {
                self.current_chunk().emit_op(Op::Unit, span);
            }

            ExprKind::Binary(left, op, right) => {
                self.compile_expr(left)?;
                self.compile_expr(right)?;
                let opcode = match op {
                    BinOp::Add => Op::Add,
                    BinOp::Sub => Op::Sub,
                    BinOp::Mul => Op::Mul,
                    BinOp::Div => Op::Div,
                    BinOp::Mod => Op::Mod,
                    BinOp::Eq => Op::Eq,
                    BinOp::Neq => Op::Neq,
                    BinOp::Lt => Op::Lt,
                    BinOp::Gt => Op::Gt,
                    BinOp::Leq => Op::Leq,
                    BinOp::Geq => Op::Geq,
                    BinOp::And => Op::And,
                    BinOp::Or => Op::Or,
                };
                self.current_chunk().emit_op(opcode, span);
            }

            ExprKind::Unary(op, operand) => {
                self.compile_expr(operand)?;
                let opcode = match op {
                    UnaryOp::Neg => Op::Negate,
                    UnaryOp::Not => Op::Not,
                };
                self.current_chunk().emit_op(opcode, span);
            }

            ExprKind::Block(stmts) => {
                self.begin_scope();

                if stmts.is_empty() {
                    // Empty block evaluates to Unit.
                    self.current_chunk().emit_op(Op::Unit, span);
                } else {
                    let last_idx = stmts.len() - 1;
                    for (i, stmt) in stmts.iter().enumerate() {
                        self.compile_stmt(stmt, i == last_idx)?;
                    }
                }

                self.end_scope(span);
            }

            ExprKind::Ident(name) => {
                if let Some(slot) = self.resolve_local(name) {
                    self.current_chunk().emit_op(Op::GetLocal, span);
                    self.current_chunk().emit_u16(slot, span);
                } else if let Some(idx) = self.resolve_upvalue(name) {
                    self.current_chunk().emit_op(Op::GetUpvalue, span);
                    self.current_chunk().emit_u8(idx, span);
                } else {
                    let name_idx =
                        self.current_chunk().add_constant(Value::String(name.clone()));
                    self.current_chunk().emit_op(Op::GetGlobal, span);
                    self.current_chunk().emit_u16(name_idx, span);
                }
            }

            ExprKind::Call(callee, args) => {
                self.compile_expr(callee)?;
                for arg in args {
                    self.compile_expr(arg)?;
                }
                let argc = args.len() as u8;
                self.current_chunk().emit_op(Op::Call, span);
                self.current_chunk().emit_u8(argc, span);
            }

            ExprKind::StringInterp(parts) => {
                let mut count: u8 = 0;
                for part in parts {
                    match part {
                        StringPart::Literal(s) => {
                            let idx =
                                self.current_chunk().add_constant(Value::String(s.clone()));
                            self.current_chunk().emit_op(Op::Constant, span);
                            self.current_chunk().emit_u16(idx, span);
                            count += 1;
                        }
                        StringPart::Expr(e) => {
                            self.compile_expr(e)?;
                            self.current_chunk().emit_op(Op::DisplayValue, span);
                            count += 1;
                        }
                    }
                }
                self.current_chunk().emit_op(Op::StringConcat, span);
                self.current_chunk().emit_u8(count, span);
            }

            ExprKind::Return(maybe_expr) => {
                if let Some(e) = maybe_expr {
                    self.compile_expr(e)?;
                } else {
                    self.current_chunk().emit_op(Op::Unit, span);
                }
                self.current_chunk().emit_op(Op::Return, span);
            }

            // Phase 1: unsupported expression kinds produce errors.
            _ => {
                return Err(format!(
                    "unsupported expression kind in compiler: {:?}",
                    std::mem::discriminant(&expr.kind)
                ));
            }
        }

        Ok(())
    }

    // ── Context & scope helpers ───────────────────────────────────

    fn ctx(&self) -> &CompileContext {
        self.contexts.last().expect("no active compile context")
    }

    fn ctx_mut(&mut self) -> &mut CompileContext {
        self.contexts.last_mut().expect("no active compile context")
    }

    fn current_chunk(&mut self) -> &mut Chunk {
        &mut self.ctx_mut().function.chunk
    }

    fn begin_scope(&mut self) {
        self.ctx_mut().scope_depth += 1;
    }

    fn end_scope(&mut self, span: Span) {
        let depth = self.ctx().scope_depth;
        // Pop locals belonging to the scope we are leaving.
        let mut pop_count: u8 = 0;
        while self
            .ctx()
            .locals
            .last()
            .map_or(false, |l| l.depth >= depth)
        {
            self.ctx_mut().locals.pop();
            pop_count += 1;
        }
        // We need to keep the block's result value on TOS while discarding
        // the locals beneath it. The result is already on TOS, and the
        // locals are below it. We need to pop them out from under TOS.
        //
        // Strategy: for each local to discard, emit a swap-then-pop sequence.
        // But we don't have Swap. Simpler: we pop them *before* the block
        // result is computed. Actually, the block result *is* already on TOS.
        // The locals sit below it in the stack. The VM uses frame-relative
        // addressing, so those slots will simply be abandoned when the scope
        // ends. But we still need the stack to be clean.
        //
        // The simplest Phase-1 approach: if there are locals to pop, emit
        // PopN to remove them from under TOS. But PopN pops from TOS.
        // We need to be careful: the block's result is on TOS, locals below.
        //
        // Actually, with a stack VM, the locals *are* on the stack below TOS.
        // We can't directly pop them from under TOS without a special opcode.
        // For Phase 1 let's not pop scope locals at all -- the VM's frame
        // mechanism will reclaim them when the function returns. This is
        // correct as long as we don't reuse local slots across scopes, which
        // we don't in Phase 1.
        //
        // TODO(phase2): emit proper scope cleanup for nested scopes.
        let _ = pop_count;
        let _ = span;

        self.ctx_mut().scope_depth -= 1;
    }

    fn add_local(&mut self, name: String) -> u16 {
        let depth = self.ctx().scope_depth;
        let slot = self.ctx().locals.len() as u16;
        self.ctx_mut().locals.push(Local { name, depth, slot });
        slot
    }

    fn resolve_local(&self, name: &str) -> Option<u16> {
        let ctx = self.ctx();
        // Search from the innermost local outward.
        for local in ctx.locals.iter().rev() {
            if local.name == name {
                return Some(local.slot);
            }
        }
        None
    }

    fn resolve_upvalue(&self, _name: &str) -> Option<u8> {
        // Phase 1: upvalue resolution is a stub.
        // A full implementation would walk enclosing contexts.
        None
    }
}
