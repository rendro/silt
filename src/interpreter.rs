use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use crate::ast::*;
use crate::env::Env;
use crate::module::ModuleLoader;
use crate::scheduler::{Scheduler, TaskState};
use crate::value::{Closure, TryReceiveResult, TrySendResult, Value};

// ── Runtime error ────────────────────────────────────────────────────

pub enum RuntimeError {
    Error(String),
    Return(Value),
    TailCall(Rc<Closure>, Vec<Value>),
}

impl std::fmt::Debug for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Error(msg) => f.debug_tuple("Error").field(msg).finish(),
            RuntimeError::Return(val) => f.debug_tuple("Return").field(val).finish(),
            RuntimeError::TailCall(_, args) => {
                f.debug_tuple("TailCall").field(&"<closure>").field(args).finish()
            }
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Error(msg) => write!(f, "runtime error: {msg}"),
            RuntimeError::Return(_) => write!(f, "unexpected return outside function"),
            RuntimeError::TailCall(_, _) => write!(f, "unhandled tail call"),
        }
    }
}

type Result<T> = std::result::Result<T, RuntimeError>;

fn err(msg: impl Into<String>) -> RuntimeError {
    RuntimeError::Error(msg.into())
}

// ── Interpreter ──────────────────────────────────────────────────────

pub struct Interpreter {
    global: Env,
    /// Maps variant constructor names to their parent type name.
    variant_types: std::collections::HashMap<String, String>,
    /// Cooperative scheduler for concurrency.
    scheduler: RefCell<Scheduler>,
    /// Module loader for file-based imports.
    module_loader: RefCell<ModuleLoader>,
}

impl Interpreter {
    pub fn new() -> Self {
        Self::with_project_root(PathBuf::from("."))
    }

    pub fn with_project_root(project_root: PathBuf) -> Self {
        let global = Env::new();
        register_builtins(&global);
        Self {
            global,
            variant_types: std::collections::HashMap::new(),
            scheduler: RefCell::new(Scheduler::new()),
            module_loader: RefCell::new(ModuleLoader::new(project_root)),
        }
    }

    pub fn run(&mut self, program: &Program) -> Result<Value> {
        // First pass: register all top-level declarations
        for decl in &program.decls {
            self.register_decl(decl)?;
        }

        // Find and call main()
        match self.global.get("main") {
            Some(Value::Closure(c)) => self.call_closure(&c, &[]),
            Some(Value::BuiltinFn(_, _)) => Err(err("main cannot be a builtin")),
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
                // Register trait methods as `TypeName.method_name`
                for method in &ti.methods {
                    let key = format!("{}.{}", ti.target_type, method.name);
                    let closure = Value::Closure(Rc::new(Closure {
                        params: method.params.clone(),
                        body: method.body.clone(),
                        env: self.global.clone(),
                    }));
                    self.global.define(key, closure);
                }
            }
            Decl::Trait(_) => {
                // Trait declarations just define the interface; nothing to do at runtime
            }
            Decl::Import(target) => {
                self.process_import(target)?;
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

            ExprKind::Ident(name) => env.get(name).ok_or_else(|| err(format!("undefined: {name}"))),

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
                    let key = match self.eval(k, env)? {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
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
                let l = self.eval(left, env)?;
                let r = self.eval(right, env)?;
                eval_binary(l, *op, r)
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
                // Intercept concurrency builtins by name
                if let ExprKind::Ident(name) = &callee.kind {
                    match name.as_str() {
                        "chan" => return self.builtin_chan(args, env),
                        "send" => return self.builtin_send(args, env),
                        "receive" => return self.builtin_receive(args, env),
                        "close" => return self.builtin_close(args, env),
                        "try_send" => return self.builtin_try_send(args, env),
                        "try_receive" => return self.builtin_try_receive(args, env),
                        "spawn" => return self.builtin_spawn(args, env),
                        "join" => return self.builtin_join(args, env),
                        "cancel" => return self.builtin_cancel(args, env),
                        _ => {}
                    }
                }
                let func = self.eval(callee, env)?;
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<Result<_>>()?;
                if tail {
                    if let Value::Closure(c) = &func {
                        return Err(RuntimeError::TailCall(c.clone(), arg_vals));
                    }
                }
                self.call_value(&func, &arg_vals)
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
                        let mut new_fields = (*base_fields).clone();
                        for (fname, fexpr) in fields {
                            new_fields.insert(fname.clone(), self.eval(fexpr, env)?);
                        }
                        Ok(Value::Record(name, Rc::new(new_fields)))
                    }
                    _ => Err(err("record update on non-record value")),
                }
            }

            ExprKind::Match { expr, arms } => {
                let val = self.eval(expr, env)?;
                self.eval_match(&val, arms, env, tail)
            }

            ExprKind::Select { arms } => {
                self.eval_select(arms, env)
            }

            ExprKind::Return(val) => {
                let v = match val {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Unit,
                };
                Err(RuntimeError::Return(v))
            }

