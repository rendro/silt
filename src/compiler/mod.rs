//! AST-to-bytecode compiler for Silt.
//!
//! Walks the AST and emits stack-based bytecode into `Function` objects.
//! Phase 4: full pattern matching compilation for all pattern types,
//! including nested/recursive patterns, or-patterns, guards, ranges,
//! list/tuple/record/map destructuring, pin patterns, when/else,
//! plus all previous features (closures, upvalues, pipes, lambdas).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::ast::{
    BinOp, Decl, Expr, ExprKind, ImportTarget, ListElem, MatchArm, Pattern, Program, Stmt,
    StringPart, TypeExpr, UnaryOp,
};
use crate::bytecode::{Chunk, Function, Op, UpvalueDesc, VmClosure};
use crate::intern::{Symbol, intern, resolve};
use crate::lexer::{Lexer, Span};
use crate::module;
use crate::parser::Parser;
use crate::typechecker;
use crate::value::Value;

mod patterns;

// ── Type encoding for record field metadata ─────────────────────────

/// Encode a TypeExpr as a compact string for runtime JSON parsing.
/// Examples: "String", "Int", "List:String", "Option:Int", "Record:Address"
fn encode_type_expr(te: &TypeExpr) -> String {
    match te {
        TypeExpr::Named(n) => {
            let s = resolve(*n);
            match s.as_str() {
                "Int" | "Float" | "String" | "Bool" | "Date" | "Time" | "DateTime" => s,
                _ if s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) => {
                    format!("Record:{s}")
                }
                _ => "String".to_string(),
            }
        }
        TypeExpr::Generic(name, args) => match resolve(*name).as_str() {
            "List" => {
                let inner = args
                    .first()
                    .map(encode_type_expr)
                    .unwrap_or_else(|| "String".into());
                format!("List:{inner}")
            }
            "Option" => {
                let inner = args
                    .first()
                    .map(encode_type_expr)
                    .unwrap_or_else(|| "String".into());
                format!("Option:{inner}")
            }
            _ => "String".to_string(),
        },
        TypeExpr::SelfType => "Self".to_string(),
        _ => "String".to_string(),
    }
}

// ── Bind destruct kind ───────────────────────────────────────────────

/// Describes how to destructure a sub-value from a compound pattern.
enum BindDestructKind {
    Variant(u8),
    Tuple(u8),
    List(u8),
    ListRest(u8),
    RecordField(Symbol),
    MapValue(String),
}

// ── Compiler context ──────────────────────────────────────────────────

/// Per-function compilation state.
struct CompileContext {
    function: Function,
    locals: Vec<Local>,
    scope_depth: usize,
    /// Upvalue descriptors for this function/closure.
    upvalues: Vec<UpvalueDesc>,
    /// Loop context stack: (first_loop_slot, loop_start_offset, binding_count)
    loop_stack: Vec<LoopInfo>,
}

struct LoopInfo {
    first_slot: u16,
    loop_start: usize,
    #[allow(dead_code)]
    binding_count: u8,
}

impl CompileContext {
    fn new(name: String, arity: u8) -> Self {
        Self {
            function: Function::new(name, arity),
            locals: Vec::new(),
            scope_depth: 0,
            upvalues: Vec::new(),
            loop_stack: Vec::new(),
        }
    }
}

struct Local {
    name: Symbol,
    depth: usize,
    slot: u16,
    /// Whether this local is captured by a nested closure.
    #[allow(dead_code)]
    captured: bool,
}

// ── Compiler warnings ────────────────────────────────────────────────

pub struct CompileWarning {
    pub message: String,
    pub span: Span,
}

// ── Compiler errors ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompileError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}:{}] {}", self.span.line, self.span.col, self.message)
    }
}

/// Render a lex/parse error that happened inside an imported module into a
/// human-readable CompileError message. The message embeds a `file:line:col`
/// prefix plus a source snippet and caret pointing at the offending location
/// in the *module* file (not the main file), so the outer error reporter can
/// show the user where the bug is even though it only has the main file's
/// source for its own snippet rendering.
/// Format a cross-module error as a single-line summary suitable for
/// embedding in an outer `CompileError.message`. The outer `SourceError`
/// Display impl already renders the import site's caret and snippet, so
/// we deliberately avoid duplicating the inner file's source-line context
/// here — older versions of this helper did that and the result was the
/// inner snippet being printed twice (L7).
fn format_module_source_error(
    module_name: &str,
    file_path: &str,
    _source: &str,
    kind: &str,
    inner_message: &str,
    span: Span,
) -> String {
    format!(
        "module '{module_name}': {kind} at {file_path}:{line}:{col} — {inner_message}",
        line = span.line,
        col = span.col,
    )
}

// ── Compiler ──────────────────────────────────────────────────────────

