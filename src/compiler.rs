//! AST-to-bytecode compiler for Silt.
//!
//! Walks the AST and emits stack-based bytecode into `Function` objects.
//! Phase 4: full pattern matching compilation for all pattern types,
//! including nested/recursive patterns, or-patterns, guards, ranges,
//! list/tuple/record/map destructuring, pin patterns, when/else,
//! plus all previous features (closures, upvalues, pipes, lambdas).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::ast::{
    BinOp, Decl, Expr, ExprKind, ImportTarget, ListElem, MatchArm, Pattern, Program, Stmt,
    StringPart, TypeExpr, UnaryOp,
};
use crate::bytecode::{Chunk, Function, Op, UpvalueDesc, VmClosure};
use crate::lexer::{Lexer, Span};
use crate::module;
use crate::parser::Parser;
use crate::value::Value;

// ── Type encoding for record field metadata ─────────────────────────

/// Encode a TypeExpr as a compact string for runtime JSON parsing.
/// Examples: "String", "Int", "List:String", "Option:Int", "Record:Address"
fn encode_type_expr(te: &TypeExpr) -> String {
    match te {
        TypeExpr::Named(n) => match n.as_str() {
            "Int" | "Float" | "String" | "Bool" | "Date" | "Time" | "DateTime" => n.clone(),
            _ if n.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) => {
                format!("Record:{n}")
            }
            _ => "String".to_string(),
        },
        TypeExpr::Generic(name, args) => match name.as_str() {
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
    RecordField(String),
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
    name: String,
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
        }
    }

    /// Returns warnings emitted during compilation.
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }

    /// Mark all builtin modules as imported (used by the REPL).
    pub fn import_all_builtins(&mut self) {
        for name in &[
            "io", "string", "int", "float", "list", "map", "result", "option", "test", "channel",
            "task", "regex", "json", "set", "math",
        ] {
            self.imported_builtin_modules.insert(name.to_string());
        }
    }

    // ── Public entry point ────────────────────────────────────────

    /// Compile a full program, returning all functions.
    ///
    /// The first function in the returned `Vec` is the top-level `<script>`,
    /// which ends with `GetGlobal "main" ; Call 0 ; Return`.
    pub fn compile_program(&mut self, program: &Program) -> Result<Vec<Function>, String> {
        // Push a top-level script context.
        self.contexts
            .push(CompileContext::new("<script>".into(), 0));

        for decl in &program.decls {
            self.compile_decl(decl)?;
        }

        // Emit: GetGlobal "main", Call 0, Return
        let span = Span::new(0, 0);
        let name_idx = self
            .current_chunk()
            .add_constant(Value::String("main".into()));
        self.current_chunk().emit_op(Op::GetGlobal, span);
        self.current_chunk().emit_u16(name_idx, span);
        self.current_chunk().emit_op(Op::Call, span);
        self.current_chunk().emit_u8(0, span);
        self.current_chunk().emit_op(Op::Return, span);

        let script = self
            .contexts
            .pop()
            .ok_or("compiler bug: missing script context")?
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
    pub fn compile_declarations(&mut self, program: &Program) -> Result<Vec<Function>, String> {
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
            .ok_or("compiler bug: missing script context")?
            .function;
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

                // Add parameters as locals. Each parameter occupies one slot initially.
                // For non-Ident patterns, we use a hidden name and destructure after.
                let mut param_slots = Vec::new();
                for (i, param) in fn_decl.params.iter().enumerate() {
                    match &param.pattern {
                        Pattern::Ident(name) => {
                            self.warn_if_shadows_module(name, span);
                            self.add_local(name.clone());
                            param_slots.push((i, None)); // no destructuring needed
                        }
                        _ => {
                            let slot = self.add_local(format!("__param_{i}__"));
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
                        let _hidden = self.add_local("__param_copy__".into());
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
                let ctx = self
                    .contexts
                    .pop()
                    .ok_or("compiler bug: missing function context")?;
                let func = ctx.function;

                // Store the function as a VmClosure constant in the enclosing chunk.
                let vm_closure = Arc::new(VmClosure {
                    function: Arc::new(func),
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
                        let name_idx = self
                            .current_chunk()
                            .add_constant(Value::String(name.clone()));
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

            Decl::Type(type_decl) => {
                let span = type_decl.span;
                match &type_decl.body {
                    crate::ast::TypeBody::Enum(variants) => {
                        for variant in variants {
                            let name = &variant.name;
                            let arity = variant.fields.len();
                            if arity == 0 {
                                // Nullary variant: register as a Variant value
                                let val = Value::Variant(name.clone(), Vec::new());
                                let val_idx = self.current_chunk().add_constant(val);
                                self.current_chunk().emit_op(Op::Constant, span);
                                self.current_chunk().emit_u16(val_idx, span);
                            } else {
                                // Variant constructor
                                let val = Value::VariantConstructor(name.clone(), arity);
                                let val_idx = self.current_chunk().add_constant(val);
                                self.current_chunk().emit_op(Op::Constant, span);
                                self.current_chunk().emit_u16(val_idx, span);
                            }
                            let name_idx = self
                                .current_chunk()
                                .add_constant(Value::String(name.clone()));
                            self.current_chunk().emit_op(Op::SetGlobal, span);
                            self.current_chunk().emit_u16(name_idx, span);
                            self.current_chunk().emit_op(Op::Pop, span);

                            // Register variant -> type mapping for method dispatch.
                            let mapping_key = format!("__type_of__{name}");
                            let key_idx = self
                                .current_chunk()
                                .add_constant(Value::String(mapping_key));
                            let type_val_idx = self
                                .current_chunk()
                                .add_constant(Value::String(type_decl.name.clone()));
                            self.current_chunk().emit_op(Op::Constant, span);
                            self.current_chunk().emit_u16(type_val_idx, span);
                            self.current_chunk().emit_op(Op::SetGlobal, span);
                            self.current_chunk().emit_u16(key_idx, span);
                            self.current_chunk().emit_op(Op::Pop, span);
                        }
                    }
                    crate::ast::TypeBody::Record(fields) => {
                        // Register the record type name as a RecordDescriptor global.
                        let val = Value::RecordDescriptor(type_decl.name.clone());
                        let val_idx = self.current_chunk().add_constant(val);
                        self.current_chunk().emit_op(Op::Constant, span);
                        self.current_chunk().emit_u16(val_idx, span);
                        let name_idx = self
                            .current_chunk()
                            .add_constant(Value::String(type_decl.name.clone()));
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);

                        // Emit record field metadata as a global list for json module.
                        // Format: list of alternating [field_name, type_encoding, ...]
                        let field_count = fields.len();
                        for f in fields {
                            let fname = self
                                .current_chunk()
                                .add_constant(Value::String(f.name.clone()));
                            self.current_chunk().emit_op(Op::Constant, span);
                            self.current_chunk().emit_u16(fname, span);
                            let ftype = self
                                .current_chunk()
                                .add_constant(Value::String(encode_type_expr(&f.ty)));
                            self.current_chunk().emit_op(Op::Constant, span);
                            self.current_chunk().emit_u16(ftype, span);
                        }
                        self.current_chunk().emit_op(Op::MakeList, span);
                        self.current_chunk()
                            .emit_u16((field_count * 2) as u16, span);
                        let meta_key = self.current_chunk().add_constant(Value::String(format!(
                            "__record_fields__{}",
                            type_decl.name
                        )));
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
                                self.warn_if_shadows_module(name, span);
                                self.add_local(name.clone());
                            }
                            _ => {
                                let slot = self.add_local(format!("__param_{i}__"));
                                self.current_chunk().emit_op(Op::GetLocal, span);
                                self.current_chunk().emit_u16(slot, span);
                                let _hidden = self.add_local("__param_copy__".into());
                                self.current_chunk().emit_op(Op::SetLocal, span);
                                self.current_chunk().emit_u16(_hidden, span);
                                self.compile_pattern_bind(&param.pattern, span)?;
                            }
                        }
                    }

                    self.compile_expr(&method.body)?;
                    self.current_chunk().emit_op(Op::Return, span);

                    let ctx = self
                        .contexts
                        .pop()
                        .ok_or("compiler bug: missing trait method context")?;
                    let func = ctx.function;
                    let vm_closure = Arc::new(VmClosure {
                        function: Arc::new(func),
                        upvalues: vec![],
                    });
                    let closure_val = Value::VmClosure(vm_closure);
                    let fi = self.current_chunk().add_constant(closure_val);
                    self.current_chunk().emit_op(Op::Constant, span);
                    self.current_chunk().emit_u16(fi, span);

                    let name_idx = self
                        .current_chunk()
                        .add_constant(Value::String(qualified_name));
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

            Decl::Import(target) => self.compile_import(target),
        }
    }

    // ── Import compilation ─────────────────────────────────────────

    fn compile_import(&mut self, target: &ImportTarget) -> Result<(), String> {
        match target {
            ImportTarget::Module(name) => {
                // Builtin modules (io, string, list, ...) are already registered
                // in the VM's global table. Record the import for gating.
                if module::is_builtin_module(name) {
                    self.imported_builtin_modules.insert(name.clone());
                    return Ok(());
                }
                self.compile_file_module(name)?;
                Ok(())
            }
            ImportTarget::Items(module_name, items) => {
                if module::is_builtin_module(module_name) {
                    self.imported_builtin_modules.insert(module_name.clone());
                    // For builtin modules, create aliases: bare "item" -> "module.item"
                    let span = Span::new(0, 0);
                    for item in items {
                        let qualified = format!("{module_name}.{item}");
                        let qi = self.current_chunk().add_constant(Value::String(qualified));
                        self.current_chunk().emit_op(Op::GetGlobal, span);
                        self.current_chunk().emit_u16(qi, span);
                        let bare_i = self
                            .current_chunk()
                            .add_constant(Value::String(item.clone()));
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(bare_i, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    return Ok(());
                }
                // File-based selective import: compile the module, then alias
                // "module.item" -> bare "item" for each selected name.
                self.compile_file_module(module_name)?;
                let span = Span::new(0, 0);
                for item in items {
                    let qualified = format!("{module_name}.{item}");
                    let qi = self.current_chunk().add_constant(Value::String(qualified));
                    self.current_chunk().emit_op(Op::GetGlobal, span);
                    self.current_chunk().emit_u16(qi, span);
                    let bare_i = self
                        .current_chunk()
                        .add_constant(Value::String(item.clone()));
                    self.current_chunk().emit_op(Op::SetGlobal, span);
                    self.current_chunk().emit_u16(bare_i, span);
                    self.current_chunk().emit_op(Op::Pop, span);
                }
                Ok(())
            }
            ImportTarget::Alias(module_name, alias) => {
                if module::is_builtin_module(module_name) {
                    self.imported_builtin_modules.insert(module_name.clone());
                    // Builtin alias: copy all "module.func" globals to "alias.func".
                    let span = Span::new(0, 0);
                    for func in module::builtin_module_functions(module_name) {
                        let qualified = format!("{module_name}.{func}");
                        let qi = self.current_chunk().add_constant(Value::String(qualified));
                        self.current_chunk().emit_op(Op::GetGlobal, span);
                        self.current_chunk().emit_u16(qi, span);
                        let alias_name = format!("{alias}.{func}");
                        let ai = self.current_chunk().add_constant(Value::String(alias_name));
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(ai, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    return Ok(());
                }
                // File module with alias: compile under original name, then
                // re-register each public declaration under the alias prefix.
                let public_names = self.compile_file_module(module_name)?;
                let span = Span::new(0, 0);
                for name in &public_names {
                    let original = format!("{module_name}.{name}");
                    let qi = self.current_chunk().add_constant(Value::String(original));
                    self.current_chunk().emit_op(Op::GetGlobal, span);
                    self.current_chunk().emit_u16(qi, span);
                    let alias_name = format!("{alias}.{name}");
                    let ai = self.current_chunk().add_constant(Value::String(alias_name));
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
    fn compile_file_module(&mut self, module_name: &str) -> Result<Vec<String>, String> {
        // Guard against double-compilation.
        if self.compiled_modules.contains(module_name) {
            return Ok(vec![]);
        }

        // Detect circular imports.
        if self.compiling_modules.contains(module_name) {
            return Err(format!(
                "circular import detected: module '{module_name}' imports itself (directly or indirectly)"
            ));
        }
        self.compiling_modules.insert(module_name.to_string());

        let result = self.compile_file_module_inner(module_name);

        self.compiling_modules.remove(module_name);
        if result.is_ok() {
            self.compiled_modules.insert(module_name.to_string());
        }
        result
    }

    /// Inner implementation of file module compilation, separated so that
    /// the circular-import guard can wrap it cleanly.
    fn compile_file_module_inner(&mut self, module_name: &str) -> Result<Vec<String>, String> {
        let project_root = self.project_root.as_ref().ok_or_else(|| {
            format!(
                "cannot import module '{module_name}': no project root set (use Compiler::with_project_root)"
            )
        })?;

        // Resolve, read, lex, parse the module file.
        let file_path = project_root.join(format!("{module_name}.silt"));
        let source = std::fs::read_to_string(&file_path)
            .map_err(|e| format!("cannot load module '{module_name}': {e}"))?;

        let tokens = Lexer::new(&source)
            .tokenize()
            .map_err(|e| format!("module '{module_name}': lex error: {e}"))?;

        let program = Parser::new(tokens)
            .parse_program()
            .map_err(|e| format!("module '{module_name}': parse error: {e}"))?;

        // Collect public names so we know which to export.
        let mut public_fns = HashSet::new();
        let mut public_types = HashSet::new();
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) if f.is_pub => {
                    public_fns.insert(f.name.clone());
                }
                Decl::Type(t) if t.is_pub => {
                    public_types.insert(t.name.clone());
                }
                _ => {}
            }
        }

        // Track all exported names (functions + types + variants) for alias support.
        let mut exported_names: Vec<String> = Vec::new();

        // Compile each declaration. Functions get registered as
        // "module_name.fn_name" for public ones, or just compiled (for
        // internal helpers that closures might reference).
        let span = Span::new(0, 0);
        for decl in &program.decls {
            match decl {
                Decl::Fn(fn_decl) => {
                    let arity = fn_decl.params.len() as u8;
                    let fn_span = fn_decl.span;

                    self.contexts
                        .push(CompileContext::new(fn_decl.name.clone(), arity));

                    // Add parameters as locals.
                    let mut param_slots = Vec::new();
                    for (i, param) in fn_decl.params.iter().enumerate() {
                        match &param.pattern {
                            Pattern::Ident(name) => {
                                self.warn_if_shadows_module(name, fn_span);
                                self.add_local(name.clone());
                                param_slots.push((i, None));
                            }
                            _ => {
                                let slot = self.add_local(format!("__param_{i}__"));
                                param_slots.push((i, Some((slot, param.pattern.clone()))));
                            }
                        }
                    }
                    for (_i, maybe_destruct) in &param_slots {
                        if let Some((slot, pattern)) = maybe_destruct {
                            self.current_chunk().emit_op(Op::GetLocal, fn_span);
                            self.current_chunk().emit_u16(*slot, fn_span);
                            let _hidden = self.add_local("__param_copy__".into());
                            self.current_chunk().emit_op(Op::SetLocal, fn_span);
                            self.current_chunk().emit_u16(_hidden, fn_span);
                            self.compile_pattern_bind(pattern, fn_span)?;
                        }
                    }

                    self.compile_expr(&fn_decl.body)?;
                    self.current_chunk().emit_op(Op::Return, fn_span);

                    let ctx = self
                        .contexts
                        .pop()
                        .ok_or("compiler bug: missing module function context")?;
                    let func = ctx.function;

                    let vm_closure = Arc::new(VmClosure {
                        function: Arc::new(func),
                        upvalues: vec![],
                    });
                    let closure_val = Value::VmClosure(vm_closure);
                    let fi = self.current_chunk().add_constant(closure_val);
                    self.current_chunk().emit_op(Op::Constant, span);
                    self.current_chunk().emit_u16(fi, span);

                    if public_fns.contains(&fn_decl.name) {
                        // Register as "module_name.fn_name"
                        let qualified = format!("{module_name}.{}", fn_decl.name);
                        let name_idx = self.current_chunk().add_constant(Value::String(qualified));
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                        exported_names.push(fn_decl.name.clone());
                    } else {
                        // Internal function — still register so closures / calls work,
                        // but under a mangled private name.
                        let private_name = format!("__{module_name}__{}", fn_decl.name);
                        let name_idx = self
                            .current_chunk()
                            .add_constant(Value::String(private_name));
                        self.current_chunk().emit_op(Op::SetGlobal, span);
                        self.current_chunk().emit_u16(name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                }
                Decl::Type(type_decl) if public_types.contains(&type_decl.name) => {
                    // Compile the type declaration — registers variants under bare names.
                    self.compile_decl(decl)?;
                    // Also register type name and variants under qualified names.
                    exported_names.push(type_decl.name.clone());
                    match &type_decl.body {
                        crate::ast::TypeBody::Enum(variants) => {
                            for variant in variants {
                                // Copy bare "VariantName" -> "module.VariantName"
                                let bare_idx = self
                                    .current_chunk()
                                    .add_constant(Value::String(variant.name.clone()));
                                self.current_chunk().emit_op(Op::GetGlobal, span);
                                self.current_chunk().emit_u16(bare_idx, span);
                                let qual = format!("{module_name}.{}", variant.name);
                                let qual_idx =
                                    self.current_chunk().add_constant(Value::String(qual));
                                self.current_chunk().emit_op(Op::SetGlobal, span);
                                self.current_chunk().emit_u16(qual_idx, span);
                                self.current_chunk().emit_op(Op::Pop, span);
                                exported_names.push(variant.name.clone());
                            }
                            // Register the type name itself as a qualified global
                            // (pointing to the type name string for use in `import mod.{ Type }`).
                            let type_val = Value::String(type_decl.name.clone());
                            let type_val_idx = self.current_chunk().add_constant(type_val);
                            self.current_chunk().emit_op(Op::Constant, span);
                            self.current_chunk().emit_u16(type_val_idx, span);
                            let qual_type = format!("{module_name}.{}", type_decl.name);
                            let qual_type_idx =
                                self.current_chunk().add_constant(Value::String(qual_type));
                            self.current_chunk().emit_op(Op::SetGlobal, span);
                            self.current_chunk().emit_u16(qual_type_idx, span);
                            self.current_chunk().emit_op(Op::Pop, span);
                        }
                        crate::ast::TypeBody::Record(_) => {
                            // Copy bare type name -> "module.TypeName"
                            let bare_idx = self
                                .current_chunk()
                                .add_constant(Value::String(type_decl.name.clone()));
                            self.current_chunk().emit_op(Op::GetGlobal, span);
                            self.current_chunk().emit_u16(bare_idx, span);
                            let qual = format!("{module_name}.{}", type_decl.name);
                            let qual_idx = self.current_chunk().add_constant(Value::String(qual));
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
                Decl::Import(_) => {
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
        Ok(exported_names)
    }

    // ── Statements ────────────────────────────────────────────────

    fn compile_stmt(&mut self, stmt: &Stmt, is_last: bool) -> Result<(), String> {
        match stmt {
            Stmt::Let { pattern, value, .. } => {
                self.compile_expr(value)?;
                let span = value.span;

                match pattern {
                    Pattern::Ident(name) => {
                        self.warn_if_shadows_module(name, span);
                        let slot = self.add_local(name.clone());
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
                        let _val_slot = self.add_local("__let_val__".into());
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
                self.current_chunk().patch_jump(else_jump);
                self.compile_expr(else_body)?;
                // The else body must diverge (return or panic).
                // If it doesn't, we just pop its value and continue.
                self.current_chunk().emit_op(Op::Pop, condition.span);

                self.current_chunk().patch_jump(end_jump);

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
                    self.current_chunk().patch_jump(fj);
                }
                self.current_chunk().emit_op(Op::Pop, span); // pop scrutinee
                self.compile_expr(else_body)?;
                self.current_chunk().emit_op(Op::Pop, span); // pop else result

                self.current_chunk().patch_jump(end_jump);

                if is_last {
                    self.current_chunk().emit_op(Op::Unit, span);
                }
                Ok(())
            }
        }
    }

    // ── Expressions ───────────────────────────────────────────────

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), String> {
        let span = expr.span;
        let tail = self.in_tail_position;
        self.in_tail_position = false;

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
                        self.current_chunk().patch_jump(jump);
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
                        self.current_chunk().patch_jump(jump);
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
                if let Some(slot) = self.resolve_local(name) {
                    self.current_chunk().emit_op(Op::GetLocal, span);
                    self.current_chunk().emit_u16(slot, span);
                } else if let Some(idx) = self.resolve_upvalue(name) {
                    self.current_chunk().emit_op(Op::GetUpvalue, span);
                    self.current_chunk().emit_u8(idx, span);
                } else {
                    // Gate constructors that require module imports
                    if let Some(required) = module::gated_constructor_module(name)
                        && !self.imported_builtin_modules.contains(required)
                    {
                        return Err(format!("'{name}' requires `import {required}`"));
                    }
                    let name_idx = self
                        .current_chunk()
                        .add_constant(Value::String(name.clone()));
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
                    let name_idx = self
                        .current_chunk()
                        .add_constant(Value::String(builtin_name));
                    self.current_chunk().emit_op(Op::CallBuiltin, span);
                    self.current_chunk().emit_u16(name_idx, span);
                    self.current_chunk().emit_u8(argc, span);
                } else if let ExprKind::FieldAccess(receiver, method) = &callee.kind {
                    // Check if this is a module-qualified call on a non-local ident
                    let is_module_call = if let ExprKind::Ident(name) = &receiver.kind {
                        self.resolve_local(name).is_none()
                            && self.resolve_upvalue_peek(name).is_none()
                    } else {
                        false
                    };
                    if is_module_call {
                        if let ExprKind::Ident(module) = &receiver.kind {
                            // Gate: require import for builtin modules
                            if module::is_builtin_module(module)
                                && !self.imported_builtin_modules.contains(module.as_str())
                            {
                                return Err(format!(
                                    "module '{module}' is not imported; add `import {module}` at the top of the file"
                                ));
                            }
                            // Module-qualified call on a global module name.
                            let qualified = format!("{module}.{method}");
                            let name_idx =
                                self.current_chunk().add_constant(Value::String(qualified));
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
                        let method_idx = self
                            .current_chunk()
                            .add_constant(Value::String(method.clone()));
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
                    let is_local =
                        self.resolve_local(name).is_some() || self.resolve_upvalue(name).is_some();
                    if !is_local {
                        // Gate: require import for builtin modules
                        if module::is_builtin_module(name)
                            && !self.imported_builtin_modules.contains(name.as_str())
                        {
                            return Err(format!(
                                "module '{name}' is not imported; add `import {name}` at the top of the file"
                            ));
                        }
                        // Module-qualified global: list.map, string.length, etc.
                        let qualified = format!("{name}.{field}");
                        let name_idx = self.current_chunk().add_constant(Value::String(qualified));
                        self.current_chunk().emit_op(Op::GetGlobal, span);
                        self.current_chunk().emit_u16(name_idx, span);
                        return Ok(());
                    }
                }
                if let Ok(index) = field.parse::<u8>() {
                    // Tuple index access: expr.0, expr.1, etc.
                    self.compile_expr(expr)?;
                    self.current_chunk().emit_op(Op::GetIndex, span);
                    self.current_chunk().emit_u8(index, span);
                } else {
                    // Compile the expression and access field
                    self.compile_expr(expr)?;
                    let name_idx = self
                        .current_chunk()
                        .add_constant(Value::String(field.clone()));
                    self.current_chunk().emit_op(Op::GetField, span);
                    self.current_chunk().emit_u16(name_idx, span);
                }
            }

            ExprKind::StringInterp(parts) => {
                let mut count: u8 = 0;
                for part in parts {
                    match part {
                        StringPart::Literal(s) => {
                            let idx = self.current_chunk().add_constant(Value::String(s.clone()));
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
                            self.warn_if_shadows_module(name, span);
                            self.add_local(name.clone());
                            lambda_param_slots.push(None);
                        }
                        _ => {
                            let slot = self.add_local(format!("__param_{i}__"));
                            lambda_param_slots.push(Some((slot, param.pattern.clone())));
                        }
                    }
                }

                // Emit destructuring for non-Ident lambda parameter patterns.
                for (slot, pattern) in lambda_param_slots.iter().flatten() {
                    self.current_chunk().emit_op(Op::GetLocal, span);
                    self.current_chunk().emit_u16(*slot, span);
                    let _hidden = self.add_local("__param_copy__".into());
                    self.current_chunk().emit_op(Op::SetLocal, span);
                    self.current_chunk().emit_u16(_hidden, span);
                    self.compile_pattern_bind(pattern, span)?;
                }

                // Compile the lambda body in tail position for TCO.
                self.in_tail_position = true;
                self.compile_expr(body)?;
                self.in_tail_position = false;
                self.current_chunk().emit_op(Op::Return, span);

                let ctx = self
                    .contexts
                    .pop()
                    .ok_or("compiler bug: missing lambda context")?;
                let upvalue_descs = ctx.upvalues.clone();
                let func = ctx.function;

                let vm_closure = Arc::new(VmClosure {
                    function: Arc::new(func),
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
                self.compile_pipe(left, right, span)?;
            }

            ExprKind::QuestionMark(inner) => {
                self.compile_expr(inner)?;
                self.current_chunk().emit_op(Op::QuestionMark, span);
            }

            ExprKind::Ascription(inner, _) => {
                self.compile_expr(inner)?;
            }

            ExprKind::RecordCreate { name, fields } => {
                // Push field values in order
                let field_names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
                for (_, val) in fields {
                    self.compile_expr(val)?;
                }
                let type_name_idx = self
                    .current_chunk()
                    .add_constant(Value::String(name.clone()));
                self.current_chunk().emit_op(Op::MakeRecord, span);
                self.current_chunk().emit_u16(type_name_idx, span);
                self.current_chunk().emit_u8(field_names.len() as u8, span);
                for fname in &field_names {
                    let field_idx = self
                        .current_chunk()
                        .add_constant(Value::String(fname.clone()));
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
                    let field_idx = self
                        .current_chunk()
                        .add_constant(Value::String(fname.clone()));
                    self.current_chunk().emit_u16(field_idx, span);
                }
            }

            ExprKind::Loop { bindings, body } => {
                self.compile_loop(bindings, body, span)?;
            }

            ExprKind::Recur(args) => {
                let loop_info = self
                    .ctx()
                    .loop_stack
                    .last()
                    .ok_or_else(|| "recur outside of loop".to_string())?;
                let first_slot = loop_info.first_slot;
                let loop_start = loop_info.loop_start;
                let expected = loop_info.binding_count as usize;
                if args.len() != expected {
                    return Err(format!(
                        "loop() expects {} argument(s), got {}",
                        expected,
                        args.len()
                    ));
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
    ) -> Result<(), String> {
        // ── Guardless match (no scrutinee) ───────────────────────
        let Some(scrutinee) = scrutinee else {
            return self.compile_guardless_match(arms, span, tail);
        };

        // Compile the scrutinee and save it in a known local slot.
        // This lets us GetLocal it for each arm's test and binding.
        self.compile_expr(scrutinee)?;
        self.begin_scope();
        let scrutinee_slot = self.add_local("__scrutinee__".into());
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
            let _bind_copy = self.add_local("__bind_src__".into());
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
                self.current_chunk().patch_jump(gj);
            }
            for fj in fail_jumps {
                self.current_chunk().patch_jump(fj);
            }
        }

        // No arm matched — panic
        let msg_idx = self
            .current_chunk()
            .add_constant(Value::String("non-exhaustive match: no arm matched".into()));
        self.current_chunk().emit_op(Op::Constant, span);
        self.current_chunk().emit_u16(msg_idx, span);
        self.current_chunk().emit_op(Op::Panic, span);

        self.end_scope(span);

        // Patch all end jumps to here
        for ej in end_jumps {
            self.current_chunk().patch_jump(ej);
        }

        Ok(())
    }

    /// Compile a guardless match: `match { cond1 -> body1, ... }`
    fn compile_guardless_match(
        &mut self,
        arms: &[MatchArm],
        span: Span,
        tail: bool,
    ) -> Result<(), String> {
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

                self.current_chunk().patch_jump(fail_jump);
            } else {
                // Wildcard / default arm — always matches
                self.in_tail_position = tail;
                self.compile_expr(&arm.body)?;
                let end_jump = self.current_chunk().emit_jump(Op::Jump, span);
                end_jumps.push(end_jump);
            }
        }

        // No arm matched — panic
        let msg_idx = self.current_chunk().add_constant(Value::String(
            "non-exhaustive match: no condition was true".into(),
        ));
        self.current_chunk().emit_op(Op::Constant, span);
        self.current_chunk().emit_u16(msg_idx, span);
        self.current_chunk().emit_op(Op::Panic, span);

        for ej in end_jumps {
            self.current_chunk().patch_jump(ej);
        }

        Ok(())
    }

    // ── Recursive pattern test ───────────────────────────────────
    //
    // Emit test opcodes for a pattern. The value to test is on TOS
    // (peeked, not consumed). Returns jump-patch addresses for failure.
    // For nested patterns, uses Dup + Destruct to get sub-values.

    fn compile_pattern_test(
        &mut self,
        pattern: &Pattern,
        span: Span,
    ) -> Result<Vec<usize>, String> {
        match pattern {
            Pattern::Wildcard | Pattern::Ident(_) => {
                // Always matches, no test needed
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

            Pattern::Constructor(name, fields) => {
                // Gate constructors that require module imports
                if let Some(required) = module::gated_constructor_module(name)
                    && !self.imported_builtin_modules.contains(required)
                {
                    return Err(format!("'{name}' requires `import {required}`"));
                }
                // Test: tag matches?
                let idx = self
                    .current_chunk()
                    .add_constant(Value::String(name.clone()));
                self.current_chunk().emit_op(Op::TestTag, span);
                self.current_chunk().emit_u16(idx, span);
                let tag_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                let mut all_jumps = vec![tag_jump];

                // Test nested field patterns
                for (i, field_pat) in fields.iter().enumerate() {
                    if !self.pattern_is_irrefutable(field_pat) {
                        // Destructure to get sub-value, test it, then pop
                        self.current_chunk().emit_op(Op::DestructVariant, span);
                        self.current_chunk().emit_u8(i as u8, span);
                        let sub_fails = self.compile_pattern_test(field_pat, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }

            Pattern::Tuple(pats) => {
                // Test length
                self.current_chunk().emit_op(Op::TestTupleLen, span);
                self.current_chunk().emit_u8(pats.len() as u8, span);
                let len_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                let mut all_jumps = vec![len_jump];

                // Test nested element patterns
                for (i, pat) in pats.iter().enumerate() {
                    if !self.pattern_is_irrefutable(pat) {
                        self.current_chunk().emit_op(Op::DestructTuple, span);
                        self.current_chunk().emit_u8(i as u8, span);
                        let sub_fails = self.compile_pattern_test(pat, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }

            Pattern::List(elements, rest) => {
                let elem_count = elements.len() as u8;

                if rest.is_some() {
                    // [h, ..t] — at least elem_count elements
                    self.current_chunk().emit_op(Op::TestListMin, span);
                    self.current_chunk().emit_u8(elem_count, span);
                } else {
                    // [a, b, c] — exactly elem_count elements
                    self.current_chunk().emit_op(Op::TestListExact, span);
                    self.current_chunk().emit_u8(elem_count, span);
                }
                let len_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                let mut all_jumps = vec![len_jump];

                // Test nested element patterns
                for (i, pat) in elements.iter().enumerate() {
                    if !self.pattern_is_irrefutable(pat) {
                        self.current_chunk().emit_op(Op::DestructList, span);
                        self.current_chunk().emit_u8(i as u8, span);
                        let sub_fails = self.compile_pattern_test(pat, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                // Test rest pattern if it's refutable
                if let Some(rest_pat) = rest
                    && !self.pattern_is_irrefutable(rest_pat)
                {
                    self.current_chunk().emit_op(Op::DestructListRest, span);
                    self.current_chunk().emit_u8(elem_count, span);
                    let sub_fails = self.compile_pattern_test(rest_pat, span)?;
                    self.current_chunk().emit_op(Op::Pop, span);
                    all_jumps.extend(sub_fails);
                }

                Ok(all_jumps)
            }

            Pattern::Record { name, fields, .. } => {
                let mut all_jumps = Vec::new();

                // Test tag if present
                if let Some(type_name) = name {
                    let idx = self
                        .current_chunk()
                        .add_constant(Value::String(type_name.clone()));
                    self.current_chunk().emit_op(Op::TestRecordTag, span);
                    self.current_chunk().emit_u16(idx, span);
                    let tag_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                    all_jumps.push(tag_jump);
                }

                // Test each field's sub-pattern
                for (field_name, sub_pat) in fields {
                    let sub_pattern = match sub_pat {
                        Some(p) => p,
                        None => continue, // shorthand binding {name} — always matches
                    };
                    if !self.pattern_is_irrefutable(sub_pattern) {
                        let field_idx = self
                            .current_chunk()
                            .add_constant(Value::String(field_name.clone()));
                        self.current_chunk().emit_op(Op::DestructRecordField, span);
                        self.current_chunk().emit_u16(field_idx, span);
                        let sub_fails = self.compile_pattern_test(sub_pattern, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
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

            Pattern::FloatRange(lo, hi) => {
                let lo_idx = self.current_chunk().add_constant(Value::Float(*lo));
                let hi_idx = self.current_chunk().add_constant(Value::Float(*hi));
                self.current_chunk().emit_op(Op::TestFloatRange, span);
                self.current_chunk().emit_u16(lo_idx, span);
                self.current_chunk().emit_u16(hi_idx, span);
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }

            Pattern::Or(alternatives) => {
                // Try each alternative; if any succeeds, jump to success.
                let mut fail_jumps = Vec::new();
                let mut success_jumps = Vec::new();

                for (i, alt) in alternatives.iter().enumerate() {
                    let sub_fails = self.compile_pattern_test(alt, span)?;

                    if i < alternatives.len() - 1 {
                        // Not the last alt: if it matched, jump to success
                        let success = self.current_chunk().emit_jump(Op::Jump, span);
                        success_jumps.push(success);
                        // Patch this alt's failures to try the next
                        for fj in sub_fails {
                            self.current_chunk().patch_jump(fj);
                        }
                    } else {
                        // Last alt: its failures are the overall failures
                        fail_jumps = sub_fails;
                    }
                }

                // Patch all success jumps to here
                for sj in success_jumps {
                    self.current_chunk().patch_jump(sj);
                }

                Ok(fail_jumps)
            }

            Pattern::Pin(name) => {
                // Pin pattern: match against the existing variable's value.
                // TOS = scrutinee (peeked, not consumed).
                // Strategy: Dup scrutinee, push pin value, Eq (pops both), JumpIfFalse.
                // After: scrutinee remains on stack below the bool result.

                // Dup the scrutinee
                self.current_chunk().emit_op(Op::Dup, span);

                // Push the pin value
                if let Some(slot) = self.resolve_local(name) {
                    self.current_chunk().emit_op(Op::GetLocal, span);
                    self.current_chunk().emit_u16(slot, span);
                } else if let Some(idx) = self.resolve_upvalue(name) {
                    self.current_chunk().emit_op(Op::GetUpvalue, span);
                    self.current_chunk().emit_u8(idx, span);
                } else {
                    let name_idx = self
                        .current_chunk()
                        .add_constant(Value::String(name.clone()));
                    self.current_chunk().emit_op(Op::GetGlobal, span);
                    self.current_chunk().emit_u16(name_idx, span);
                }

                // Stack: [... scrutinee, scrutinee_copy, pin_value]
                self.current_chunk().emit_op(Op::Eq, span);
                // Stack: [... scrutinee, bool_result]
                let jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                Ok(vec![jump])
            }

            Pattern::Map(entries) => {
                let mut all_jumps = Vec::new();

                for (key, sub_pat) in entries {
                    // Test if key exists
                    let key_idx = self
                        .current_chunk()
                        .add_constant(Value::String(key.clone()));
                    self.current_chunk().emit_op(Op::TestMapHasKey, span);
                    self.current_chunk().emit_u16(key_idx, span);
                    let key_jump = self.current_chunk().emit_jump(Op::JumpIfFalse, span);
                    all_jumps.push(key_jump);

                    // Test sub-pattern if refutable
                    if !self.pattern_is_irrefutable(sub_pat) {
                        let key_idx2 = self
                            .current_chunk()
                            .add_constant(Value::String(key.clone()));
                        self.current_chunk().emit_op(Op::DestructMapValue, span);
                        self.current_chunk().emit_u16(key_idx2, span);
                        let sub_fails = self.compile_pattern_test(sub_pat, span)?;
                        self.current_chunk().emit_op(Op::Pop, span);
                        all_jumps.extend(sub_fails);
                    }
                }

                Ok(all_jumps)
            }
        }
    }

    // ── Recursive pattern bind ───────────────────────────────────
    //
    // Emit binding opcodes for a pattern after test has succeeded.
    // The value to bind FROM is on TOS.
    //
    // Contract: TOS has the value. After this call, TOS is unchanged
    // (the value is still there). New locals are pushed ABOVE it on
    // the stack via GetLocal + Destruct sequences.
    //
    // Stack layout for compound patterns like (a, b):
    //   Before: [..., tuple]
    //   After:  [..., tuple, tuple_copy(hidden), elem0, a_local,
    //                        tuple_copy2(hidden), elem1, b_local]
    // Where each GetLocal pushes a copy, Destruct pushes the element,
    // and the Ident bind dups it as the named local.

    fn compile_pattern_bind(&mut self, pattern: &Pattern, span: Span) -> Result<(), String> {
        match pattern {
            Pattern::Ident(name) => {
                // Dup the value, the dup'd copy becomes the local's stack slot.
                self.current_chunk().emit_op(Op::Dup, span);
                self.warn_if_shadows_module(name, span);
                let slot = self.add_local(name.clone());
                self.current_chunk().emit_op(Op::SetLocal, span);
                self.current_chunk().emit_u16(slot, span);
            }

            Pattern::Constructor(_, fields) => {
                self.compile_compound_bind(
                    fields
                        .iter()
                        .enumerate()
                        .filter_map(|(i, pat)| {
                            if self.pattern_has_bindings(pat) {
                                Some((BindDestructKind::Variant(i as u8), pat.clone()))
                            } else {
                                None
                            }
                        })
                        .collect(),
                    span,
                )?;
            }

            Pattern::Tuple(pats) => {
                self.compile_compound_bind(
                    pats.iter()
                        .enumerate()
                        .filter_map(|(i, pat)| {
                            if self.pattern_has_bindings(pat) {
                                Some((BindDestructKind::Tuple(i as u8), pat.clone()))
                            } else {
                                None
                            }
                        })
                        .collect(),
                    span,
                )?;
            }

            Pattern::List(elements, rest) => {
                let mut items: Vec<(BindDestructKind, Pattern)> = elements
                    .iter()
                    .enumerate()
                    .filter_map(|(i, pat)| {
                        if self.pattern_has_bindings(pat) {
                            Some((BindDestructKind::List(i as u8), pat.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                if let Some(rest_pat) = rest
                    && self.pattern_has_bindings(rest_pat)
                {
                    items.push((
                        BindDestructKind::ListRest(elements.len() as u8),
                        (**rest_pat).clone(),
                    ));
                }
                self.compile_compound_bind(items, span)?;
            }

            Pattern::Record { fields, .. } => {
                let mut items: Vec<(BindDestructKind, Pattern)> = Vec::new();
                for (field_name, sub_pat) in fields {
                    match sub_pat {
                        Some(pat) => {
                            if self.pattern_has_bindings(pat) {
                                items.push((
                                    BindDestructKind::RecordField(field_name.clone()),
                                    pat.clone(),
                                ));
                            }
                        }
                        None => {
                            // Shorthand: { name } binds field to local with same name
                            items.push((
                                BindDestructKind::RecordField(field_name.clone()),
                                Pattern::Ident(field_name.clone()),
                            ));
                        }
                    }
                }
                self.compile_compound_bind(items, span)?;
            }

            Pattern::Map(entries) => {
                let items: Vec<(BindDestructKind, Pattern)> = entries
                    .iter()
                    .filter_map(|(key, sub_pat)| {
                        if self.pattern_has_bindings(sub_pat) {
                            Some((BindDestructKind::MapValue(key.clone()), sub_pat.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                self.compile_compound_bind(items, span)?;
            }

            Pattern::Or(alternatives) => {
                // All alternatives bind the same variables. Bind using first alt's structure.
                if let Some(first) = alternatives.first() {
                    self.compile_pattern_bind(first, span)?;
                }
            }

            // Patterns with no bindings
            Pattern::Wildcard
            | Pattern::Int(_)
            | Pattern::Float(_)
            | Pattern::Bool(_)
            | Pattern::StringLit(_)
            | Pattern::Range(..)
            | Pattern::FloatRange(..)
            | Pattern::Pin(_) => {
                // No bindings to create
            }
        }
        Ok(())
    }

    /// Compile bindings for a compound pattern (tuple, constructor, list, record, map).
    ///
    /// The parent value is on TOS. For each sub-pattern that has bindings,
    /// we GetLocal the parent, Destruct the sub-value, register intermediate
    /// stack values as hidden locals, and recurse.
    ///
    /// This approach "wastes" stack slots for intermediate copies but ensures
    /// local slot numbers always match actual stack positions.
    fn compile_compound_bind(
        &mut self,
        items: Vec<(BindDestructKind, Pattern)>,
        span: Span,
    ) -> Result<(), String> {
        if items.is_empty() {
            return Ok(());
        }

        // The parent is on TOS. We need it in a known local slot so we
        // can GetLocal it repeatedly. We know TOS is at the "next" stack
        // position, so we can register it as a hidden local.
        // But TOS may not yet be registered. We need to check: is TOS already
        // at the expected slot position?
        //
        // Strategy: just Dup + add_local + SetLocal to get a known slot.
        // The Dup'd copy becomes a hidden local.
        self.current_chunk().emit_op(Op::Dup, span);
        let parent_slot = self.add_local("__bind_parent__".into());
        self.current_chunk().emit_op(Op::SetLocal, span);
        self.current_chunk().emit_u16(parent_slot, span);

        for (kind, sub_pat) in &items {
            // Push the parent value from the known slot
            self.current_chunk().emit_op(Op::GetLocal, span);
            self.current_chunk().emit_u16(parent_slot, span);

            // Destruct to get the sub-value
            match kind {
                BindDestructKind::Variant(i) => {
                    self.current_chunk().emit_op(Op::DestructVariant, span);
                    self.current_chunk().emit_u8(*i, span);
                }
                BindDestructKind::Tuple(i) => {
                    self.current_chunk().emit_op(Op::DestructTuple, span);
                    self.current_chunk().emit_u8(*i, span);
                }
                BindDestructKind::List(i) => {
                    self.current_chunk().emit_op(Op::DestructList, span);
                    self.current_chunk().emit_u8(*i, span);
                }
                BindDestructKind::ListRest(start) => {
                    self.current_chunk().emit_op(Op::DestructListRest, span);
                    self.current_chunk().emit_u8(*start, span);
                }
                BindDestructKind::RecordField(name) => {
                    let field_idx = self
                        .current_chunk()
                        .add_constant(Value::String(name.clone()));
                    self.current_chunk().emit_op(Op::DestructRecordField, span);
                    self.current_chunk().emit_u16(field_idx, span);
                }
                BindDestructKind::MapValue(key) => {
                    let key_idx = self
                        .current_chunk()
                        .add_constant(Value::String(key.clone()));
                    self.current_chunk().emit_op(Op::DestructMapValue, span);
                    self.current_chunk().emit_u16(key_idx, span);
                }
            }

            // Stack: [..., parent_copy_from_GetLocal, sub_value]
            // Register the parent_copy as a hidden local
            let _copy_slot = self.add_local("__destruct_copy__".into());
            // Now sub_value is at the next stack position, ready for recursion.

            // Recurse into the sub-pattern for binding
            self.compile_pattern_bind(sub_pat, span)?;
        }

        Ok(())
    }

    // ── Pattern analysis helpers ─────────────────────────────────

    /// Returns true if the pattern always matches (no runtime test needed).
    fn pattern_is_irrefutable(&self, pattern: &Pattern) -> bool {
        matches!(pattern, Pattern::Wildcard | Pattern::Ident(_))
    }

    /// Returns true if the pattern (or any sub-pattern) binds any variable.
    fn pattern_has_bindings(&self, pattern: &Pattern) -> bool {
        match pattern {
            Pattern::Ident(_) => true,
            Pattern::Wildcard
            | Pattern::Int(_)
            | Pattern::Float(_)
            | Pattern::Bool(_)
            | Pattern::StringLit(_)
            | Pattern::Range(..)
            | Pattern::FloatRange(..)
            | Pattern::Pin(_) => false,
            Pattern::Constructor(_, fields) => fields.iter().any(|p| self.pattern_has_bindings(p)),
            Pattern::Tuple(pats) => pats.iter().any(|p| self.pattern_has_bindings(p)),
            Pattern::List(elems, rest) => {
                elems.iter().any(|p| self.pattern_has_bindings(p))
                    || rest.as_ref().is_some_and(|r| self.pattern_has_bindings(r))
            }
            Pattern::Record { fields, .. } => fields.iter().any(|(_, p)| {
                match p {
                    Some(pat) => self.pattern_has_bindings(pat),
                    None => true, // shorthand {name} always binds
                }
            }),
            Pattern::Or(alts) => alts.iter().any(|p| self.pattern_has_bindings(p)),
            Pattern::Map(entries) => entries.iter().any(|(_, p)| self.pattern_has_bindings(p)),
        }
    }

    // ── Pipe compilation ─────────────────────────────────────────

    fn compile_pipe(&mut self, left: &Expr, right: &Expr, span: Span) -> Result<(), String> {
        // Compile the left value first
        self.compile_expr(left)?;

        // val |> f(args) -> f(val, args)
        // val |> f       -> f(val)
        match &right.kind {
            ExprKind::Call(callee, args) => {
                // Check if callee is a module-qualified builtin
                if let Some(builtin_name) = self.extract_builtin_name(callee)? {
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
                    // Store val in a hidden local so it persists while we compile callee.
                    // SetLocal does NOT pop; the value stays on the stack at the local's slot.
                    let pipe_slot = self.add_local("__pipe_val__".into());
                    self.current_chunk().emit_op(Op::SetLocal, span);
                    self.current_chunk().emit_u16(pipe_slot, span);

                    // Compile callee
                    self.compile_expr(callee)?;
                    // Push val back from its local slot
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
            _ => {
                // val |> f or val |> expr
                // Store val in a hidden local, compile RHS, get val, call.
                let pipe_slot = self.add_local("__pipe_val__".into());
                self.current_chunk().emit_op(Op::SetLocal, span);
                self.current_chunk().emit_u16(pipe_slot, span);

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

        // Compile initial values and store in locals.
        // Record the first slot so Recur knows where to write.
        // Note: do NOT pop after SetLocal — the value stays on the stack as the local's slot.
        let mut first_slot = 0u16;
        for (i, (name, init)) in bindings.iter().enumerate() {
            self.compile_expr(init)?;
            self.warn_if_shadows_module(name, span);
            let slot = self.add_local(name.clone());
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
    fn extract_builtin_name(&self, callee: &Expr) -> Result<Option<String>, String> {
        if let ExprKind::FieldAccess(expr, field) = &callee.kind
            && let ExprKind::Ident(module) = &expr.kind
        {
            // Check if it's a local or upvalue first
            if self.resolve_local(module).is_none()
                && self.resolve_upvalue_peek(module).is_none()
                && module::is_builtin_module(module)
            {
                if !self.imported_builtin_modules.contains(module.as_str()) {
                    return Err(format!(
                        "module '{module}' is not imported; add `import {module}` at the top of the file"
                    ));
                }
                return Ok(Some(format!("{module}.{field}")));
            }
        }
        Ok(None)
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

    fn add_local(&mut self, name: String) -> u16 {
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
    fn warn_if_shadows_module(&mut self, name: &str, span: Span) {
        if module::is_builtin_module(name) {
            self.warnings.push(CompileWarning {
                message: format!(
                    "variable '{name}' shadows the builtin '{name}' module; \
                     use a different name to access '{name}.* functions"
                ),
                span,
            });
        }
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

    /// Non-mutating check if a variable could be resolved as an upvalue.
    /// Used for determining if an identifier is a variable vs module name.
    fn resolve_upvalue_peek(&self, name: &str) -> Option<()> {
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
            // Add an upvalue descriptor to the current context.
            return Some(self.add_upvalue(
                context_index,
                UpvalueDesc {
                    is_local: true,
                    index: slot as u8,
                },
            ));
        }

        // Not a local in the enclosing scope -- try recursively as an upvalue.
        if let Some(parent_upvalue_idx) = self.resolve_upvalue_in(name, enclosing_idx) {
            // The enclosing scope has it as an upvalue. Chain it.
            return Some(self.add_upvalue(
                context_index,
                UpvalueDesc {
                    is_local: false,
                    index: parent_upvalue_idx,
                },
            ));
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