            ExprKind::Block(stmts) => self.eval_block_inner(stmts, env, tail),
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
                if !self.try_bind_pattern(pattern, &val, env) {
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
            Value::BuiltinFn(name, f) => {
                // Collection builtins that take closures need the interpreter
                // for method resolution (e.g., variant trait methods).
                if let Some(result) = self.try_collection_builtin(name, args) {
                    return result;
                }
                f(args).map_err(|e| err(format!("{name}: {e}")))
            }
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

    /// Handle collection builtins (map, filter, each, fold, find) that need
    /// the interpreter for closure calls (variant method resolution, etc.).
    fn try_collection_builtin(&self, name: &str, args: &[Value]) -> Option<Result<Value>> {
        match name {
            "map" if args.len() == 2 => Some(self.builtin_map(&args[0], &args[1])),
            "filter" if args.len() == 2 => Some(self.builtin_filter(&args[0], &args[1])),
            "each" if args.len() == 2 => Some(self.builtin_each(&args[0], &args[1])),
            "fold" if args.len() == 3 => Some(self.builtin_fold(&args[0], &args[1], &args[2])),
            "find" if args.len() == 2 => Some(self.builtin_find(&args[0], &args[1])),
            _ => None,
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
                TryReceiveResult::Value(val) => return Ok(val),
                TryReceiveResult::Closed => {
                    // Channel is closed and drained — return None variant.
                    return Ok(Value::Variant("None".into(), vec![]));
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
            TryReceiveResult::Value(val) => Ok(Value::Variant("Some".into(), vec![val])),
            TryReceiveResult::Empty | TryReceiveResult::Closed => {
                Ok(Value::Variant("None".into(), Vec::new()))
            }
        }
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
                        Err(RuntimeError::Error(msg)) => Err(msg),
                        Err(RuntimeError::Return(val)) => Ok(val),
                        Err(RuntimeError::TailCall(_, _)) => Err("unhandled tail call in task".into()),
                    }
                }
                Err(RuntimeError::Error(msg)) => Err(msg),
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

    /// Evaluate a select expression: poll multiple channels and execute the first available.
    fn eval_select(&self, arms: &[SelectArm], env: &Env) -> Result<Value> {
        let max_retries = 10000;
        for _ in 0..max_retries {
            let mut all_closed = true;
            // Try each arm in order
            for arm in arms {
                let ch_val = self.eval(&arm.channel, env)?;
                let Value::Channel(ch) = ch_val else {
                    return Err(err("select arm channel must be a channel"));
                };
                match ch.try_receive() {
                    TryReceiveResult::Value(val) => {
                        // Bind the received value and execute the body
                        let arm_env = env.child();
                        arm_env.define(arm.binding.clone(), val);
                        return self.eval(&arm.body, &arm_env);
                    }
                    TryReceiveResult::Closed => {
                        // Skip closed, empty channels
                        continue;
                    }
                    TryReceiveResult::Empty => {
                        all_closed = false;
                    }
                }
            }
            if all_closed {
                // All channels are closed and drained
                return Ok(Value::Variant("None".into(), vec![]));
            }
            // No channel had data; run pending tasks to try to produce some
            if !self.run_pending_tasks_once()? {
                return Err(err("select: deadlock detected - no channels have data and no tasks can make progress"));
            }
        }
        Err(err("select: exceeded maximum retries"))
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
        match val {
            Value::Record(type_name, fields) => {
                // First check for direct field access
                if let Some(v) = fields.get(field) {
                    return Ok(v.clone());
                }
                // Then check for trait method
                let method_key = format!("{type_name}.{field}");
                if let Some(method) = self.global.get(&method_key) {
                    match method {
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
                    }
                } else {
                    Err(err(format!(
                        "no field or method '{field}' on {type_name}"
                    )))
                }
            }
            Value::Variant(variant_name, _) => {
                let parent_type = self
                    .variant_types
                    .get(variant_name)
                    .cloned()
                    .unwrap_or_else(|| variant_name.clone());
                let method_key = format!("{parent_type}.{field}");
                if let Some(method) = self.global.get(&method_key) {
                    match method {
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
                    }
                } else {
                    Err(err(format!("no method '{field}' on {parent_type}")))
                }
            }
            Value::Tuple(elems) => {
                // Numeric field access on tuples
                if let Ok(idx) = field.parse::<usize>() {
                    elems
                        .get(idx)
                        .cloned()
                        .ok_or_else(|| err(format!("tuple index {idx} out of bounds")))
                } else {
                    Err(err(format!("no field '{field}' on tuple")))
                }
            }
            Value::Map(m) => m
                .get(field)
                .cloned()
                .ok_or_else(|| err(format!("key '{field}' not found in map"))),
            _ => Err(err(format!("cannot access field '{field}' on {val}"))),
        }
    }

    // ── Pattern matching ─────────────────────────────────────────────

    fn eval_match(&self, val: &Value, arms: &[MatchArm], env: &Env, tail: bool) -> Result<Value> {
        for arm in arms {
            let arm_env = env.child();
            if self.try_bind_pattern(&arm.pattern, val, &arm_env) {
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

    fn try_bind_pattern(&self, pattern: &Pattern, val: &Value, env: &Env) -> bool {
        match (pattern, val) {
            (Pattern::Wildcard, _) => true,
            (Pattern::Ident(name), _) => {
                env.define(name.clone(), val.clone());
                true
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
                    .all(|(p, v)| self.try_bind_pattern(p, v, env))
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
                    .all(|(p, v)| self.try_bind_pattern(p, v, env))
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
                    if !self.try_bind_pattern(pat, val, env) {
                        return false;
                    }
                }
                if let Some(rest_pat) = rest {
                    let remaining: Vec<Value> = items[pats.len()..].to_vec();
                    let rest_val = Value::List(Rc::new(remaining));
                    if !self.try_bind_pattern(rest_pat, &rest_val, env) {
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
                            if !self.try_bind_pattern(pat, val, env) {
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
                alts.iter().any(|alt| self.try_bind_pattern(alt, val, env))
            }
            (Pattern::Range(start, end), Value::Int(n)) => {
                *n >= *start && *n <= *end
            }
            (Pattern::Map(entries), Value::Map(map)) => {
                for (key, pat) in entries {
                    let Some(val) = map.get(key) else { return false; };
                    if !self.try_bind_pattern(pat, val, env) { return false; }
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
        if self.try_bind_pattern(pattern, val, env) {
            Ok(())
        } else {
            Err(err(format!(
                "pattern match failed: cannot bind {val} to {pattern:?}"
            )))
        }
    }
}

// ── Binary operations ────────────────────────────────────────────────

fn eval_binary(left: Value, op: BinOp, right: Value) -> Result<Value> {
    match (&left, op, &right) {
        // Integer arithmetic
        (Value::Int(a), BinOp::Add, Value::Int(b)) => Ok(Value::Int(a + b)),
        (Value::Int(a), BinOp::Sub, Value::Int(b)) => Ok(Value::Int(a - b)),
        (Value::Int(a), BinOp::Mul, Value::Int(b)) => Ok(Value::Int(a * b)),
        (Value::Int(a), BinOp::Div, Value::Int(b)) => {
            if *b == 0 {
                Err(err("division by zero"))
            } else {
                Ok(Value::Int(a / b))
            }
        }
        (Value::Int(a), BinOp::Mod, Value::Int(b)) => {
            if *b == 0 {
                Err(err("modulo by zero"))
            } else {
                Ok(Value::Int(a % b))
            }
        }

        // Float arithmetic
        (Value::Float(a), BinOp::Add, Value::Float(b)) => Ok(Value::Float(a + b)),
        (Value::Float(a), BinOp::Sub, Value::Float(b)) => Ok(Value::Float(a - b)),
        (Value::Float(a), BinOp::Mul, Value::Float(b)) => Ok(Value::Float(a * b)),
        (Value::Float(a), BinOp::Div, Value::Float(b)) => Ok(Value::Float(a / b)),

        // Mixed int/float
        (Value::Int(a), BinOp::Mul, Value::Float(b)) => Ok(Value::Float(*a as f64 * b)),
        (Value::Float(a), BinOp::Mul, Value::Int(b)) => Ok(Value::Float(a * *b as f64)),

        // String concatenation
        (Value::String(a), BinOp::Add, Value::String(b)) => {
            Ok(Value::String(format!("{a}{b}")))
        }

        // Comparisons
        (_, BinOp::Eq, _) => Ok(Value::Bool(left == right)),
        (_, BinOp::Neq, _) => Ok(Value::Bool(left != right)),
        (_, BinOp::Lt, _) => Ok(Value::Bool(left < right)),
        (_, BinOp::Gt, _) => Ok(Value::Bool(left > right)),
        (_, BinOp::Leq, _) => Ok(Value::Bool(left <= right)),
        (_, BinOp::Geq, _) => Ok(Value::Bool(left >= right)),

        // Boolean
        (Value::Bool(a), BinOp::And, Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
        (Value::Bool(a), BinOp::Or, Value::Bool(b)) => Ok(Value::Bool(*a || *b)),

        _ => Err(err(format!(
            "unsupported operation: {left} {op} {right}"
        ))),
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

// ── Builtins ─────────────────────────────────────────────────────────

fn register_builtins(env: &Env) {
    fn builtin(name: &str, f: fn(&[Value]) -> std::result::Result<Value, String>) -> Value {
        Value::BuiltinFn(name.to_string(), f)
    }

    env.define("print".into(), builtin("print", |args| {
        for (i, arg) in args.iter().enumerate() {
            if i > 0 { print!(" "); }
            print!("{arg}");
        }
        Ok(Value::Unit)
    }));

    env.define("println".into(), builtin("println", |args| {
        for (i, arg) in args.iter().enumerate() {
            if i > 0 { print!(" "); }
            print!("{arg}");
        }
        println!();
        Ok(Value::Unit)
    }));

    env.define("inspect".into(), builtin("inspect", |args| {
        if args.len() != 1 {
            return Err("inspect takes 1 argument".into());
        }
        Ok(Value::String(format!("{:?}", args[0])))
    }));

    env.define("panic".into(), builtin("panic", |args| {
        let msg = args.first().map(|v| v.to_string()).unwrap_or_default();
        Err(format!("panic: {msg}"))
    }));

    // List operations
    env.define("map".into(), builtin("map", |_| {
        Err("map requires closure argument (use pipeline syntax)".into())
    }));
    // We'll handle map/filter/each/fold specially in the interpreter

    env.define("map".into(), builtin("map", |args| {
        if args.len() != 2 {
            return Err("map takes 2 arguments (list, fn)".into());
        }
        let list = match &args[0] {
            Value::List(xs) => xs.clone(),
            _ => return Err("first argument to map must be a list".into()),
        };
        let func = &args[1];
        let mut results = Vec::new();
        for item in list.iter() {
            let result = call_value_static(func, &[item.clone()])?;
            results.push(result);
        }
        Ok(Value::List(Rc::new(results)))
    }));

    env.define("filter".into(), builtin("filter", |args| {
        if args.len() != 2 {
            return Err("filter takes 2 arguments (list, fn)".into());
        }
        let list = match &args[0] {
            Value::List(xs) => xs.clone(),
            _ => return Err("first argument to filter must be a list".into()),
        };
        let func = &args[1];
        let mut results = Vec::new();
        for item in list.iter() {
            let result = call_value_static(func, &[item.clone()])?;
            if is_truthy(&result) {
                results.push(item.clone());
            }
        }
        Ok(Value::List(Rc::new(results)))
    }));

    env.define("each".into(), builtin("each", |args| {
        if args.len() != 2 {
            return Err("each takes 2 arguments (list, fn)".into());
        }
        let list = match &args[0] {
            Value::List(xs) => xs.clone(),
            _ => return Err("first argument to each must be a list".into()),
        };
        let func = &args[1];
        for item in list.iter() {
            call_value_static(func, &[item.clone()])?;
        }
        Ok(Value::Unit)
    }));

    env.define("fold".into(), builtin("fold", |args| {
        if args.len() != 3 {
            return Err("fold takes 3 arguments (list, init, fn)".into());
        }
        let list = match &args[0] {
            Value::List(xs) => xs.clone(),
            _ => return Err("first argument to fold must be a list".into()),
        };
        let mut acc = args[1].clone();
        let func = &args[2];
        for item in list.iter() {
            acc = call_value_static(func, &[acc, item.clone()])?;
        }
        Ok(acc)
    }));

    env.define("find".into(), builtin("find", |args| {
        if args.len() != 2 {
            return Err("find takes 2 arguments (list, fn)".into());
        }
        let list = match &args[0] {
            Value::List(xs) => xs.clone(),
            _ => return Err("first argument to find must be a list".into()),
        };
        let func = &args[1];
        for item in list.iter() {
            let result = call_value_static(func, &[item.clone()])?;
            if is_truthy(&result) {
                return Ok(Value::Variant("Some".into(), vec![item.clone()]));
            }
        }
        Ok(Value::Variant("None".into(), Vec::new()))
    }));

    env.define("zip".into(), builtin("zip", |args| {
        if args.len() != 2 {
            return Err("zip takes 2 arguments".into());
        }
        let a = match &args[0] {
            Value::List(xs) => xs.clone(),
            _ => return Err("first argument to zip must be a list".into()),
        };
        let b = match &args[1] {
            Value::List(xs) => xs.clone(),
            _ => return Err("second argument to zip must be a list".into()),
        };
        let pairs: Vec<Value> = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| Value::Tuple(vec![x.clone(), y.clone()]))
            .collect();
        Ok(Value::List(Rc::new(pairs)))
    }));

    env.define("flatten".into(), builtin("flatten", |args| {
        if args.len() != 1 {
            return Err("flatten takes 1 argument".into());
        }
        let list = match &args[0] {
            Value::List(xs) => xs.clone(),
            _ => return Err("argument to flatten must be a list".into()),
        };
        let mut result = Vec::new();
        for item in list.iter() {
            match item {
                Value::List(inner) => result.extend(inner.iter().cloned()),
                other => result.push(other.clone()),
            }
        }
        Ok(Value::List(Rc::new(result)))
    }));

    env.define("len".into(), builtin("len", |args| {
        if args.len() != 1 {
            return Err("len takes 1 argument".into());
        }
        match &args[0] {
            Value::List(xs) => Ok(Value::Int(xs.len() as i64)),
            Value::String(s) => Ok(Value::Int(s.len() as i64)),
            Value::Map(m) => Ok(Value::Int(m.len() as i64)),
            _ => Err("len requires a list, string, or map".into()),
        }
    }));

    // Result/Option helpers
    env.define("unwrap_or".into(), builtin("unwrap_or", |args| {
        if args.len() != 2 {
            return Err("unwrap_or takes 2 arguments".into());
        }
        match &args[0] {
            Value::Variant(name, fields) if name == "Ok" || name == "Some" => {
                Ok(fields.first().cloned().unwrap_or(Value::Unit))
            }
            _ => Ok(args[1].clone()),
        }
    }));

    env.define("map_ok".into(), builtin("map_ok", |args| {
        if args.len() != 2 {
            return Err("map_ok takes 2 arguments".into());
        }
        match &args[0] {
            Value::Variant(name, fields) if name == "Ok" && fields.len() == 1 => {
                let result = call_value_static(&args[1], &[fields[0].clone()])?;
                Ok(Value::Variant("Ok".into(), vec![result]))
            }
            Value::Variant(name, fields) if name == "Err" => {
                Ok(Value::Variant(name.clone(), fields.clone()))
            }
            _ => Err("map_ok requires a Result value".into()),
        }
    }));

    // Constructors for Ok, Err, Some, None
    env.define("Ok".into(), builtin("Ok", |args| {
        if args.len() != 1 {
            return Err("Ok takes 1 argument".into());
        }
        Ok(Value::Variant("Ok".into(), vec![args[0].clone()]))
    }));

    env.define("Err".into(), builtin("Err", |args| {
        if args.len() != 1 {
            return Err("Err takes 1 argument".into());
        }
        Ok(Value::Variant("Err".into(), vec![args[0].clone()]))
    }));

    env.define("Some".into(), builtin("Some", |args| {
        if args.len() != 1 {
            return Err("Some takes 1 argument".into());
        }
        Ok(Value::Variant("Some".into(), vec![args[0].clone()]))
    }));

    env.define("None".into(), Value::Variant("None".into(), Vec::new()));

    // String module functions (available as builtins for now)
    env.define("string.split".into(), builtin("string.split", |args| {
        if args.len() != 2 {
            return Err("string.split takes 2 arguments".into());
        }
        let (Value::String(s), Value::String(sep)) = (&args[0], &args[1]) else {
            return Err("string.split requires string arguments".into());
        };
        let parts: Vec<Value> = s.split(sep.as_str()).map(|p| Value::String(p.to_string())).collect();
        Ok(Value::List(Rc::new(parts)))
    }));

    env.define("string.trim".into(), builtin("string.trim", |args| {
        if args.len() != 1 {
            return Err("string.trim takes 1 argument".into());
        }
        let Value::String(s) = &args[0] else {
            return Err("string.trim requires a string".into());
        };
        Ok(Value::String(s.trim().to_string()))
    }));

    env.define("string.contains".into(), builtin("string.contains", |args| {
        if args.len() != 2 {
            return Err("string.contains takes 2 arguments".into());
        }
        let (Value::String(s), Value::String(sub)) = (&args[0], &args[1]) else {
            return Err("string.contains requires string arguments".into());
        };
        Ok(Value::Bool(s.contains(sub.as_str())))
    }));

    env.define("string.replace".into(), builtin("string.replace", |args| {
        if args.len() != 3 {
            return Err("string.replace takes 3 arguments (string, from, to)".into());
        }
        let (Value::String(s), Value::String(from), Value::String(to)) = (&args[0], &args[1], &args[2]) else {
            return Err("string.replace requires string arguments".into());
        };
        Ok(Value::String(s.replace(from.as_str(), to.as_str())))
    }));

    env.define("string.join".into(), builtin("string.join", |args| {
        if args.len() != 2 {
            return Err("string.join takes 2 arguments (list, separator)".into());
        }
        let Value::List(xs) = &args[0] else {
            return Err("first argument to string.join must be a list".into());
        };
        let Value::String(sep) = &args[1] else {
            return Err("second argument to string.join must be a string".into());
        };
        let strs: Vec<String> = xs.iter().map(|v| v.to_string()).collect();
        Ok(Value::String(strs.join(sep.as_str())))
    }));

    // Int module
    env.define("int.parse".into(), builtin("int.parse", |args| {
        if args.len() != 1 {
            return Err("int.parse takes 1 argument".into());
        }
        let Value::String(s) = &args[0] else {
            return Err("int.parse requires a string".into());
        };
        match s.trim().parse::<i64>() {
            Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Int(n)])),
            Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
        }
    }));

    // IO module
    env.define("io.read_file".into(), builtin("io.read_file", |args| {
        if args.len() != 1 {
            return Err("io.read_file takes 1 argument".into());
        }
        let Value::String(path) = &args[0] else {
            return Err("io.read_file requires a string path".into());
        };
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(Value::Variant("Ok".into(), vec![Value::String(content)])),
            Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
        }
    }));

    env.define("io.write_file".into(), builtin("io.write_file", |args| {
        if args.len() != 2 {
            return Err("io.write_file takes 2 arguments".into());
        }
        let (Value::String(path), Value::String(content)) = (&args[0], &args[1]) else {
            return Err("io.write_file requires string arguments".into());
        };
        match std::fs::write(path, content) {
            Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
            Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
        }
    }));

    env.define("io.read_line".into(), builtin("io.read_line", |args| {
        if !args.is_empty() {
            return Err("io.read_line takes no arguments".into());
        }
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(_) => Ok(Value::Variant("Ok".into(), vec![Value::String(line.trim_end().to_string())])),
            Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
        }
    }));

    env.define("io.args".into(), builtin("io.args", |_| {
        let args: Vec<Value> = std::env::args().map(Value::String).collect();
        Ok(Value::List(Rc::new(args)))
    }));

    // Test module
    env.define("assert".into(), builtin("assert", |args| {
        if args.len() != 1 {
            return Err("assert takes 1 argument".into());
        }
        if is_truthy(&args[0]) {
            Ok(Value::Unit)
        } else {
            Err(format!("assertion failed: {:?}", args[0]))
        }
    }));

    env.define("assert_eq".into(), builtin("assert_eq", |args| {
        if args.len() != 2 {
            return Err("assert_eq takes 2 arguments".into());
        }
        if args[0] == args[1] {
            Ok(Value::Unit)
        } else {
            Err(format!("assertion failed: {:?} != {:?}", args[0], args[1]))
        }
    }));

    env.define("assert_ne".into(), builtin("assert_ne", |args| {
        if args.len() != 2 {
            return Err("assert_ne takes 2 arguments".into());
        }
        if args[0] != args[1] {
            Ok(Value::Unit)
        } else {
            Err(format!("assertion failed: {:?} == {:?}", args[0], args[1]))
        }
    }));

    // ── int module ──────────────────────────────────────────────────

    env.define("int.abs".into(), builtin("int.abs", |args| {
        if args.len() != 1 {
            return Err("int.abs takes 1 argument".into());
        }
        let Value::Int(n) = &args[0] else {
            return Err("int.abs requires an int".into());
        };
        Ok(Value::Int(n.abs()))
    }));

    env.define("int.min".into(), builtin("int.min", |args| {
        if args.len() != 2 {
            return Err("int.min takes 2 arguments".into());
        }
        let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else {
            return Err("int.min requires int arguments".into());
        };
        Ok(Value::Int(*a.min(b)))
    }));

    env.define("int.max".into(), builtin("int.max", |args| {
        if args.len() != 2 {
            return Err("int.max takes 2 arguments".into());
        }
        let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else {
            return Err("int.max requires int arguments".into());
        };
        Ok(Value::Int(*a.max(b)))
    }));

    env.define("int.to_float".into(), builtin("int.to_float", |args| {
        if args.len() != 1 {
            return Err("int.to_float takes 1 argument".into());
        }
        let Value::Int(n) = &args[0] else {
            return Err("int.to_float requires an int".into());
        };
        Ok(Value::Float(*n as f64))
    }));

    // ── float module ────────────────────────────────────────────────

    env.define("float.parse".into(), builtin("float.parse", |args| {
        if args.len() != 1 {
            return Err("float.parse takes 1 argument".into());
        }
        let Value::String(s) = &args[0] else {
            return Err("float.parse requires a string".into());
        };
        match s.trim().parse::<f64>() {
            Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Float(n)])),
            Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
        }
    }));

    env.define("float.round".into(), builtin("float.round", |args| {
        if args.len() != 1 {
            return Err("float.round takes 1 argument".into());
        }
        let Value::Float(f) = &args[0] else {
            return Err("float.round requires a float".into());
        };
        Ok(Value::Int(f.round() as i64))
    }));

    env.define("float.ceil".into(), builtin("float.ceil", |args| {
        if args.len() != 1 {
            return Err("float.ceil takes 1 argument".into());
        }
        let Value::Float(f) = &args[0] else {
            return Err("float.ceil requires a float".into());
        };
        Ok(Value::Int(f.ceil() as i64))
    }));

    env.define("float.floor".into(), builtin("float.floor", |args| {
        if args.len() != 1 {
            return Err("float.floor takes 1 argument".into());
        }
        let Value::Float(f) = &args[0] else {
            return Err("float.floor requires a float".into());
        };
        Ok(Value::Int(f.floor() as i64))
    }));

    env.define("float.abs".into(), builtin("float.abs", |args| {
        if args.len() != 1 {
            return Err("float.abs takes 1 argument".into());
        }
        let Value::Float(f) = &args[0] else {
            return Err("float.abs requires a float".into());
        };
        Ok(Value::Float(f.abs()))
    }));

    // ── map module ──────────────────────────────────────────────────

    env.define("map.get".into(), builtin("map.get", |args| {
        if args.len() != 2 {
            return Err("map.get takes 2 arguments".into());
        }
        let Value::Map(m) = &args[0] else {
            return Err("map.get requires a map as first argument".into());
        };
        let Value::String(key) = &args[1] else {
            return Err("map.get requires a string key".into());
        };
        match m.get(key.as_str()) {
            Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
            None => Ok(Value::Variant("None".into(), Vec::new())),
        }
    }));

    env.define("map.set".into(), builtin("map.set", |args| {
        if args.len() != 3 {
            return Err("map.set takes 3 arguments".into());
        }
        let Value::Map(m) = &args[0] else {
            return Err("map.set requires a map as first argument".into());
        };
        let Value::String(key) = &args[1] else {
            return Err("map.set requires a string key".into());
        };
        let mut new_map = (**m).clone();
        new_map.insert(key.clone(), args[2].clone());
        Ok(Value::Map(Rc::new(new_map)))
    }));

    env.define("map.delete".into(), builtin("map.delete", |args| {
        if args.len() != 2 {
            return Err("map.delete takes 2 arguments".into());
        }
        let Value::Map(m) = &args[0] else {
            return Err("map.delete requires a map as first argument".into());
        };
        let Value::String(key) = &args[1] else {
            return Err("map.delete requires a string key".into());
        };
        let mut new_map = (**m).clone();
        new_map.remove(key.as_str());
        Ok(Value::Map(Rc::new(new_map)))
    }));

    env.define("map.keys".into(), builtin("map.keys", |args| {
        if args.len() != 1 {
            return Err("map.keys takes 1 argument".into());
        }
        let Value::Map(m) = &args[0] else {
            return Err("map.keys requires a map".into());
        };
        let keys: Vec<Value> = m.keys().map(|k| Value::String(k.clone())).collect();
        Ok(Value::List(Rc::new(keys)))
    }));

    env.define("map.values".into(), builtin("map.values", |args| {
        if args.len() != 1 {
            return Err("map.values takes 1 argument".into());
        }
        let Value::Map(m) = &args[0] else {
            return Err("map.values requires a map".into());
        };
        let vals: Vec<Value> = m.values().cloned().collect();
        Ok(Value::List(Rc::new(vals)))
    }));

    env.define("map.merge".into(), builtin("map.merge", |args| {
        if args.len() != 2 {
            return Err("map.merge takes 2 arguments".into());
        }
        let Value::Map(m1) = &args[0] else {
            return Err("map.merge requires maps".into());
        };
        let Value::Map(m2) = &args[1] else {
            return Err("map.merge requires maps".into());
        };
        let mut merged = (**m1).clone();
        for (k, v) in m2.iter() {
            merged.insert(k.clone(), v.clone());
        }
        Ok(Value::Map(Rc::new(merged)))
    }));

    // ── list module ─────────────────────────────────────────────────

    env.define("list.head".into(), builtin("list.head", |args| {
        if args.len() != 1 {
            return Err("list.head takes 1 argument".into());
        }
        let Value::List(xs) = &args[0] else {
            return Err("list.head requires a list".into());
        };
        match xs.first() {
            Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
            None => Ok(Value::Variant("None".into(), Vec::new())),
        }
    }));

    env.define("list.tail".into(), builtin("list.tail", |args| {
        if args.len() != 1 {
            return Err("list.tail takes 1 argument".into());
        }
        let Value::List(xs) = &args[0] else {
            return Err("list.tail requires a list".into());
        };
        if xs.is_empty() {
            Ok(Value::List(Rc::new(Vec::new())))
        } else {
            Ok(Value::List(Rc::new(xs[1..].to_vec())))
        }
    }));

    env.define("list.last".into(), builtin("list.last", |args| {
        if args.len() != 1 {
            return Err("list.last takes 1 argument".into());
        }
        let Value::List(xs) = &args[0] else {
            return Err("list.last requires a list".into());
        };
        match xs.last() {
            Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
            None => Ok(Value::Variant("None".into(), Vec::new())),
        }
    }));

    env.define("list.reverse".into(), builtin("list.reverse", |args| {
        if args.len() != 1 {
            return Err("list.reverse takes 1 argument".into());
        }
        let Value::List(xs) = &args[0] else {
            return Err("list.reverse requires a list".into());
        };
        let mut reversed = (**xs).clone();
        reversed.reverse();
        Ok(Value::List(Rc::new(reversed)))
    }));

    env.define("list.sort".into(), builtin("list.sort", |args| {
        if args.len() != 1 {
            return Err("list.sort takes 1 argument".into());
        }
        let Value::List(xs) = &args[0] else {
            return Err("list.sort requires a list".into());
        };
        let mut sorted = (**xs).clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Ok(Value::List(Rc::new(sorted)))
    }));

    env.define("list.contains".into(), builtin("list.contains", |args| {
        if args.len() != 2 {
            return Err("list.contains takes 2 arguments".into());
        }
        let Value::List(xs) = &args[0] else {
            return Err("list.contains requires a list as first argument".into());
        };
        Ok(Value::Bool(xs.contains(&args[1])))
    }));

    env.define("list.length".into(), builtin("list.length", |args| {
        if args.len() != 1 {
            return Err("list.length takes 1 argument".into());
        }
        let Value::List(xs) = &args[0] else {
            return Err("list.length requires a list".into());
        };
        Ok(Value::Int(xs.len() as i64))
    }));

    // ── result module ───────────────────────────────────────────────

    env.define("result.map_err".into(), builtin("result.map_err", |args| {
        if args.len() != 2 {
            return Err("result.map_err takes 2 arguments".into());
        }
        match &args[0] {
            Value::Variant(name, fields) if name == "Err" && fields.len() == 1 => {
                let result = call_value_static(&args[1], &[fields[0].clone()])?;
                Ok(Value::Variant("Err".into(), vec![result]))
            }
            Value::Variant(name, fields) if name == "Ok" => {
                Ok(Value::Variant(name.clone(), fields.clone()))
            }
            _ => Err("result.map_err requires a Result value".into()),
        }
    }));

    env.define("result.flatten".into(), builtin("result.flatten", |args| {
        if args.len() != 1 {
            return Err("result.flatten takes 1 argument".into());
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
            _ => Err("result.flatten requires a Result value".into()),
        }
    }));

    env.define("result.is_ok".into(), builtin("result.is_ok", |args| {
        if args.len() != 1 {
            return Err("result.is_ok takes 1 argument".into());
        }
        match &args[0] {
            Value::Variant(name, _) if name == "Ok" => Ok(Value::Bool(true)),
            Value::Variant(name, _) if name == "Err" => Ok(Value::Bool(false)),
            _ => Err("result.is_ok requires a Result value".into()),
        }
    }));

    env.define("result.is_err".into(), builtin("result.is_err", |args| {
        if args.len() != 1 {
            return Err("result.is_err takes 1 argument".into());
        }
        match &args[0] {
            Value::Variant(name, _) if name == "Err" => Ok(Value::Bool(true)),
            Value::Variant(name, _) if name == "Ok" => Ok(Value::Bool(false)),
            _ => Err("result.is_err requires a Result value".into()),
        }
    }));

    // ── option module ───────────────────────────────────────────────

    env.define("option.map".into(), builtin("option.map", |args| {
        if args.len() != 2 {
            return Err("option.map takes 2 arguments".into());
        }
        match &args[0] {
            Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
                let result = call_value_static(&args[1], &[fields[0].clone()])?;
                Ok(Value::Variant("Some".into(), vec![result]))
            }
            Value::Variant(name, _) if name == "None" => {
                Ok(Value::Variant("None".into(), Vec::new()))
            }
            _ => Err("option.map requires an Option value".into()),
        }
    }));

    env.define("option.unwrap_or".into(), builtin("option.unwrap_or", |args| {
        if args.len() != 2 {
            return Err("option.unwrap_or takes 2 arguments".into());
        }
        match &args[0] {
            Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
                Ok(fields[0].clone())
            }
            Value::Variant(name, _) if name == "None" => {
                Ok(args[1].clone())
            }
            _ => Err("option.unwrap_or requires an Option value".into()),
        }
    }));

    env.define("option.to_result".into(), builtin("option.to_result", |args| {
        if args.len() != 2 {
            return Err("option.to_result takes 2 arguments".into());
        }
        match &args[0] {
            Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
                Ok(Value::Variant("Ok".into(), vec![fields[0].clone()]))
            }
            Value::Variant(name, _) if name == "None" => {
                Ok(Value::Variant("Err".into(), vec![args[1].clone()]))
            }
            _ => Err("option.to_result requires an Option value".into()),
        }
    }));

