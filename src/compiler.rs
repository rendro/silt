//! AST-to-bytecode compiler for Silt.
//!
//! Walks the AST and emits stack-based bytecode into `Function` objects.
//! Phase 3: closures with upvalue capture (value-copy semantics),
//! let tuple destructuring, plus all Phase 2 features (function calls,
//! builtins, let bindings, string interpolation, match, pipes, lambdas).

use std::rc::Rc;

use crate::ast::{
    BinOp, Decl, Expr, ExprKind, ListElem, MatchArm, Pattern, Program, Stmt, StringPart, UnaryOp,
};
use crate::bytecode::{Chunk, Function, Op, UpvalueDesc, VmClosure};
use crate::lexer::Span;
use crate::value::Value;

// ── Compiler context ──────────────────────────────────────────────────

/// Per-function compilation state.
struct CompileContext {
    function: Function,
    locals: Vec<Local>,
    scope_depth: usize,
    /// Upvalue descriptors for this function/closure.
    upvalues: Vec<UpvalueDesc>,
}

impl CompileContext {
    fn new(name: String, arity: u8) -> Self {
        Self {
            function: Function::new(name, arity),
            locals: Vec::new(),
            scope_depth: 0,
            upvalues: Vec::new(),
        }
    }
}

struct Local {
    name: String,
    depth: usize,
    slot: u16,
    /// Whether this local is captured by a nested closure.
    #[allow(dead_code)]
    captured: bool,
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

