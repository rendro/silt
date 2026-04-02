use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use regex::Regex;
use serde_json;

use crate::ast::*;
use crate::env::Env;
use crate::lexer::Span;
use crate::module::ModuleLoader;
use crate::scheduler::{Scheduler, TaskState};
#[allow(unused_imports)] // Foundation for future typed fast-paths
use crate::types::Type;
use crate::value::{Channel, Closure, TryReceiveResult, TrySendResult, Value};

// ── Call stack frame ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CallFrame {
    pub name: String,
    pub span: Span,
}

// ── Runtime error ────────────────────────────────────────────────────

pub enum RuntimeError {
    Error(String, Option<Span>),
    Return(Value),
    TailCall(Rc<Closure>, Vec<Value>),
    LoopRecur(Vec<Value>),
}

impl RuntimeError {
    /// Return the source span, if available.
    pub fn span(&self) -> Option<Span> {
        match self {
            RuntimeError::Error(_, span) => *span,
            _ => None,
        }
    }

    /// Return the error message, if this is an Error variant.
    pub fn message(&self) -> Option<&str> {
        match self {
            RuntimeError::Error(msg, _) => Some(msg),
            _ => None,
        }
    }
}

impl std::fmt::Debug for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Error(msg, _) => f.debug_tuple("Error").field(msg).finish(),
            RuntimeError::Return(val) => f.debug_tuple("Return").field(val).finish(),
            RuntimeError::TailCall(_, args) => {
                f.debug_tuple("TailCall").field(&"<closure>").field(args).finish()
            }
            RuntimeError::LoopRecur(args) => {
                f.debug_tuple("LoopRecur").field(args).finish()
            }
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Error(msg, Some(span)) => write!(f, "runtime error at {span}: {msg}"),
            RuntimeError::Error(msg, None) => write!(f, "runtime error: {msg}"),
            RuntimeError::Return(_) => write!(f, "unexpected return outside function"),
            RuntimeError::TailCall(_, _) => write!(f, "unhandled tail call"),
            RuntimeError::LoopRecur(_) => write!(f, "loop() used outside of loop body"),
        }
    }
}

type Result<T> = std::result::Result<T, RuntimeError>;

fn err(msg: impl Into<String>) -> RuntimeError {
    RuntimeError::Error(msg.into(), None)
}

fn err_at(msg: impl Into<String>, span: Span) -> RuntimeError {
    RuntimeError::Error(msg.into(), Some(span))
}

// ── Interpreter ──────────────────────────────────────────────────────

/// Number of expression evaluations between scheduler checks.
const YIELD_INTERVAL: usize = 1000;

/// Runtime method dispatch entry.
#[derive(Clone)]
enum RuntimeMethod {
    Closure(Rc<Closure>),
    Builtin(BuiltinTraitMethod),
}

#[derive(Clone, Copy)]
enum BuiltinTraitMethod {
    Display,
    Equal,
    Compare,
    Hash,
}

pub struct Interpreter {
    global: Env,
    /// Maps variant constructor names to their parent type name.
    variant_types: std::collections::HashMap<String, String>,
    /// Cooperative scheduler for concurrency.
    scheduler: RefCell<Scheduler>,
    /// Module loader for file-based imports.
    module_loader: RefCell<ModuleLoader>,
    /// Step counter for preemptive yielding to the scheduler.
    step_counter: std::cell::Cell<usize>,
    /// Call stack for error reporting.
    call_stack: RefCell<Vec<CallFrame>>,
    /// Method table: (type_name, method_name) → dispatch entry.
    method_table: std::collections::HashMap<(String, String), RuntimeMethod>,
    /// Temporary storage for builtin trait method receiver (set during access_field,
    /// consumed during dispatch_builtin for __trait.* methods).
    trait_method_receiver: RefCell<Option<Value>>,
}

impl Interpreter {
    pub fn new() -> Self {
        Self::with_project_root(PathBuf::from("."))
    }

    pub fn with_project_root(project_root: PathBuf) -> Self {
        let global = Env::new();
        register_builtins(&global);
        let method_table = Self::builtin_method_table();
        Self {
            global,
            variant_types: std::collections::HashMap::new(),
            scheduler: RefCell::new(Scheduler::new()),
            module_loader: RefCell::new(ModuleLoader::new(project_root)),
            step_counter: std::cell::Cell::new(0),
            call_stack: RefCell::new(Vec::new()),
            method_table,
            trait_method_receiver: RefCell::new(None),
        }
    }

    /// Build the builtin method table: auto-derived Display/Equal/Compare/Hash
    /// for all primitive and collection types.
    fn builtin_method_table() -> std::collections::HashMap<(String, String), RuntimeMethod> {
        let mut table = std::collections::HashMap::new();
        let types = ["Int", "Float", "Bool", "String", "Unit", "List", "Tuple", "Map"];
        let methods = [
            ("display", BuiltinTraitMethod::Display),
            ("equal", BuiltinTraitMethod::Equal),
            ("compare", BuiltinTraitMethod::Compare),
            ("hash", BuiltinTraitMethod::Hash),
        ];
        for type_name in &types {
            for (method_name, method) in &methods {
                table.insert(
                    (type_name.to_string(), method_name.to_string()),
                    RuntimeMethod::Builtin(*method),
                );
            }
        }
        table
    }

    /// Snapshot the current call stack (for error reporting).
    pub fn call_stack(&self) -> Vec<CallFrame> {
        self.call_stack.borrow().clone()
    }

    /// List user-defined names (excluding builtins) for REPL :env command.
    pub fn defined_names(&self) -> Vec<String> {
        let all = self.global.bindings_with_prefix("");
        let mut names: Vec<String> = all.into_iter()
            .filter(|(k, v)| {
                !matches!(v, Value::BuiltinFn(_))
                    && !k.contains('.')
                    && !matches!(k.as_str(),
                        "Ok" | "Err" | "Some" | "None" | "Stop" | "Continue"
                        | "Message" | "Closed" | "Empty")
            })
            .map(|(k, _)| k)
            .collect();
        names.sort();
        names
    }

    pub fn run(&mut self, program: &Program) -> Result<Value> {
        // First pass: register all top-level declarations
        for decl in &program.decls {
            self.register_decl(decl)?;
        }

        // Find and call main()
        match self.global.get("main") {
            Some(Value::Closure(c)) => {
                self.call_stack.borrow_mut().push(CallFrame {
                    name: "main".into(),
                    span: c.body.span,
                });
                let result = self.call_closure(&c, &[]);
                if result.is_ok() {
                    self.call_stack.borrow_mut().pop();
                }
                result
            }
            Some(Value::BuiltinFn(_)) => Err(err("main cannot be a builtin")),
            Some(_) => Err(err("main is not a function")),
            None => Err(err("no main() function found")),
        }
    }

    pub fn register_decl(&mut self, decl: &Decl) -> Result<()> {
        match decl {
            Decl::Fn(f) => {
                let closure = Value::Closure(Rc::new(Closure {
                    params: f.params.clone(),
                    body: f.body.clone(),
                    env: self.global.clone(),
                }));
                self.global.define(f.name.clone(), closure);
            }
            Decl::Type(td) => {
                self.register_type(td)?;
            }
            Decl::TraitImpl(ti) => {
                for method in &ti.methods {
                    let closure = Rc::new(Closure {
                        params: method.params.clone(),
                        body: method.body.clone(),
                        env: self.global.clone(),
                    });
                    // Register in method table (new system).
                    self.method_table.insert(
                        (ti.target_type.clone(), method.name.clone()),
                        RuntimeMethod::Closure(closure.clone()),
                    );
                    // Legacy: also register as "TypeName.method_name" in global env.
                    let key = format!("{}.{}", ti.target_type, method.name);
                    self.global.define(key, Value::Closure(closure));
                }
            }
            Decl::Trait(_) => {
                // Trait declarations just define the interface; nothing to do at runtime
            }
            Decl::Import(target) => {
                self.process_import(target)?;
            }
            Decl::Let { pattern, value, .. } => {
                let val = self.eval(value, &self.global.clone())?;
                self.bind_pattern(pattern, &val, &self.global)?;
            }
        }
        Ok(())
    }

    fn register_type(&mut self, td: &TypeDecl) -> Result<()> {
        match &td.body {
            TypeBody::Enum(variants) => {
                for variant in variants {
                    let name = variant.name.clone();
                    self.variant_types
                        .insert(name.clone(), td.name.clone());
                    if variant.fields.is_empty() {
                        self.global
                            .define(name.clone(), Value::Variant(name, Vec::new()));
                    } else {
                        let arity = variant.fields.len();
                        self.global
                            .define(name.clone(), Value::VariantConstructor(name, arity));
                    }
                }
            }
            TypeBody::Record(fields) => {
                // Register a constructor function for the record type
                let type_name = td.name.clone();
                let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
                let tname = type_name.clone();
                let fnames = field_names.clone();
                // The record constructor is invoked via `TypeName { field: value }` syntax,
                // not as a function. So nothing to register as a callable.
                // But let's store the field order for validation.
                let _ = (tname, fnames);
            }
        }
        Ok(())
    }

    // ── Import handling ───────────────────────────────────────────────

    fn process_import(&mut self, target: &ImportTarget) -> Result<()> {
        match target {
            ImportTarget::Module(name) => {
                // `import math` — make module.func accessible
                if ModuleLoader::is_builtin_module(name) {
                    // Builtin modules are already registered as "module.func" in global env.
                    // Nothing extra needed — field access fallback already resolves them.
                    return Ok(());
                }
                let exports = self.load_file_module(name)?;
                // Register all public names as "module.name" in the global env
                for pub_name in &exports.public_names {
                    let qualified = format!("{name}.{pub_name}");
                    if let Some(val) = exports.env.get(pub_name) {
                        self.global.define(qualified, val);
                    }
                }
                Ok(())
            }
            ImportTarget::Items(module_name, items) => {
                // `import math.{ add, Point }` — bring specific names directly into scope
                if ModuleLoader::is_builtin_module(module_name) {
                    // For builtin modules, copy "module.item" bindings to bare "item" names
                    for item in items {
                        let qualified = format!("{module_name}.{item}");
                        match self.global.get(&qualified) {
                            Some(val) => {
                                self.global.define(item.clone(), val);
                            }
                            None => {
                                return Err(err(format!(
                                    "module '{module_name}' has no exported item '{item}'"
                                )));
                            }
                        }
                    }
                    return Ok(());
                }
                let exports = self.load_file_module(module_name)?;
                for item in items {
                    if !exports.public_names.contains(item) {
                        return Err(err(format!(
                            "module '{module_name}' has no public item '{item}'"
                        )));
                    }
                    // If this is a type name with enum variants, import all its constructors
                    if let Some(variant_names) = exports.type_variants.get(item) {
                        for vn in variant_names {
                            if let Some(val) = exports.env.get(vn) {
                                self.global.define(vn.clone(), val);
                            }
                        }
                    }
                    // Import the item itself (for functions, or type name if in env)
                    if let Some(val) = exports.env.get(item) {
                        self.global.define(item.clone(), val);
                    }
                    // Note: type names may not be in the env directly (only constructors are).
                    // That's fine — we've already imported the constructors above.
                }
                Ok(())
            }
            ImportTarget::Alias(module_name, alias) => {
                // `import math as m` — make alias.func accessible
                if ModuleLoader::is_builtin_module(module_name) {
                    // Re-register all "module.func" bindings under "alias.func"
                    let prefix = format!("{module_name}.");
                    let bindings = self.global.bindings_with_prefix(&prefix);
                    for (key, val) in bindings {
                        let suffix = &key[prefix.len()..];
                        let aliased = format!("{alias}.{suffix}");
                        self.global.define(aliased, val);
                    }
                    return Ok(());
                }
                let exports = self.load_file_module(module_name)?;
                for pub_name in &exports.public_names {
                    let aliased = format!("{alias}.{pub_name}");
                    if let Some(val) = exports.env.get(pub_name) {
                        self.global.define(aliased, val);
                    }
                }
                Ok(())
            }
        }
    }

    /// Load a file-based module: parse the file, evaluate its declarations,
    /// and return the module exports.
    fn load_file_module(
        &mut self,
        module_name: &str,
    ) -> Result<crate::module::ModuleExports> {
        // Check cache first
        {
            let loader = self.module_loader.borrow();
            if let Some(exports) = loader.get_cached(module_name) {
                return Ok(exports.clone());
            }
        }

        // Parse the module file
        let (program, public_names, type_variants) = {
            let mut loader = self.module_loader.borrow_mut();
            loader
                .parse_module(module_name)
                .map_err(|e| {
                    if e == "__already_loaded__" {
                        // Already loaded — get from cache
                        return err("__already_loaded__".to_string());
                    }
                    err(e)
                })?
        };

        // Create a fresh environment for the module (with builtins)
        let module_env = Env::new();
        register_builtins(&module_env);

        // Evaluate all declarations in the module environment
        // We need a temporary interpreter-like context for this.
        // Save and swap the global env.
        let saved_global = self.global.clone();
        let saved_variant_types = self.variant_types.clone();
        self.global = module_env;

        for decl in &program.decls {
            // Skip imports in the module for now — they would need recursive loading
            // which is handled by the circular-import guard.
            if let Err(e) = self.register_decl(decl) {
                // Restore original state on error
                self.global = saved_global;
                self.variant_types = saved_variant_types;
                return Err(e);
            }
        }

        let evaluated_env = self.global.clone();

        // Restore the original interpreter state
        self.global = saved_global;
        self.variant_types = saved_variant_types;

        // Finish the load in the module loader
        {
            let mut loader = self.module_loader.borrow_mut();
            loader.finish_load(module_name, evaluated_env.clone(), public_names.clone(), type_variants.clone());
        }

        Ok(crate::module::ModuleExports {
            env: evaluated_env,
            public_names,
            type_variants,
        })
    }

    /// Register all declarations (for test setup).
    pub fn run_test_setup(&mut self, program: &Program) -> Result<Value> {
        for decl in &program.decls {
            self.register_decl(decl)?;
        }
        Ok(Value::Unit)
    }

    /// Run a single test function by name.
    pub fn run_test(&mut self, name: &str) -> Result<()> {
        match self.global.get(name) {
            Some(Value::Closure(c)) => {
                self.call_closure(&c, &[])?;
                Ok(())
            }
            Some(_) => Err(err(format!("{name} is not a function"))),
            None => Err(err(format!("test function {name} not found"))),
        }
    }

    /// Evaluate statements in the global scope (for REPL persistence).
    pub fn eval_in_global(&mut self, stmts: &[Stmt]) -> Result<Value> {
        let mut last = Value::Unit;
        for stmt in stmts {
            last = self.eval_stmt(stmt, &self.global.clone())?;
        }
        Ok(last)
    }