    env.define("option.is_some".into(), builtin("option.is_some", |args| {
        if args.len() != 1 {
            return Err("option.is_some takes 1 argument".into());
        }
        match &args[0] {
            Value::Variant(name, _) if name == "Some" => Ok(Value::Bool(true)),
            Value::Variant(name, _) if name == "None" => Ok(Value::Bool(false)),
            _ => Err("option.is_some requires an Option value".into()),
        }
    }));

    env.define("option.is_none".into(), builtin("option.is_none", |args| {
        if args.len() != 1 {
            return Err("option.is_none takes 1 argument".into());
        }
        match &args[0] {
            Value::Variant(name, _) if name == "None" => Ok(Value::Bool(true)),
            Value::Variant(name, _) if name == "Some" => Ok(Value::Bool(false)),
            _ => Err("option.is_none requires an Option value".into()),
        }
    }));

    // ── string module (additional) ──────────────────────────────────

    env.define("string.length".into(), builtin("string.length", |args| {
        if args.len() != 1 {
            return Err("string.length takes 1 argument".into());
        }
        let Value::String(s) = &args[0] else {
            return Err("string.length requires a string".into());
        };
        Ok(Value::Int(s.len() as i64))
    }));

    env.define("string.to_upper".into(), builtin("string.to_upper", |args| {
        if args.len() != 1 {
            return Err("string.to_upper takes 1 argument".into());
        }
        let Value::String(s) = &args[0] else {
            return Err("string.to_upper requires a string".into());
        };
        Ok(Value::String(s.to_uppercase()))
    }));