                // Store the function as a VmClosure constant in the enclosing chunk.
                let vm_closure = Rc::new(VmClosure {
                    function: Rc::new(func),
                    upvalues: vec![],
                });
                let closure_val = Value::VmClosure(vm_closure);
                let fi = self.current_chunk().add_constant(closure_val);
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
                // Skip type/trait/import declarations silently.
                Ok(())
            }
        }
    }

    // ── Statements ────────────────────────────────────────────────

    fn compile_stmt(&mut self, stmt: &Stmt, is_last: bool) -> Result<(), String> {
        match stmt {
            Stmt::Let { pattern, value, .. } => {
                self.compile_expr(value)?;
                let span = value.span;

                match pattern {
                    Pattern::Ident(name) => {
                        let slot = self.add_local(name.clone());
                        // The compiled expression left the value on TOS, which IS
                        // the local's stack slot. SetLocal copies TOS into the slot
                        // (they're the same position for sequential allocation).
                        // The value stays on the stack -- locals are stack-resident.
                        self.current_chunk().emit_op(Op::SetLocal, span);
                        self.current_chunk().emit_u16(slot, span);
                        // Don't pop: the value stays as the local slot.
                        // If this is the last statement, push Unit as the block result.
                        if is_last {
                            self.current_chunk().emit_op(Op::Unit, span);
                        }
                    }
                    Pattern::Tuple(pats) => {
                        // let (a, b) = expr
                        // Reserve a hidden local slot for the tuple so subsequent
                        // locals get correct slot numbers.
                        let tuple_slot = self.add_local("__tuple__".into());
                        self.current_chunk().emit_op(Op::SetLocal, span);
                        self.current_chunk().emit_u16(tuple_slot, span);

                        // For each sub-pattern, push a copy of the tuple, destructure
                        // the element, and bind it to a local. DestructTuple peeks
                        // TOS, so after GetLocal + DestructTuple the stack has:
                        //   [..., tuple(slot), tuple_copy, element]
                        // We reserve a dummy slot for tuple_copy, then set the
                        // named local to the element value.
                        for (i, pat) in pats.iter().enumerate() {
                            match pat {
                                Pattern::Ident(name) => {
                                    self.current_chunk().emit_op(Op::GetLocal, span);
                                    self.current_chunk().emit_u16(tuple_slot, span);
                                    self.current_chunk().emit_op(Op::DestructTuple, span);
                                    self.current_chunk().emit_u8(i as u8, span);
                                    let _copy_slot = self.add_local("__destruct_copy__".into());
                                    let elem_slot = self.add_local(name.clone());
                                    self.current_chunk().emit_op(Op::SetLocal, span);
                                    self.current_chunk().emit_u16(elem_slot, span);
                                }
                                Pattern::Wildcard => {
                                    // No binding needed.
                                }
                                _ => {
                                    return Err("unsupported nested pattern in let tuple destructuring".into());
                                }
                            }
                        }
                        if is_last {
                            self.current_chunk().emit_op(Op::Unit, span);
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
                // Check if this is a module-qualified builtin call like list.map(...)
                if let Some(builtin_name) = self.extract_builtin_name(callee) {
                    // Emit arguments first
                    for arg in args {
                        self.compile_expr(arg)?;
                    }
                    let argc = args.len() as u8;
                    let name_idx = self
                        .current_chunk()
                        .add_constant(Value::String(builtin_name));
                    self.current_chunk().emit_op(Op::CallBuiltin, span);
                    self.current_chunk().emit_u16(name_idx, span);
                    self.current_chunk().emit_u8(argc, span);
                } else {
                    // Normal function call
                    self.compile_expr(callee)?;
                    for arg in args {
                        self.compile_expr(arg)?;
                    }
                    let argc = args.len() as u8;
                    self.current_chunk().emit_op(Op::Call, span);
                    self.current_chunk().emit_u8(argc, span);
                }
            }

            ExprKind::FieldAccess(expr, field) => {
                // Check if this is a module-qualified name like list.map
                if let ExprKind::Ident(module) = &expr.kind {
                    let qualified = format!("{module}.{field}");
                    let name_idx =
                        self.current_chunk().add_constant(Value::String(qualified));
                    self.current_chunk().emit_op(Op::GetGlobal, span);
                    self.current_chunk().emit_u16(name_idx, span);
                } else {
                    // Compile the expression and access field
                    self.compile_expr(expr)?;
                    let name_idx =
                        self.current_chunk().add_constant(Value::String(field.clone()));
                    self.current_chunk().emit_op(Op::GetField, span);
                    self.current_chunk().emit_u16(name_idx, span);
                }
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

            ExprKind::Match { expr, arms } => {
                self.compile_match(expr.as_deref(), arms, span)?;
            }

            ExprKind::Lambda { params, body } => {
                let arity = params.len() as u8;

                // Push a new context for the lambda body.
                self.contexts
                    .push(CompileContext::new("<lambda>".into(), arity));

                // Add parameters as locals.
                for param in params {
                    match &param.pattern {
                        Pattern::Ident(name) => {
                            self.add_local(name.clone());
                        }
                        _ => {
                            return Err("unsupported parameter pattern in lambda".into());
                        }
                    }
                }

                // Compile the lambda body.
                self.compile_expr(body)?;
                self.current_chunk().emit_op(Op::Return, span);

                let ctx = self.contexts.pop().unwrap();
                let upvalue_descs = ctx.upvalues.clone();
                let func = ctx.function;

                let vm_closure = Rc::new(VmClosure {
                    function: Rc::new(func),
                    upvalues: vec![],
                });
                let closure_val = Value::VmClosure(vm_closure);
                let fi = self.current_chunk().add_constant(closure_val);

                if upvalue_descs.is_empty() {
                    // No upvalues: just push the constant directly.
                    self.current_chunk().emit_op(Op::Constant, span);
                    self.current_chunk().emit_u16(fi, span);
                } else {
                    // Has upvalues: emit MakeClosure with descriptors.
                    self.current_chunk().emit_op(Op::MakeClosure, span);
                    self.current_chunk().emit_u16(fi, span);
                    self.current_chunk().emit_u8(upvalue_descs.len() as u8, span);
                    for desc in &upvalue_descs {
                        self.current_chunk().emit_u8(if desc.is_local { 1 } else { 0 }, span);
                        self.current_chunk().emit_u8(desc.index, span);
                    }
                }
            }

            ExprKind::Tuple(elems) => {
                for elem in elems {
                    self.compile_expr(elem)?;
                }
                self.current_chunk().emit_op(Op::MakeTuple, span);
                self.current_chunk().emit_u8(elems.len() as u8, span);
            }

            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) => self.compile_expr(e)?,
                        ListElem::Spread(_) => {
                            return Err("spread in list literals not yet supported in compiler".into());
                        }
                    }
                }
                let count = elems.len() as u16;
                self.current_chunk().emit_op(Op::MakeList, span);
                self.current_chunk().emit_u16(count, span);
            }

            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.compile_expr(k)?;
                    self.compile_expr(v)?;
                }
                let pair_count = pairs.len() as u16;
                self.current_chunk().emit_op(Op::MakeMap, span);
                self.current_chunk().emit_u16(pair_count, span);
            }

            ExprKind::SetLit(elems) => {
                for elem in elems {
                    self.compile_expr(elem)?;
                }
                let count = elems.len() as u16;
                self.current_chunk().emit_op(Op::MakeSet, span);
                self.current_chunk().emit_u16(count, span);
            }

            ExprKind::Range(start, end) => {
                self.compile_expr(start)?;
                self.compile_expr(end)?;
                self.current_chunk().emit_op(Op::MakeRange, span);
            }

            ExprKind::Pipe(left, right) => {
                // val |> f(args) --> f(val, args)
                // val |> f       --> f(val)
                self.compile_pipe(left, right, span)?;
            }

            ExprKind::QuestionMark(inner) => {
                self.compile_expr(inner)?;
                self.current_chunk().emit_op(Op::QuestionMark, span);
            }

            ExprKind::RecordCreate { name, fields } => {
                // Push field values in order
                let field_names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
                for (_, val) in fields {
                    self.compile_expr(val)?;
                }
                let type_name_idx = self.current_chunk().add_constant(Value::String(name.clone()));
                self.current_chunk().emit_op(Op::MakeRecord, span);
                self.current_chunk().emit_u16(type_name_idx, span);
                self.current_chunk().emit_u8(field_names.len() as u8, span);
                for fname in &field_names {
                    let field_idx = self.current_chunk().add_constant(Value::String(fname.clone()));
                    self.current_chunk().emit_u16(field_idx, span);
                }
            }

            ExprKind::RecordUpdate { expr, fields } => {
                self.compile_expr(expr)?;
                let field_names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
                for (_, val) in fields {
                    self.compile_expr(val)?;
                }
                self.current_chunk().emit_op(Op::RecordUpdate, span);
                self.current_chunk().emit_u8(field_names.len() as u8, span);
                for fname in &field_names {
                    let field_idx = self.current_chunk().add_constant(Value::String(fname.clone()));
                    self.current_chunk().emit_u16(field_idx, span);
                }
            }

            ExprKind::Loop { bindings, body } => {
                self.compile_loop(bindings, body, span)?;
            }

            ExprKind::Recur(args) => {
                for arg in args {
                    self.compile_expr(arg)?;
                }
                self.current_chunk().emit_op(Op::Recur, span);
                self.current_chunk().emit_u8(args.len() as u8, span);
            }

            // All expression kinds are handled above. If new ones are added,
            // the match will become non-exhaustive and the compiler will error.
        }

        Ok(())
    }

    // ── Match compilation ────────────────────────────────────────

    fn compile_match(
        &mut self,
        scrutinee: Option<&Expr>,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<(), String> {
        // If there's a scrutinee, compile it and leave it on TOS.
        if let Some(scrutinee_expr) = scrutinee {
            self.compile_expr(scrutinee_expr)?;
        }

        // For each arm, we test the pattern, jump over if it doesn't match,
        // compile the body, then jump to the end.
        let mut end_jumps = Vec::new();

        for arm in arms {
            // For each arm:
            //   1. Test the pattern against TOS (scrutinee is peeked, not consumed)
            //   2. If no match, jump to next arm
            //   3. Bind pattern variables
            //   4. Pop scrutinee, compile body
            //   5. Jump to end

            let next_arm_jump = self.compile_pattern_test(&arm.pattern, span)?;

            // Guard (if present)
            let guard_jump = if let Some(guard) = &arm.guard {
                self.compile_expr(guard)?;
                let j = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Some(j)
            } else {
                None
            };

            // Begin a scope for pattern bindings
            self.begin_scope();

            // Bind pattern variables
            if scrutinee.is_some() {
                self.compile_pattern_bindings(&arm.pattern, span)?;
            }

            // Pop the scrutinee (it's been peeked during tests)
            if scrutinee.is_some() {
                self.current_chunk().emit_op(Op::Pop, span);
            }

            // Compile the arm body
            self.compile_expr(&arm.body)?;

            self.end_scope(span);

            // Jump to end
            let end_jump = self.current_chunk().emit_jump(Op::Jump, span);
            end_jumps.push(end_jump);

            // Patch the jumps for no-match
            if let Some(gj) = guard_jump {
                self.current_chunk().patch_jump(gj);
            }
            for nj in next_arm_jump {
                self.current_chunk().patch_jump(nj);
            }
        }

        // If no arm matched, push Unit as default
        if scrutinee.is_some() {
            self.current_chunk().emit_op(Op::Pop, span); // pop scrutinee
        }
        self.current_chunk().emit_op(Op::Unit, span);

        // Patch all end jumps
        for ej in end_jumps {
            self.current_chunk().patch_jump(ej);
        }

        Ok(())
    }

    /// Compile a pattern test. Returns a list of jump offsets to patch
    /// (they should jump to the next arm if the test fails).
    fn compile_pattern_test(
        &mut self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<Vec<usize>, String> {
        match pattern {
            Pattern::Wildcard | Pattern::Ident(_) => {
                // Always matches
                Ok(vec![])
            }
            Pattern::Int(n) => {
                let idx = self.current_chunk().add_constant(Value::Int(*n));
                self.current_chunk().emit_op(Op::TestEqual, span);
                self.current_chunk().emit_u16(idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }
            Pattern::Float(n) => {
                let idx = self.current_chunk().add_constant(Value::Float(*n));
                self.current_chunk().emit_op(Op::TestEqual, span);
                self.current_chunk().emit_u16(idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }
            Pattern::Bool(b) => {
                self.current_chunk().emit_op(Op::TestBool, span);
                self.current_chunk().emit_u8(if *b { 1 } else { 0 }, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }
            Pattern::StringLit(s) => {
                let idx = self.current_chunk().add_constant(Value::String(s.clone()));
                self.current_chunk().emit_op(Op::TestEqual, span);
                self.current_chunk().emit_u16(idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }
            Pattern::Constructor(name, _) => {
                let idx = self.current_chunk().add_constant(Value::String(name.clone()));
                self.current_chunk().emit_op(Op::TestTag, span);
                self.current_chunk().emit_u16(idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }
            Pattern::Tuple(pats) => {
                self.current_chunk().emit_op(Op::TestTupleLen, span);
                self.current_chunk().emit_u8(pats.len() as u8, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }
            Pattern::Range(lo, hi) => {
                let lo_idx = self.current_chunk().add_constant(Value::Int(*lo));
                let hi_idx = self.current_chunk().add_constant(Value::Int(*hi));
                self.current_chunk().emit_op(Op::TestIntRange, span);
                self.current_chunk().emit_u16(lo_idx, span);
                self.current_chunk().emit_u16(hi_idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }
            Pattern::Or(pats) => {
                // For or-patterns, we test each sub-pattern and jump to the body
                // if ANY of them match.
                let mut fail_jumps = Vec::new();
                let mut success_jumps = Vec::new();
                for (i, pat) in pats.iter().enumerate() {
                    let sub_fails = self.compile_pattern_test(pat, span)?;
                    if i < pats.len() - 1 {
                        // If this sub-pattern matched, jump to success
                        let success = self.current_chunk().emit_jump(Op::Jump, span);
                        success_jumps.push(success);
                        // Patch the failure jumps for this sub-pattern to try next
                        for fj in sub_fails {
                            self.current_chunk().patch_jump(fj);
                        }
                    } else {
                        // Last sub-pattern: its failures are the overall failures
                        fail_jumps = sub_fails;
                    }
                }
                // Patch success jumps to here
                for sj in success_jumps {
                    self.current_chunk().patch_jump(sj);
                }
                Ok(fail_jumps)
            }
            _ => {
                // For other patterns (List, Record, etc.) -- treat as always-match for now
                // and let runtime handle it
                Ok(vec![])
            }
        }
    }

    /// Compile pattern bindings: destructure the value on TOS into local variables.
    fn compile_pattern_bindings(
        &mut self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<(), String> {
        match pattern {
            Pattern::Ident(name) => {
                // Peek the scrutinee and bind it
                let val = self.peek_tos(span);
                let slot = self.add_local(name.clone());
                self.current_chunk().emit_op(Op::SetLocal, span);
                self.current_chunk().emit_u16(slot, span);
                self.current_chunk().emit_op(Op::Pop, span);
                let _ = val; // SetLocal peeks, so we pop the extra
            }
            Pattern::Constructor(_, fields) => {
                for (i, field_pat) in fields.iter().enumerate() {
                    if let Pattern::Ident(name) = field_pat {
                        // Destructure variant field
                        self.current_chunk().emit_op(Op::DestructVariant, span);
                        self.current_chunk().emit_u8(i as u8, span);
                        let slot = self.add_local(name.clone());
                        self.current_chunk().emit_op(Op::SetLocal, span);
                        self.current_chunk().emit_u16(slot, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    } else if let Pattern::Wildcard = field_pat {
                        // skip
                    } else {
                        // For nested patterns, we'd need recursive destructuring
                        // For now, skip
                    }
                }
            }
            Pattern::Tuple(pats) => {
                for (i, pat) in pats.iter().enumerate() {
                    if let Pattern::Ident(name) = pat {
                        self.current_chunk().emit_op(Op::DestructTuple, span);
                        self.current_chunk().emit_u8(i as u8, span);
                        let slot = self.add_local(name.clone());
                        self.current_chunk().emit_op(Op::SetLocal, span);
                        self.current_chunk().emit_u16(slot, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                }
            }
            Pattern::Wildcard
            | Pattern::Int(_)
            | Pattern::Float(_)
            | Pattern::Bool(_)
            | Pattern::StringLit(_)
            | Pattern::Range(..)
            | Pattern::Or(_) => {
                // No bindings to create
            }
            _ => {
                // Other patterns: skip bindings for now
            }
        }
        Ok(())
    }

    /// Helper: emit a Dup to peek TOS (for pattern binding)
    fn peek_tos(&mut self, span: Span) {
        self.current_chunk().emit_op(Op::Dup, span);
    }

    // ── Pipe compilation ─────────────────────────────────────────

    fn compile_pipe(
        &mut self,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Result<(), String> {
        // Compile the left value first
        self.compile_expr(left)?;

        // val |> f(args) -> f(val, args)
        // val |> f       -> f(val)
        match &right.kind {
            ExprKind::Call(callee, args) => {
                // Check if callee is a module-qualified builtin
                if let Some(builtin_name) = self.extract_builtin_name(callee) {
                    // left is already on stack, compile remaining args
                    for arg in args {
                        self.compile_expr(arg)?;
                    }
                    let argc = (args.len() + 1) as u8;
                    let name_idx = self
                        .current_chunk()
                        .add_constant(Value::String(builtin_name));
                    self.current_chunk().emit_op(Op::CallBuiltin, span);
                    self.current_chunk().emit_u16(name_idx, span);
                    self.current_chunk().emit_u8(argc, span);
                } else {
                    // Compile callee, then rearrange: [val, func, arg1, ...] -> Call
                    // We need func first, then val, then args.
                    // Strategy: compile func to get [val, func], then swap.
                    // We don't have a Swap op, so let's restructure:
                    // Actually the stack is: [... val], compile callee -> [... val, func]
                    // We need [... func, val, arg1, ...argN]
                    // Use a temporary local? No, let's just reorder compilation:
                    //   1. Save val in stack
                    //   2. Compile callee
                    //   3. Re-push val (from underneath)
                    //   4. Compile args
                    //   5. Call
                    // But we can't easily re-order the stack without Swap.
                    //
                    // Simpler approach: compile callee first, then emit left under it.
                    // Actually, left is already compiled. Let's just do callee then reorganize.
                    //
                    // The cleanest approach for now: compile it as GetGlobal("func"),
                    // then rearrange. Since we don't have Swap, let's just pop val,
                    // compile func + val + args, call.
                    //
                    // Actually, let's just store val in a temp. We can use the stack:
                    // Stack: [... val]
                    // Compile callee -> [... val, func]
                    // We need: [... func, val, args]
                    // Since we don't have Swap, let's just pop val to a temp global.
                    // No, that's ugly.
                    //
                    // Best approach: restructure so we compile in the right order.
                    // Pipe `a |> f(b)` compiles to: push f, push a, push b, Call 2
                    // But we already pushed a. So let's pop it, compile f, push a, push args.

                    // Pop the already-pushed left value to re-push later
                    // Actually, the val is on the stack. Let's compile callee,
                    // which will be on top of val. Then swap would help, but
                    // let's use a different strategy:
                    // Store val as a local, compile callee, get local, compile args, call.

                    // Actually simplest for now: store in a hidden local
                    let pipe_slot = self.add_local("__pipe_val__".into());
                    self.current_chunk().emit_op(Op::SetLocal, span);
                    self.current_chunk().emit_u16(pipe_slot, span);
                    self.current_chunk().emit_op(Op::Pop, span);

                    // Now compile callee
                    self.compile_expr(callee)?;
                    // Push val back
                    self.current_chunk().emit_op(Op::GetLocal, span);
                    self.current_chunk().emit_u16(pipe_slot, span);
                    // Push remaining args
                    for arg in args {
                        self.compile_expr(arg)?;
                    }
                    let argc = (args.len() + 1) as u8;
                    self.current_chunk().emit_op(Op::Call, span);
                    self.current_chunk().emit_u8(argc, span);
                }
            }
            ExprKind::Ident(_) => {
                // val |> f -> f(val)
                // Stack: [... val]
                // Need: [... func, val], Call 1
                let pipe_slot = self.add_local("__pipe_val__".into());
                self.current_chunk().emit_op(Op::SetLocal, span);
                self.current_chunk().emit_u16(pipe_slot, span);
                self.current_chunk().emit_op(Op::Pop, span);

                self.compile_expr(right)?;
                self.current_chunk().emit_op(Op::GetLocal, span);
                self.current_chunk().emit_u16(pipe_slot, span);
                self.current_chunk().emit_op(Op::Call, span);
                self.current_chunk().emit_u8(1, span);
            }
            _ => {
                // val |> expr -> expr(val)
                let pipe_slot = self.add_local("__pipe_val__".into());
                self.current_chunk().emit_op(Op::SetLocal, span);
                self.current_chunk().emit_u16(pipe_slot, span);
                self.current_chunk().emit_op(Op::Pop, span);

                self.compile_expr(right)?;
                self.current_chunk().emit_op(Op::GetLocal, span);
                self.current_chunk().emit_u16(pipe_slot, span);
                self.current_chunk().emit_op(Op::Call, span);
                self.current_chunk().emit_u8(1, span);
            }
        }
        Ok(())
    }

    // ── Loop compilation ─────────────────────────────────────────

    fn compile_loop(
        &mut self,
        bindings: &[(String, Expr)],
        body: &Expr,
        span: Span,
    ) -> Result<(), String> {
        self.begin_scope();

        // Compile initial values and store in locals
        for (name, init) in bindings {
            self.compile_expr(init)?;
            let slot = self.add_local(name.clone());
            self.current_chunk().emit_op(Op::SetLocal, span);
            self.current_chunk().emit_u16(slot, span);
            self.current_chunk().emit_op(Op::Pop, span);
        }

        // Record the loop start for JumpBack
        let loop_start = self.current_chunk().len();

        // Compile body
        self.compile_expr(body)?;

        // The body should either have used `recur` (which jumps back) or fallen through.
        // If it fell through, the result is on the stack.

        self.end_scope(span);
        // After Recur, we need a JumpBack to loop_start. But Recur is compiled inline
        // in the body. For now, the loop body's result is the final value.
        // TODO: properly handle loop/recur with JumpBack
        let _ = loop_start;

        Ok(())
    }

    // ── Helper: extract builtin name ─────────────────────────────

    /// If the callee is a module-qualified builtin (e.g., `list.map`),
    /// return the qualified name.
    fn extract_builtin_name(&self, callee: &Expr) -> Option<String> {
        if let ExprKind::FieldAccess(expr, field) = &callee.kind {
            if let ExprKind::Ident(module) = &expr.kind {
                return Some(format!("{module}.{field}"));
            }
        }
        None
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
        // The block's result is on TOS. The locals sit below it in the stack.
        // With a stack VM, we can't pop them from under TOS without a Swap op.
        // For now, the VM's frame mechanism reclaims them on function return.
        // This is correct as long as we don't reuse local slots across scopes.
        let _ = pop_count;
        let _ = span;

        self.ctx_mut().scope_depth -= 1;
    }

    fn add_local(&mut self, name: String) -> u16 {
        let depth = self.ctx().scope_depth;
        let slot = self.ctx().locals.len() as u16;
        self.ctx_mut().locals.push(Local { name, depth, slot, captured: false });
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

    /// Resolve a variable as an upvalue by walking enclosing compile contexts.
    ///
    /// If the variable is found as a local in an enclosing scope, it is captured
    /// as an upvalue (is_local = true). If the enclosing scope already has it as
    /// an upvalue, it is chained through (is_local = false, transitive capture).
    fn resolve_upvalue(&mut self, name: &str) -> Option<u8> {
        let current_idx = self.contexts.len() - 1;
        if current_idx == 0 {
            return None; // Top-level script has no enclosing scope.
        }
        self.resolve_upvalue_in(name, current_idx)
    }

    fn resolve_upvalue_in(&mut self, name: &str, context_index: usize) -> Option<u8> {
        if context_index == 0 {
            return None; // No more enclosing scopes.
        }
        let enclosing_idx = context_index - 1;

        // Check if the variable is a local in the immediately enclosing context.
        let local_slot = {
            let enclosing = &self.contexts[enclosing_idx];
            enclosing.locals.iter().rev().find_map(|l| {
                if l.name == name { Some(l.slot) } else { None }
            })
        };

        if let Some(slot) = local_slot {
            // Mark the local as captured.
            let enclosing = &mut self.contexts[enclosing_idx];
            if let Some(local) = enclosing.locals.iter_mut().find(|l| l.name == name) {
                local.captured = true;
            }
            // Add an upvalue descriptor to the current context.
            return Some(self.add_upvalue(context_index, UpvalueDesc {
                is_local: true,
                index: slot as u8,
            }));
        }

        // Not a local in the enclosing scope -- try recursively as an upvalue.
        if let Some(parent_upvalue_idx) = self.resolve_upvalue_in(name, enclosing_idx) {
            // The enclosing scope has it as an upvalue. Chain it.
            return Some(self.add_upvalue(context_index, UpvalueDesc {
                is_local: false,
                index: parent_upvalue_idx,
            }));
        }

        None
    }

    /// Add an upvalue descriptor to a context, deduplicating.
    fn add_upvalue(&mut self, context_index: usize, desc: UpvalueDesc) -> u8 {
        let ctx = &mut self.contexts[context_index];
        // Check if we already have this exact upvalue.
        for (i, existing) in ctx.upvalues.iter().enumerate() {
            if existing.is_local == desc.is_local && existing.index == desc.index {
                return i as u8;
            }
        }
        let index = ctx.upvalues.len();
        assert!(index <= u8::MAX as usize, "too many upvalues");
        ctx.upvalues.push(desc);
        ctx.function.upvalue_count = ctx.upvalues.len() as u8;
        index as u8
    }
}