pub struct Compiler {
    contexts: Vec<CompileContext>,
    /// Accumulated compiled functions (one per `Decl::Fn`).
    functions: Vec<Function>,
    /// Project root directory for resolving file-based modules.
    project_root: Option<PathBuf>,
    /// Modules already compiled in this compilation unit (avoids double-compile).
    compiled_modules: HashSet<String>,
    /// Modules currently being compiled (for circular import detection).
    compiling_modules: HashSet<String>,
    /// Warnings emitted during compilation.
    warnings: Vec<CompileWarning>,
    /// Builtin modules that have been explicitly imported in this compilation unit.
    imported_builtin_modules: HashSet<String>,
    /// Whether the current expression is in tail position (for TCO).
    in_tail_position: bool,
    /// When compiling inside a file-based module, maps bare function names
    /// to their qualified equivalents so intra-module calls resolve.
    /// Value is (module_name, map_of fn_name -> is_public).
    module_scope: Option<(String, HashMap<String, bool>)>,
    /// Whether this compiler is being used to compile a REPL entry. In REPL
    /// mode, an unknown `name.field` where `name` is neither a local nor a
    /// known builtin module falls through to `GetGlobal(name) + GetField(field)`
    /// so that a previously-bound REPL value (stored as a VM global) can have
    /// its fields accessed. In non-REPL mode, unknown `name.field` is still
    /// emitted as `GetGlobal("name.field")` so that foreign-function modules
    /// (e.g. `mylib.double` registered via `register_fn1`) and file-module
    /// aliases (`import string as s` → `s.split`) continue to work.
    repl_mode: bool,
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            contexts: Vec::new(),
            functions: Vec::new(),
            project_root: None,
            compiled_modules: HashSet::new(),
            compiling_modules: HashSet::new(),
            warnings: Vec::new(),
            imported_builtin_modules: HashSet::new(),
            in_tail_position: false,
            module_scope: None,
            repl_mode: false,
        }
    }

    /// Create a compiler that can resolve file-based imports relative to `root`.
    pub fn with_project_root(root: PathBuf) -> Self {
        Self {
            contexts: Vec::new(),
            functions: Vec::new(),
            project_root: Some(root),
            compiled_modules: HashSet::new(),
            compiling_modules: HashSet::new(),
            warnings: Vec::new(),
            imported_builtin_modules: HashSet::new(),
            in_tail_position: false,
            module_scope: None,
            repl_mode: false,
        }
    }

    /// Enable REPL mode. See the `repl_mode` field for semantics.
    pub fn set_repl_mode(&mut self, enabled: bool) {
        self.repl_mode = enabled;
    }

    /// Returns warnings emitted during compilation.
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }

    /// Mark all builtin modules as imported (used by the REPL).
    pub fn import_all_builtins(&mut self) {
        for name in crate::module::BUILTIN_MODULES {
            self.imported_builtin_modules.insert(name.to_string());
        }
    }

    // ── Public entry point ────────────────────────────────────────

    /// Compile a full program, returning all functions.
    ///
    /// The first function in the returned `Vec` is the top-level `<script>`,
    /// which ends with `GetGlobal "main" ; Call 0 ; Return`.
    pub fn compile_program(&mut self, program: &Program) -> Result<Vec<Function>, CompileError> {
        // Push a top-level script context.
        self.contexts
            .push(CompileContext::new("<script>".into(), 0));

        for decl in &program.decls {
            self.compile_decl(decl)?;
        }

        // Emit: GetGlobal "main", Call 0, Return
        let span = Span::new(0, 0);
        let name_idx = self.add_constant(Value::String("main".into()), span)?;
        self.current_chunk().emit_op(Op::GetGlobal, span);
        self.current_chunk().emit_u16(name_idx, span);
        self.current_chunk().emit_op(Op::Call, span);
        self.current_chunk().emit_u8(0, span);
        self.current_chunk().emit_op(Op::Return, span);

        let script = self
            .contexts
            .pop()
            .ok_or(CompileError {
                message: "compiler bug: missing script context".into(),
                span: Span::new(0, 0),
            })?
            .function;

        // Build the result: script first, then all compiled functions.
        let mut result = vec![script];
        result.append(&mut self.functions);
        Ok(result)
    }

    /// Compile all declarations without calling `main()`.
    ///
    /// Returns all compiled functions. The first is a `<script>` that
    /// registers globals and returns Unit.  Useful for test runners and the
    /// REPL where `main()` is not the entry-point.
    pub fn compile_declarations(
        &mut self,
        program: &Program,
    ) -> Result<Vec<Function>, CompileError> {
        self.contexts
            .push(CompileContext::new("<script>".into(), 0));

        for decl in &program.decls {
            self.compile_decl(decl)?;
        }

        // Return Unit instead of calling main.
        let span = Span::new(0, 0);
        self.current_chunk().emit_op(Op::Unit, span);
        self.current_chunk().emit_op(Op::Return, span);

        let script = self
            .contexts
            .pop()
            .ok_or(CompileError {
                message: "compiler bug: missing script context".into(),
                span: Span::new(0, 0),
            })?
            .function;
        let mut result = vec![script];
        result.append(&mut self.functions);
        Ok(result)
    }

    // ── Declarations ──────────────────────────────────────────────

    fn compile_decl(&mut self, decl: &Decl) -> Result<(), CompileError> {
        match decl {
            Decl::Fn(fn_decl) => {
                let arity = fn_decl.params.len() as u8;
                let span = fn_decl.span;

                // Push a new context for the function body.
                self.contexts
                    .push(CompileContext::new(resolve(fn_decl.name), arity));

                // Add parameters as locals. Each parameter occupies one slot initially.
                // For non-Ident patterns, we use a hidden name and destructure after.
                let mut param_slots = Vec::new();
                for (i, param) in fn_decl.params.iter().enumerate() {
                    match &param.pattern {
                        Pattern::Ident(name) => {
                            self.warn_if_shadows_module(*name, span);
                            self.add_local(*name);
                            param_slots.push((i, None)); // no destructuring needed
                        }
                        _ => {
                            let slot = self.add_local(intern(&format!("__param_{i}__")));
                            param_slots.push((i, Some((slot, param.pattern.clone()))));
                        }
                    }
                }

                // Emit destructuring for non-Ident parameter patterns.
                // The parameter value is already on the stack as a local.
                // compile_pattern_bind expects TOS = value. Since the param
                // IS at its local slot (which IS a stack position), we just
                // GetLocal it to put it on TOS, then bind.
                for (_i, maybe_destruct) in &param_slots {
                    if let Some((slot, pattern)) = maybe_destruct {
                        self.current_chunk().emit_op(Op::GetLocal, span);
                        self.current_chunk().emit_u16(*slot, span);
                        // This GetLocal pushes a copy. Register it as a hidden local.
                        let _hidden = self.add_local(intern("__param_copy__"));
                        self.current_chunk().emit_op(Op::SetLocal, span);
                        self.current_chunk().emit_u16(_hidden, span);
                        // Now TOS = param value copy (as hidden local). Bind sub-patterns.
                        self.compile_pattern_bind(pattern, span)?;
                    }
                }

                // Compile the function body in tail position for TCO.
                self.in_tail_position = true;
                self.compile_expr(&fn_decl.body)?;
                self.in_tail_position = false;

                // Emit Return (may be dead code if body ends with a tail call).
                self.current_chunk().emit_op(Op::Return, span);

                // Pop the context, recovering the compiled function.
                let ctx = self.contexts.pop().ok_or(CompileError {
                    message: "compiler bug: missing function context".into(),
                    span,
                })?;
                let func = ctx.function;

                // Store the function as a VmClosure constant in the enclosing chunk.
                let vm_closure = Arc::new(VmClosure {
                    function: Arc::new(func),
                    upvalues: vec![],
                });
                let closure_val = Value::VmClosure(vm_closure);
                let fi = self.add_constant(closure_val, span)?;
                self.current_chunk().emit_op(Op::Constant, span);
                self.current_chunk().emit_u16(fi, span);

                let name_idx = self.add_constant(Value::String(resolve(fn_decl.name)), span)?;
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
                        let name_idx = self.add_constant(Value::String(resolve(*name)), span)?;
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    _ => {
                        return Err(CompileError {
                            message: "unsupported pattern in top-level let".into(),
                            span,
                        });
                    }
                }

                Ok(())
            }

            Decl::Type(type_decl) => {
                let span = type_decl.span;
                match &type_decl.body {
                    crate::ast::TypeBody::Enum(variants) => {
                        for variant in variants {
                            let vname = resolve(variant.name);
                            let arity = variant.fields.len();
                            if arity == 0 {
                                // Nullary variant: register as a Variant value
                                let val = Value::Variant(vname.clone(), Vec::new());
                                let val_idx = self.add_constant(val, span)?;
                                self.current_chunk().emit_op(Op::Constant, span);
                                self.current_chunk().emit_u16(val_idx, span);
                            } else {
                                // Variant constructor
                                let val = Value::VariantConstructor(vname.clone(), arity);
                                let val_idx = self.add_constant(val, span)?;
                                self.current_chunk().emit_op(Op::Constant, span);
                                self.current_chunk().emit_u16(val_idx, span);
                            }
                            let name_idx = self.add_constant(Value::String(vname.clone()), span)?;
                            self.current_chunk().emit_op(Op::SetGlobal, span);
                            self.current_chunk().emit_u16(name_idx, span);
                            self.current_chunk().emit_op(Op::Pop, span);

                            // Register variant -> type mapping for method dispatch.
                            let mapping_key = format!("__type_of__{vname}");
                            let key_idx = self.add_constant(Value::String(mapping_key), span)?;
                            let type_val_idx =
                                self.add_constant(Value::String(resolve(type_decl.name)), span)?;
                            self.current_chunk().emit_op(Op::Constant, span);
                            self.current_chunk().emit_u16(type_val_idx, span);
                            self.current_chunk().emit_op(Op::SetGlobal, span);
                            self.current_chunk().emit_u16(key_idx, span);
                            self.current_chunk().emit_op(Op::Pop, span);
                        }
                    }
                    crate::ast::TypeBody::Record(fields) => {
                        // Register the record type name as a RecordDescriptor global.
                        let val = Value::RecordDescriptor(resolve(type_decl.name));
                        let val_idx = self.add_constant(val, span)?;
                        self.current_chunk().emit_op(Op::Constant, span);
                        self.current_chunk().emit_u16(val_idx, span);
                        let name_idx =
                            self.add_constant(Value::String(resolve(type_decl.name)), span)?;
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);

                        // Emit record field metadata as a global list for json module.
                        // Format: list of alternating [field_name, type_encoding, ...]
                        let field_count = fields.len();
                        for f in fields {
                            let fname = self.add_constant(Value::String(resolve(f.name)), span)?;
                            self.current_chunk().emit_op(Op::Constant, span);
                            self.current_chunk().emit_u16(fname, span);
                            let ftype =
                                self.add_constant(Value::String(encode_type_expr(&f.ty)), span)?;
                            self.current_chunk().emit_op(Op::Constant, span);
                            self.current_chunk().emit_u16(ftype, span);
                        }
                        self.current_chunk().emit_op(Op::MakeList, span);
                        self.current_chunk()
                            .emit_u16((field_count * 2) as u16, span);
                        let meta_key = self.add_constant(
                            Value::String(format!("__record_fields__{}", type_decl.name)),
                            span,
                        )?;
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(meta_key, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                }
                Ok(())
            }

            Decl::TraitImpl(trait_impl) => {
                // Compile each method and register as "TypeName.method_name" global.
                for method in &trait_impl.methods {
                    let arity = method.params.len() as u8;
                    let qualified_name = format!("{}.{}", trait_impl.target_type, method.name);
                    let span = method.span;

                    self.contexts
                        .push(CompileContext::new(qualified_name.clone(), arity));

                    // Add parameters as locals.
                    for (i, param) in method.params.iter().enumerate() {
                        match &param.pattern {
                            Pattern::Ident(name) => {
                                self.warn_if_shadows_module(*name, span);
                                self.add_local(*name);
                            }
                            _ => {
                                let slot = self.add_local(intern(&format!("__param_{i}__")));
                                self.current_chunk().emit_op(Op::GetLocal, span);
                                self.current_chunk().emit_u16(slot, span);
                                let _hidden = self.add_local(intern("__param_copy__"));
                                self.current_chunk().emit_op(Op::SetLocal, span);
                                self.current_chunk().emit_u16(_hidden, span);
                                self.compile_pattern_bind(&param.pattern, span)?;
                            }
                        }
                    }

                    self.compile_expr(&method.body)?;
                    self.current_chunk().emit_op(Op::Return, span);

                    let ctx = self.contexts.pop().ok_or(CompileError {
                        message: "compiler bug: missing trait method context".into(),
                        span,
                    })?;
                    let func = ctx.function;
                    let vm_closure = Arc::new(VmClosure {
                        function: Arc::new(func),
                        upvalues: vec![],
                    });
                    let closure_val = Value::VmClosure(vm_closure);
                    let fi = self.add_constant(closure_val, span)?;
                    self.current_chunk().emit_op(Op::Constant, span);
                    self.current_chunk().emit_u16(fi, span);

                    let name_idx = self.add_constant(Value::String(qualified_name), span)?;
                    self.current_chunk().emit_op(Op::SetGlobal, span);
                    self.current_chunk().emit_u16(name_idx, span);
                    self.current_chunk().emit_op(Op::Pop, span);
                }
                Ok(())
            }

            Decl::Trait(_) => {
                // Trait declarations just define the interface; nothing to emit.
                Ok(())
            }

            Decl::Import(target, span) => self.compile_import(target, *span),
        }
    }

    // ── Import compilation ─────────────────────────────────────────

    fn compile_import(&mut self, target: &ImportTarget, span: Span) -> Result<(), CompileError> {
        match target {
            ImportTarget::Module(name) => {
                // Builtin modules (io, string, list, ...) are already registered
                // in the VM's global table. Record the import for gating.
                let name_str = resolve(*name);
                if module::is_builtin_module(&name_str) {
                    self.imported_builtin_modules.insert(name_str);
                    return Ok(());
                }
                self.compile_file_module(&name_str, span)?;
                Ok(())
            }
            ImportTarget::Items(module_name, items) => {
                let mod_str = resolve(*module_name);
                if module::is_builtin_module(&mod_str) {
                    self.imported_builtin_modules.insert(mod_str.clone());
                    // For builtin modules, create aliases: bare "item" -> "module.item"
                    for item in items {
                        let item_str = resolve(*item);
                        let qualified = format!("{mod_str}.{item_str}");
                        let qi = self.add_constant(Value::String(qualified), span)?;
                        self.current_chunk().emit_op(Op::GetGlobal, span);
                        self.current_chunk().emit_u16(qi, span);
                        let bare_i = self.add_constant(Value::String(item_str), span)?;
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(bare_i, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    return Ok(());
                }
                // File-based selective import: compile the module, then alias
                // "module.item" -> bare "item" for each selected name.
                self.compile_file_module(&mod_str, span)?;
                for item in items {
                    let item_str = resolve(*item);
                    let qualified = format!("{mod_str}.{item_str}");
                    let qi = self.add_constant(Value::String(qualified), span)?;
                    self.current_chunk().emit_op(Op::GetGlobal, span);
                    self.current_chunk().emit_u16(qi, span);
                    let bare_i = self.add_constant(Value::String(item_str), span)?;
                    self.current_chunk().emit_op(Op::SetGlobal, span);
                    self.current_chunk().emit_u16(bare_i, span);
                    self.current_chunk().emit_op(Op::Pop, span);
                }
                Ok(())
            }
            ImportTarget::Alias(module_name, alias) => {
                let mod_str = resolve(*module_name);
                let alias_str = resolve(*alias);
                if module::is_builtin_module(&mod_str) {
                    self.imported_builtin_modules.insert(mod_str.clone());
                    // Builtin alias: copy all "module.func" globals to "alias.func".
                    let names = module::builtin_module_functions(&mod_str)
                        .into_iter()
                        .chain(module::builtin_module_constants(&mod_str));
                    for func in names {
                        let qualified = format!("{mod_str}.{func}");
                        let qi = self.add_constant(Value::String(qualified), span)?;
                        self.current_chunk().emit_op(Op::GetGlobal, span);
                        self.current_chunk().emit_u16(qi, span);
                        let alias_name = format!("{alias_str}.{func}");
                        let ai = self.add_constant(Value::String(alias_name), span)?;
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(ai, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    return Ok(());
                }
                // File module with alias: compile under original name, then
                // re-register each public declaration under the alias prefix.
                let public_names = self.compile_file_module(&mod_str, span)?;
                for name in &public_names {
                    let original = format!("{mod_str}.{name}");
                    let qi = self.add_constant(Value::String(original), span)?;
                    self.current_chunk().emit_op(Op::GetGlobal, span);
                    self.current_chunk().emit_u16(qi, span);
                    let alias_name = format!("{alias_str}.{name}");
                    let ai = self.add_constant(Value::String(alias_name), span)?;
                    self.current_chunk().emit_op(Op::SetGlobal, span);
                    self.current_chunk().emit_u16(ai, span);
                    self.current_chunk().emit_op(Op::Pop, span);
                }
                Ok(())
            }
        }
    }

    /// Compile a file-based module's declarations into the current compilation
    /// unit. Each public declaration is registered as a global named
    /// `"module_name.decl_name"`. Returns the list of public names exported by
    /// this module.
    fn compile_file_module(
        &mut self,
        module_name: &str,
        span: Span,
    ) -> Result<Vec<String>, CompileError> {
        // Guard against double-compilation.
        if self.compiled_modules.contains(module_name) {
            return Ok(vec![]);
        }

        // Detect circular imports.
        if self.compiling_modules.contains(module_name) {
            return Err(CompileError {
                message: format!(
                    "circular import detected: module '{module_name}' imports itself (directly or indirectly)"
                ),
                span,
            });
        }
        self.compiling_modules.insert(module_name.to_string());

        let result = self.compile_file_module_inner(module_name, span);

        self.compiling_modules.remove(module_name);
        if result.is_ok() {
            self.compiled_modules.insert(module_name.to_string());
        }
        result
    }

    /// Inner implementation of file module compilation, separated so that
    /// the circular-import guard can wrap it cleanly.
    fn compile_file_module_inner(
        &mut self,
        module_name: &str,
        span: Span,
    ) -> Result<Vec<String>, CompileError> {
        let project_root = self.project_root.as_ref().ok_or_else(|| {
            CompileError {
                message: format!("cannot import module '{module_name}': no project root set (use Compiler::with_project_root)"),
                span,
            }
        })?;

        // Resolve, read, lex, parse the module file.
        let file_path = project_root.join(format!("{module_name}.silt"));
        let source = std::fs::read_to_string(&file_path).map_err(|e| CompileError {
            message: format!("cannot load module '{module_name}': {e}"),
            span,
        })?;

        let file_display = file_path.display().to_string();

        let tokens = Lexer::new(&source).tokenize().map_err(|e| CompileError {
            message: format_module_source_error(
                module_name,
                &file_display,
                &source,
                "lex error",
                &e.message,
                e.span,
            ),
            span,
        })?;

        let mut program = Parser::new(tokens)
            .parse_program()
            .map_err(|e| CompileError {
                message: format_module_source_error(
                    module_name,
                    &file_display,
                    &source,
                    "parse error",
                    &e.message,
                    e.span,
                ),
                span,
            })?;

        // Type-check the imported module before compiling.
        // Type errors are not fatal here — modules with transitive imports will
        // have "undefined" errors from the type checker because module resolution
        // only happens during compilation.  The compiler resolves them below.
        let _type_errors = typechecker::check(&mut program);

        // Collect public names so we know which to export.
        let mut public_fns = HashSet::new();
        let mut public_types = HashSet::new();
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) if f.is_pub => {
                    public_fns.insert(f.name);
                }
                Decl::Type(t) if t.is_pub => {
                    public_types.insert(t.name);
                }
                _ => {}
            }
        }

        // Track all exported names (functions + types + variants) for alias support.
        let mut exported_names: Vec<String> = Vec::new();

        // Build module scope: all function names in this module.
        // Public functions are registered as "module.fn", private as "__module__fn".
        // This lets intra-module calls resolve bare names to the correct global.
        // Save the parent scope first — recursive module compilation (imports) will
        // overwrite it, so we need to restore ours after processing imports.
        let saved_scope = self.module_scope.take();
        let mut all_fn_names: HashMap<String, bool> = HashMap::new();
        for decl in &program.decls {
            if let Decl::Fn(f) = decl {
                all_fn_names.insert(resolve(f.name), f.is_pub);
            }
        }
        self.module_scope = Some((module_name.to_string(), all_fn_names));

        // Compile each declaration. Functions get registered as
        // "module_name.fn_name" for public ones, or just compiled (for
        // internal helpers that closures might reference). Synthetic emissions
        // below (Op::SetGlobal, constants, etc.) carry the import statement's
        // span so anything that blames them points back to the import site.
        for decl in &program.decls {
            match decl {
                Decl::Fn(fn_decl) => {
                    let arity = fn_decl.params.len() as u8;
                    let fn_span = fn_decl.span;

                    self.contexts
                        .push(CompileContext::new(resolve(fn_decl.name), arity));

                    // Add parameters as locals.
                    let mut param_slots = Vec::new();
                    for (i, param) in fn_decl.params.iter().enumerate() {
                        match &param.pattern {
                            Pattern::Ident(name) => {
                                self.warn_if_shadows_module(*name, fn_span);
                                self.add_local(*name);
                                param_slots.push((i, None));
                            }
                            _ => {
                                let slot = self.add_local(intern(&format!("__param_{i}__")));
                                param_slots.push((i, Some((slot, param.pattern.clone()))));
                            }
                        }
                    }
                    for (_i, maybe_destruct) in &param_slots {
                        if let Some((slot, pattern)) = maybe_destruct {
                            self.current_chunk().emit_op(Op::GetLocal, fn_span);
                            self.current_chunk().emit_u16(*slot, fn_span);
                            let _hidden = self.add_local(intern("__param_copy__"));
                            self.current_chunk().emit_op(Op::SetLocal, fn_span);
                            self.current_chunk().emit_u16(_hidden, fn_span);
                            self.compile_pattern_bind(pattern, fn_span)?;
                        }
                    }

                    self.compile_expr(&fn_decl.body)?;
                    self.current_chunk().emit_op(Op::Return, fn_span);

                    let ctx = self.contexts.pop().ok_or(CompileError {
                        message: "compiler bug: missing module function context".into(),
                        span,
                    })?;
                    let func = ctx.function;

                    let vm_closure = Arc::new(VmClosure {
                        function: Arc::new(func),
                        upvalues: vec![],
                    });
                    let closure_val = Value::VmClosure(vm_closure);
                    let fi = self.add_constant(closure_val, span)?;
                    self.current_chunk().emit_op(Op::Constant, span);
                    self.current_chunk().emit_u16(fi, span);

                    if public_fns.contains(&fn_decl.name) {
                        // Register as "module_name.fn_name"
                        let qualified = format!("{module_name}.{}", fn_decl.name);
                        let name_idx = self.add_constant(Value::String(qualified), span)?;
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                        exported_names.push(resolve(fn_decl.name));
                    } else {
                        // Internal function — still register so closures / calls work,
                        // but under a mangled private name.
                        let private_name = format!("__{module_name}__{}", fn_decl.name);
                        let name_idx = self.add_constant(Value::String(private_name), span)?;
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                }
                Decl::Type(type_decl) if public_types.contains(&type_decl.name) => {
                    // Compile the type declaration — registers variants under bare names.
                    self.compile_decl(decl)?;
                    // Also register type name and variants under qualified names.
                    exported_names.push(resolve(type_decl.name));
                    match &type_decl.body {
                        crate::ast::TypeBody::Enum(variants) => {
                            for variant in variants {
                                // Copy bare "VariantName" -> "module.VariantName"
                                let vname = resolve(variant.name);
                                let bare_idx =
                                    self.add_constant(Value::String(vname.clone()), span)?;
                                self.current_chunk().emit_op(Op::GetGlobal, span);
                                self.current_chunk().emit_u16(bare_idx, span);
                                let qual = format!("{module_name}.{vname}");
                                let qual_idx = self.add_constant(Value::String(qual), span)?;
                                self.current_chunk().emit_op(Op::SetGlobal, span);
                                self.current_chunk().emit_u16(qual_idx, span);
                                self.current_chunk().emit_op(Op::Pop, span);
                                exported_names.push(vname);
                            }
                            // Register the type name itself as a qualified global
                            // (pointing to the type name string for use in `import mod.{ Type }`).
                            let type_val = Value::String(resolve(type_decl.name));
                            let type_val_idx = self.add_constant(type_val, span)?;
                            self.current_chunk().emit_op(Op::Constant, span);
                            self.current_chunk().emit_u16(type_val_idx, span);
                            let qual_type = format!("{module_name}.{}", type_decl.name);
                            let qual_type_idx =
                                self.add_constant(Value::String(qual_type), span)?;
                            self.current_chunk().emit_op(Op::SetGlobal, span);
                            self.current_chunk().emit_u16(qual_type_idx, span);
                            self.current_chunk().emit_op(Op::Pop, span);
                        }
                        crate::ast::TypeBody::Record(_) => {
                            // Copy bare type name -> "module.TypeName"
                            let bare_idx =
                                self.add_constant(Value::String(resolve(type_decl.name)), span)?;
                            self.current_chunk().emit_op(Op::GetGlobal, span);
                            self.current_chunk().emit_u16(bare_idx, span);
                            let qual = format!("{module_name}.{}", type_decl.name);
                            let qual_idx = self.add_constant(Value::String(qual), span)?;
                            self.current_chunk().emit_op(Op::SetGlobal, span);
                            self.current_chunk().emit_u16(qual_idx, span);
                            self.current_chunk().emit_op(Op::Pop, span);
                        }
                    }
                }
                Decl::Type(_) => {
                    // Private type — compile it anyway (might be referenced).
                    self.compile_decl(decl)?;
                }
                Decl::Import(..) => {
                    // Nested imports from within a module.
                    self.compile_decl(decl)?;
                }
                Decl::TraitImpl(_) => {
                    self.compile_decl(decl)?;
                }
                Decl::Trait(_) => {
                    // Skip.
                }
                Decl::Let { .. } => {
                    self.compile_decl(decl)?;
                }
            }
        }
        self.module_scope = saved_scope;
        Ok(exported_names)
    }

    // ── Statements ────────────────────────────────────────────────

    fn compile_stmt(&mut self, stmt: &Stmt, is_last: bool) -> Result<(), CompileError> {
        match stmt {
            Stmt::Let { pattern, value, .. } => {
                self.compile_expr(value)?;
                let span = value.span;

                match pattern {
                    Pattern::Ident(name) => {
                        self.warn_if_shadows_module(*name, span);
                        let slot = self.add_local(*name);
                        self.current_chunk().emit_op(Op::SetLocal, span);
                        self.current_chunk().emit_u16(slot, span);
                        if is_last {
                            self.current_chunk().emit_op(Op::Unit, span);
                        }
                    }
                    _ => {
                        // General pattern destructuring for let bindings.
                        // The value is on TOS. Register it as a hidden local,
                        // then recursively bind sub-patterns.
                        let _val_slot = self.add_local(intern("__let_val__"));
                        self.current_chunk().emit_op(Op::SetLocal, span);
                        self.current_chunk().emit_u16(_val_slot, span);

                        // The value is now on the stack as a hidden local.
                        // compile_pattern_bind expects the value on TOS.
                        // TOS IS the value (it's the hidden local slot).
                        self.compile_pattern_bind(pattern, span)?;

                        if is_last {
                            self.current_chunk().emit_op(Op::Unit, span);
                        }
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

            Stmt::WhenBool {
                condition,
                else_body,
            } => {
                // Compile condition, jump to else if false
                self.compile_expr(condition)?;
                let else_jump = self
                    .current_chunk()
                    .emit_jump(Op::JumpIfFalse, condition.span);

                // Condition was true — skip else block
                let end_jump = self.current_chunk().emit_jump(Op::Jump, condition.span);

                // Else block: condition was false
                self.patch_jump(else_jump, condition.span)?;
                self.compile_expr(else_body)?;
                // The else body must diverge (return or panic).
                // If it doesn't, we just pop its value and continue.
                self.current_chunk().emit_op(Op::Pop, condition.span);

                self.patch_jump(end_jump, condition.span)?;

                if is_last {
                    self.current_chunk().emit_op(Op::Unit, condition.span);
                }
                Ok(())
            }

            Stmt::When {
                pattern,
                expr,
                else_body,
            } => {
                // Compile expression
                self.compile_expr(expr)?;
                let span = expr.span;

                // Test pattern
                self.current_chunk().emit_op(Op::Dup, span);
                let fail_jumps = self.compile_pattern_test(pattern, span)?;

                // Pattern matched — bind variables
                self.compile_pattern_bind(pattern, span)?;
                self.current_chunk().emit_op(Op::Pop, span); // pop scrutinee
                let end_jump = self.current_chunk().emit_jump(Op::Jump, span);

                // Pattern didn't match
                for fj in fail_jumps {
                    self.patch_jump(fj, span)?;
                }
                self.current_chunk().emit_op(Op::Pop, span); // pop scrutinee
                self.compile_expr(else_body)?;
                self.current_chunk().emit_op(Op::Pop, span); // pop else result

                self.patch_jump(end_jump, span)?;

                if is_last {
                    self.current_chunk().emit_op(Op::Unit, span);
                }
                Ok(())
            }
        }
    }

    // ── Expressions ───────────────────────────────────────────────

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), CompileError> {
        let span = expr.span;
        let tail = self.in_tail_position;
        self.in_tail_position = false;

        match &expr.kind {
            ExprKind::Int(n) => {
                let idx = self.add_constant(Value::Int(*n), span)?;
                self.current_chunk().emit_op(Op::Constant, span);
                self.current_chunk().emit_u16(idx, span);
            }

            ExprKind::Float(n) => {
                let idx = self.add_constant(Value::Float(*n), span)?;
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

            ExprKind::StringLit(s, _) => {
                let idx = self.add_constant(Value::String(s.clone()), span)?;
                self.current_chunk().emit_op(Op::Constant, span);
                self.current_chunk().emit_u16(idx, span);
            }

            ExprKind::Unit => {
                self.current_chunk().emit_op(Op::Unit, span);
            }

            ExprKind::Binary(left, op, right) => {
                match op {
                    BinOp::And => {
                        // Short-circuit: if left is false, skip right
                        self.compile_expr(left)?;
                        // Duplicate TOS so we can test and still have the value
                        self.current_chunk().emit_op(Op::Dup, span);
                        let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                        // Left was truthy, discard it and evaluate right
                        self.current_chunk().emit_op(Op::Pop, span);
                        self.compile_expr(right)?;
                        self.patch_jump(jump, span)?;
                    }
                    BinOp::Or => {
                        // Short-circuit: if left is true, skip right
                        self.compile_expr(left)?;
                        // Duplicate TOS so we can test and still have the value
                        self.current_chunk().emit_op(Op::Dup, span);
                        let jump = self.current_chunk().emit_jump(Op::JumpIfTrue, span);
                        // Left was falsy, discard it and evaluate right
                        self.current_chunk().emit_op(Op::Pop, span);
                        self.compile_expr(right)?;
                        self.patch_jump(jump, span)?;
                    }
                    _ => {
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
                            BinOp::And | BinOp::Or => unreachable!(),
                        };
                        self.current_chunk().emit_op(opcode, span);
                    }
                }
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
                        if i == last_idx {
                            self.in_tail_position = tail;
                        }
                        self.compile_stmt(stmt, i == last_idx)?;
                    }
                }

                self.end_scope(span);
            }

            ExprKind::Ident(name) => {
                if let Some(slot) = self.resolve_local(*name) {
                    self.current_chunk().emit_op(Op::GetLocal, span);
                    self.current_chunk().emit_u16(slot, span);
                } else if let Some(idx) = self.resolve_upvalue(*name)? {
                    self.current_chunk().emit_op(Op::GetUpvalue, span);
                    self.current_chunk().emit_u8(idx, span);
                } else {
                    // Gate constructors that require module imports
                    let name_str = resolve(*name);
                    if let Some(required) = module::gated_constructor_module(&name_str)
                        && !self.imported_builtin_modules.contains(required)
                    {
                        return Err(CompileError {
                            message: format!("'{name}' requires `import {required}`"),
                            span,
                        });
                    }
                    // If we're inside a module and this name matches a sibling function,
                    // qualify it so intra-module calls resolve correctly.
                    // Public fns: "module.name", private fns: "__module__name".
                    let resolved_name = if let Some((ref mod_name, ref fn_map)) = self.module_scope
                    {
                        match fn_map.get(&name_str) {
                            Some(true) => format!("{mod_name}.{name_str}"),
                            Some(false) => format!("__{mod_name}__{name_str}"),
                            None => name_str,
                        }
                    } else {
                        name_str
                    };
                    let name_idx = self.add_constant(Value::String(resolved_name), span)?;
                    self.current_chunk().emit_op(Op::GetGlobal, span);
                    self.current_chunk().emit_u16(name_idx, span);
                }
            }

            ExprKind::Call(callee, args) => {
                // Check if this is a module-qualified builtin call like list.map(...)
                if let Some(builtin_name) = self.extract_builtin_name(callee)? {
                    // Emit arguments first
                    for arg in args {
                        self.compile_expr(arg)?;
                    }
                    let argc = args.len() as u8;
                    let name_idx = self.add_constant(Value::String(builtin_name), span)?;
                    self.current_chunk().emit_op(Op::CallBuiltin, span);
                    self.current_chunk().emit_u16(name_idx, span);
                    self.current_chunk().emit_u8(argc, span);
                } else if let ExprKind::FieldAccess(receiver, method) = &callee.kind {
                    // Check if this is a module-qualified call on a non-local ident
                    let is_module_call = if let ExprKind::Ident(name) = &receiver.kind {
                        self.resolve_local(*name).is_none()
                            && self.resolve_upvalue_peek(*name).is_none()
                    } else {
                        false
                    };
                    if is_module_call {
                        if let ExprKind::Ident(module) = &receiver.kind {
                            // Gate: require import for builtin modules
                            let mod_str = resolve(*module);
                            if module::is_builtin_module(&mod_str)
                                && !self.imported_builtin_modules.contains(&mod_str)
                            {
                                return Err(CompileError {
                                    message: format!(
                                        "module '{module}' is not imported; add `import {module}` at the top of the file"
                                    ),
                                    span,
                                });
                            }
                            // Module-qualified call on a global module name.
                            let qualified = format!("{module}.{method}");
                            let name_idx = self.add_constant(Value::String(qualified), span)?;
                            self.current_chunk().emit_op(Op::GetGlobal, span);
                            self.current_chunk().emit_u16(name_idx, span);
                            for arg in args {
                                self.compile_expr(arg)?;
                            }
                            let argc = args.len() as u8;
                            if tail {
                                self.current_chunk().emit_op(Op::TailCall, span);
                                self.current_chunk().emit_u8(argc, span);
                                self.current_chunk().emit_op(Op::Return, span);
                            } else {
                                self.current_chunk().emit_op(Op::Call, span);
                                self.current_chunk().emit_u8(argc, span);
                            }
                        }
                    } else {
                        // Method call on a value: expr.method(args)
                        // Compile receiver as first argument.
                        self.compile_expr(receiver)?;
                        for arg in args {
                            self.compile_expr(arg)?;
                        }
                        let argc = (args.len() + 1) as u8; // receiver + args
                        let method_idx =
                            self.add_constant(Value::String(resolve(*method)), span)?;
                        self.current_chunk().emit_op(Op::CallMethod, span);
                        self.current_chunk().emit_u16(method_idx, span);
                        self.current_chunk().emit_u8(argc, span);
                    }
                } else {
                    // Normal function call
                    self.compile_expr(callee)?;
                    for arg in args {
                        self.compile_expr(arg)?;
                    }
                    let argc = args.len() as u8;
                    if tail {
                        self.current_chunk().emit_op(Op::TailCall, span);
                        self.current_chunk().emit_u8(argc, span);
                        self.current_chunk().emit_op(Op::Return, span);
                    } else {
                        self.current_chunk().emit_op(Op::Call, span);
                        self.current_chunk().emit_u8(argc, span);
                    }
                }
            }

            ExprKind::FieldAccess(expr, field) => {
                // Check if this is a module-qualified name like list.map
                // But only if the identifier is NOT a known local or upvalue.
                if let ExprKind::Ident(name) = &expr.kind {
                    let is_local = self.resolve_local(*name).is_some()
                        || self.resolve_upvalue(*name)?.is_some();
                    if !is_local {
                        let name_str = resolve(*name);
                        // Gate: require import for builtin modules.
                        if module::is_builtin_module(&name_str)
                            && !self.imported_builtin_modules.contains(&name_str)
                        {
                            return Err(CompileError {
                                message: format!(
                                    "module '{name}' is not imported; add `import {name}` at the top of the file"
                                ),
                                span,
                            });
                        }
                        // B8: In REPL mode, a previously-bound value like `p`
                        // is a VM global (created via `eval_declaration`), not
                        // a module. When the identifier is NOT a known builtin
                        // module name, fall through to the receiver-expression
                        // path below, which emits `GetGlobal(name)` followed
                        // by `GetField(field)`. That resolves `p.x` against
                        // the stored record value.
                        //
                        // In non-REPL mode we preserve the long-standing
                        // behaviour of emitting `GetGlobal("name.field")`,
                        // which is how foreign-function modules registered
                        // via `vm.register_fn1("mylib.double", ...)` and
                        // file-module aliases like `import string as s` are
                        // found (the alias path registers `s.split` as a
                        // standalone global).
                        if !self.repl_mode || module::is_builtin_module(&name_str) {
                            let qualified = format!("{name}.{field}");
                            let name_idx = self.add_constant(Value::String(qualified), span)?;
                            self.current_chunk().emit_op(Op::GetGlobal, span);
                            self.current_chunk().emit_u16(name_idx, span);
                            return Ok(());
                        }
                        // REPL mode, non-module name — fall through.
                    }
                }
                let field_str = resolve(*field);
                if let Ok(index) = field_str.parse::<u8>() {
                    // Tuple index access: expr.0, expr.1, etc.
                    self.compile_expr(expr)?;
                    self.current_chunk().emit_op(Op::GetIndex, span);
                    self.current_chunk().emit_u8(index, span);
                } else {
                    // Compile the expression and access field
                    self.compile_expr(expr)?;
                    let name_idx = self.add_constant(Value::String(field_str), span)?;
                    self.current_chunk().emit_op(Op::GetField, span);
                    self.current_chunk().emit_u16(name_idx, span);
                }
            }

            ExprKind::StringInterp(parts) => {
                let mut count: u8 = 0;
                for part in parts {
                    match part {
                        StringPart::Literal(s) => {
                            let idx = self.add_constant(Value::String(s.clone()), span)?;
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
                    // Explicit return is always in tail position.
                    self.in_tail_position = true;
                    self.compile_expr(e)?;
                } else {
                    self.current_chunk().emit_op(Op::Unit, span);
                }
                self.current_chunk().emit_op(Op::Return, span);
            }

            ExprKind::Match { expr, arms } => {
                self.compile_match(expr.as_deref(), arms, span, tail)?;
            }

            ExprKind::Lambda { params, body } => {
                let arity = params.len() as u8;

                // Push a new context for the lambda body.
                self.contexts
                    .push(CompileContext::new("<lambda>".into(), arity));

                // Add parameters as locals, with destructuring support.
                let mut lambda_param_slots = Vec::new();
                for (i, param) in params.iter().enumerate() {
                    match &param.pattern {
                        Pattern::Ident(name) => {
                            self.warn_if_shadows_module(*name, span);
                            self.add_local(*name);
                            lambda_param_slots.push(None);
                        }
                        _ => {
                            let slot = self.add_local(intern(&format!("__param_{i}__")));
                            lambda_param_slots.push(Some((slot, param.pattern.clone())));
                        }
                    }
                }

                // Emit destructuring for non-Ident lambda parameter patterns.
                for (slot, pattern) in lambda_param_slots.iter().flatten() {
                    self.current_chunk().emit_op(Op::GetLocal, span);
                    self.current_chunk().emit_u16(*slot, span);
                    let _hidden = self.add_local(intern("__param_copy__"));
                    self.current_chunk().emit_op(Op::SetLocal, span);
                    self.current_chunk().emit_u16(_hidden, span);
                    self.compile_pattern_bind(pattern, span)?;
                }

                // Compile the lambda body in tail position for TCO.
                self.in_tail_position = true;
                self.compile_expr(body)?;
                self.in_tail_position = false;
                self.current_chunk().emit_op(Op::Return, span);

                let ctx = self.contexts.pop().ok_or(CompileError {
                    message: "compiler bug: missing lambda context".into(),
                    span,
                })?;
                let upvalue_descs = ctx.upvalues.clone();
                let func = ctx.function;

                let vm_closure = Arc::new(VmClosure {
                    function: Arc::new(func),
                    upvalues: vec![],
                });
                let closure_val = Value::VmClosure(vm_closure);
                let fi = self.add_constant(closure_val, span)?;

                if upvalue_descs.is_empty() {
                    // No upvalues: just push the constant directly.
                    self.current_chunk().emit_op(Op::Constant, span);
                    self.current_chunk().emit_u16(fi, span);
                } else {
                    // Has upvalues: emit MakeClosure with descriptors.
                    self.current_chunk().emit_op(Op::MakeClosure, span);
                    self.current_chunk().emit_u16(fi, span);
                    self.current_chunk()
                        .emit_u8(upvalue_descs.len() as u8, span);
                    for desc in &upvalue_descs {
                        self.current_chunk()
                            .emit_u8(if desc.is_local { 1 } else { 0 }, span);
                        self.current_chunk().emit_u8(desc.index, span);
                    }
                }
            }

            ExprKind::Tuple(elems) => {
                if elems.len() > u8::MAX as usize {
                    return Err(CompileError {
                        message: "tuple cannot have more than 255 elements".into(),
                        span,
                    });
                }
                for elem in elems {
                    self.compile_expr(elem)?;
                }
                self.current_chunk().emit_op(Op::MakeTuple, span);
                self.current_chunk().emit_u8(elems.len() as u8, span);
            }

            ExprKind::List(elems) => {
                let has_spread = elems.iter().any(|e| matches!(e, ListElem::Spread(_)));
                if !has_spread {
                    // Fast path: no spreads, just compile all singles
                    for elem in elems {
                        if let ListElem::Single(e) = elem {
                            self.compile_expr(e)?;
                        }
                    }
                    let count = elems.len() as u16;
                    self.current_chunk().emit_op(Op::MakeList, span);
                    self.current_chunk().emit_u16(count, span);
                } else {
                    // Spread path: group consecutive singles into segments,
                    // compile each spread, and ListConcat them together.
                    let mut have_accumulated = false;
                    let mut single_count: u16 = 0;

                    for elem in elems {
                        match elem {
                            ListElem::Single(e) => {
                                self.compile_expr(e)?;
                                single_count += 1;
                            }
                            ListElem::Spread(e) => {
                                // Flush any pending singles as a MakeList
                                if single_count > 0 {
                                    self.current_chunk().emit_op(Op::MakeList, span);
                                    self.current_chunk().emit_u16(single_count, span);
                                    if have_accumulated {
                                        self.current_chunk().emit_op(Op::ListConcat, span);
                                    }
                                    have_accumulated = true;
                                    single_count = 0;
                                }
                                // Compile the spread expression (should be a list or range)
                                self.compile_expr(e)?;
                                if have_accumulated {
                                    self.current_chunk().emit_op(Op::ListConcat, span);
                                } else {
                                    have_accumulated = true;
                                }
                            }
                        }
                    }
                    // Flush any trailing singles
                    if single_count > 0 {
                        self.current_chunk().emit_op(Op::MakeList, span);
                        self.current_chunk().emit_u16(single_count, span);
                        if have_accumulated {
                            self.current_chunk().emit_op(Op::ListConcat, span);
                        }
                    } else if !have_accumulated {
                        // Edge case: empty list with spreads (shouldn't happen, but be safe)
                        self.current_chunk().emit_op(Op::MakeList, span);
                        self.current_chunk().emit_u16(0, span);
                    }
                }
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
                self.compile_pipe(left, right, span, tail)?;
            }

            ExprKind::QuestionMark(inner) => {
                self.compile_expr(inner)?;
                self.current_chunk().emit_op(Op::QuestionMark, span);
            }

            ExprKind::Ascription(inner, _) => {
                self.compile_expr(inner)?;
            }

            ExprKind::RecordCreate { name, fields } => {
                if fields.len() > u8::MAX as usize {
                    return Err(CompileError {
                        message: "record cannot have more than 255 fields".into(),
                        span,
                    });
                }
                // Push field values in order
                let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                for (_, val) in fields {
                    self.compile_expr(val)?;
                }
                let type_name_idx = self.add_constant(Value::String(resolve(*name)), span)?;
                self.current_chunk().emit_op(Op::MakeRecord, span);
                self.current_chunk().emit_u16(type_name_idx, span);
                self.current_chunk().emit_u8(field_names.len() as u8, span);
                for fname in &field_names {
                    let field_idx = self.add_constant(Value::String(resolve(*fname)), span)?;
                    self.current_chunk().emit_u16(field_idx, span);
                }
            }

            ExprKind::RecordUpdate { expr, fields } => {
                if fields.len() > u8::MAX as usize {
                    return Err(CompileError {
                        message: "record update cannot have more than 255 fields".into(),
                        span,
                    });
                }
                self.compile_expr(expr)?;
                let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                for (_, val) in fields {
                    self.compile_expr(val)?;
                }
                self.current_chunk().emit_op(Op::RecordUpdate, span);
                self.current_chunk().emit_u8(field_names.len() as u8, span);
                for fname in &field_names {
                    let field_idx = self.add_constant(Value::String(resolve(*fname)), span)?;
                    self.current_chunk().emit_u16(field_idx, span);
                }
            }

            ExprKind::Loop { bindings, body } => {
                self.compile_loop(bindings, body, span)?;
            }

            ExprKind::Recur(args) => {
                let loop_info = self.ctx().loop_stack.last().ok_or_else(|| CompileError {
                    message: "recur outside of loop".into(),
                    span,
                })?;
                let first_slot = loop_info.first_slot;
                let loop_start = loop_info.loop_start;
                let expected = loop_info.binding_count as usize;
                if args.len() != expected {
                    return Err(CompileError {
                        message: format!(
                            "loop() expects {} argument(s), got {}",
                            expected,
                            args.len()
                        ),
                        span,
                    });
                }

                for arg in args {
                    self.compile_expr(arg)?;
                }
                self.current_chunk().emit_op(Op::Recur, span);
                self.current_chunk().emit_u8(args.len() as u8, span);
                self.current_chunk().emit_u16(first_slot, span);

                // Emit JumpBack to loop start.
                let current_offset = self.current_chunk().len();
                // JumpBack operand is how far back to jump from after the operand.
                let jump_back_dist = current_offset + 3 - loop_start; // +3 for opcode + u16
                self.current_chunk().emit_op(Op::JumpBack, span);
                self.current_chunk().emit_u16(jump_back_dist as u16, span);
            }

            ExprKind::FloatElse(expr, fallback) => {
                self.compile_expr(expr)?;
                let jump = self.current_chunk().emit_jump(Op::NarrowFloat, span);
                self.compile_expr(fallback)?;
                self.patch_jump(jump, span)?;
            } // All expression kinds are handled above. If new ones are added,
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
        tail: bool,
    ) -> Result<(), CompileError> {
        // ── Guardless match (no scrutinee) ───────────────────────
        let Some(scrutinee) = scrutinee else {
            return self.compile_guardless_match(arms, span, tail);
        };

        // Compile the scrutinee and save it in a known local slot.
        // This lets us GetLocal it for each arm's test and binding.
        self.compile_expr(scrutinee)?;
        self.begin_scope();
        let scrutinee_slot = self.add_local(intern("__scrutinee__"));
        self.current_chunk().emit_op(Op::SetLocal, span);
        self.current_chunk().emit_u16(scrutinee_slot, span);

        let mut end_jumps = Vec::new();

        for arm in arms {
            // 1. Push scrutinee for testing
            self.current_chunk().emit_op(Op::GetLocal, span);
            self.current_chunk().emit_u16(scrutinee_slot, span);

            // 2. Test the pattern (value is on TOS, tests peek it)
            let fail_jumps = self.compile_pattern_test(&arm.pattern, span)?;

            // 3. Pop the test copy
            self.current_chunk().emit_op(Op::Pop, span);

            // 4. Begin a scope for this arm's bindings
            self.begin_scope();

            // 5. Push scrutinee again and bind pattern variables
            self.current_chunk().emit_op(Op::GetLocal, span);
            self.current_chunk().emit_u16(scrutinee_slot, span);
            // Register this GetLocal'd copy as a hidden local
            let _bind_copy = self.add_local(intern("__bind_src__"));
            self.current_chunk().emit_op(Op::SetLocal, span);
            self.current_chunk().emit_u16(_bind_copy, span);
            self.compile_pattern_bind(&arm.pattern, span)?;

            // 6. Guard (if present)
            let guard_jump = if let Some(guard) = &arm.guard {
                self.compile_expr(guard)?;
                let j = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Some(j)
            } else {
                None
            };

            // 7. Compile the arm body (in tail position if the match is)
            self.in_tail_position = tail;
            self.compile_expr(&arm.body)?;

            self.end_scope(span);

            // 8. Jump to end of match
            let end_jump = self.current_chunk().emit_jump(Op::Jump, span);
            end_jumps.push(end_jump);

            // 9. Patch failure / guard jumps to here (next arm)
            if let Some(gj) = guard_jump {
                self.patch_jump(gj, span)?;
            }
            for fj in fail_jumps {
                self.patch_jump(fj, span)?;
            }
        }

        // No arm matched — panic
        let msg_idx = self.add_constant(
            Value::String("non-exhaustive match: no arm matched".into()),
            span,
        )?;
        self.current_chunk().emit_op(Op::Constant, span);
        self.current_chunk().emit_u16(msg_idx, span);
        self.current_chunk().emit_op(Op::Panic, span);

        self.end_scope(span);

        // Patch all end jumps to here
        for ej in end_jumps {
            self.patch_jump(ej, span)?;
        }

        Ok(())
    }

    /// Compile a guardless match: `match { cond1 -> body1, ... }`
    fn compile_guardless_match(
        &mut self,
        arms: &[MatchArm],
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        let mut end_jumps = Vec::new();

        for arm in arms {
            if let Some(guard) = &arm.guard {
                // The guard IS the condition in a guardless match
                self.compile_expr(guard)?;
                let fail_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);

                self.in_tail_position = tail;
                self.compile_expr(&arm.body)?;
                let end_jump = self.current_chunk().emit_jump(Op::Jump, span);
                end_jumps.push(end_jump);

                self.patch_jump(fail_jump, span)?;
            } else {
                // Wildcard / default arm — always matches
                self.in_tail_position = tail;
                self.compile_expr(&arm.body)?;
                let end_jump = self.current_chunk().emit_jump(Op::Jump, span);
                end_jumps.push(end_jump);
            }
        }

        // No arm matched — panic
        let msg_idx = self.add_constant(
            Value::String("non-exhaustive match: no condition was true".into()),
            span,
        )?;
        self.current_chunk().emit_op(Op::Constant, span);
        self.current_chunk().emit_u16(msg_idx, span);
        self.current_chunk().emit_op(Op::Panic, span);

        for ej in end_jumps {
            self.patch_jump(ej, span)?;
        }

        Ok(())
    }

    // ── Pipe compilation ─────────────────────────────────────────

    fn compile_pipe(
        &mut self,
        left: &Expr,
        right: &Expr,
        span: Span,
        tail: bool,
    ) -> Result<(), CompileError> {
        // val |> f(args) -> f(val, args)
        // val |> f       -> f(val)
        //
        // For builtins (CallBuiltin): val first, then args — the builtin
        // reads them positionally, no callee on the stack.
        //
        // For non-builtins (Call): callee first, then val, then args — Call
        // pops callee + N args.  Compiling callee before val avoids needing
        // a hidden local to stash the pipe value, which previously leaked a
        // ghost stack slot and corrupted record field assignments.
        match &right.kind {
            ExprKind::Call(callee, args) => {
                if let Some(builtin_name) = self.extract_builtin_name(callee)? {
                    // Builtins: val on stack first, then args
                    self.compile_expr(left)?;
                    for arg in args {
                        self.compile_expr(arg)?;
                    }
                    let argc = (args.len() + 1) as u8;
                    let name_idx = self.add_constant(Value::String(builtin_name), span)?;
                    self.current_chunk().emit_op(Op::CallBuiltin, span);
                    self.current_chunk().emit_u16(name_idx, span);
                    self.current_chunk().emit_u8(argc, span);
                } else {
                    // Non-builtin: callee first, then val, then args
                    self.compile_expr(callee)?;
                    self.compile_expr(left)?;
                    for arg in args {
                        self.compile_expr(arg)?;
                    }
                    let argc = (args.len() + 1) as u8;
                    if tail {
                        self.current_chunk().emit_op(Op::TailCall, span);
                        self.current_chunk().emit_u8(argc, span);
                        self.current_chunk().emit_op(Op::Return, span);
                    } else {
                        self.current_chunk().emit_op(Op::Call, span);
                        self.current_chunk().emit_u8(argc, span);
                    }
                }
            }
            _ => {
                // val |> f: callee first, then val
                self.compile_expr(right)?;
                self.compile_expr(left)?;
                if tail {
                    self.current_chunk().emit_op(Op::TailCall, span);
                    self.current_chunk().emit_u8(1, span);
                    self.current_chunk().emit_op(Op::Return, span);
                } else {
                    self.current_chunk().emit_op(Op::Call, span);
                    self.current_chunk().emit_u8(1, span);
                }
            }
        }
        Ok(())
    }

    // ── Loop compilation ─────────────────────────────────────────

    fn compile_loop(
        &mut self,
        bindings: &[(Symbol, Expr)],
        body: &Expr,
        span: Span,
    ) -> Result<(), CompileError> {
        self.begin_scope();

        // Compile initial values and store in locals.
        // Record the first slot so Recur knows where to write.
        // Note: do NOT pop after SetLocal — the value stays on the stack as the local's slot.
        let mut first_slot = 0u16;
        for (i, (name, init)) in bindings.iter().enumerate() {
            self.compile_expr(init)?;
            self.warn_if_shadows_module(*name, span);
            let slot = self.add_local(*name);
            if i == 0 {
                first_slot = slot;
            }
            self.current_chunk().emit_op(Op::SetLocal, span);
            self.current_chunk().emit_u16(slot, span);
        }

        // Record the loop start for JumpBack.
        let loop_start = self.current_chunk().len();

        // Push loop info so Recur knows what to do.
        self.ctx_mut().loop_stack.push(LoopInfo {
            first_slot,
            loop_start,
            binding_count: bindings.len() as u8,
        });

        // Compile body.
        self.compile_expr(body)?;

        // Pop loop info.
        self.ctx_mut().loop_stack.pop();

        // The body either used `recur` (which updates locals and jumps back)
        // or fell through with the final value on the stack.

        self.end_scope(span);

        Ok(())
    }

    // ── Helper: extract builtin name ─────────────────────────────

    /// If the callee is a module-qualified builtin (e.g., `list.map`),
    /// return the qualified name. Only returns Some if the ident is NOT a
    /// local/upvalue AND belongs to a known builtin module.
    fn extract_builtin_name(&self, callee: &Expr) -> Result<Option<String>, CompileError> {
        if let ExprKind::FieldAccess(expr, field) = &callee.kind
            && let ExprKind::Ident(module) = &expr.kind
        {
            // Check if it's a local or upvalue first
            let mod_str = resolve(*module);
            if self.resolve_local(*module).is_none()
                && self.resolve_upvalue_peek(*module).is_none()
                && module::is_builtin_module(&mod_str)
            {
                if !self.imported_builtin_modules.contains(&mod_str) {
                    return Err(CompileError {
                        message: format!(
                            "module '{module}' is not imported; add `import {module}` at the top of the file"
                        ),
                        span: callee.span,
                    });
                }
                return Ok(Some(format!("{module}.{field}")));
            }
        }
        Ok(None)
    }

    // ── Context & scope helpers ───────────────────────────────────

    fn ctx(&self) -> &CompileContext {
        debug_assert!(
            !self.contexts.is_empty(),
            "Compiler::ctx() called with empty context stack — \
             this indicates a mismatched push_context/pop_context pair"
        );
        self.contexts.last().unwrap_or_else(|| {
            panic!(
                "internal compiler error: context stack is empty in ctx(); \
                 this indicates a mismatched push_context/pop_context pair"
            )
        })
    }

    fn ctx_mut(&mut self) -> &mut CompileContext {
        debug_assert!(
            !self.contexts.is_empty(),
            "Compiler::ctx_mut() called with empty context stack — \
             this indicates a mismatched push_context/pop_context pair"
        );
        self.contexts.last_mut().unwrap_or_else(|| {
            panic!(
                "internal compiler error: context stack is empty in ctx_mut(); \
                 this indicates a mismatched push_context/pop_context pair"
            )
        })
    }

    fn current_chunk(&mut self) -> &mut Chunk {
        &mut self.ctx_mut().function.chunk
    }

    /// Add a constant to the current chunk, converting overflow to `CompileError`.
    fn add_constant(&mut self, value: Value, span: Span) -> Result<u16, CompileError> {
        self.current_chunk()
            .add_constant(value)
            .map_err(|msg| CompileError { message: msg, span })
    }

    /// Patch a jump in the current chunk, converting overflow to `CompileError`.
    fn patch_jump(&mut self, patch_offset: usize, span: Span) -> Result<(), CompileError> {
        self.current_chunk()
            .patch_jump(patch_offset)
            .map_err(|msg| CompileError { message: msg, span })
    }

    fn begin_scope(&mut self) {
        self.ctx_mut().scope_depth += 1;
    }

    fn end_scope(&mut self, span: Span) {
        let depth = self.ctx().scope_depth;
        // Pop locals belonging to the scope we are leaving.
        let mut pop_count: u8 = 0;
        while self.ctx().locals.last().is_some_and(|l| l.depth >= depth) {
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

    fn add_local(&mut self, name: Symbol) -> u16 {
        let depth = self.ctx().scope_depth;
        let slot = self.ctx().locals.len() as u16;
        self.ctx_mut().locals.push(Local {
            name,
            depth,
            slot,
            captured: false,
        });
        slot
    }

    /// Emit a warning if `name` shadows a builtin module like `json`, `int`, etc.
    fn warn_if_shadows_module(&mut self, name: Symbol, span: Span) {
        let s = resolve(name);
        if module::is_builtin_module(&s) {
            self.warnings.push(CompileWarning {
                message: format!(
                    "variable '{s}' shadows the builtin '{s}' module; \
                     use a different name to access '{s}.* functions"
                ),
                span,
            });
        }
    }

    fn resolve_local(&self, name: Symbol) -> Option<u16> {
        let ctx = self.ctx();
        // Search from the innermost local outward.
        for local in ctx.locals.iter().rev() {
            if local.name == name {
                return Some(local.slot);
            }
        }
        None
    }

    /// Non-mutating check if a variable could be resolved as an upvalue.
    /// Used for determining if an identifier is a variable vs module name.
    fn resolve_upvalue_peek(&self, name: Symbol) -> Option<()> {
        let current_idx = self.contexts.len() - 1;
        if current_idx == 0 {
            return None;
        }
        // Check if the variable exists as a local in any enclosing context
        // or as an upvalue already captured.
        for i in (0..current_idx).rev() {
            let ctx = &self.contexts[i];
            if ctx.locals.iter().any(|l| l.name == name) {
                return Some(());
            }
            if ctx.upvalues.iter().any(|_| false) {
                // Can't easily check names of upvalues, but the local check is enough
            }
        }
        // Also check if it's already captured as an upvalue in the current context
        // This is a heuristic — we just need to know if it's a variable, not necessarily capture it
        None
    }

    /// Resolve a variable as an upvalue by walking enclosing compile contexts.
    ///
    /// If the variable is found as a local in an enclosing scope, it is captured
    /// as an upvalue (is_local = true). If the enclosing scope already has it as
    /// an upvalue, it is chained through (is_local = false, transitive capture).
    fn resolve_upvalue(&mut self, name: Symbol) -> Result<Option<u8>, CompileError> {
        let current_idx = self.contexts.len() - 1;
        if current_idx == 0 {
            return Ok(None); // Top-level script has no enclosing scope.
        }
        self.resolve_upvalue_in(name, current_idx)
    }

    fn resolve_upvalue_in(
        &mut self,
        name: Symbol,
        context_index: usize,
    ) -> Result<Option<u8>, CompileError> {
        if context_index == 0 {
            return Ok(None); // No more enclosing scopes.
        }
        let enclosing_idx = context_index - 1;

        // Check if the variable is a local in the immediately enclosing context.
        let local_slot = {
            let enclosing = &self.contexts[enclosing_idx];
            enclosing.locals.iter().rev().find_map(
                |l| {
                    if l.name == name { Some(l.slot) } else { None }
                },
            )
        };

        if let Some(slot) = local_slot {
            // Mark the local as captured.
            let enclosing = &mut self.contexts[enclosing_idx];
            if let Some(local) = enclosing.locals.iter_mut().find(|l| l.name == name) {
                local.captured = true;
            }
            let index = if slot > u8::MAX as u16 {
                return Err(CompileError {
                    message: format!(
                        "cannot capture local in slot {slot} as upvalue (max slot 255)"
                    ),
                    span: Span::new(0, 0),
                });
            } else {
                slot as u8
            };
            // Add an upvalue descriptor to the current context.
            return Ok(Some(self.add_upvalue(
                context_index,
                UpvalueDesc {
                    is_local: true,
                    index,
                },
            )));
        }

        // Not a local in the enclosing scope -- try recursively as an upvalue.
        if let Some(parent_upvalue_idx) = self.resolve_upvalue_in(name, enclosing_idx)? {
            // The enclosing scope has it as an upvalue. Chain it.
            return Ok(Some(self.add_upvalue(
                context_index,
                UpvalueDesc {
                    is_local: false,
                    index: parent_upvalue_idx,
                },
            )));
        }

        Ok(None)
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

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Op;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    /// Compile declarations (no main call) and return all functions.
    fn compile(input: &str) -> Vec<Function> {
        let tokens = Lexer::new(input).tokenize().unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut compiler = Compiler::new();
        compiler.import_all_builtins();
        compiler.compile_declarations(&program).unwrap()
    }

    /// Compile expecting an error, return the error.
    fn compile_err(input: &str) -> CompileError {
        let tokens = Lexer::new(input).tokenize().unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut compiler = Compiler::new();
        compiler.compile_declarations(&program).unwrap_err()
    }

    /// Compile without builtin imports (to test import gating).
    fn compile_no_imports(input: &str) -> Result<Vec<Function>, CompileError> {
        let tokens = Lexer::new(input).tokenize().unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut compiler = Compiler::new();
        compiler.compile_declarations(&program)
    }

    /// Check if a specific opcode byte appears in the chunk's bytecode.
    fn has_op(chunk: &Chunk, op: Op) -> bool {
        chunk.code.contains(&(op as u8))
    }

    /// Check if a string constant exists in the chunk.
    fn has_string_constant(chunk: &Chunk, s: &str) -> bool {
        chunk
            .constants
            .iter()
            .any(|c| matches!(c, Value::String(v) if v == s))
    }

    /// Check if an int constant exists in the chunk.
    fn has_int_constant(chunk: &Chunk, n: i64) -> bool {
        chunk
            .constants
            .iter()
            .any(|c| matches!(c, Value::Int(v) if *v == n))
    }

    /// Find a function by name in the compiled output.
    /// Functions are embedded as VmClosure constants in the script's chunk,
    /// so we search through all constants recursively.
    fn find_fn<'a>(fns: &'a [Function], name: &str) -> &'a Function {
        // First check top-level functions
        for f in fns {
            if f.name == name {
                return f;
            }
        }
        // Search VmClosure constants in each function's chunk
        for f in fns {
            if let Some(found) = find_fn_in_constants(&f.chunk, name) {
                return found;
            }
        }
        panic!("function '{name}' not found")
    }

    fn find_fn_in_constants<'a>(chunk: &'a Chunk, name: &str) -> Option<&'a Function> {
        for constant in &chunk.constants {
            if let Value::VmClosure(closure) = constant {
                if closure.function.name == name {
                    return Some(&closure.function);
                }
                // Recurse into nested closures
                if let Some(found) = find_fn_in_constants(&closure.function.chunk, name) {
                    return Some(found);
                }
            }
        }
        None
    }

    // ── Basic literal compilation ──────────────────────────────────

    #[test]
    fn test_compile_int_literal() {
        let fns = compile("fn main() { 42 }");
        let main = find_fn(&fns, "main");
        assert!(has_int_constant(&main.chunk, 42));
        assert!(has_op(&main.chunk, Op::Constant));
        assert!(has_op(&main.chunk, Op::Return));
    }

    #[test]
    fn test_compile_float_literal() {
        let fns = compile("fn main() { 3.14 }");
        let main = find_fn(&fns, "main");
        assert!(
            main.chunk
                .constants
                .iter()
                .any(|c| matches!(c, Value::Float(f) if (*f - 3.14).abs() < f64::EPSILON))
        );
    }

    #[test]
    fn test_compile_bool_literals() {
        let fns = compile("fn main() { true }");
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::True));

        let fns = compile("fn main() { false }");
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::False));
    }

    #[test]
    fn test_compile_string_literal() {
        let fns = compile(r#"fn main() { "hello" }"#);
        let main = find_fn(&fns, "main");
        assert!(has_string_constant(&main.chunk, "hello"));
    }

    #[test]
    fn test_compile_unit() {
        let fns = compile("fn main() { () }");
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::Unit));
    }

    // ── Arithmetic & binary operations ─────────────────────────────

    #[test]
    fn test_compile_arithmetic() {
        let fns = compile("fn add(a, b) { a + b }");
        let f = find_fn(&fns, "add");
        assert_eq!(f.arity, 2);
        assert!(has_op(&f.chunk, Op::Add));

        let fns = compile("fn sub(a, b) { a - b }");
        assert!(has_op(&find_fn(&fns, "sub").chunk, Op::Sub));

        let fns = compile("fn mul(a, b) { a * b }");
        assert!(has_op(&find_fn(&fns, "mul").chunk, Op::Mul));

        let fns = compile("fn div(a, b) { a / b }");
        assert!(has_op(&find_fn(&fns, "div").chunk, Op::Div));

        let fns = compile("fn modulo(a, b) { a % b }");
        assert!(has_op(&find_fn(&fns, "modulo").chunk, Op::Mod));
    }

    #[test]
    fn test_compile_comparison() {
        let cases = [
            ("a == b", Op::Eq),
            ("a != b", Op::Neq),
            ("a < b", Op::Lt),
            ("a > b", Op::Gt),
            ("a <= b", Op::Leq),
            ("a >= b", Op::Geq),
        ];
        for (expr, expected_op) in cases {
            let src = format!("fn cmp(a, b) {{ {expr} }}");
            let fns = compile(&src);
            let f = find_fn(&fns, "cmp");
            assert!(
                has_op(&f.chunk, expected_op),
                "missing {expected_op:?} for {expr}"
            );
        }
    }

    #[test]
    fn test_compile_short_circuit_and() {
        let fns = compile("fn f(a, b) { a && b }");
        let f = find_fn(&fns, "f");
        // Short-circuit and uses Dup + JumpIfFalse + Pop
        assert!(has_op(&f.chunk, Op::Dup));
        assert!(has_op(&f.chunk, Op::JumpIfFalse));
    }

    #[test]
    fn test_compile_short_circuit_or() {
        let fns = compile("fn f(a, b) { a || b }");
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::Dup));
        assert!(has_op(&f.chunk, Op::JumpIfTrue));
    }

    // ── Unary operations ───────────────────────────────────────────

    #[test]
    fn test_compile_negate() {
        let fns = compile("fn f(x) { -x }");
        assert!(has_op(&find_fn(&fns, "f").chunk, Op::Negate));
    }

    #[test]
    fn test_compile_not() {
        let fns = compile("fn f(x) { !x }");
        assert!(has_op(&find_fn(&fns, "f").chunk, Op::Not));
    }

    // ── Variable binding ───────────────────────────────────────────

    #[test]
    fn test_compile_local_variable() {
        let fns = compile("fn f() { let x = 42\n x }");
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::SetLocal));
        assert!(has_op(&f.chunk, Op::GetLocal));
    }

    #[test]
    fn test_compile_global_let() {
        let fns = compile("let x = 10\nfn main() { x }");
        let script = &fns[0]; // script is first
        assert_eq!(script.name, "<script>");
        assert!(has_op(&script.chunk, Op::SetGlobal));
    }

    // ── Function compilation ───────────────────────────────────────

    #[test]
    fn test_compile_function_arity() {
        let fns = compile("fn f(a, b, c) { a }");
        let f = find_fn(&fns, "f");
        assert_eq!(f.arity, 3);
    }

    #[test]
    fn test_compile_function_zero_arity() {
        let fns = compile("fn f() { 42 }");
        let f = find_fn(&fns, "f");
        assert_eq!(f.arity, 0);
    }

    #[test]
    fn test_compile_multiple_functions() {
        let fns =
            compile("fn add(a, b) { a + b }\nfn sub(a, b) { a - b }\nfn main() { add(1, 2) }");
        // Script + 3 functions (as closures in the script's constant pool)
        assert_eq!(fns[0].name, "<script>");
        // Functions are compiled as constants in the script, so we look for them there
        assert!(has_string_constant(&fns[0].chunk, "add"));
        assert!(has_string_constant(&fns[0].chunk, "sub"));
        assert!(has_string_constant(&fns[0].chunk, "main"));
    }

    #[test]
    fn test_compile_function_call() {
        let fns = compile("fn id(x) { x }\nfn main() { let r = id(42)\n r }");
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::Call));
    }

    #[test]
    fn test_compile_tail_call() {
        // The body of a function in tail position should emit TailCall
        let fns = compile("fn f(n) { f(n - 1) }");
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TailCall));
    }

    // ── Lambda / closure compilation ───────────────────────────────

    #[test]
    fn test_compile_lambda() {
        let fns = compile("fn main() { let f = fn(x) { x + 1 }\n f(5) }");
        let main = find_fn(&fns, "main");
        // Lambda is compiled as a VmClosure constant
        assert!(
            main.chunk
                .constants
                .iter()
                .any(|c| matches!(c, Value::VmClosure(_)))
        );
    }

    #[test]
    fn test_compile_closure_with_upvalue() {
        let fns = compile(
            r#"
fn make_adder(n) {
    fn(x) { x + n }
}
"#,
        );
        let f = find_fn(&fns, "make_adder");
        // The inner lambda captures `n` as an upvalue — should have MakeClosure
        assert!(has_op(&f.chunk, Op::MakeClosure));
    }

    // ── Collection compilation ─────────────────────────────────────

    #[test]
    fn test_compile_list() {
        let fns = compile("fn main() { [1, 2, 3] }");
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::MakeList));
    }

    #[test]
    fn test_compile_tuple() {
        let fns = compile("fn main() { (1, 2) }");
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::MakeTuple));
    }

    #[test]
    fn test_compile_map() {
        let fns = compile(r#"fn main() { #{ "a": 1, "b": 2 } }"#);
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::MakeMap));
    }

    #[test]
    fn test_compile_set() {
        let fns = compile(r#"fn main() { #[1, 2, 3] }"#);
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::MakeSet));
    }

    #[test]
    fn test_compile_range() {
        let fns = compile("fn main() { 1..10 }");
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::MakeRange));
    }

    #[test]
    fn test_compile_list_spread() {
        let fns = compile("fn main() { let a = [1, 2]\n [..a, 3] }");
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::ListConcat));
    }

    // ── String interpolation ───────────────────────────────────────

    #[test]
    fn test_compile_string_interp() {
        let fns = compile(r#"fn greet(name) { "hello {name}" }"#);
        let f = find_fn(&fns, "greet");
        assert!(has_op(&f.chunk, Op::StringConcat));
        assert!(has_op(&f.chunk, Op::DisplayValue));
    }

    // ── Record compilation ─────────────────────────────────────────

    #[test]
    fn test_compile_record_create() {
        let fns = compile(
            r#"
type User { name: String, age: Int }
fn main() { User { name: "Alice", age: 30 } }
"#,
        );
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::MakeRecord));
    }

    #[test]
    fn test_compile_record_update() {
        let fns = compile(
            r#"
type User { name: String, age: Int }
fn main() {
    let u = User { name: "Alice", age: 30 }
    u.{ age: 31 }
}
"#,
        );
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::RecordUpdate));
    }

    #[test]
    fn test_compile_field_access() {
        let fns = compile(
            r#"
type User { name: String, age: Int }
fn main() {
    let u = User { name: "Alice", age: 30 }
    u.name
}
"#,
        );
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::GetField));
    }

    // ── Enum type declarations ─────────────────────────────────────

    #[test]
    fn test_compile_enum_variants() {
        let fns = compile(
            r#"
type Color { Red, Green, Blue }
fn main() { Red }
"#,
        );
        let script = &fns[0];
        // Nullary variants are registered as Variant values
        assert!(script.chunk.constants.iter().any(
            |c| matches!(c, Value::Variant(name, fields) if name == "Red" && fields.is_empty())
        ));
        assert!(has_string_constant(&script.chunk, "Red"));
        assert!(has_string_constant(&script.chunk, "Green"));
        assert!(has_string_constant(&script.chunk, "Blue"));
    }

    #[test]
    fn test_compile_enum_variant_constructors() {
        let fns = compile(
            r#"
type Shape { Circle(Float), Rect(Float, Float) }
fn main() { Circle(1.0) }
"#,
        );
        let script = &fns[0];
        // Constructor variants are registered as VariantConstructor values
        assert!(script.chunk.constants.iter().any(|c| matches!(c, Value::VariantConstructor(name, arity) if name == "Circle" && *arity == 1)));
        assert!(script.chunk.constants.iter().any(
            |c| matches!(c, Value::VariantConstructor(name, arity) if name == "Rect" && *arity == 2)
        ));
    }

    // ── Match compilation ──────────────────────────────────────────

    #[test]
    fn test_compile_match_literal_pattern() {
        let fns = compile(
            r#"
fn f(x) {
    match x {
        1 -> "one"
        2 -> "two"
        _ -> "other"
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TestEqual));
        assert!(has_op(&f.chunk, Op::JumpIfFalse));
    }

    #[test]
    fn test_compile_match_bool_pattern() {
        let fns = compile(
            r#"
fn f(x) {
    match x {
        true -> "yes"
        false -> "no"
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TestBool));
    }

    #[test]
    fn test_compile_match_constructor_pattern() {
        let fns = compile(
            r#"
type Opt { Some(Int), None }
fn f(x) {
    match x {
        Some(v) -> v
        None -> 0
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TestTag));
        assert!(has_op(&f.chunk, Op::DestructVariant));
    }

    #[test]
    fn test_compile_match_tuple_pattern() {
        let fns = compile(
            r#"
fn f(x) {
    match x {
        (1, y) -> y
        _ -> 0
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TestTupleLen));
    }

    #[test]
    fn test_compile_match_list_pattern() {
        let fns = compile(
            r#"
fn f(xs) {
    match xs {
        [h, ..t] -> h
        [] -> 0
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TestListMin));
        assert!(has_op(&f.chunk, Op::TestListExact));
    }

    #[test]
    fn test_compile_match_range_pattern() {
        let fns = compile(
            r#"
fn f(x) {
    match x {
        1..10 -> "low"
        _ -> "high"
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TestIntRange));
    }

    #[test]
    fn test_compile_match_record_pattern() {
        let fns = compile(
            r#"
type Point { x: Int, y: Int }
fn f(p) {
    match p {
        Point { x, y } -> x + y
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TestRecordTag));
        assert!(has_op(&f.chunk, Op::DestructRecordField));
    }

    #[test]
    fn test_compile_match_non_exhaustive_panic() {
        let fns = compile(
            r#"
fn f(x) {
    match x {
        1 -> "one"
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::Panic));
        assert!(has_string_constant(
            &f.chunk,
            "non-exhaustive match: no arm matched"
        ));
    }

    #[test]
    fn test_compile_guardless_match() {
        let fns = compile(
            r#"
fn f(x) {
    match {
        x > 0 -> "positive"
        _ -> "non-positive"
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::JumpIfFalse));
    }

    #[test]
    fn test_compile_match_with_guard() {
        let fns = compile(
            r#"
fn f(x) {
    match x {
        n when n > 0 -> "positive"
        _ -> "other"
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        // Guard compiles to a condition + JumpIfFalse
        assert!(has_op(&f.chunk, Op::Gt));
        assert!(has_op(&f.chunk, Op::JumpIfFalse));
    }

    // ── Pipe compilation ───────────────────────────────────────────

    #[test]
    fn test_compile_pipe_to_function() {
        let fns = compile("fn double(x) { x * 2 }\nfn main() { 5 |> double }");
        let main = find_fn(&fns, "main");
        // Pipe in tail position emits TailCall
        assert!(has_op(&main.chunk, Op::TailCall));
    }

    #[test]
    fn test_compile_pipe_to_builtin() {
        let fns = compile(
            r#"
import list
fn main() { [3, 1, 2] |> list.length() }
"#,
        );
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::CallBuiltin));
    }

    // ── Loop/Recur compilation ─────────────────────────────────────

    #[test]
    fn test_compile_loop_recur() {
        let fns = compile(
            r#"
fn main() {
    loop i = 0 {
        match i >= 10 {
            true -> i
            false -> loop(i + 1)
        }
    }
}
"#,
        );
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::Recur));
        assert!(has_op(&main.chunk, Op::JumpBack));
    }

    // ── Question mark ──────────────────────────────────────────────

    #[test]
    fn test_compile_question_mark() {
        let fns = compile("fn f(x) { x? }");
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::QuestionMark));
    }

    // ── Return statement ───────────────────────────────────────────

    #[test]
    fn test_compile_explicit_return() {
        let fns = compile("fn f(x) { return 42 }");
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::Return));
        assert!(has_int_constant(&f.chunk, 42));
    }

    #[test]
    fn test_compile_return_unit() {
        let fns = compile("fn f() { return }");
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::Unit));
        assert!(has_op(&f.chunk, Op::Return));
    }

    // ── Blocks ─────────────────────────────────────────────────────

    #[test]
    fn test_compile_empty_block() {
        let fns = compile("fn f() { { } }");
        let f = find_fn(&fns, "f");
        // Empty block evaluates to Unit
        assert!(has_op(&f.chunk, Op::Unit));
    }

    #[test]
    fn test_compile_block_with_let() {
        let fns = compile(
            r#"
fn f() {
    let x = 1
    let y = 2
    x + y
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::SetLocal));
        assert!(has_op(&f.chunk, Op::GetLocal));
        assert!(has_op(&f.chunk, Op::Add));
    }

    // ── Type ascription ────────────────────────────────────────────

    #[test]
    fn test_compile_ascription_is_transparent() {
        // Ascription compiles to just the inner expression
        let fns = compile("fn f() { 42 as Int }");
        let f = find_fn(&fns, "f");
        assert!(has_int_constant(&f.chunk, 42));
        assert!(has_op(&f.chunk, Op::Constant));
    }

    // ── Trait impl compilation ─────────────────────────────────────

    #[test]
    fn test_compile_trait_impl() {
        let fns = compile(
            r#"
type Color { Red, Green, Blue }
trait Display for Color {
    fn display(self) -> String {
        "color"
    }
}
"#,
        );
        let script = &fns[0];
        // Trait method registered as "Color.display" global
        assert!(has_string_constant(&script.chunk, "Color.display"));
    }

    // ── Import gating ──────────────────────────────────────────────

    #[test]
    fn test_import_gating_error() {
        // Using a module without importing should error
        let err = compile_err(
            r#"
fn main() {
    list.length([1, 2])
}
"#,
        );
        assert!(
            err.message.contains("not imported"),
            "expected import error, got: {}",
            err.message
        );
    }

    #[test]
    fn test_import_gating_success() {
        // With import, should compile fine
        let result = compile_no_imports(
            r#"
import list
fn main() {
    list.length([1, 2])
}
"#,
        );
        assert!(result.is_ok());
    }

    // ── Builtin module calls ───────────────────────────────────────

    #[test]
    fn test_compile_builtin_call() {
        let fns = compile(
            r#"
import list
fn main() { list.length([1, 2, 3]) }
"#,
        );
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::CallBuiltin));
        assert!(has_string_constant(&main.chunk, "list.length"));
    }

    // ── Method call compilation ────────────────────────────────────

    #[test]
    fn test_compile_method_call() {
        let fns = compile(
            r#"
type Foo { x: Int }
trait Display for Foo {
    fn display(self) -> String { "foo" }
}
fn main() {
    let f = Foo { x: 1 }
    f.display()
}
"#,
        );
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::CallMethod));
    }

    // ── Tuple index access ─────────────────────────────────────────

    #[test]
    fn test_compile_tuple_index() {
        let fns = compile("fn main() { let t = (1, 2)\n t.0 }");
        let main = find_fn(&fns, "main");
        assert!(has_op(&main.chunk, Op::GetIndex));
    }

    // ── compile_program vs compile_declarations ────────────────────

    #[test]
    fn test_compile_program_calls_main() {
        let tokens = Lexer::new("fn main() { 42 }").tokenize().unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut compiler = Compiler::new();
        let fns = compiler.compile_program(&program).unwrap();
        let script = &fns[0];
        // compile_program emits GetGlobal "main", Call 0, Return
        assert!(has_string_constant(&script.chunk, "main"));
        assert!(has_op(&script.chunk, Op::Call));
    }

    #[test]
    fn test_compile_declarations_returns_unit() {
        let fns = compile("fn main() { 42 }");
        let script = &fns[0];
        // compile_declarations emits Unit, Return (no main call)
        assert!(has_op(&script.chunk, Op::Unit));
        assert!(has_op(&script.chunk, Op::Return));
    }

    // ── Warnings ───────────────────────────────────────────────────

    #[test]
    fn test_shadow_module_warning() {
        let tokens = Lexer::new("fn main() { let list = 42\n list }")
            .tokenize()
            .unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut compiler = Compiler::new();
        compiler.import_all_builtins();
        compiler.compile_declarations(&program).unwrap();
        assert!(
            compiler
                .warnings()
                .iter()
                .any(|w| w.message.contains("shadows")),
            "expected shadow warning"
        );
    }

    // ── Selective import compilation ────────────────────────────────

    #[test]
    fn test_compile_selective_import() {
        let result = compile_no_imports(
            r#"
import list.{ length, map }
fn main() { length([1, 2]) }
"#,
        );
        assert!(result.is_ok());
        let fns = result.unwrap();
        let script = &fns[0];
        // Selective import creates aliases: "length" -> "list.length"
        assert!(has_string_constant(&script.chunk, "list.length"));
        assert!(has_string_constant(&script.chunk, "length"));
    }

    #[test]
    fn test_compile_aliased_import() {
        let result = compile_no_imports(
            r#"
import list as l
fn main() { l.length([1]) }
"#,
        );
        assert!(result.is_ok());
    }

    // ── Pattern destructuring in function params ───────────────────

    #[test]
    fn test_compile_destructured_lambda_param() {
        let fns = compile(
            r#"
import list
fn main() {
    let pairs = [(1, 2)]
    list.map(pairs) { (a, b) -> a + b }
}
"#,
        );
        let main = find_fn(&fns, "main");
        // Lambda with destructured param is a VmClosure constant
        let lambda = main.chunk.constants.iter().find_map(|c| {
            if let Value::VmClosure(cl) = c {
                if cl.function.name == "<lambda>" {
                    return Some(&cl.function);
                }
            }
            None
        });
        assert!(lambda.is_some(), "expected lambda in main's constants");
        let lambda = lambda.unwrap();
        assert!(has_op(&lambda.chunk, Op::DestructTuple));
    }

    // ── Map pattern in match ───────────────────────────────────────

    #[test]
    fn test_compile_match_map_pattern() {
        let fns = compile(
            r#"
fn f(m) {
    match m {
        #{ "key": v } -> v
        _ -> "default"
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TestMapHasKey));
    }

    // ── When statement compilation ─────────────────────────────────

    #[test]
    fn test_compile_when_pattern() {
        let fns = compile(
            r#"
fn f(x) {
    when Some(v) = x else { return 0 }
    v
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::TestTag));
    }

    #[test]
    fn test_compile_when_bool() {
        let fns = compile(
            r#"
fn f(x) {
    when x > 0 else { return 0 }
    x
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(&f.chunk, Op::JumpIfFalse));
    }

    // ── Recur outside loop is an error ─────────────────────────────

    #[test]
    fn test_compile_recur_outside_loop() {
        let err = compile_err("fn f() { loop(1) }");
        assert!(err.message.contains("recur outside of loop"));
    }

    // ── Record field metadata ──────────────────────────────────────

    #[test]
    fn test_compile_record_field_metadata() {
        let fns = compile("type User { name: String, age: Int }");
        let script = &fns[0];
        // Record field metadata is stored as __record_fields__User
        assert!(has_string_constant(&script.chunk, "__record_fields__User"));
    }

    // ── Or-pattern in match ────────────────────────────────────────

    #[test]
    fn test_compile_or_pattern() {
        let fns = compile(
            r#"
fn f(x) {
    match x {
        1 | 2 | 3 -> "small"
        _ -> "big"
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        // Or-pattern has multiple TestEqual ops
        let test_count = f
            .chunk
            .code
            .iter()
            .filter(|&&b| b == Op::TestEqual as u8)
            .count();
        assert!(
            test_count >= 3,
            "expected at least 3 TestEqual ops for or-pattern, got {test_count}"
        );
    }

    // ── Pin pattern in match ───────────────────────────────────────

    #[test]
    fn test_compile_pin_pattern() {
        let fns = compile(
            r#"
fn f(expected, actual) {
    match actual {
        ^expected -> true
        _ -> false
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        // Pin pattern uses Dup + GetLocal + Eq
        assert!(has_op(&f.chunk, Op::Dup));
        assert!(has_op(&f.chunk, Op::Eq));
    }
}