    env.define("string.to_lower".into(), builtin("string.to_lower", |args| {
        if args.len() != 1 {
            return Err("string.to_lower takes 1 argument".into());
        }
        let Value::String(s) = &args[0] else {
            return Err("string.to_lower requires a string".into());
        };
        Ok(Value::String(s.to_lowercase()))
    }));

    env.define("string.starts_with".into(), builtin("string.starts_with", |args| {
        if args.len() != 2 {
            return Err("string.starts_with takes 2 arguments".into());
        }
        let (Value::String(s), Value::String(prefix)) = (&args[0], &args[1]) else {
            return Err("string.starts_with requires string arguments".into());
        };
        Ok(Value::Bool(s.starts_with(prefix.as_str())))
    }));

    env.define("string.ends_with".into(), builtin("string.ends_with", |args| {
        if args.len() != 2 {
            return Err("string.ends_with takes 2 arguments".into());
        }
        let (Value::String(s), Value::String(suffix)) = (&args[0], &args[1]) else {
            return Err("string.ends_with requires string arguments".into());
        };
        Ok(Value::Bool(s.ends_with(suffix.as_str())))
    }));

    env.define("string.chars".into(), builtin("string.chars", |args| {
        if args.len() != 1 {
            return Err("string.chars takes 1 argument".into());
        }
        let Value::String(s) = &args[0] else {
            return Err("string.chars requires a string".into());
        };
        let chars: Vec<Value> = s.chars().map(|c| Value::String(c.to_string())).collect();
        Ok(Value::List(Rc::new(chars)))
    }));