    /// Call a function by name (for REPL).
    pub fn call_by_name(&self, name: &str) -> Result<Value> {
        match self.global.get(name) {
            Some(Value::Closure(c)) => self.call_closure(&c, &[]),
            Some(_) => Err(err(format!("{name} is not a function"))),
            None => Err(err(format!("undefined: {name}"))),
        }
    }

    // ── Evaluation ───────────────────────────────────────────────────

    fn eval(&self, expr: &Expr, env: &Env) -> Result<Value> {
        self.eval_inner(expr, env, false)
    }

    fn eval_tail(&self, expr: &Expr, env: &Env) -> Result<Value> {
        self.eval_inner(expr, env, true)
    }

    fn eval_inner(&self, expr: &Expr, env: &Env, tail: bool) -> Result<Value> {
        // Preemptive yielding: periodically give pending tasks a chance to run.
        let steps = self.step_counter.get() + 1;
        self.step_counter.set(steps);
        if steps % YIELD_INTERVAL == 0 && self.scheduler.borrow().has_pending_tasks() {
            let _ = self.run_pending_tasks_once();
        }

        match &expr.kind {
            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Float(n) => Ok(Value::Float(*n)),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::StringLit(s) => Ok(Value::String(s.clone())),
            ExprKind::Unit => Ok(Value::Unit),

            ExprKind::StringInterp(parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        StringPart::Literal(s) => result.push_str(s),
                        StringPart::Expr(e) => {
                            let val = self.eval(e, env)?;
                            result.push_str(&val.to_string());
                        }
                    }
                }
                Ok(Value::String(result))
            }

            ExprKind::Ident(name) => env.get(name).ok_or_else(|| err_at(format!("undefined: {name}"), expr.span)),

            ExprKind::List(elems) => {
                let vals: Vec<Value> = elems
                    .iter()
                    .map(|e| self.eval(e, env))
                    .collect::<Result<_>>()?;
                Ok(Value::List(Rc::new(vals)))
            }

            ExprKind::Map(pairs) => {
                let mut map = BTreeMap::new();
                for (k, v) in pairs {
                    let key = self.eval(k, env)?;
                    let val = self.eval(v, env)?;
                    map.insert(key, val);
                }
                Ok(Value::Map(Rc::new(map)))
            }

            ExprKind::Tuple(elems) => {
                let vals: Vec<Value> = elems
                    .iter()
                    .map(|e| self.eval(e, env))
                    .collect::<Result<_>>()?;
                Ok(Value::Tuple(vals))
            }

            ExprKind::Binary(left, op, right) => {
                // Short-circuit && and ||
                if *op == BinOp::And {
                    let l = self.eval(left, env)?;
                    return if !is_truthy(&l) {
                        Ok(Value::Bool(false))
                    } else {
                        Ok(Value::Bool(is_truthy(&self.eval(right, env)?)))
                    };
                }
                if *op == BinOp::Or {
                    let l = self.eval(left, env)?;
                    return if is_truthy(&l) {
                        Ok(Value::Bool(true))
                    } else {
                        Ok(Value::Bool(is_truthy(&self.eval(right, env)?)))
                    };
                }
                let l = self.eval(left, env)?;
                let r = self.eval(right, env)?;
                eval_binary(l, *op, r, expr.span)
            }

            ExprKind::Unary(op, expr) => {
                let val = self.eval(expr, env)?;
                match op {
                    UnaryOp::Neg => match val {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(n) => Ok(Value::Float(-n)),
                        _ => Err(err("cannot negate non-numeric value")),
                    },
                    UnaryOp::Not => match val {
                        Value::Bool(b) => Ok(Value::Bool(!b)),
                        _ => Err(err("cannot apply ! to non-boolean")),
                    },
                }
            }

            ExprKind::Range(start, end) => {
                let s = self.eval(start, env)?;
                let e = self.eval(end, env)?;
                match (s, e) {
                    (Value::Int(a), Value::Int(b)) => {
                        let items: Vec<Value> = (a..b).map(Value::Int).collect();
                        Ok(Value::List(Rc::new(items)))
                    }
                    _ => Err(err("range requires integer operands")),
                }
            }

            ExprKind::Pipe(left, right) => {
                let val = self.eval(left, env)?;
                self.eval_pipe(val, right, env)
            }