    env.define("string.repeat".into(), builtin("string.repeat", |args| {
        if args.len() != 2 {
            return Err("string.repeat takes 2 arguments".into());
        }
        let Value::String(s) = &args[0] else {
            return Err("string.repeat requires a string as first argument".into());
        };
        let Value::Int(n) = &args[1] else {
            return Err("string.repeat requires an int as second argument".into());
        };
        if *n < 0 {
            return Err("string.repeat count must be non-negative".into());
        }
        Ok(Value::String(s.repeat(*n as usize)))
    }));
}

/// Call a Value as a function from within a builtin (no Interpreter context).
fn call_value_static(func: &Value, args: &[Value]) -> std::result::Result<Value, String> {
    match func {
        Value::Closure(c) => {
            let call_env = c.env.child();
            for (param, arg) in c.params.iter().zip(args.iter()) {
                bind_pattern_simple(&param.pattern, arg, &call_env)
                    .map_err(|e| format!("{e}"))?;
            }
            // We need a temporary interpreter to eval
            let interp = Interpreter {
                global: Env::new(),
                variant_types: std::collections::HashMap::new(),
                scheduler: RefCell::new(Scheduler::new()),
                module_loader: RefCell::new(ModuleLoader::new(PathBuf::from("."))),
            };
            match interp.eval(&c.body, &call_env) {
                Ok(val) => Ok(val),
                Err(RuntimeError::Return(val)) => Ok(val),
                Err(RuntimeError::TailCall(tc, ta)) => {
                    match interp.call_closure(&tc, &ta) {
                        Ok(val) => Ok(val),
                        Err(RuntimeError::Error(msg)) => Err(msg),
                        Err(RuntimeError::Return(val)) => Ok(val),
                        Err(RuntimeError::TailCall(_, _)) => Err("unhandled tail call".into()),
                    }
                }
                Err(RuntimeError::Error(msg)) => Err(msg),
            }
        }
        Value::BuiltinFn(name, f) => f(args).map_err(|e| format!("{name}: {e}")),
        Value::VariantConstructor(name, arity) => {
            if args.len() != *arity {
                Err(format!("{name} expects {arity} args, got {}", args.len()))
            } else {
                Ok(Value::Variant(name.clone(), args.to_vec()))
            }
        }
        _ => Err(format!("not callable: {func}")),
    }
}

fn bind_pattern_simple(
    pattern: &Pattern,
    val: &Value,
    env: &Env,
) -> std::result::Result<(), String> {
    match (pattern, val) {
        (Pattern::Wildcard, _) => Ok(()),
        (Pattern::Ident(name), _) => {
            env.define(name.clone(), val.clone());
            Ok(())
        }
        (Pattern::Tuple(pats), Value::Tuple(vals)) => {
            if pats.len() != vals.len() {
                return Err("tuple pattern length mismatch".into());
            }
            for (p, v) in pats.iter().zip(vals.iter()) {
                bind_pattern_simple(p, v, env)?;
            }
            Ok(())
        }
        (Pattern::List(pats, rest), Value::List(list)) => {
            let items = list.as_ref();
            if rest.is_some() {
                if items.len() < pats.len() {
                    return Err("list pattern length mismatch".into());
                }
            } else {
                if items.len() != pats.len() {
                    return Err("list pattern length mismatch".into());
                }
            }
            for (p, v) in pats.iter().zip(items.iter()) {
                bind_pattern_simple(p, v, env)?;
            }
            if let Some(rest_pat) = rest {
                let remaining: Vec<Value> = items[pats.len()..].to_vec();
                let rest_val = Value::List(Rc::new(remaining));
                bind_pattern_simple(rest_pat, &rest_val, env)?;
            }
            Ok(())
        }
        (Pattern::Or(alts), _) => {
            for alt in alts {
                if bind_pattern_simple(alt, val, env).is_ok() {
                    return Ok(());
                }
            }
            Err("or-pattern: no alternative matched".into())
        }
        (Pattern::Range(start, end), Value::Int(n)) => {
            if *n >= *start && *n <= *end {
                Ok(())
            } else {
                Err("range pattern match failed".into())
            }
        }
        (Pattern::Map(entries), Value::Map(map)) => {
            for (key, pat) in entries {
                let Some(val) = map.get(key) else {
                    return Err(format!("map pattern: missing key '{key}'"));
                };
                bind_pattern_simple(pat, val, env)?;
            }
            Ok(())
        }
        _ => Err("pattern match failed in closure call".into()),
    }
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
                xs |> map { x -> x * 2 }
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
                [1, 2, 3, 4, 5] |> filter { x -> x > 3 }
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
                [1, 2, 3] |> fold(0) { acc, x -> acc + x }
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
                len(ks)
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
                (h, len(t), l)
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
                len(cs)
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