            ExprKind::QuestionMark(expr) => {
                let val = self.eval(expr, env)?;
                match &val {
                    Value::Variant(name, fields) if name == "Ok" && fields.len() == 1 => {
                        Ok(fields[0].clone())
                    }
                    Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
                        Ok(fields[0].clone())
                    }
                    Value::Variant(name, _) if name == "Err" || name == "None" => {
                        Err(RuntimeError::Return(val))
                    }
                    _ => Err(err("? operator requires Result or Option")),
                }
            }

            ExprKind::Call(callee, args) => {
                // Intercept concurrency builtins by name (bare or qualified)
                let builtin_name = match &callee.kind {
                    ExprKind::Ident(name) => Some(name.clone()),
                    ExprKind::FieldAccess(expr, field) => {
                        if let ExprKind::Ident(module) = &expr.kind {
                            Some(format!("{module}.{field}"))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(ref name) = builtin_name {
                    match name.as_str() {
                        "channel.new" => return self.builtin_chan(args, env),
                        "channel.send" => return self.builtin_send(args, env),
                        "channel.receive" => return self.builtin_receive(args, env),
                        "channel.close" => return self.builtin_close(args, env),
                        "channel.try_send" => return self.builtin_try_send(args, env),
                        "channel.try_receive" => return self.builtin_try_receive(args, env),
                        "channel.select" => return self.builtin_select(args, env),
                        "task.spawn" => return self.builtin_spawn(args, env),
                        "task.join" => return self.builtin_join(args, env),
                        "task.cancel" => return self.builtin_cancel(args, env),
                        "try" => return self.builtin_try(args, env),
                        _ => {}
                    }
                }
                let func = self.eval(callee, env)?;
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<Result<_>>()?;
                // Push call frame for stack traces
                let frame_name = builtin_name.as_deref()
                    .unwrap_or_else(|| match &callee.kind {
                        ExprKind::Ident(n) => n.as_str(),
                        _ => "<closure>",
                    }).to_string();
                if tail {
                    if let Value::Closure(c) = &func {
                        return Err(RuntimeError::TailCall(c.clone(), arg_vals));
                    }
                }
                self.call_stack.borrow_mut().push(CallFrame {
                    name: frame_name,
                    span: expr.span,
                });
                let result = self.call_value(&func, &arg_vals);
                match &result {
                    Err(RuntimeError::Error(_, _)) => {
                        // Leave frame on stack for diagnostics.
                    }
                    _ => {
                        self.call_stack.borrow_mut().pop();
                    }
                }
                result
            }

            ExprKind::Lambda { params, body } => Ok(Value::Closure(Rc::new(Closure {
                params: params.clone(),
                body: *body.clone(),
                env: env.clone(),
            }))),

            ExprKind::FieldAccess(expr, field) => {
                // First: try qualified name lookup (e.g., map.get, string.split)
                // before evaluating the expression, so module.func works even
                // when the module name is not a runtime value.
                if let ExprKind::Ident(module) = &expr.kind {
                    let qualified = format!("{module}.{field}");
                    if let Some(val) = self.global.get(&qualified) {
                        return Ok(val);
                    }
                }
                // Then evaluate the expression and access the field on the result
                match self.eval(expr, env) {
                    Ok(val) => self.access_field(&val, field),
                    Err(_) => {
                        Err(err(format!("undefined: {}.{field}", expr.kind.kind_name())))
                    }
                }
            }

            ExprKind::RecordCreate { name, fields } => {
                let mut map = BTreeMap::new();
                for (fname, fexpr) in fields {
                    map.insert(fname.clone(), self.eval(fexpr, env)?);
                }
                Ok(Value::Record(name.clone(), Rc::new(map)))
            }

            ExprKind::RecordUpdate { expr, fields } => {
                let base = self.eval(expr, env)?;
                match base {
                    Value::Record(name, base_fields) => {
                        let mut rc = base_fields.clone();
                        for (fname, fexpr) in fields {
                            Rc::make_mut(&mut rc).insert(fname.clone(), self.eval(fexpr, env)?);
                        }
                        Ok(Value::Record(name, rc))
                    }
                    _ => Err(err("record update on non-record value")),
                }
            }

            ExprKind::Match { expr, arms } => {
                match expr {
                    Some(scrutinee) => {
                        let val = self.eval(scrutinee, env)?;
                        self.eval_match(&val, arms, env, tail)
                    }
                    None => self.eval_guardless_match(arms, env, tail),
                }
            }

            ExprKind::Return(val) => {
                let v = match val {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Unit,
                };
                Err(RuntimeError::Return(v))
            }

            ExprKind::Block(stmts) => self.eval_block_inner(stmts, env, tail),

            ExprKind::Loop { bindings, body } => {
                // Evaluate initial binding values
                let mut current_vals: Vec<Value> = bindings
                    .iter()
                    .map(|(_, init)| self.eval(init, env))
                    .collect::<Result<_>>()?;
                let names: Vec<&str> = bindings.iter().map(|(n, _)| n.as_str()).collect();

                loop {
                    let loop_env = env.child();
                    for (name, val) in names.iter().zip(current_vals.iter()) {
                        loop_env.define((*name).to_string(), val.clone());
                    }

                    match self.eval_tail(body, &loop_env) {
                        Ok(val) => return Ok(val),
                        Err(RuntimeError::LoopRecur(new_vals)) => {
                            if new_vals.len() != bindings.len() {
                                return Err(err(format!(
                                    "loop() expects {} argument(s), got {}",
                                    bindings.len(),
                                    new_vals.len()
                                )));
                            }
                            current_vals = new_vals;
                        }
                        Err(RuntimeError::Return(val)) => {
                            return Err(RuntimeError::Return(val));
                        }
                        Err(RuntimeError::TailCall(c, a)) => {
                            return self.call_closure(&c, &a);
                        }
                        Err(e) => return Err(e),
                    }
                }
            }

            ExprKind::Recur(args) => {
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<Result<_>>()?;
                Err(RuntimeError::LoopRecur(arg_vals))
            }
        }
    }

    fn eval_block_inner(&self, stmts: &[Stmt], env: &Env, tail: bool) -> Result<Value> {
        let block_env = env.child();
        let len = stmts.len();
        let mut last = Value::Unit;
        for (i, stmt) in stmts.iter().enumerate() {
            let is_last = i + 1 == len;
            last = self.eval_stmt_inner(stmt, &block_env, tail && is_last)?;
        }
        Ok(last)
    }

    fn eval_stmt(&self, stmt: &Stmt, env: &Env) -> Result<Value> {
        self.eval_stmt_inner(stmt, env, false)
    }

    fn eval_stmt_inner(&self, stmt: &Stmt, env: &Env, tail: bool) -> Result<Value> {
        match stmt {
            Stmt::Let { pattern, value, .. } => {
                let val = self.eval(value, env)?;
                self.bind_pattern(pattern, &val, env)?;
                Ok(Value::Unit)
            }
            Stmt::When {
                pattern,
                expr,
                else_body,
            } => {
                let val = self.eval(expr, env)?;
                if !self.try_bind_pattern(pattern, &val, env, env) {
                    match self.eval(else_body, env) {
                        Err(RuntimeError::Return(v)) => return Err(RuntimeError::Return(v)),
                        Err(e) => return Err(e),
                        Ok(_) => {
                            return Err(err("when-else block must diverge (return or panic)"));
                        }
                    }
                }
                Ok(Value::Unit)
            }
            Stmt::Expr(expr) => self.eval_inner(expr, env, tail),
        }
    }

    // ── Pipe ─────────────────────────────────────────────────────────

    fn eval_pipe(&self, val: Value, right: &Expr, env: &Env) -> Result<Value> {
        // Desugar: val |> f(args) { closure } → f(val, args, closure)
        // val |> f(args) → f(val, args)
        // val |> f { closure } → f(val, closure)
        // val |> f → f(val)
        match &right.kind {
            ExprKind::Call(callee, args) => {
                let func = self.eval(callee, env)?;
                let mut all_args = vec![val];
                for a in args {
                    all_args.push(self.eval(a, env)?);
                }
                self.call_value(&func, &all_args)
            }
            ExprKind::Ident(name) => {
                let func = env
                    .get(name)
                    .ok_or_else(|| err(format!("undefined: {name}")))?;
                self.call_value(&func, &[val])
            }
            _ => {
                let func = self.eval(right, env)?;
                self.call_value(&func, &[val])
            }
        }
    }

    // ── Function calls ───────────────────────────────────────────────

    fn call_value(&self, func: &Value, args: &[Value]) -> Result<Value> {
        match func {
            Value::Closure(c) => self.call_closure(c, args),
            Value::BuiltinFn(name) => self.dispatch_builtin(name, args),
            Value::VariantConstructor(name, arity) => {
                if args.len() != *arity {
                    Err(err(format!(
                        "{name} expects {arity} arguments, got {}",
                        args.len()
                    )))
                } else {
                    Ok(Value::Variant(name.clone(), args.to_vec()))
                }
            }
            Value::Variant(name, _) if args.is_empty() => {
                // Nullary constructor called with no args, return as-is
                Ok(func.clone())
            }
            _ => Err(err(format!("not callable: {func}"))),
        }
    }

    fn dispatch_builtin(&self, name: &str, args: &[Value]) -> Result<Value> {
        if let Some((module, func)) = name.split_once('.') {
            match module {
                "list" => self.dispatch_list(func, args),
                "string" => self.dispatch_string(func, args),
                "int" => self.dispatch_int(func, args),
                "float" => self.dispatch_float(func, args),
                "map" => self.dispatch_map(func, args),
                "result" => self.dispatch_result(func, args),
                "option" => self.dispatch_option(func, args),
                "io" => self.dispatch_io(func, args),
                "test" => self.dispatch_test(func, args),
                "regex" => self.dispatch_regex(func, args),
                "json" => self.dispatch_json(func, args),
                "math" => self.dispatch_math(func, args),
                "__trait" => self.dispatch_trait_method(func, args),
                _ => Err(err(format!("unknown module: {module}"))),
            }
        } else {
            // Globals
            match name {
                // ── print / println ─────────────────────────────────────
                "print" => {
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 { print!(" "); }
                        print!("{arg}");
                    }
                    Ok(Value::Unit)
                }
                "println" => {
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 { print!(" "); }
                        print!("{arg}");
                    }
                    println!();
                    Ok(Value::Unit)
                }
                // ── panic ───────────────────────────────────────────────
                "panic" => {
                    let msg = args.first().map(|v| v.to_string()).unwrap_or_default();
                    Err(err(format!("panic: panic: {msg}")))
                }
                _ => Err(err(format!("unknown builtin: {name}"))),
            }
        }
    }

    fn dispatch_list(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── list module (closure-based) ─────────────────────────
            "map" => {
                if args.len() != 2 {
                    return Err(err("list.map takes 2 arguments (list, fn)"));
                }
                self.builtin_map(&args[0], &args[1])
            }
            "filter" => {
                if args.len() != 2 {
                    return Err(err("list.filter takes 2 arguments (list, fn)"));
                }
                self.builtin_filter(&args[0], &args[1])
            }
            "each" => {
                if args.len() != 2 {
                    return Err(err("list.each takes 2 arguments (list, fn)"));
                }
                self.builtin_each(&args[0], &args[1])
            }
            "fold" => {
                if args.len() != 3 {
                    return Err(err("list.fold takes 3 arguments (list, init, fn)"));
                }
                self.builtin_fold(&args[0], &args[1], &args[2])
            }
            "find" => {
                if args.len() != 2 {
                    return Err(err("list.find takes 2 arguments (list, fn)"));
                }
                self.builtin_find(&args[0], &args[1])
            }
            "sort_by" => {
                if args.len() != 2 {
                    return Err(err("list.sort_by takes 2 arguments (list, key_fn)"));
                }
                self.builtin_sort_by(&args[0], &args[1])
            }
            "flat_map" => {
                if args.len() != 2 {
                    return Err(err("list.flat_map takes 2 arguments (list, fn)"));
                }
                self.builtin_flat_map(&args[0], &args[1])
            }
            "any" => {
                if args.len() != 2 {
                    return Err(err("list.any takes 2 arguments (list, fn)"));
                }
                self.builtin_any(&args[0], &args[1])
            }
            "all" => {
                if args.len() != 2 {
                    return Err(err("list.all takes 2 arguments (list, fn)"));
                }
                self.builtin_all(&args[0], &args[1])
            }
            "fold_until" => {
                if args.len() != 3 {
                    return Err(err("list.fold_until takes 3 arguments (list, init, fn)"));
                }
                self.builtin_fold_until(&args[0], &args[1], &args[2])
            }
            "unfold" => {
                if args.len() != 2 {
                    return Err(err("list.unfold takes 2 arguments (seed, fn)"));
                }
                self.builtin_unfold(&args[0], &args[1])
            }

            // ── list module (non-closure) ───────────────────────────
            "zip" => {
                if args.len() != 2 {
                    return Err(err("list.zip takes 2 arguments"));
                }
                let a = match &args[0] {
                    Value::List(xs) => xs.clone(),
                    _ => return Err(err("first argument to list.zip must be a list")),
                };
                let b = match &args[1] {
                    Value::List(xs) => xs.clone(),
                    _ => return Err(err("second argument to list.zip must be a list")),
                };
                let pairs: Vec<Value> = a
                    .iter()
                    .zip(b.iter())
                    .map(|(x, y)| Value::Tuple(vec![x.clone(), y.clone()]))
                    .collect();
                Ok(Value::List(Rc::new(pairs)))
            }
            "flatten" => {
                if args.len() != 1 {
                    return Err(err("list.flatten takes 1 argument"));
                }
                let list = match &args[0] {
                    Value::List(xs) => xs.clone(),
                    _ => return Err(err("argument to list.flatten must be a list")),
                };
                let mut result = Vec::new();
                for item in list.iter() {
                    match item {
                        Value::List(inner) => result.extend(inner.iter().cloned()),
                        other => result.push(other.clone()),
                    }
                }
                Ok(Value::List(Rc::new(result)))
            }
            "head" => {
                if args.len() != 1 {
                    return Err(err("list.head takes 1 argument"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("list.head requires a list"));
                };
                match xs.first() {
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "tail" => {
                if args.len() != 1 {
                    return Err(err("list.tail takes 1 argument"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("list.tail requires a list"));
                };
                if xs.is_empty() {
                    Ok(Value::List(Rc::new(Vec::new())))
                } else {
                    Ok(Value::List(Rc::new(xs[1..].to_vec())))
                }
            }
            "last" => {
                if args.len() != 1 {
                    return Err(err("list.last takes 1 argument"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("list.last requires a list"));
                };
                match xs.last() {
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "reverse" => {
                if args.len() != 1 {
                    return Err(err("list.reverse takes 1 argument"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("list.reverse requires a list"));
                };
                let mut rc = xs.clone();
                Rc::make_mut(&mut rc).reverse();
                Ok(Value::List(rc))
            }
            "sort" => {
                if args.len() != 1 {
                    return Err(err("list.sort takes 1 argument"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("list.sort requires a list"));
                };
                let mut rc = xs.clone();
                Rc::make_mut(&mut rc).sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                Ok(Value::List(rc))
            }
            "unique" => {
                if args.len() != 1 {
                    return Err(err("list.unique takes 1 argument"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("list.unique requires a list"));
                };
                let mut seen = Vec::new();
                let mut result = Vec::new();
                for x in xs.iter() {
                    if !seen.iter().any(|s| s == x) {
                        seen.push(x.clone());
                        result.push(x.clone());
                    }
                }
                Ok(Value::List(Rc::new(result)))
            }
            "contains" => {
                if args.len() != 2 {
                    return Err(err("list.contains takes 2 arguments"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("list.contains requires a list as first argument"));
                };
                Ok(Value::Bool(xs.contains(&args[1])))
            }
            "length" => {
                if args.len() != 1 {
                    return Err(err("list.length takes 1 argument"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("list.length requires a list"));
                };
                Ok(Value::Int(xs.len() as i64))
            }
            "append" => {
                if args.len() != 2 {
                    return Err(err("list.append takes 2 arguments (list, element)"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("first argument must be a list"));
                };
                let mut rc = xs.clone();
                Rc::make_mut(&mut rc).push(args[1].clone());
                Ok(Value::List(rc))
            }
            "prepend" => {
                if args.len() != 2 {
                    return Err(err("list.prepend takes 2 arguments (list, element)"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("first argument must be a list"));
                };
                let mut rc = xs.clone();
                Rc::make_mut(&mut rc).insert(0, args[1].clone());
                Ok(Value::List(rc))
            }
            "concat" => {
                if args.len() != 2 {
                    return Err(err("list.concat takes 2 arguments (list, list)"));
                }
                let Value::List(a) = &args[0] else {
                    return Err(err("first argument must be a list"));
                };
                let Value::List(b) = &args[1] else {
                    return Err(err("second argument must be a list"));
                };
                let mut rc = a.clone();
                Rc::make_mut(&mut rc).extend((**b).iter().cloned());
                Ok(Value::List(rc))
            }
            "get" => {
                if args.len() != 2 { return Err(err("list.get takes 2 arguments (list, index)")); }
                let Value::List(xs) = &args[0] else { return Err(err("first argument must be a list")); };
                let Value::Int(n) = &args[1] else { return Err(err("second argument must be an int")); };
                let idx = *n as usize;
                match xs.get(idx) {
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "take" => {
                if args.len() != 2 { return Err(err("list.take takes 2 arguments (list, n)")); }
                let Value::List(xs) = &args[0] else { return Err(err("first argument must be a list")); };
                let Value::Int(n) = &args[1] else { return Err(err("second argument must be an int")); };
                let n = (*n as usize).min(xs.len());
                Ok(Value::List(Rc::new(xs[..n].to_vec())))
            }
            "drop" => {
                if args.len() != 2 { return Err(err("list.drop takes 2 arguments (list, n)")); }
                let Value::List(xs) = &args[0] else { return Err(err("first argument must be a list")); };
                let Value::Int(n) = &args[1] else { return Err(err("second argument must be an int")); };
                let n = (*n as usize).min(xs.len());
                Ok(Value::List(Rc::new(xs[n..].to_vec())))
            }
            "enumerate" => {
                if args.len() != 1 { return Err(err("list.enumerate takes 1 argument")); }
                let Value::List(xs) = &args[0] else { return Err(err("argument must be a list")); };
                let result: Vec<Value> = xs.iter().enumerate()
                    .map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v.clone()]))
                    .collect();
                Ok(Value::List(Rc::new(result)))
            }
            "group_by" => {
                if args.len() != 2 {
                    return Err(err("list.group_by takes 2 arguments (list, key_fn)"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("first argument to list.group_by must be a list"));
                };
                let mut groups: BTreeMap<Value, Vec<Value>> = BTreeMap::new();
                for item in xs.iter() {
                    let key = self.call_value(&args[1], &[item.clone()])?;
                    groups.entry(key).or_default().push(item.clone());
                }
                let result: BTreeMap<Value, Value> = groups.into_iter()
                    .map(|(k, v)| (k, Value::List(Rc::new(v))))
                    .collect();
                Ok(Value::Map(Rc::new(result)))
            }
            _ => Err(err(format!("unknown list function: {name}"))),
        }
    }

    fn dispatch_string(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── string module ───────────────────────────────────────
            "split" => {
                if args.len() != 2 {
                    return Err(err("string.split takes 2 arguments"));
                }
                let (Value::String(s), Value::String(sep)) = (&args[0], &args[1]) else {
                    return Err(err("string.split requires string arguments"));
                };
                let parts: Vec<Value> = s.split(sep.as_str()).map(|p| Value::String(p.to_string())).collect();
                Ok(Value::List(Rc::new(parts)))
            }
            "trim" => {
                if args.len() != 1 {
                    return Err(err("string.trim takes 1 argument"));
                }
                let Value::String(s) = &args[0] else {
                    return Err(err("string.trim requires a string"));
                };
                Ok(Value::String(s.trim().to_string()))
            }
            "contains" => {
                if args.len() != 2 {
                    return Err(err("string.contains takes 2 arguments"));
                }
                let (Value::String(s), Value::String(sub)) = (&args[0], &args[1]) else {
                    return Err(err("string.contains requires string arguments"));
                };
                Ok(Value::Bool(s.contains(sub.as_str())))
            }
            "replace" => {
                if args.len() != 3 {
                    return Err(err("string.replace takes 3 arguments (string, from, to)"));
                }
                let (Value::String(s), Value::String(from), Value::String(to)) = (&args[0], &args[1], &args[2]) else {
                    return Err(err("string.replace requires string arguments"));
                };
                Ok(Value::String(s.replace(from.as_str(), to.as_str())))
            }
            "join" => {
                if args.len() != 2 {
                    return Err(err("string.join takes 2 arguments (list, separator)"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("first argument to string.join must be a list"));
                };
                let Value::String(sep) = &args[1] else {
                    return Err(err("second argument to string.join must be a string"));
                };
                let strs: Vec<String> = xs.iter().map(|v| v.to_string()).collect();
                Ok(Value::String(strs.join(sep.as_str())))
            }
            "length" => {
                if args.len() != 1 {
                    return Err(err("string.length takes 1 argument"));
                }
                let Value::String(s) = &args[0] else {
                    return Err(err("string.length requires a string"));
                };
                Ok(Value::Int(s.len() as i64))
            }
            "to_upper" => {
                if args.len() != 1 {
                    return Err(err("string.to_upper takes 1 argument"));
                }
                let Value::String(s) = &args[0] else {
                    return Err(err("string.to_upper requires a string"));
                };
                Ok(Value::String(s.to_uppercase()))
            }
            "to_lower" => {
                if args.len() != 1 {
                    return Err(err("string.to_lower takes 1 argument"));
                }
                let Value::String(s) = &args[0] else {
                    return Err(err("string.to_lower requires a string"));
                };
                Ok(Value::String(s.to_lowercase()))
            }
            "starts_with" => {
                if args.len() != 2 {
                    return Err(err("string.starts_with takes 2 arguments"));
                }
                let (Value::String(s), Value::String(prefix)) = (&args[0], &args[1]) else {
                    return Err(err("string.starts_with requires string arguments"));
                };
                Ok(Value::Bool(s.starts_with(prefix.as_str())))
            }
            "ends_with" => {
                if args.len() != 2 {
                    return Err(err("string.ends_with takes 2 arguments"));
                }
                let (Value::String(s), Value::String(suffix)) = (&args[0], &args[1]) else {
                    return Err(err("string.ends_with requires string arguments"));
                };
                Ok(Value::Bool(s.ends_with(suffix.as_str())))
            }
            "chars" => {
                if args.len() != 1 {
                    return Err(err("string.chars takes 1 argument"));
                }
                let Value::String(s) = &args[0] else {
                    return Err(err("string.chars requires a string"));
                };
                let chars: Vec<Value> = s.chars().map(|c| Value::String(c.to_string())).collect();
                Ok(Value::List(Rc::new(chars)))
            }
            "repeat" => {
                if args.len() != 2 {
                    return Err(err("string.repeat takes 2 arguments"));
                }
                let Value::String(s) = &args[0] else {
                    return Err(err("string.repeat requires a string as first argument"));
                };
                let Value::Int(n) = &args[1] else {
                    return Err(err("string.repeat requires an int as second argument"));
                };
                if *n < 0 {
                    return Err(err("string.repeat count must be non-negative"));
                }
                Ok(Value::String(s.repeat(*n as usize)))
            }
            "index_of" => {
                if args.len() != 2 { return Err(err("string.index_of takes 2 arguments")); }
                let (Value::String(s), Value::String(needle)) = (&args[0], &args[1]) else {
                    return Err(err("string.index_of requires string arguments"));
                };
                match s.find(needle.as_str()) {
                    Some(idx) => Ok(Value::Variant("Some".into(), vec![Value::Int(idx as i64)])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "slice" => {
                if args.len() != 3 { return Err(err("string.slice takes 3 arguments (string, start, end)")); }
                let Value::String(s) = &args[0] else { return Err(err("first argument must be a string")); };
                let Value::Int(start) = &args[1] else { return Err(err("second argument must be an int")); };
                let Value::Int(end) = &args[2] else { return Err(err("third argument must be an int")); };
                let start = (*start as usize).min(s.len());
                let end = (*end as usize).min(s.len());
                if start > end {
                    Ok(Value::String(String::new()))
                } else {
                    let chars: Vec<char> = s.chars().collect();
                    let start = start.min(chars.len());
                    let end = end.min(chars.len());
                    Ok(Value::String(chars[start..end].iter().collect()))
                }
            }
            "pad_left" => {
                if args.len() != 3 { return Err(err("string.pad_left takes 3 arguments (string, width, pad_char)")); }
                let Value::String(s) = &args[0] else { return Err(err("first arg must be string")); };
                let Value::Int(width) = &args[1] else { return Err(err("second arg must be int")); };
                let Value::String(pad) = &args[2] else { return Err(err("third arg must be string")); };
                let width = *width as usize;
                let pad_char = pad.chars().next().unwrap_or(' ');
                if s.len() >= width {
                    Ok(Value::String(s.clone()))
                } else {
                    let padding: String = (0..width - s.len()).map(|_| pad_char).collect();
                    Ok(Value::String(format!("{padding}{s}")))
                }
            }
            "pad_right" => {
                if args.len() != 3 { return Err(err("string.pad_right takes 3 arguments (string, width, pad_char)")); }
                let Value::String(s) = &args[0] else { return Err(err("first arg must be string")); };
                let Value::Int(width) = &args[1] else { return Err(err("second arg must be int")); };
                let Value::String(pad) = &args[2] else { return Err(err("third arg must be string")); };
                let width = *width as usize;
                let pad_char = pad.chars().next().unwrap_or(' ');
                if s.len() >= width {
                    Ok(Value::String(s.clone()))
                } else {
                    let padding: String = (0..width - s.len()).map(|_| pad_char).collect();
                    Ok(Value::String(format!("{s}{padding}")))
                }
            }
            _ => Err(err(format!("unknown string function: {name}"))),
        }
    }

    fn dispatch_int(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── int module ──────────────────────────────────────────
            "parse" => {
                if args.len() != 1 {
                    return Err(err("int.parse takes 1 argument"));
                }
                let Value::String(s) = &args[0] else {
                    return Err(err("int.parse requires a string"));
                };
                match s.trim().parse::<i64>() {
                    Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Int(n)])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "abs" => {
                if args.len() != 1 {
                    return Err(err("int.abs takes 1 argument"));
                }
                let Value::Int(n) = &args[0] else {
                    return Err(err("int.abs requires an int"));
                };
                Ok(Value::Int(n.abs()))
            }
            "min" => {
                if args.len() != 2 {
                    return Err(err("int.min takes 2 arguments"));
                }
                let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else {
                    return Err(err("int.min requires int arguments"));
                };
                Ok(Value::Int(*a.min(b)))
            }
            "max" => {
                if args.len() != 2 {
                    return Err(err("int.max takes 2 arguments"));
                }
                let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else {
                    return Err(err("int.max requires int arguments"));
                };
                Ok(Value::Int(*a.max(b)))
            }
            "to_float" => {
                if args.len() != 1 {
                    return Err(err("int.to_float takes 1 argument"));
                }
                let Value::Int(n) = &args[0] else {
                    return Err(err("int.to_float requires an int"));
                };
                Ok(Value::Float(*n as f64))
            }
            "to_string" => {
                if args.len() != 1 {
                    return Err(err("int.to_string takes 1 argument"));
                }
                let Value::Int(n) = &args[0] else {
                    return Err(err("int.to_string requires an int"));
                };
                Ok(Value::String(n.to_string()))
            }
            _ => Err(err(format!("unknown int function: {name}"))),
        }
    }

    fn dispatch_float(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── float module ────────────────────────────────────────
            "parse" => {
                if args.len() != 1 {
                    return Err(err("float.parse takes 1 argument"));
                }
                let Value::String(s) = &args[0] else {
                    return Err(err("float.parse requires a string"));
                };
                match s.trim().parse::<f64>() {
                    Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Float(n)])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "round" => {
                if args.len() != 1 {
                    return Err(err("float.round takes 1 argument"));
                }
                let Value::Float(f) = &args[0] else {
                    return Err(err("float.round requires a float"));
                };
                Ok(Value::Int(f.round() as i64))
            }
            "ceil" => {
                if args.len() != 1 {
                    return Err(err("float.ceil takes 1 argument"));
                }
                let Value::Float(f) = &args[0] else {
                    return Err(err("float.ceil requires a float"));
                };
                Ok(Value::Int(f.ceil() as i64))
            }
            "floor" => {
                if args.len() != 1 {
                    return Err(err("float.floor takes 1 argument"));
                }
                let Value::Float(f) = &args[0] else {
                    return Err(err("float.floor requires a float"));
                };
                Ok(Value::Int(f.floor() as i64))
            }
            "abs" => {
                if args.len() != 1 {
                    return Err(err("float.abs takes 1 argument"));
                }
                let Value::Float(f) = &args[0] else {
                    return Err(err("float.abs requires a float"));
                };
                Ok(Value::Float(f.abs()))
            }
            "to_string" => {
                match args.len() {
                    1 => {
                        let Value::Float(f) = &args[0] else {
                            return Err(err("float.to_string requires a float"));
                        };
                        Ok(Value::String(f.to_string()))
                    }
                    2 => {
                        let Value::Float(f) = &args[0] else {
                            return Err(err("float.to_string requires a float as first argument"));
                        };
                        let Value::Int(decimals) = &args[1] else {
                            return Err(err("float.to_string requires an int for decimal places"));
                        };
                        if *decimals < 0 {
                            return Err(err("decimal places must be non-negative"));
                        }
                        Ok(Value::String(format!("{:.prec$}", f, prec = *decimals as usize)))
                    }
                    _ => Err(err("float.to_string takes 1 or 2 arguments (float) or (float, decimals)")),
                }
            }
            "to_int" => {
                if args.len() != 1 {
                    return Err(err("float.to_int takes 1 argument"));
                }
                let Value::Float(f) = &args[0] else {
                    return Err(err("float.to_int requires a float"));
                };
                Ok(Value::Int(*f as i64))
            }
            "min" => {
                if args.len() != 2 { return Err(err("float.min takes 2 arguments")); }
                let (Value::Float(a), Value::Float(b)) = (&args[0], &args[1]) else {
                    return Err(err("float.min requires float arguments"));
                };
                Ok(Value::Float(a.min(*b)))
            }
            "max" => {
                if args.len() != 2 { return Err(err("float.max takes 2 arguments")); }
                let (Value::Float(a), Value::Float(b)) = (&args[0], &args[1]) else {
                    return Err(err("float.max requires float arguments"));
                };
                Ok(Value::Float(a.max(*b)))
            }
            _ => Err(err(format!("unknown float function: {name}"))),
        }
    }

    fn dispatch_map(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── map module ──────────────────────────────────────────
            "get" => {
                if args.len() != 2 {
                    return Err(err("map.get takes 2 arguments"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.get requires a map as first argument"));
                };
                match m.get(&args[1]) {
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "has_key" => {
                if args.len() != 2 {
                    return Err(err("map.has_key takes 2 arguments (map, key)"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.has_key requires a map as first argument"));
                };
                Ok(Value::Bool(m.contains_key(&args[1])))
            }
            "set" => {
                if args.len() != 3 {
                    return Err(err("map.set takes 3 arguments"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.set requires a map as first argument"));
                };
                let mut rc = m.clone();
                Rc::make_mut(&mut rc).insert(args[1].clone(), args[2].clone());
                Ok(Value::Map(rc))
            }
            "delete" => {
                if args.len() != 2 {
                    return Err(err("map.delete takes 2 arguments"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.delete requires a map as first argument"));
                };
                let mut rc = m.clone();
                Rc::make_mut(&mut rc).remove(&args[1]);
                Ok(Value::Map(rc))
            }
            "keys" => {
                if args.len() != 1 {
                    return Err(err("map.keys takes 1 argument"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.keys requires a map"));
                };
                let keys: Vec<Value> = m.keys().cloned().collect();
                Ok(Value::List(Rc::new(keys)))
            }
            "values" => {
                if args.len() != 1 {
                    return Err(err("map.values takes 1 argument"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.values requires a map"));
                };
                let vals: Vec<Value> = m.values().cloned().collect();
                Ok(Value::List(Rc::new(vals)))
            }
            "length" => {
                if args.len() != 1 {
                    return Err(err("map.length takes 1 argument"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.length requires a map"));
                };
                Ok(Value::Int(m.len() as i64))
            }
            "merge" => {
                if args.len() != 2 {
                    return Err(err("map.merge takes 2 arguments"));
                }
                let Value::Map(m1) = &args[0] else {
                    return Err(err("map.merge requires maps"));
                };
                let Value::Map(m2) = &args[1] else {
                    return Err(err("map.merge requires maps"));
                };
                let mut rc = m1.clone();
                let merged = Rc::make_mut(&mut rc);
                for (k, v) in m2.iter() {
                    merged.insert(k.clone(), v.clone());
                }
                Ok(Value::Map(rc))
            }
            "filter" => {
                if args.len() != 2 {
                    return Err(err("map.filter takes 2 arguments (map, fn)"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.filter requires a map as first argument"));
                };
                let mut result = BTreeMap::new();
                for (k, v) in m.iter() {
                    let keep = self.call_value(&args[1], &[k.clone(), v.clone()])?;
                    if is_truthy(&keep) {
                        result.insert(k.clone(), v.clone());
                    }
                }
                Ok(Value::Map(Rc::new(result)))
            }
            "map" => {
                if args.len() != 2 {
                    return Err(err("map.map takes 2 arguments (map, fn)"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.map requires a map as first argument"));
                };
                let mut result = BTreeMap::new();
                for (k, v) in m.iter() {
                    let mapped = self.call_value(&args[1], &[k.clone(), v.clone()])?;
                    match mapped {
                        Value::Tuple(pair) if pair.len() == 2 => {
                            result.insert(pair[0].clone(), pair[1].clone());
                        }
                        _ => return Err(err("map.map callback must return a (key, value) tuple")),
                    }
                }
                Ok(Value::Map(Rc::new(result)))
            }
            "entries" => {
                if args.len() != 1 {
                    return Err(err("map.entries takes 1 argument"));
                }
                let Value::Map(m) = &args[0] else {
                    return Err(err("map.entries requires a map"));
                };
                let entries: Vec<Value> = m.iter()
                    .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                    .collect();
                Ok(Value::List(Rc::new(entries)))
            }
            "from_entries" => {
                if args.len() != 1 {
                    return Err(err("map.from_entries takes 1 argument"));
                }
                let Value::List(xs) = &args[0] else {
                    return Err(err("map.from_entries requires a list of (key, value) tuples"));
                };
                let mut result = BTreeMap::new();
                for item in xs.iter() {
                    match item {
                        Value::Tuple(pair) if pair.len() == 2 => {
                            result.insert(pair[0].clone(), pair[1].clone());
                        }
                        _ => return Err(err("map.from_entries requires (key, value) tuples")),
                    }
                }
                Ok(Value::Map(Rc::new(result)))
            }
            _ => Err(err(format!("unknown map function: {name}"))),
        }
    }

    fn dispatch_result(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── result module ───────────────────────────────────────
            "unwrap_or" => {
                if args.len() != 2 {
                    return Err(err("result.unwrap_or takes 2 arguments"));
                }
                match &args[0] {
                    Value::Variant(name, fields) if name == "Ok" || name == "Some" => {
                        Ok(fields.first().cloned().unwrap_or(Value::Unit))
                    }
                    _ => Ok(args[1].clone()),
                }
            }
            "map_ok" => {
                if args.len() != 2 {
                    return Err(err("result.map_ok takes 2 arguments"));
                }
                match &args[0] {
                    Value::Variant(name, fields) if name == "Ok" && fields.len() == 1 => {
                        let result = self.call_value(&args[1], &[fields[0].clone()])?;
                        Ok(Value::Variant("Ok".into(), vec![result]))
                    }
                    Value::Variant(name, fields) if name == "Err" => {
                        Ok(Value::Variant(name.clone(), fields.clone()))
                    }
                    _ => Err(err("result.map_ok requires a Result value")),
                }
            }
            "map_err" => {
                if args.len() != 2 {
                    return Err(err("result.map_err takes 2 arguments"));
                }
                match &args[0] {
                    Value::Variant(name, fields) if name == "Err" && fields.len() == 1 => {
                        let result = self.call_value(&args[1], &[fields[0].clone()])?;
                        Ok(Value::Variant("Err".into(), vec![result]))
                    }
                    Value::Variant(name, fields) if name == "Ok" => {
                        Ok(Value::Variant(name.clone(), fields.clone()))
                    }
                    _ => Err(err("result.map_err requires a Result value")),
                }
            }
            "flatten" => {
                if args.len() != 1 {
                    return Err(err("result.flatten takes 1 argument"));
                }
                match &args[0] {
                    Value::Variant(name, fields) if name == "Ok" && fields.len() == 1 => {
                        match &fields[0] {
                            Value::Variant(inner_name, _) if inner_name == "Ok" || inner_name == "Err" => {
                                Ok(fields[0].clone())
                            }
                            _ => Ok(args[0].clone()),
                        }
                    }
                    Value::Variant(name, _) if name == "Err" => Ok(args[0].clone()),
                    _ => Err(err("result.flatten requires a Result value")),
                }
            }
            "is_ok" => {
                if args.len() != 1 {
                    return Err(err("result.is_ok takes 1 argument"));
                }
                match &args[0] {
                    Value::Variant(name, _) if name == "Ok" => Ok(Value::Bool(true)),
                    Value::Variant(name, _) if name == "Err" => Ok(Value::Bool(false)),
                    _ => Err(err("result.is_ok requires a Result value")),
                }
            }
            "is_err" => {
                if args.len() != 1 {
                    return Err(err("result.is_err takes 1 argument"));
                }
                match &args[0] {
                    Value::Variant(name, _) if name == "Err" => Ok(Value::Bool(true)),
                    Value::Variant(name, _) if name == "Ok" => Ok(Value::Bool(false)),
                    _ => Err(err("result.is_err requires a Result value")),
                }
            }
            _ => Err(err(format!("unknown result function: {name}"))),
        }
    }

    fn dispatch_option(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── option module ───────────────────────────────────────
            "map" => {
                if args.len() != 2 {
                    return Err(err("option.map takes 2 arguments"));
                }
                match &args[0] {
                    Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
                        let result = self.call_value(&args[1], &[fields[0].clone()])?;
                        Ok(Value::Variant("Some".into(), vec![result]))
                    }
                    Value::Variant(name, _) if name == "None" => {
                        Ok(Value::Variant("None".into(), Vec::new()))
                    }
                    _ => Err(err("option.map requires an Option value")),
                }
            }
            "unwrap_or" => {
                if args.len() != 2 {
                    return Err(err("option.unwrap_or takes 2 arguments"));
                }
                match &args[0] {
                    Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
                        Ok(fields[0].clone())
                    }
                    Value::Variant(name, _) if name == "None" => {
                        Ok(args[1].clone())
                    }
                    _ => Err(err("option.unwrap_or requires an Option value")),
                }
            }
            "to_result" => {
                if args.len() != 2 {
                    return Err(err("option.to_result takes 2 arguments"));
                }
                match &args[0] {
                    Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
                        Ok(Value::Variant("Ok".into(), vec![fields[0].clone()]))
                    }
                    Value::Variant(name, _) if name == "None" => {
                        Ok(Value::Variant("Err".into(), vec![args[1].clone()]))
                    }
                    _ => Err(err("option.to_result requires an Option value")),
                }
            }
            "is_some" => {
                if args.len() != 1 {
                    return Err(err("option.is_some takes 1 argument"));
                }
                match &args[0] {
                    Value::Variant(name, _) if name == "Some" => Ok(Value::Bool(true)),
                    Value::Variant(name, _) if name == "None" => Ok(Value::Bool(false)),
                    _ => Err(err("option.is_some requires an Option value")),
                }
            }
            "is_none" => {
                if args.len() != 1 {
                    return Err(err("option.is_none takes 1 argument"));
                }
                match &args[0] {
                    Value::Variant(name, _) if name == "None" => Ok(Value::Bool(true)),
                    Value::Variant(name, _) if name == "Some" => Ok(Value::Bool(false)),
                    _ => Err(err("option.is_none requires an Option value")),
                }
            }
            _ => Err(err(format!("unknown option function: {name}"))),
        }
    }

    fn dispatch_io(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── io module ───────────────────────────────────────────
            "inspect" => {
                if args.len() != 1 {
                    return Err(err("io.inspect takes 1 argument"));
                }
                Ok(Value::String(format!("{:?}", args[0])))
            }
            "read_file" => {
                if args.len() != 1 {
                    return Err(err("io.read_file takes 1 argument"));
                }
                let Value::String(path) = &args[0] else {
                    return Err(err("io.read_file requires a string path"));
                };
                match std::fs::read_to_string(path) {
                    Ok(content) => Ok(Value::Variant("Ok".into(), vec![Value::String(content)])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "write_file" => {
                if args.len() != 2 {
                    return Err(err("io.write_file takes 2 arguments"));
                }
                let (Value::String(path), Value::String(content)) = (&args[0], &args[1]) else {
                    return Err(err("io.write_file requires string arguments"));
                };
                match std::fs::write(path, content) {
                    Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "read_line" => {
                if !args.is_empty() {
                    return Err(err("io.read_line takes no arguments"));
                }
                let mut line = String::new();
                match std::io::stdin().read_line(&mut line) {
                    Ok(_) => Ok(Value::Variant("Ok".into(), vec![Value::String(line.trim_end().to_string())])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "args" => {
                let args_list: Vec<Value> = std::env::args().map(Value::String).collect();
                Ok(Value::List(Rc::new(args_list)))
            }
            _ => Err(err(format!("unknown io function: {name}"))),
        }
    }

    fn dispatch_test(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── test module ─────────────────────────────────────────
            "assert" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(err("test.assert takes 1-2 arguments (value) or (value, message)"));
                }
                if is_truthy(&args[0]) {
                    Ok(Value::Unit)
                } else {
                    let msg = if args.len() == 2 {
                        format!("assertion failed: {}", args[1])
                    } else {
                        format!("assertion failed: {:?}", args[0])
                    };
                    Err(err(msg))
                }
            }
            "assert_eq" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(err("test.assert_eq takes 2-3 arguments (left, right) or (left, right, message)"));
                }
                if args[0] == args[1] {
                    Ok(Value::Unit)
                } else {
                    let msg = if args.len() == 3 {
                        format!("assertion failed: {}: {:?} != {:?}", args[2], args[0], args[1])
                    } else {
                        format!("assertion failed: {:?} != {:?}", args[0], args[1])
                    };
                    Err(err(msg))
                }
            }
            "assert_ne" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(err("test.assert_ne takes 2-3 arguments (left, right) or (left, right, message)"));
                }
                if args[0] != args[1] {
                    Ok(Value::Unit)
                } else {
                    let msg = if args.len() == 3 {
                        format!("assertion failed: {}: {:?} == {:?}", args[2], args[0], args[1])
                    } else {
                        format!("assertion failed: {:?} == {:?}", args[0], args[1])
                    };
                    Err(err(msg))
                }
            }
            _ => Err(err(format!("unknown test function: {name}"))),
        }
    }

    fn dispatch_regex(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── regex module ────────────────────────────────────────
            "is_match" => {
                if args.len() != 2 {
                    return Err(err("regex.is_match takes 2 arguments (pattern, text)"));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(err("regex.is_match requires string arguments"));
                };
                let re = Regex::new(pattern).map_err(|e| err(format!("invalid regex: {e}")))?;
                Ok(Value::Bool(re.is_match(text)))
            }
            "find" => {
                if args.len() != 2 {
                    return Err(err("regex.find takes 2 arguments (pattern, text)"));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(err("regex.find requires string arguments"));
                };
                let re = Regex::new(pattern).map_err(|e| err(format!("invalid regex: {e}")))?;
                match re.find(text) {
                    Some(m) => Ok(Value::Variant("Some".into(), vec![Value::String(m.as_str().to_string())])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "find_all" => {
                if args.len() != 2 {
                    return Err(err("regex.find_all takes 2 arguments (pattern, text)"));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(err("regex.find_all requires string arguments"));
                };
                let re = Regex::new(pattern).map_err(|e| err(format!("invalid regex: {e}")))?;
                let matches: Vec<Value> = re.find_iter(text)
                    .map(|m| Value::String(m.as_str().to_string()))
                    .collect();
                Ok(Value::List(Rc::new(matches)))
            }
            "split" => {
                if args.len() != 2 {
                    return Err(err("regex.split takes 2 arguments (pattern, text)"));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(err("regex.split requires string arguments"));
                };
                let re = Regex::new(pattern).map_err(|e| err(format!("invalid regex: {e}")))?;
                let parts: Vec<Value> = re.split(text).map(|s| Value::String(s.to_string())).collect();
                Ok(Value::List(Rc::new(parts)))
            }
            "replace" => {
                if args.len() != 3 {
                    return Err(err("regex.replace takes 3 arguments (pattern, text, replacement)"));
                }
                let (Value::String(pattern), Value::String(text), Value::String(replacement)) = (&args[0], &args[1], &args[2]) else {
                    return Err(err("regex.replace requires string arguments"));
                };
                let re = Regex::new(pattern).map_err(|e| err(format!("invalid regex: {e}")))?;
                Ok(Value::String(re.replace(text, replacement.as_str()).to_string()))
            }
            "replace_all" => {
                if args.len() != 3 {
                    return Err(err("regex.replace_all takes 3 arguments (pattern, text, replacement)"));
                }
                let (Value::String(pattern), Value::String(text), Value::String(replacement)) = (&args[0], &args[1], &args[2]) else {
                    return Err(err("regex.replace_all requires string arguments"));
                };
                let re = Regex::new(pattern).map_err(|e| err(format!("invalid regex: {e}")))?;
                Ok(Value::String(re.replace_all(text, replacement.as_str()).to_string()))
            }
            "captures" => {
                if args.len() != 2 { return Err(err("regex.captures takes 2 arguments (pattern, text)")); }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(err("regex.captures requires string arguments"));
                };
                let re = Regex::new(pattern).map_err(|e| err(format!("invalid regex: {e}")))?;
                match re.captures(text) {
                    Some(caps) => {
                        let groups: Vec<Value> = caps.iter()
                            .map(|m| match m {
                                Some(m) => Value::String(m.as_str().to_string()),
                                None => Value::String(String::new()),
                            })
                            .collect();
                        Ok(Value::Variant("Some".into(), vec![Value::List(Rc::new(groups))]))
                    }
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "captures_all" => {
                if args.len() != 2 { return Err(err("regex.captures_all takes 2 arguments (pattern, text)")); }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(err("regex.captures_all requires string arguments"));
                };
                let re = Regex::new(pattern).map_err(|e| err(format!("invalid regex: {e}")))?;
                let all_captures: Vec<Value> = re.captures_iter(text)
                    .map(|caps| {
                        let groups: Vec<Value> = caps.iter()
                            .map(|m| match m {
                                Some(m) => Value::String(m.as_str().to_string()),
                                None => Value::String(String::new()),
                            })
                            .collect();
                        Value::List(Rc::new(groups))
                    })
                    .collect();
                Ok(Value::List(Rc::new(all_captures)))
            }
            _ => Err(err(format!("unknown regex function: {name}"))),
        }
    }

    fn dispatch_json(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            // ── json module ─────────────────────────────────────────
            "parse" => {
                if args.len() != 1 {
                    return Err(err("json.parse takes 1 argument (string)"));
                }
                let Value::String(s) = &args[0] else {
                    return Err(err("json.parse requires a string argument"));
                };
                match serde_json::from_str::<serde_json::Value>(s) {
                    Ok(v) => Ok(Value::Variant("Ok".into(), vec![json_to_value(v)])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "stringify" => {
                if args.len() != 1 {
                    return Err(err("json.stringify takes 1 argument"));
                }
                let j = value_to_json(&args[0]);
                Ok(Value::String(j.to_string()))
            }
            "pretty" => {
                if args.len() != 1 {
                    return Err(err("json.pretty takes 1 argument"));
                }
                let j = value_to_json(&args[0]);
                Ok(Value::String(serde_json::to_string_pretty(&j).unwrap_or_else(|_| j.to_string())))
            }
            _ => Err(err(format!("unknown json function: {name}"))),
        }
    }

    fn dispatch_math(&self, name: &str, args: &[Value]) -> Result<Value> {
        match name {
            "sqrt" => {
                if args.len() != 1 { return Err(err("math.sqrt takes 1 argument")); }
                let Value::Float(f) = &args[0] else { return Err(err("math.sqrt requires a float")); };
                Ok(Value::Float(f.sqrt()))
            }
            "pow" => {
                if args.len() != 2 { return Err(err("math.pow takes 2 arguments")); }
                let Value::Float(base) = &args[0] else { return Err(err("math.pow requires floats")); };
                let Value::Float(exp) = &args[1] else { return Err(err("math.pow requires floats")); };
                Ok(Value::Float(base.powf(*exp)))
            }
            "log" => {
                if args.len() != 1 { return Err(err("math.log takes 1 argument")); }
                let Value::Float(f) = &args[0] else { return Err(err("math.log requires a float")); };
                Ok(Value::Float(f.ln()))
            }
            "log10" => {
                if args.len() != 1 { return Err(err("math.log10 takes 1 argument")); }
                let Value::Float(f) = &args[0] else { return Err(err("math.log10 requires a float")); };
                Ok(Value::Float(f.log10()))
            }
            "sin" => {
                if args.len() != 1 { return Err(err("math.sin takes 1 argument")); }
                let Value::Float(f) = &args[0] else { return Err(err("math.sin requires a float")); };
                Ok(Value::Float(f.sin()))
            }
            "cos" => {
                if args.len() != 1 { return Err(err("math.cos takes 1 argument")); }
                let Value::Float(f) = &args[0] else { return Err(err("math.cos requires a float")); };
                Ok(Value::Float(f.cos()))
            }
            "tan" => {
                if args.len() != 1 { return Err(err("math.tan takes 1 argument")); }
                let Value::Float(f) = &args[0] else { return Err(err("math.tan requires a float")); };
                Ok(Value::Float(f.tan()))
            }
            "asin" => {
                if args.len() != 1 { return Err(err("math.asin takes 1 argument")); }
                let Value::Float(f) = &args[0] else { return Err(err("math.asin requires a float")); };
                Ok(Value::Float(f.asin()))
            }
            "acos" => {
                if args.len() != 1 { return Err(err("math.acos takes 1 argument")); }
                let Value::Float(f) = &args[0] else { return Err(err("math.acos requires a float")); };
                Ok(Value::Float(f.acos()))
            }
            "atan" => {
                if args.len() != 1 { return Err(err("math.atan takes 1 argument")); }
                let Value::Float(f) = &args[0] else { return Err(err("math.atan requires a float")); };
                Ok(Value::Float(f.atan()))
            }
            "atan2" => {
                if args.len() != 2 { return Err(err("math.atan2 takes 2 arguments")); }
                let Value::Float(y) = &args[0] else { return Err(err("math.atan2 requires floats")); };
                let Value::Float(x) = &args[1] else { return Err(err("math.atan2 requires floats")); };
                Ok(Value::Float(y.atan2(*x)))
            }
            "pi" => Ok(Value::Float(std::f64::consts::PI)),
            "e" => Ok(Value::Float(std::f64::consts::E)),
            _ => Err(err(format!("unknown math function: {name}"))),
        }
    }

    fn dispatch_trait_method(&self, name: &str, args: &[Value]) -> Result<Value> {
        let receiver = self.trait_method_receiver.borrow_mut().take()
            .ok_or_else(|| err("internal: no receiver for trait method"))?;
        match name {
            "display" => Ok(Value::String(format!("{receiver}"))),
            "equal" => {
                let other = args.first().ok_or_else(|| err("equal() requires 1 argument"))?;
                Ok(Value::Bool(receiver == *other))
            }
            "compare" => {
                let other = args.first().ok_or_else(|| err("compare() requires 1 argument"))?;
                let ord = receiver.cmp(other);
                Ok(Value::Int(match ord {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
            }
            "hash" => {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                receiver.hash(&mut hasher);
                Ok(Value::Int(hasher.finish() as i64))
            }
            _ => Err(err(format!("unknown trait method: {name}"))),
        }
    }

    fn builtin_map(&self, list: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to map must be a list"));
        };
        let mut results = Vec::new();
        for item in xs.iter() {
            results.push(self.call_value(func, &[item.clone()])?);
        }
        Ok(Value::List(Rc::new(results)))
    }

    fn builtin_filter(&self, list: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to filter must be a list"));
        };
        let mut results = Vec::new();
        for item in xs.iter() {
            let result = self.call_value(func, &[item.clone()])?;
            if is_truthy(&result) {
                results.push(item.clone());
            }
        }
        Ok(Value::List(Rc::new(results)))
    }

    fn builtin_each(&self, list: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to each must be a list"));
        };
        for item in xs.iter() {
            self.call_value(func, &[item.clone()])?;
        }
        Ok(Value::Unit)
    }

    fn builtin_fold(&self, list: &Value, init: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to fold must be a list"));
        };
        let mut acc = init.clone();
        for item in xs.iter() {
            acc = self.call_value(func, &[acc, item.clone()])?;
        }
        Ok(acc)
    }

    fn builtin_find(&self, list: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to find must be a list"));
        };
        for item in xs.iter() {
            let result = self.call_value(func, &[item.clone()])?;
            if is_truthy(&result) {
                return Ok(Value::Variant("Some".into(), vec![item.clone()]));
            }
        }
        Ok(Value::Variant("None".into(), Vec::new()))
    }

    fn builtin_sort_by(&self, list: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to sort_by must be a list"));
        };
        let mut pairs: Vec<(Value, Value)> = Vec::new();
        for item in xs.iter() {
            let key = self.call_value(func, &[item.clone()])?;
            pairs.push((key, item.clone()));
        }
        pairs.sort_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let sorted: Vec<Value> = pairs.into_iter().map(|(_, v)| v).collect();
        Ok(Value::List(Rc::new(sorted)))
    }

    fn builtin_flat_map(&self, list: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to list.flat_map must be a list"));
        };
        let mut results = Vec::new();
        for item in xs.iter() {
            let mapped = self.call_value(func, &[item.clone()])?;
            match mapped {
                Value::List(inner) => results.extend(inner.iter().cloned()),
                other => results.push(other),
            }
        }
        Ok(Value::List(Rc::new(results)))
    }

    fn builtin_any(&self, list: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to list.any must be a list"));
        };
        for item in xs.iter() {
            let result = self.call_value(func, &[item.clone()])?;
            if is_truthy(&result) {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    }

    fn builtin_all(&self, list: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to list.all must be a list"));
        };
        for item in xs.iter() {
            let result = self.call_value(func, &[item.clone()])?;
            if !is_truthy(&result) {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    }

    fn builtin_fold_until(&self, list: &Value, init: &Value, func: &Value) -> Result<Value> {
        let Value::List(xs) = list else {
            return Err(err("first argument to list.fold_until must be a list"));
        };
        let mut acc = init.clone();
        for item in xs.iter() {
            let result = self.call_value(func, &[acc.clone(), item.clone()])?;
            match &result {
                Value::Variant(name, fields) if name == "Continue" && fields.len() == 1 => {
                    acc = fields[0].clone();
                }
                Value::Variant(name, fields) if name == "Stop" && fields.len() == 1 => {
                    return Ok(fields[0].clone());
                }
                _ => {
                    return Err(err(
                        "list.fold_until callback must return Continue(acc) or Stop(result)",
                    ));
                }
            }
        }
        Ok(acc)
    }

    fn builtin_unfold(&self, seed: &Value, func: &Value) -> Result<Value> {
        let mut state = seed.clone();
        let mut result = Vec::new();
        loop {
            let step = self.call_value(func, &[state.clone()])?;
            match &step {
                Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
                    match &fields[0] {
                        Value::Tuple(pair) if pair.len() == 2 => {
                            result.push(pair[0].clone());
                            state = pair[1].clone();
                        }
                        _ => {
                            return Err(err(
                                "list.unfold: Some must contain a (value, next_state) tuple",
                            ));
                        }
                    }
                }
                Value::Variant(name, _) if name == "None" => {
                    break;
                }
                _ => {
                    return Err(err(
                        "list.unfold callback must return Some((value, next_state)) or None",
                    ));
                }
            }
        }
        Ok(Value::List(Rc::new(result)))
    }

    // ── Concurrency builtins ────────────────────────────────────────

    fn builtin_chan(&self, args: &[Expr], env: &Env) -> Result<Value> {
        let capacity = if args.is_empty() {
            0 // unbuffered
        } else if args.len() == 1 {
            let val = self.eval(&args[0], env)?;
            match val {
                Value::Int(n) if n >= 0 => n as usize,
                _ => return Err(err("chan() capacity must be a non-negative integer")),
            }
        } else {
            return Err(err("chan() takes 0 or 1 arguments"));
        };
        Ok(self.scheduler.borrow_mut().create_channel(capacity))
    }

    fn builtin_send(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 2 {
            return Err(err("send() takes 2 arguments (channel, value)"));
        }
        let ch_val = self.eval(&args[0], env)?;
        let val = self.eval(&args[1], env)?;
        let Value::Channel(ch) = ch_val else {
            return Err(err("first argument to send must be a channel"));
        };
        // In cooperative mode, try to send. If buffer is full, run pending tasks
        // to drain the channel, then retry.
        let max_retries = 10000;
        for _ in 0..max_retries {
            match ch.try_send(val.clone()) {
                TrySendResult::Sent => return Ok(Value::Unit),
                TrySendResult::Closed => {
                    return Err(err(format!("send on closed channel {}", ch.id)));
                }
                TrySendResult::Full => {}
            }
            // Run one round of pending tasks to try to unblock
            if !self.run_pending_tasks_once()? {
                return Err(err(format!(
                    "deadlock: channel {} is full and no task can drain it",
                    ch.id
                )));
            }
        }
        Err(err(format!(
            "deadlock: channel {} is full and no task can drain it",
            ch.id
        )))
    }

    fn builtin_receive(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 1 {
            return Err(err("receive() takes 1 argument (channel)"));
        }
        let ch_val = self.eval(&args[0], env)?;
        let Value::Channel(ch) = ch_val else {
            return Err(err("first argument to receive must be a channel"));
        };
        // In cooperative mode, try to receive. If empty, run pending tasks.
        let max_retries = 10000;
        for _ in 0..max_retries {
            match ch.try_receive() {
                TryReceiveResult::Value(val) => return Ok(Value::Variant("Message".into(), vec![val])),
                TryReceiveResult::Closed => {
                    // Channel is closed and drained — return Closed variant.
                    return Ok(Value::Variant("Closed".into(), vec![]));
                }
                TryReceiveResult::Empty => {}
            }
            // Run one round of pending tasks to try to produce a value
            if !self.run_pending_tasks_once()? {
                return Err(err(format!(
                    "deadlock: channel {} is empty and no task can fill it",
                    ch.id
                )));
            }
        }
        Err(err(format!(
            "deadlock: channel {} is empty and no task can fill it",
            ch.id
        )))
    }

    fn builtin_close(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 1 {
            return Err(err("close() takes 1 argument (channel)"));
        }
        let ch_val = self.eval(&args[0], env)?;
        let Value::Channel(ch) = ch_val else {
            return Err(err("close() requires a channel argument"));
        };
        ch.close();
        Ok(Value::Unit)
    }

    fn builtin_try_send(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 2 {
            return Err(err("try_send takes 2 arguments (channel, value)"));
        }
        let ch_val = self.eval(&args[0], env)?;
        let val = self.eval(&args[1], env)?;
        let Value::Channel(ch) = ch_val else {
            return Err(err("try_send: first argument must be a channel"));
        };
        match ch.try_send(val) {
            TrySendResult::Sent => Ok(Value::Bool(true)),
            TrySendResult::Full | TrySendResult::Closed => Ok(Value::Bool(false)),
        }
    }

    fn builtin_try_receive(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 1 {
            return Err(err("try_receive takes 1 argument (channel)"));
        }
        let ch_val = self.eval(&args[0], env)?;
        let Value::Channel(ch) = ch_val else {
            return Err(err("try_receive: argument must be a channel"));
        };
        match ch.try_receive() {
            TryReceiveResult::Value(val) => Ok(Value::Variant("Message".into(), vec![val])),
            TryReceiveResult::Empty => Ok(Value::Variant("Empty".into(), Vec::new())),
            TryReceiveResult::Closed => Ok(Value::Variant("Closed".into(), Vec::new())),
        }
    }

    fn builtin_select(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 1 {
            return Err(err("channel.select takes 1 argument (list of channels)"));
        }
        let list_val = self.eval(&args[0], env)?;
        let Value::List(channels) = list_val else {
            return Err(err("channel.select argument must be a list of channels"));
        };

        let channel_refs: Vec<Rc<Channel>> = channels
            .iter()
            .map(|v| match v {
                Value::Channel(ch) => Ok(ch.clone()),
                _ => Err(err("channel.select list must contain only channels")),
            })
            .collect::<Result<_>>()?;

        if channel_refs.is_empty() {
            return Err(err("channel.select requires at least one channel"));
        }

        let max_retries = 10000;
        for _ in 0..max_retries {
            let mut all_closed = true;
            for ch in &channel_refs {
                match ch.try_receive() {
                    TryReceiveResult::Value(val) => {
                        return Ok(Value::Tuple(vec![
                            Value::Channel(ch.clone()),
                            val,
                        ]));
                    }
                    TryReceiveResult::Closed => {
                        continue;
                    }
                    TryReceiveResult::Empty => {
                        all_closed = false;
                    }
                }
            }
            if all_closed {
                return Ok(Value::Variant("Closed".into(), vec![]));
            }
            if !self.run_pending_tasks_once()? {
                return Err(err(
                    "channel.select: deadlock detected - no channels have data and no tasks can make progress",
                ));
            }
        }
        Err(err("channel.select: exceeded maximum retries"))
    }

    fn builtin_spawn(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 1 {
            return Err(err("spawn takes 1 argument (a function)"));
        }
        let func_val = self.eval(&args[0], env)?;
        match func_val {
            Value::Closure(c) => {
                // Create a task with the closure body and its captured environment
                let task_env = c.env.child();
                // Bind any parameters (should be zero for spawn fn() { ... })
                Ok(self.scheduler.borrow_mut().spawn(c.body.clone(), task_env))
            }
            _ => Err(err("spawn requires a function argument")),
        }
    }

    fn builtin_join(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 1 {
            return Err(err("join() takes 1 argument (handle)"));
        }
        let handle_val = self.eval(&args[0], env)?;
        let Value::Handle(handle) = handle_val else {
            return Err(err("join() requires a handle argument"));
        };

        // Run all tasks until the target task completes
        let max_iterations = 100000;
        for _ in 0..max_iterations {
            // Check if the result is already available
            if let Some(result) = handle.result.borrow().as_ref() {
                return match result {
                    Ok(val) => Ok(val.clone()),
                    Err(msg) => Err(err(format!("joined task failed: {msg}"))),
                };
            }

            // Run one round of pending tasks
            if !self.run_pending_tasks_once()? {
                // No progress was made - check if target is done
                if let Some(result) = handle.result.borrow().as_ref() {
                    return match result {
                        Ok(val) => Ok(val.clone()),
                        Err(msg) => Err(err(format!("joined task failed: {msg}"))),
                    };
                }
                return Err(err("join: deadlock detected - target task not completed and no progress"));
            }
        }
        Err(err("join: exceeded maximum iterations"))
    }

    fn builtin_cancel(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 1 {
            return Err(err("cancel() takes 1 argument (handle)"));
        }
        let handle_val = self.eval(&args[0], env)?;
        let Value::Handle(handle) = handle_val else {
            return Err(err("cancel() requires a handle argument"));
        };
        self.scheduler.borrow_mut().cancel(handle.id);
        Ok(Value::Unit)
    }

    fn builtin_try(&self, args: &[Expr], env: &Env) -> Result<Value> {
        if args.len() != 1 {
            return Err(err("try takes 1 argument (a zero-argument function)"));
        }
        let func_val = self.eval(&args[0], env)?;
        match self.call_value(&func_val, &[]) {
            Ok(val) => Ok(Value::Variant("Ok".into(), vec![val])),
            Err(RuntimeError::Error(msg, _)) => {
                Ok(Value::Variant("Err".into(), vec![Value::String(msg)]))
            }
            Err(RuntimeError::Return(val)) => Ok(Value::Variant("Ok".into(), vec![val])),
            Err(RuntimeError::TailCall(_, _)) => Ok(Value::Variant(
                "Err".into(),
                vec![Value::String("unexpected tail call".into())],
            )),
            Err(RuntimeError::LoopRecur(_)) => Ok(Value::Variant(
                "Err".into(),
                vec![Value::String("loop() used outside of loop body".into())],
            )),
        }
    }

    /// Run one round of pending tasks. Returns true if any progress was made.
    fn run_pending_tasks_once(&self) -> Result<bool> {
        let mut tasks = self.scheduler.borrow_mut().take_ready_tasks();
        if tasks.is_empty() {
            return Ok(false);
        }

        let mut made_progress = false;
        let mut completed_indices = Vec::new();

        for (i, task) in tasks.iter_mut().enumerate() {
            if task.state != TaskState::Ready {
                continue;
            }
            // Evaluate the task body
            let result = match self.eval(&task.body, &task.env) {
                Ok(val) => Ok(val),
                Err(RuntimeError::Return(val)) => Ok(val),
                Err(RuntimeError::TailCall(c, args)) => {
                    match self.call_closure(&c, &args) {
                        Ok(val) => Ok(val),
                        Err(RuntimeError::Error(msg, _)) => Err(msg),
                        Err(RuntimeError::Return(val)) => Ok(val),
                        Err(RuntimeError::TailCall(_, _)) => Err("unhandled tail call in task".into()),
                        Err(RuntimeError::LoopRecur(_)) => Err("loop() used outside of loop body".into()),
                    }
                }
                Err(RuntimeError::Error(msg, _)) => Err(msg),
                Err(RuntimeError::LoopRecur(_)) => Err("loop() used outside of loop body".into()),
            };
            *task.handle.result.borrow_mut() = Some(result);
            task.state = TaskState::Completed;
            completed_indices.push(i);
            made_progress = true;
        }

        // Return non-completed tasks
        let remaining: Vec<_> = tasks
            .into_iter()
            .filter(|t| t.state != TaskState::Completed && t.state != TaskState::Cancelled)
            .collect();
        self.scheduler.borrow_mut().return_tasks(remaining);

        Ok(made_progress)
    }

    fn call_closure(&self, closure: &Closure, args: &[Value]) -> Result<Value> {
        let mut current_closure: Rc<Closure> = Rc::new(Closure {
            params: closure.params.clone(),
            body: closure.body.clone(),
            env: closure.env.clone(),
        });
        let mut current_args: Vec<Value> = args.to_vec();
        loop {
            let call_env = current_closure.env.child();
            for (param, arg) in current_closure.params.iter().zip(current_args.iter()) {
                self.bind_pattern(&param.pattern, arg, &call_env)?;
            }
            match self.eval_tail(&current_closure.body, &call_env) {
                Ok(val) => return Ok(val),
                Err(RuntimeError::Return(val)) => return Ok(val),
                Err(RuntimeError::TailCall(next_closure, next_args)) => {
                    current_closure = next_closure;
                    current_args = next_args;
                    // Loop back for the trampoline
                }
                Err(e) => return Err(e),
            }
        }
    }

    // ── Field access ─────────────────────────────────────────────────

    fn access_field(&self, val: &Value, field: &str) -> Result<Value> {
        // 1. Direct field access (records, tuples, maps)
        match val {
            Value::Record(_, fields) => {
                if let Some(v) = fields.get(field) {
                    return Ok(v.clone());
                }
            }
            Value::Tuple(elems) => {
                if let Ok(idx) = field.parse::<usize>() {
                    return elems.get(idx).cloned()
                        .ok_or_else(|| err(format!("tuple index {idx} out of bounds")));
                }
            }
            Value::Map(m) => {
                let key_val = Value::String(field.to_string());
                if let Some(v) = m.get(&key_val) {
                    return Ok(v.clone());
                }
            }
            _ => {}
        }

        // 2. Method table lookup
        let type_name = self.value_type_name(val);
        if let Some(method) = self.method_table.get(&(type_name.clone(), field.to_string())) {
            return self.dispatch_method(val, method);
        }

        // 3. Legacy fallback: "TypeName.method" in global env
        let legacy_key = format!("{type_name}.{field}");
        if let Some(method) = self.global.get(&legacy_key) {
            return match method {
                Value::Closure(c) => {
                    let bound_env = c.env.child();
                    bound_env.define("self".to_string(), val.clone());
                    let remaining_params: Vec<Param> =
                        c.params.iter().skip(1).cloned().collect();
                    Ok(Value::Closure(Rc::new(Closure {
                        params: remaining_params,
                        body: c.body.clone(),
                        env: bound_env,
                    })))
                }
                _ => Ok(method),
            };
        }

        // 4. Error
        Err(err(format!("no field or method '{field}' on {type_name}")))
    }

    fn dispatch_method(&self, receiver: &Value, method: &RuntimeMethod) -> Result<Value> {
        match method {
            RuntimeMethod::Closure(c) => {
                let bound_env = c.env.child();
                bound_env.define("self".to_string(), receiver.clone());
                let remaining_params: Vec<Param> =
                    c.params.iter().skip(1).cloned().collect();
                Ok(Value::Closure(Rc::new(Closure {
                    params: remaining_params,
                    body: c.body.clone(),
                    env: bound_env,
                })))
            }
            RuntimeMethod::Builtin(b) => {
                *self.trait_method_receiver.borrow_mut() = Some(receiver.clone());
                let name = match b {
                    BuiltinTraitMethod::Display => "__trait.display",
                    BuiltinTraitMethod::Equal => "__trait.equal",
                    BuiltinTraitMethod::Compare => "__trait.compare",
                    BuiltinTraitMethod::Hash => "__trait.hash",
                };
                Ok(Value::BuiltinFn(name.to_string()))
            }
        }
    }

    fn value_type_name(&self, val: &Value) -> String {
        match val {
            Value::Int(_) => "Int".into(),
            Value::Float(_) => "Float".into(),
            Value::Bool(_) => "Bool".into(),
            Value::String(_) => "String".into(),
            Value::Unit => "Unit".into(),
            Value::List(_) => "List".into(),
            Value::Tuple(_) => "Tuple".into(),
            Value::Map(_) => "Map".into(),
            Value::Record(name, _) => name.clone(),
            Value::Variant(name, _) => self.variant_types.get(name).cloned().unwrap_or_else(|| name.clone()),
            Value::Channel(_) => "Channel".into(),
            _ => "<unknown>".into(),
        }
    }

    // ── Pattern matching ─────────────────────────────────────────────

    fn eval_match(&self, val: &Value, arms: &[MatchArm], env: &Env, tail: bool) -> Result<Value> {
        for arm in arms {
            let arm_env = env.child();
            if self.try_bind_pattern(&arm.pattern, val, &arm_env, env) {
                // Check guard
                if let Some(guard) = &arm.guard {
                    let guard_val = self.eval(guard, &arm_env)?;
                    if !is_truthy(&guard_val) {
                        continue;
                    }
                }
                return self.eval_inner(&arm.body, &arm_env, tail);
            }
        }
        Err(err(format!("non-exhaustive match: no arm matched {val}")))
    }

    fn eval_guardless_match(&self, arms: &[MatchArm], env: &Env, tail: bool) -> Result<Value> {
        for arm in arms {
            if let Some(condition) = &arm.guard {
                let cond_val = self.eval(condition, env)?;
                if is_truthy(&cond_val) {
                    return self.eval_inner(&arm.body, env, tail);
                }
            } else {
                // Bare wildcard — always matches (default/else case)
                return self.eval_inner(&arm.body, env, tail);
            }
        }
        Err(err("non-exhaustive match: no condition was true"))
    }

    fn try_bind_pattern(&self, pattern: &Pattern, val: &Value, env: &Env, outer_env: &Env) -> bool {
        match (pattern, val) {
            (Pattern::Wildcard, _) => true,
            (Pattern::Ident(name), _) => {
                env.define(name.clone(), val.clone());
                true
            }
            (Pattern::Pin(name), _) => {
                match outer_env.get(name) {
                    Some(pinned_val) => &pinned_val == val,
                    None => false,
                }
            }
            (Pattern::Int(n), Value::Int(v)) => n == v,
            (Pattern::Float(n), Value::Float(v)) => n == v,
            (Pattern::Bool(b), Value::Bool(v)) => b == v,
            (Pattern::StringLit(s), Value::String(v)) => s == v,
            (Pattern::Tuple(pats), Value::Tuple(vals)) => {
                if pats.len() != vals.len() {
                    return false;
                }
                pats.iter()
                    .zip(vals.iter())
                    .all(|(p, v)| self.try_bind_pattern(p, v, env, outer_env))
            }
            (Pattern::Constructor(name, pats), Value::Variant(vname, fields)) => {
                if name != vname {
                    return false;
                }
                if pats.len() != fields.len() {
                    return false;
                }
                pats.iter()
                    .zip(fields.iter())
                    .all(|(p, v)| self.try_bind_pattern(p, v, env, outer_env))
            }
            (Pattern::List(pats, rest), Value::List(list)) => {
                let items = list.as_ref();
                if rest.is_some() {
                    if items.len() < pats.len() {
                        return false;
                    }
                } else {
                    if items.len() != pats.len() {
                        return false;
                    }
                }
                for (pat, val) in pats.iter().zip(items.iter()) {
                    if !self.try_bind_pattern(pat, val, env, outer_env) {
                        return false;
                    }
                }
                if let Some(rest_pat) = rest {
                    let remaining: Vec<Value> = items[pats.len()..].to_vec();
                    let rest_val = Value::List(Rc::new(remaining));
                    if !self.try_bind_pattern(rest_pat, &rest_val, env, outer_env) {
                        return false;
                    }
                }
                true
            }
            (Pattern::Record { name: _, fields, has_rest }, Value::Record(_rname, rec_fields)) => {
                for (fname, sub_pat) in fields {
                    let Some(val) = rec_fields.get(fname) else {
                        return false;
                    };
                    match sub_pat {
                        Some(pat) => {
                            if !self.try_bind_pattern(pat, val, env, outer_env) {
                                return false;
                            }
                        }
                        None => {
                            // Shorthand: `{ name, age }` binds field values to names
                            env.define(fname.clone(), val.clone());
                        }
                    }
                }
                if !has_rest && fields.len() < rec_fields.len() {
                    // Not all fields matched and no `..`
                    // Actually, allow partial matching by default for usability
                }
                true
            }
            (Pattern::Or(alts), _) => {
                alts.iter().any(|alt| self.try_bind_pattern(alt, val, env, outer_env))
            }
            (Pattern::Range(start, end), Value::Int(n)) => {
                *n >= *start && *n <= *end
            }
            (Pattern::Map(entries), Value::Map(map)) => {
                for (key, pat) in entries {
                    let key_val = Value::String(key.clone());
                    let Some(val) = map.get(&key_val) else { return false; };
                    if !self.try_bind_pattern(pat, val, env, outer_env) { return false; }
                }
                true
            }
            // Handle matching tuples against constructor patterns (for match on tuple of ints)
            (Pattern::Int(n), _) => {
                if let Value::Int(v) = val {
                    n == v
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn bind_pattern(&self, pattern: &Pattern, val: &Value, env: &Env) -> Result<()> {
        if self.try_bind_pattern(pattern, val, env, env) {
            Ok(())
        } else {
            Err(err(format!(
                "pattern match failed: cannot bind {val} to {pattern:?}"
            )))
        }
    }
}

// ── Binary operations ────────────────────────────────────────────────

fn eval_binary(left: Value, op: BinOp, right: Value, span: Span) -> Result<Value> {
    match (&left, op, &right) {
        // Integer arithmetic
        (Value::Int(a), BinOp::Add, Value::Int(b)) => Ok(Value::Int(a + b)),
        (Value::Int(a), BinOp::Sub, Value::Int(b)) => Ok(Value::Int(a - b)),
        (Value::Int(a), BinOp::Mul, Value::Int(b)) => Ok(Value::Int(a * b)),
        (Value::Int(a), BinOp::Div, Value::Int(b)) => {
            if *b == 0 {
                Err(err_at("division by zero", span))
            } else {
                Ok(Value::Int(a / b))
            }
        }
        (Value::Int(a), BinOp::Mod, Value::Int(b)) => {
            if *b == 0 {
                Err(err_at("modulo by zero", span))
            } else {
                Ok(Value::Int(a % b))
            }
        }

        // Float arithmetic
        (Value::Float(a), BinOp::Add, Value::Float(b)) => Ok(Value::Float(a + b)),
        (Value::Float(a), BinOp::Sub, Value::Float(b)) => Ok(Value::Float(a - b)),
        (Value::Float(a), BinOp::Mul, Value::Float(b)) => Ok(Value::Float(a * b)),
        (Value::Float(a), BinOp::Div, Value::Float(b)) => Ok(Value::Float(a / b)),

        // Mixed int/float — rejected; use int.to_float() or float.to_int()
        (Value::Int(_), BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod, Value::Float(_))
        | (Value::Float(_), BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod, Value::Int(_)) => {
            Err(err_at("cannot mix Int and Float in arithmetic; use int.to_float() or float.to_int() to convert explicitly", span))
        }

        // String concatenation
        (Value::String(a), BinOp::Add, Value::String(b)) => {
            Ok(Value::String(format!("{a}{b}")))
        }

        // Comparisons — same-type only
        (Value::Int(_), BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq, Value::Int(_))
        | (Value::Float(_), BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq, Value::Float(_))
        | (Value::Bool(_), BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq, Value::Bool(_))
        | (Value::String(_), BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq, Value::String(_))
        | (Value::Unit, BinOp::Eq | BinOp::Neq, Value::Unit) => {
            match op {
                BinOp::Eq => Ok(Value::Bool(left == right)),
                BinOp::Neq => Ok(Value::Bool(left != right)),
                BinOp::Lt => Ok(Value::Bool(left < right)),
                BinOp::Gt => Ok(Value::Bool(left > right)),
                BinOp::Leq => Ok(Value::Bool(left <= right)),
                BinOp::Geq => Ok(Value::Bool(left >= right)),
                _ => unreachable!(),
            }
        }
        // Structural equality for compound types (same type required)
        (Value::List(_), BinOp::Eq, Value::List(_))
        | (Value::Tuple(_), BinOp::Eq, Value::Tuple(_))
        | (Value::Map(_), BinOp::Eq, Value::Map(_))
        | (Value::Record(..), BinOp::Eq, Value::Record(..))
        | (Value::Variant(..), BinOp::Eq, Value::Variant(..))
        | (Value::Channel(_), BinOp::Eq, Value::Channel(_)) => Ok(Value::Bool(left == right)),
        (Value::List(_), BinOp::Neq, Value::List(_))
        | (Value::Tuple(_), BinOp::Neq, Value::Tuple(_))
        | (Value::Map(_), BinOp::Neq, Value::Map(_))
        | (Value::Record(..), BinOp::Neq, Value::Record(..))
        | (Value::Variant(..), BinOp::Neq, Value::Variant(..))
        | (Value::Channel(_), BinOp::Neq, Value::Channel(_)) => Ok(Value::Bool(left != right)),
        // Ordering for compound types
        (Value::List(_), BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq, Value::List(_))
        | (Value::Tuple(_), BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq, Value::Tuple(_))
        | (Value::Variant(..), BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq, Value::Variant(..)) => {
            match op {
                BinOp::Lt => Ok(Value::Bool(left < right)),
                BinOp::Gt => Ok(Value::Bool(left > right)),
                BinOp::Leq => Ok(Value::Bool(left <= right)),
                BinOp::Geq => Ok(Value::Bool(left >= right)),
                _ => unreachable!(),
            }
        }

        // Boolean
        (Value::Bool(a), BinOp::And, Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
        (Value::Bool(a), BinOp::Or, Value::Bool(b)) => Ok(Value::Bool(*a || *b)),

        _ => Err(err_at(format!(
            "unsupported operation: {left} {op} {right}"
        ), span)),
    }
}

fn is_truthy(val: &Value) -> bool {
    match val {
        Value::Bool(b) => *b,
        Value::Int(0) => false,
        Value::String(s) if s.is_empty() => false,
        Value::Unit => false,
        Value::Variant(name, _) if name == "None" => false,
        _ => true,
    }
}

// ── JSON helpers ────────────────────────────────────────────────────

fn json_to_value(v: serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Variant("None".into(), Vec::new()),
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Float(0.0)
            }
        }
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Array(arr) => {
            Value::List(Rc::new(arr.into_iter().map(json_to_value).collect()))
        }
        serde_json::Value::Object(obj) => {
            let map: BTreeMap<Value, Value> = obj
                .into_iter()
                .map(|(k, v)| (Value::String(k), json_to_value(v)))
                .collect();
            Value::Map(Rc::new(map))
        }
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::List(xs) => serde_json::Value::Array(xs.iter().map(value_to_json).collect()),
        Value::Map(m) => {
            let obj: serde_json::Map<std::string::String, serde_json::Value> = m
                .iter()
                .map(|(k, v)| (k.to_string(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        Value::Tuple(vs) => serde_json::Value::Array(vs.iter().map(value_to_json).collect()),
        Value::Record(_name, fields) => {
            let obj: serde_json::Map<std::string::String, serde_json::Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        Value::Variant(name, fields) if name == "None" && fields.is_empty() => {
            serde_json::Value::Null
        }
        Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
            value_to_json(&fields[0])
        }
        Value::Variant(name, fields) => {
            let mut obj = serde_json::Map::new();
            obj.insert("variant".into(), serde_json::Value::String(name.clone()));
            if !fields.is_empty() {
                obj.insert(
                    "fields".into(),
                    serde_json::Value::Array(fields.iter().map(value_to_json).collect()),
                );
            }
            serde_json::Value::Object(obj)
        }
        Value::Unit => serde_json::Value::Null,
        Value::VariantConstructor(name, _) => serde_json::Value::String(name.clone()),
        _ => serde_json::Value::Null, // Closure, BuiltinFn, Channel, Handle
    }
}

// ── Builtins ─────────────────────────────────────────────────────────

fn register_builtins(env: &Env) {
    // Variant constructors
    env.define("Ok".into(), Value::VariantConstructor("Ok".into(), 1));
    env.define("Err".into(), Value::VariantConstructor("Err".into(), 1));
    env.define("Some".into(), Value::VariantConstructor("Some".into(), 1));
    env.define("None".into(), Value::Variant("None".into(), Vec::new()));
    env.define("Stop".into(), Value::VariantConstructor("Stop".into(), 1));
    env.define("Continue".into(), Value::VariantConstructor("Continue".into(), 1));
    env.define("Message".into(), Value::VariantConstructor("Message".into(), 1));
    env.define("Closed".into(), Value::Variant("Closed".into(), Vec::new()));
    env.define("Empty".into(), Value::Variant("Empty".into(), Vec::new()));

    // All builtins — just register names; dispatch_builtin handles implementation
    let builtin_names = [
        "print",
        "println",
        "io.inspect",
        "panic",
        "list.map",
        "list.filter",
        "list.each",
        "list.fold",
        "list.find",
        "list.zip",
        "list.flatten",
        "list.sort_by",
        "list.flat_map",
        "list.any",
        "list.all",
        "list.fold_until",
        "list.unfold",
        "list.head",
        "list.tail",
        "list.last",
        "list.reverse",
        "list.sort",
        "list.unique",
        "list.contains",
        "list.length",
        "list.append",
        "list.prepend",
        "list.concat",
        "list.get",
        "list.take",
        "list.drop",
        "list.enumerate",
        "list.group_by",
        "result.unwrap_or",
        "result.map_ok",
        "result.map_err",
        "result.flatten",
        "result.is_ok",
        "result.is_err",
        "option.map",
        "option.unwrap_or",
        "option.to_result",
        "option.is_some",
        "option.is_none",
        "string.split",
        "string.trim",
        "string.contains",
        "string.replace",
        "string.join",
        "string.length",
        "string.to_upper",
        "string.to_lower",
        "string.starts_with",
        "string.ends_with",
        "string.chars",
        "string.repeat",
        "string.index_of",
        "string.slice",
        "string.pad_left",
        "string.pad_right",
        "int.parse",
        "int.abs",
        "int.min",
        "int.max",
        "int.to_float",
        "int.to_string",
        "float.parse",
        "float.round",
        "float.ceil",
        "float.floor",
        "float.abs",
        "float.to_string",
        "float.to_int",
        "float.min",
        "float.max",
        "map.get",
        "map.set",
        "map.delete",
        "map.has_key",
        "map.keys",
        "map.values",
        "map.length",
        "map.merge",
        "map.filter",
        "map.map",
        "map.entries",
        "map.from_entries",
        "io.read_file",
        "io.write_file",
        "io.read_line",
        "io.args",
        "test.assert",
        "test.assert_eq",
        "test.assert_ne",
        "regex.is_match",
        "regex.find",
        "regex.find_all",
        "regex.split",
        "regex.replace",
        "regex.replace_all",
        "regex.captures",
        "regex.captures_all",
        "json.parse",
        "json.stringify",
        "json.pretty",
        "math.sqrt",
        "math.pow",
        "math.log",
        "math.log10",
        "math.sin",
        "math.cos",
        "math.tan",
        "math.asin",
        "math.acos",
        "math.atan",
        "math.atan2",
    ];

    for name in builtin_names {
        env.define(name.into(), Value::BuiltinFn(name.into()));
    }

    // Constants (registered as values, not functions)
    env.define("math.pi".into(), Value::Float(std::f64::consts::PI));
    env.define("math.e".into(), Value::Float(std::f64::consts::E));
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run(input: &str) -> Value {
        let tokens = Lexer::new(input).tokenize().unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut interp = Interpreter::new();
        interp.run(&program).unwrap()
    }

    #[test]
    fn test_hello_world() {
        run(r#"
            fn main() {
                println("hello, world")
            }
        "#);
    }

    #[test]
    fn test_arithmetic() {
        let result = run(r#"
            fn main() {
                2 + 3 * 4
            }
        "#);
        assert_eq!(result, Value::Int(14));
    }

    #[test]
    fn test_let_binding() {
        let result = run(r#"
            fn main() {
                let x = 10
                let y = 20
                x + y
            }
        "#);
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_function_call() {
        let result = run(r#"
            fn add(a, b) {
                a + b
            }
            fn main() {
                add(3, 4)
            }
        "#);
        assert_eq!(result, Value::Int(7));
    }

    #[test]
    fn test_match_expr() {
        let result = run(r#"
            fn main() {
                let x = 2
                match x {
                    1 -> "one"
                    2 -> "two"
                    _ -> "other"
                }
            }
        "#);
        assert_eq!(result, Value::String("two".into()));
    }

    #[test]
    fn test_pipe_and_map() {
        let result = run(r#"
            fn main() {
                let xs = [1, 2, 3]
                xs |> list.map { x -> x * 2 }
            }
        "#);
        assert_eq!(
            result,
            Value::List(Rc::new(vec![Value::Int(2), Value::Int(4), Value::Int(6)]))
        );
    }

    #[test]
    fn test_range() {
        let result = run(r#"
            fn main() {
                1..4
            }
        "#);
        assert_eq!(
            result,
            Value::List(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
        );
    }

    #[test]
    fn test_string_interpolation() {
        let result = run(r#"
            fn main() {
                let name = "world"
                "hello {name}"
            }
        "#);
        assert_eq!(result, Value::String("hello world".into()));
    }

    #[test]
    fn test_record_create_and_access() {
        let result = run(r#"
            type User {
                name: String,
                age: Int,
            }
            fn main() {
                let u = User { name: "Alice", age: 30 }
                u.name
            }
        "#);
        assert_eq!(result, Value::String("Alice".into()));
    }

    #[test]
    fn test_record_update() {
        let result = run(r#"
            type User {
                name: String,
                age: Int,
            }
            fn main() {
                let u = User { name: "Alice", age: 30 }
                let u2 = u.{ age: 31 }
                u2.age
            }
        "#);
        assert_eq!(result, Value::Int(31));
    }

    #[test]
    fn test_match_tuple() {
        let result = run(r#"
            fn fizzbuzz(n) {
                match (n % 3, n % 5) {
                    (0, 0) -> "FizzBuzz"
                    (0, _) -> "Fizz"
                    (_, 0) -> "Buzz"
                    _      -> "other"
                }
            }
            fn main() {
                fizzbuzz(15)
            }
        "#);
        assert_eq!(result, Value::String("FizzBuzz".into()));
    }

    #[test]
    fn test_option_matching() {
        let result = run(r#"
            fn main() {
                let x = Some(42)
                match x {
                    Some(n) -> n
                    None -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_closure() {
        let result = run(r#"
            fn apply(f, x) {
                f(x)
            }
            fn main() {
                let double = fn(x) { x * 2 }
                apply(double, 21)
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_filter() {
        let result = run(r#"
            fn main() {
                [1, 2, 3, 4, 5] |> list.filter { x -> x > 3 }
            }
        "#);
        assert_eq!(
            result,
            Value::List(Rc::new(vec![Value::Int(4), Value::Int(5)]))
        );
    }

    #[test]
    fn test_fold() {
        let result = run(r#"
            fn main() {
                [1, 2, 3] |> list.fold(0) { acc, x -> acc + x }
            }
        "#);
        assert_eq!(result, Value::Int(6));
    }

    // ── stdlib builtin tests ────────────────────────────────────────

    #[test]
    fn test_int_abs() {
        let result = run(r#"
            fn main() {
                int.abs(-42)
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_int_min_max() {
        let result = run(r#"
            fn main() {
                let a = int.min(3, 7)
                let b = int.max(3, 7)
                a + b
            }
        "#);
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_int_to_float() {
        let result = run(r#"
            fn main() {
                int.to_float(42)
            }
        "#);
        assert_eq!(result, Value::Float(42.0));
    }

    #[test]
    fn test_float_round_ceil_floor() {
        let result = run(r#"
            fn main() {
                let a = float.round(3.6)
                let b = float.ceil(3.2)
                let c = float.floor(3.9)
                (a, b, c)
            }
        "#);
        assert_eq!(result, Value::Tuple(vec![Value::Int(4), Value::Int(4), Value::Int(3)]));
    }

    #[test]
    fn test_float_abs() {
        let result = run(r#"
            fn main() {
                float.abs(-3.14)
            }
        "#);
        assert_eq!(result, Value::Float(3.14));
    }

    #[test]
    fn test_float_parse() {
        let result = run(r#"
            fn main() {
                match float.parse("3.14") {
                    Ok(n) -> float.round(n * 100.0)
                    Err(_) -> -1
                }
            }
        "#);
        assert_eq!(result, Value::Int(314));
    }

    #[test]
    fn test_map_get_set_delete() {
        let result = run(r#"
            fn main() {
                let m = #{"a": 1, "b": 2}
                let m2 = map.set(m, "c", 3)
                let m3 = map.delete(m2, "a")
                match map.get(m3, "c") {
                    Some(v) -> v
                    None -> -1
                }
            }
        "#);
        assert_eq!(result, Value::Int(3));
    }

    #[test]
    fn test_map_keys_values() {
        let result = run(r#"
            fn main() {
                let m = #{"x": 10, "y": 20}
                let ks = map.keys(m)
                list.length(ks)
            }
        "#);
        assert_eq!(result, Value::Int(2));
    }

    #[test]
    fn test_map_merge() {
        let result = run(r#"
            fn main() {
                let m1 = #{"a": 1}
                let m2 = #{"a": 99, "b": 2}
                let merged = map.merge(m1, m2)
                match map.get(merged, "a") {
                    Some(v) -> v
                    None -> -1
                }
            }
        "#);
        assert_eq!(result, Value::Int(99));
    }

    #[test]
    fn test_list_head_tail_last() {
        let result = run(r#"
            fn main() {
                let xs = [10, 20, 30]
                let h = match list.head(xs) {
                    Some(v) -> v
                    None -> 0
                }
                let t = list.tail(xs)
                let l = match list.last(xs) {
                    Some(v) -> v
                    None -> 0
                }
                (h, list.length(t), l)
            }
        "#);
        assert_eq!(result, Value::Tuple(vec![Value::Int(10), Value::Int(2), Value::Int(30)]));
    }

    #[test]
    fn test_list_reverse() {
        let result = run(r#"
            fn main() {
                list.reverse([1, 2, 3])
            }
        "#);
        assert_eq!(
            result,
            Value::List(Rc::new(vec![Value::Int(3), Value::Int(2), Value::Int(1)]))
        );
    }

    #[test]
    fn test_list_sort() {
        let result = run(r#"
            fn main() {
                list.sort([3, 1, 2])
            }
        "#);
        assert_eq!(
            result,
            Value::List(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
        );
    }

    #[test]
    fn test_list_contains_length() {
        let result = run(r#"
            fn main() {
                let xs = [1, 2, 3]
                let c = list.contains(xs, 2)
                let l = list.length(xs)
                (c, l)
            }
        "#);
        assert_eq!(result, Value::Tuple(vec![Value::Bool(true), Value::Int(3)]));
    }

    #[test]
    fn test_result_is_ok_is_err() {
        let result = run(r#"
            fn main() {
                let a = result.is_ok(Ok(1))
                let b = result.is_err(Ok(1))
                (a, b)
            }
        "#);
        assert_eq!(result, Value::Tuple(vec![Value::Bool(true), Value::Bool(false)]));
    }

    #[test]
    fn test_result_map_err() {
        let result = run(r#"
            fn main() {
                let r = Err("oops")
                match result.map_err(r, fn(e) { "error: " + e }) {
                    Err(msg) -> msg
                    Ok(_) -> "nope"
                }
            }
        "#);
        assert_eq!(result, Value::String("error: oops".into()));
    }

    #[test]
    fn test_result_flatten() {
        let result = run(r#"
            fn main() {
                let nested = Ok(Ok(42))
                match result.flatten(nested) {
                    Ok(n) -> n
                    Err(_) -> -1
                }
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_option_map() {
        let result = run(r#"
            fn main() {
                let x = Some(10)
                match option.map(x, fn(n) { n * 2 }) {
                    Some(v) -> v
                    None -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(20));
    }

    #[test]
    fn test_option_unwrap_or() {
        let result = run(r#"
            fn main() {
                let a = option.unwrap_or(Some(42), 0)
                let b = option.unwrap_or(None, 99)
                (a, b)
            }
        "#);
        assert_eq!(result, Value::Tuple(vec![Value::Int(42), Value::Int(99)]));
    }

    #[test]
    fn test_option_to_result() {
        let result = run(r#"
            fn main() {
                let a = option.to_result(Some(1), "missing")
                let b = option.to_result(None, "missing")
                let ok = result.is_ok(a)
                let err = result.is_err(b)
                (ok, err)
            }
        "#);
        assert_eq!(result, Value::Tuple(vec![Value::Bool(true), Value::Bool(true)]));
    }

    #[test]
    fn test_option_is_some_is_none() {
        let result = run(r#"
            fn main() {
                let a = option.is_some(Some(1))
                let b = option.is_none(None)
                (a, b)
            }
        "#);
        assert_eq!(result, Value::Tuple(vec![Value::Bool(true), Value::Bool(true)]));
    }

    #[test]
    fn test_string_length() {
        let result = run(r#"
            fn main() {
                string.length("hello")
            }
        "#);
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn test_string_to_upper_to_lower() {
        let result = run(r#"
            fn main() {
                let a = string.to_upper("hello")
                let b = string.to_lower("WORLD")
                (a, b)
            }
        "#);
        assert_eq!(result, Value::Tuple(vec![
            Value::String("HELLO".into()),
            Value::String("world".into()),
        ]));
    }

    #[test]
    fn test_string_starts_with_ends_with() {
        let result = run(r#"
            fn main() {
                let a = string.starts_with("hello world", "hello")
                let b = string.ends_with("hello world", "world")
                (a, b)
            }
        "#);
        assert_eq!(result, Value::Tuple(vec![Value::Bool(true), Value::Bool(true)]));
    }

    #[test]
    fn test_string_chars() {
        let result = run(r#"
            fn main() {
                let cs = string.chars("abc")
                list.length(cs)
            }
        "#);
        assert_eq!(result, Value::Int(3));
    }

    #[test]
    fn test_string_repeat() {
        let result = run(r#"
            fn main() {
                string.repeat("ab", 3)
            }
        "#);
        assert_eq!(result, Value::String("ababab".into()));
    }
}
