//! AST-to-bytecode compiler for Silt.
//!
//! Walks the AST and emits stack-based bytecode into `Function` objects.
//! Phase 4: full pattern matching compilation for all pattern types,
//! including nested/recursive patterns, or-patterns, guards, ranges,
//! list/tuple/record/map destructuring, pin patterns, when/else,
//! plus all previous features (closures, upvalues, pipes, lambdas).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast::{
    BinOp, Decl, Expr, ExprKind, ImportTarget, ListElem, MatchArm, Param, PatternKind, Program,
    Qualifier, RecordField, Stmt, StringPart, TypeBody, TypeExpr, TypeExprKind, UnaryOp,
};
use crate::bytecode::{Chunk, Function, Globals, Op, UpvalueDesc, VmClosure};
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, intern, resolve};
use crate::module;
use crate::source::Span;
use crate::typeinfo::{FieldType, Shape, Tag, TypeInfo, TypeTable, VariantInfo};
use crate::types::TypeRef;
use crate::types::canonical::{Resolver, canonical_head};
use crate::value::{HostFn, Value};

mod patterns;

// ── Types at run time ───────────────────────────────────────────────
//
// Each record and enum type a program builds values of is described to
// the VM by a `TypeInfo` (see `crate::typeinfo`): its id, its name, its
// variants or its fields. A record's fields carry their types as far as
// `json.parse(text, T)` and `toml.parse(text, T)` need them to build a
// value of `T`. Type aliases are replaced by their target. A type no
// decoder exists for is `FieldType::Unsupported`, which makes the
// decoders return `Err`; it is never mapped to some other type. A direct
// `json.parse` / `toml.parse` call whose type argument names such a
// record is rejected at compile time (see
// `Compiler::check_decode_target`).

/// The builtin functions that decode text into a value of the type named
/// by their last argument.
const DECODING_BUILTINS: &[&str] = &[
    "json.parse",
    "json.parse_list",
    "json.parse_map",
    "toml.parse",
    "toml.parse_list",
    "toml.parse_map",
];

/// The field types the decoders support, as shown in compile errors.
const DECODABLE_TYPES_HELP: &str = "decodable field types are Int, Float, String, \
     Bool, Date, Time, DateTime, List(T), Range(T), Option(T), Map(String, T), tuples, \
     non-generic record types, and aliases of these";

/// The record types a field type names.
fn collect_records(field_type: &FieldType, out: &mut Vec<crate::defs::TypeId>) {
    match field_type {
        FieldType::Record(id) => out.push(*id),
        FieldType::List(t) | FieldType::Option(t) | FieldType::Map(t) => collect_records(t, out),
        FieldType::Tuple(ts) => ts.iter().for_each(|t| collect_records(t, out)),
        _ => {}
    }
}

/// A record field that `json.parse` / `toml.parse` cannot decode.
struct UndecodableField {
    /// The record type that declares the field.
    record: String,
    field: String,
    /// The field's type as written in the declaration.
    field_type: String,
    /// The part of the field's type that has no decoder, as written.
    part: String,
}

/// Render a type expression the way it is written in source.
fn render_type_expr(te: &TypeExpr) -> String {
    fn render_list(items: &[TypeExpr]) -> String {
        items
            .iter()
            .map(render_type_expr)
            .collect::<Vec<_>>()
            .join(", ")
    }
    match &te.kind {
        TypeExprKind::Named { module, name, .. } => Qualifier::written(*module, *name),
        TypeExprKind::Generic {
            module, name, args, ..
        } => {
            format!(
                "{}({})",
                Qualifier::written(*module, *name),
                render_list(args)
            )
        }
        TypeExprKind::Tuple(elems) => format!("({})", render_list(elems)),
        TypeExprKind::Function(params, ret) => {
            format!("Fn({}) -> {}", render_list(params), render_type_expr(ret))
        }
        TypeExprKind::SelfType => "Self".to_string(),
        TypeExprKind::AssocProj {
            receiver,
            trait_module,
            trait_name,
            assoc_name,
        } => format!(
            "<{} as {}>::{}",
            render_type_expr(receiver),
            Qualifier::written(*trait_module, *trait_name),
            resolve(*assoc_name)
        ),
        TypeExprKind::AnonRecord { fields, tail } => {
            let mut items: Vec<String> = fields
                .iter()
                .map(|(n, t)| format!("{}: {}", resolve(*n), render_type_expr(t)))
                .collect();
            if let Some(row) = tail {
                items.push(format!("...{}", resolve(*row)));
            }
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// Replace the type parameters `params` by `args` in `te`. Used to expand
/// a parametric alias: `Pair(Int)` with `type Pair(a) = (a, a)` becomes
/// `(Int, Int)`.
fn substitute_type_params(te: &TypeExpr, params: &[Symbol], args: &[TypeExpr]) -> TypeExpr {
    let subst = |t: &TypeExpr| substitute_type_params(t, params, args);
    let kind = match &te.kind {
        TypeExprKind::Named {
            module: None, name, ..
        } if params.contains(name) => {
            let i = params.iter().position(|p| p == name).expect("contained");
            return args[i].clone();
        }
        TypeExprKind::Named { .. } => te.kind.clone(),
        TypeExprKind::Generic {
            module,
            name,
            name_span,
            args: type_args,
        } => TypeExprKind::Generic {
            module: *module,
            name: *name,
            name_span: *name_span,
            args: type_args.iter().map(subst).collect(),
        },
        TypeExprKind::Tuple(elems) => TypeExprKind::Tuple(elems.iter().map(subst).collect()),
        TypeExprKind::Function(fn_params, ret) => TypeExprKind::Function(
            fn_params.iter().map(subst).collect(),
            Box::new(substitute_type_params(ret, params, args)),
        ),
        TypeExprKind::SelfType => TypeExprKind::SelfType,
        TypeExprKind::AssocProj {
            receiver,
            trait_module,
            trait_name,
            assoc_name,
        } => TypeExprKind::AssocProj {
            receiver: Box::new(substitute_type_params(receiver, params, args)),
            trait_module: *trait_module,
            trait_name: *trait_name,
            assoc_name: *assoc_name,
        },
        TypeExprKind::AnonRecord { fields, tail } => TypeExprKind::AnonRecord {
            fields: fields.iter().map(|(n, t)| (*n, subst(t))).collect(),
            tail: *tail,
        },
    };
    let mut substituted = TypeExpr::new(kind, te.span);
    substituted.res = te.res;
    substituted
}

// ── Bind destruct kind ───────────────────────────────────────────────

/// Describes how to destructure a sub-value from a compound pattern.
enum BindDestructKind {
    Variant(u8),
    Tuple(u8),
    List(u8),
    ListRest(u8),
    RecordField(Symbol),
    /// Anonymous record `...rest` capture: produces a new record containing
    /// every field of the parent record except those listed here.
    RecordRest(Vec<Symbol>),
    MapValue(String),
}

// ── Compiler context ──────────────────────────────────────────────────

/// Per-function compilation state.
///
/// `height` is the compiler's model of the run-time stack: the number of
/// values the function's frame holds at the current point of the emitted
/// code. It counts parameters, locals (named and hidden), and operands
/// that a construct has evaluated and keeps on the stack while it
/// evaluates the next one (the left side of `+`, a callee, earlier
/// arguments or elements). The rules:
///
/// - Code compiled for an expression at height `h` leaves exactly one
///   more value in the frame, the expression's value in slot `h`.
///   `height` is `h` again afterwards; the value is counted only once
///   its consumer keeps it, as a local (`add_local`) or as a pending
///   operand (`compile_operands`).
/// - A local's slot is the height at which it is added, so it is the
///   local's real position in the frame.
/// - Where a scope ends or a failed pattern test lands, values the model
///   no longer counts may be left in the frame. `Op::Slide` removes them
///   there. The one exception is an expression in tail position: its
///   value is returned at once and the frame is discarded with it.
struct CompileContext {
    function: Function,
    locals: Vec<Local>,
    scope_depth: usize,
    /// Frame height at the start of every open scope, innermost last.
    scope_starts: Vec<usize>,
    /// Number of values in the frame. See the type's documentation.
    height: usize,
    /// Upvalue descriptors for this function/closure.
    upvalues: Vec<UpvalueDesc>,
    /// Loop context stack: (first_loop_slot, loop_start_offset, binding_count)
    loop_stack: Vec<LoopInfo>,
}

struct LoopInfo {
    first_slot: u16,
    loop_start: usize,
    binding_count: u8,
}

impl CompileContext {
    fn new(name: String, arity: u8) -> Self {
        Self {
            function: Function::new(name, arity),
            locals: Vec::new(),
            scope_depth: 0,
            scope_starts: Vec::new(),
            height: 0,
            upvalues: Vec::new(),
            loop_stack: Vec::new(),
        }
    }
}

/// Convert a frame height to the `u16` slot operand of `GetLocal`,
/// `SetLocal`, `Recur` and `Slide`.
fn frame_slot(height: usize, span: Span) -> Result<u16, Diagnostic> {
    u16::try_from(height).map_err(|_| {
        Diagnostic::error(
            Code::CompileLimit,
            span,
            format!(
                "this function keeps more than {} values on its stack at once \
             (its local bindings plus the values of the expression being evaluated); \
             move some of its statements into separate functions, or split a large \
             expression into smaller parts",
                u16::MAX
            ),
        )
    })
}

struct Local {
    name: Symbol,
    depth: usize,
    slot: u16,
}

// ── Compiler errors ─────────────────────────────────────────────────

/// A defect in silt: the program reached the compiler with an error the
/// typechecker reports (a builtin module used without an import, a
/// `loop(...)` outside a loop or with the wrong number of arguments). The
/// session compiles only programs whose analysis has no error.
fn checker_missed(span: Span, what: &str) -> Diagnostic {
    Diagnostic::error(
        Code::CompilerBug,
        span,
        format!("compiler bug: {what} reached the compiler; the typechecker reports it"),
    )
}

/// Validate that a computed `JumpBack` distance fits in the instruction's
/// `u16` operand. Mirrors the check in [`Chunk::patch_jump`] for forward
/// jumps — a loop body larger than 65_535 bytes of bytecode would wrap
/// around and branch to garbage. Extracted into a free function so the
/// bounds check can be unit-tested without wiring up an entire compile
/// context.
fn jumpback_fits_u16(jump_back_dist: usize, span: Span) -> Result<(), Diagnostic> {
    if jump_back_dist > u16::MAX as usize {
        return Err(Diagnostic::error(
            Code::CompileLimit,
            span,
            "loop body too large (exceeds 65535 bytes of bytecode)",
        ));
    }
    Ok(())
}

// ── Compiler ──────────────────────────────────────────────────────────

/// A module of the program, as the session hands it to the compiler:
/// parsed and typechecked, with what each of its imports names.
pub struct ModuleUnit {
    /// The module, as the session knows it.
    pub id: crate::session::ModuleId,
    /// The module's declarations, after the typechecker filled them in.
    pub program: Arc<Program>,
    /// The module's name in its package (`"lib"` for a dependency's
    /// library, `"util"` for `src/util.silt`).
    pub name: String,
    /// How a value of a type of the module names the module when two
    /// types of the program have the type's name (`util.Pt {x: 1}`):
    /// as the entry's package imports it. A module of the entry's
    /// package is its name (`util`); a dependency's library is the
    /// dependency (`db`); another module of a dependency is the
    /// dependency and the module (`db.util`).
    pub qualifier: String,
    /// The module each `import` of this module names, by the module
    /// name written after `import`. Builtin modules are not in it.
    pub imports: HashMap<Symbol, usize>,
    /// For a host module, the function each of its signatures declares,
    /// by name: the module's globals are these functions.
    pub host: HashMap<Symbol, Arc<HostFn>>,
}

/// The modules of a program, indexed by the session's module ids, and
/// which of them is the entry: the one compiled by `compile_program` or
/// `compile_declarations`. The others are compiled where they are first
/// imported.
pub struct ProgramUnits {
    pub modules: Vec<ModuleUnit>,
    pub entry: usize,
    /// The definitions the resolver's slots name.
    pub defs: Arc<crate::defs::DefTable>,
    /// For a REPL entry: what the earlier entries left in the VM. Empty
    /// for any other program.
    pub earlier: EarlierCells,
    /// The type aliases and associated-type bindings of the session:
    /// impl targets are canonicalized with them, as the checker keyed
    /// the impls.
    pub resolver: Arc<Resolver>,
}

/// What the earlier entries of a REPL session installed, which the entry
/// being compiled uses but does not install again.
#[derive(Default)]
pub struct EarlierCells {
    /// Their declarations, oldest first: the entry knows their types.
    pub programs: Vec<Arc<Program>>,
    /// The modules installed already: an import of one compiles nothing.
    pub installed: HashSet<usize>,
    /// The global slots the earlier entries took. The entry's own
    /// definitions get new ones.
    pub globals: Globals,
}

pub struct Compiler {
    contexts: Vec<CompileContext>,
    /// Accumulated compiled functions (one per `Decl::Fn`).
    functions: Vec<Function>,
    /// The modules of the program, from the session.
    units: ProgramUnits,
    /// The modules being compiled, innermost last: the importing module
    /// of an `import` met now is the last one, or the entry module.
    unit_stack: Vec<usize>,
    /// Modules already compiled in this compilation unit, so each is
    /// compiled once, where it is first imported.
    compiled_modules: HashSet<usize>,
    /// Whether the current expression is in tail position (for TCO).
    in_tail_position: bool,
    /// The names two types of the program's modules have (two modules'
    /// `Pt`, not a builtin type): such a type prints qualified.
    program_clashes: HashSet<Symbol>,
    /// The types described so far, which the VM is given.
    types: RefCell<TypeTable>,
    /// The global slots of the program.
    globals: Globals,
    /// The slot of each top-level function, `let` and host function of
    /// the modules compiled now, by module and name.
    own_slots: HashMap<(crate::session::ModuleId, Symbol), u16>,
    /// Whether the derived impls of the builtin types are installed
    /// already (by an earlier REPL entry).
    builtin_impls_installed: bool,
}

/// The names two or more types of the program's modules have (two
/// modules' `Pt`): such a type prints qualified by its module's name.
fn program_type_clashes(units: &ProgramUnits) -> HashSet<Symbol> {
    let mut seen: HashMap<Symbol, crate::defs::DefId> = HashMap::new();
    let mut clashing = HashSet::new();
    for unit in &units.modules {
        for id in units.defs.of_module(unit.id) {
            let def = units.defs.get(*id);
            if !matches!(def.kind, crate::defs::DefKind::Type(_)) {
                continue;
            }
            if seen.insert(def.name, *id).is_some_and(|other| other != *id) {
                clashing.insert(def.name);
            }
        }
    }
    clashing
}

/// A program needs more global slots than the instruction operand can
/// name.
fn too_many_globals(span: Span) -> Diagnostic {
    Diagnostic::error(
        Code::CompileLimit,
        span,
        format!(
            "this program has more than {} top-level definitions (functions, `let`s and \
             trait methods); split it into fewer, larger definitions",
            u16::MAX as usize + 1
        ),
    )
}

impl Compiler {
    /// A compiler for the modules of a program, as the session analysed
    /// them. Every definition of the program that has a global slot gets
    /// it now, so code can use a definition compiled after it.
    pub fn for_program(units: ProgramUnits) -> Result<Self, Diagnostic> {
        let program_clashes = program_type_clashes(&units);
        let mut globals = units.earlier.globals.clone();
        let builtin_impls_installed = !globals.is_empty();
        let mut compiler = Self {
            contexts: Vec::new(),
            functions: Vec::new(),
            units,
            unit_stack: Vec::new(),
            compiled_modules: HashSet::new(),
            in_tail_position: false,
            program_clashes,
            types: RefCell::new(TypeTable::default()),
            globals: Globals::default(),
            own_slots: HashMap::new(),
            builtin_impls_installed,
        };
        compiler.own_slots = compiler.assign_slots(&mut globals)?;
        compiler.globals = globals;
        Ok(compiler)
    }

    /// Give a global slot to each definition the program installs: the
    /// derived impls of the builtin types (unless an earlier REPL entry
    /// installed them), then, module by module, each function, `let`,
    /// host function and impl method. Gives the slots of the modules'
    /// own functions, `let`s and host functions by module and name.
    fn assign_slots(
        &self,
        globals: &mut Globals,
    ) -> Result<HashMap<(crate::session::ModuleId, Symbol), u16>, Diagnostic> {
        let mut own = HashMap::new();
        if !self.builtin_impls_installed {
            for decl in crate::typechecker::builtin_derived_impls().iter() {
                if let Decl::TraitImpl(ti) = decl {
                    self.assign_method_slots(ti, globals)?;
                }
            }
        }
        for (index, unit) in self.units.modules.iter().enumerate() {
            if self.units.earlier.installed.contains(&index) {
                continue;
            }
            let qualify = index != self.units.entry;
            for &id in self.units.defs.of_module(unit.id) {
                let def = self.units.defs.get(id);
                if !matches!(
                    def.kind,
                    crate::defs::DefKind::Fn
                        | crate::defs::DefKind::Let
                        | crate::defs::DefKind::Host
                ) {
                    continue;
                }
                let name = match qualify {
                    true => format!("{}.{}", unit.name, def.name),
                    false => resolve(def.name),
                };
                let slot = globals
                    .add_def(id, name)
                    .ok_or_else(|| too_many_globals(def.span))?;
                own.insert((unit.id, def.name), slot);
            }
            for decl in &unit.program.decls {
                if let Decl::TraitImpl(ti) = decl {
                    self.assign_method_slots(ti, globals)?;
                }
            }
        }
        Ok(own)
    }

    /// Give a global slot to each method of the impl `ti`.
    fn assign_method_slots(
        &self,
        ti: &crate::ast::TraitImpl,
        globals: &mut Globals,
    ) -> Result<(), Diagnostic> {
        let (Some(ty), Some(t)) = (self.impl_type(ti), self.impl_trait(ti)) else {
            return Ok(());
        };
        let type_name = self.type_info(ty).name.clone();
        for method in &ti.methods {
            globals
                .add_method(
                    t,
                    ty,
                    &resolve(method.name),
                    format!("{type_name}.{}", method.name),
                )
                .ok_or_else(|| too_many_globals(method.span))?;
        }
        Ok(())
    }

    /// The alias registries the program was checked with: read via
    /// [`crate::types::canonical::canonical_head`] when keying impl
    /// methods, so registration and lookup keys agree across the
    /// typecheck → compile boundary.
    fn resolver(&self) -> &Resolver {
        &self.units.resolver
    }

    /// The types the compiled code builds values of, for the VM.
    pub fn types(&self) -> TypeTable {
        self.types.borrow().clone()
    }

    /// The global slots of the program, for the VM.
    pub fn globals(&self) -> &Globals {
        &self.globals
    }

    // ── Public entry point ────────────────────────────────────────

    /// Compile a full program, returning all functions.
    ///
    /// The first function in the returned `Vec` is the top-level
    /// `<script>`, which ends by calling `entry`, the definition the entry
    /// module's `main` names (for a REPL entry of statements, the function
    /// that holds them, whose name no program can write): `GetGlobal
    /// main ; Call 0 ; Return`. A program without an entry point is never
    /// run: the session reports that it has no `main`.
    pub fn compile_program(
        &mut self,
        program: &Program,
        entry: Option<crate::defs::DefId>,
    ) -> Result<Vec<Function>, Diagnostic> {
        // Push a top-level script context.
        self.contexts
            .push(CompileContext::new("<script>".into(), 0));

        self.compile_builtin_derived_impls()?;
        for decl in Self::decls_in_init_order(&program.decls) {
            self.compile_decl(decl)?;
        }

        // Emit: GetGlobal <entry>, Call 0, Return. The call is made for
        // the entry point's function declaration and takes its span;
        // without one (`let main = ...`, an imported `main`), silt itself
        // makes the call.
        let span = entry
            .and_then(|def| {
                let name = self.units.defs.get(def).name;
                program.decls.iter().find_map(|decl| match decl {
                    Decl::Fn(f) if f.name == name => Some(f.span),
                    _ => None,
                })
            })
            .unwrap_or(Span::BUILTIN);
        match entry.and_then(|def| self.globals.def(def)) {
            Some(slot) => {
                self.current_chunk().emit_op_u16(Op::GetGlobal, slot, span);
                self.current_chunk().emit_op(Op::Call, span);
                self.current_chunk().emit_u8(0, span);
            }
            None => {
                self.current_chunk().emit_op(Op::Unit, span);
            }
        }
        self.current_chunk().emit_op(Op::Return, span);

        let script = self
            .contexts
            .pop()
            .ok_or(Diagnostic::error(
                Code::CompilerBug,
                Span::BUILTIN,
                "compiler bug: missing script context",
            ))?
            .function;

        // Build the result: script first, then all compiled functions.
        let mut result = vec![script];
        result.append(&mut self.functions);
        Ok(result)
    }

    /// Compile all declarations without calling `main()`.
    ///
    /// Returns all compiled functions. The first is a `<script>` that
    /// installs the globals and returns Unit.  Useful for test runners and
    /// the REPL where `main()` is not the entry-point.
    pub fn compile_declarations(&mut self, program: &Program) -> Result<Vec<Function>, Diagnostic> {
        self.contexts
            .push(CompileContext::new("<script>".into(), 0));

        self.compile_builtin_derived_impls()?;
        for decl in Self::decls_in_init_order(&program.decls) {
            self.compile_decl(decl)?;
        }

        // Return Unit instead of calling main: code silt adds itself.
        let span = Span::BUILTIN;
        self.current_chunk().emit_op(Op::Unit, span);
        self.current_chunk().emit_op(Op::Return, span);

        let script = self
            .contexts
            .pop()
            .ok_or(Diagnostic::error(
                Code::CompilerBug,
                Span::BUILTIN,
                "compiler bug: missing script context",
            ))?
            .function;
        let mut result = vec![script];
        result.append(&mut self.functions);
        Ok(result)
    }

    /// Compile the derived impls of the builtin types, which the
    /// typechecker derives and checks once (see
    /// [`crate::typechecker::builtin_derived_impls`]), at the start of a
    /// program's script; a REPL session installs them once.
    fn compile_builtin_derived_impls(&mut self) -> Result<(), Diagnostic> {
        if self.builtin_impls_installed {
            return Ok(());
        }
        for decl in crate::typechecker::builtin_derived_impls().iter() {
            self.compile_decl(decl)?;
        }
        Ok(())
    }

    // ── Declarations ──────────────────────────────────────────────

    /// The slot of the top-level function, `let` or host function `name`
    /// of the module being compiled.
    fn own_slot(&self, name: Symbol, span: Span) -> Result<u16, Diagnostic> {
        self.own_def_slot(name).ok_or_else(|| {
            checker_missed(
                span,
                &format!("the top-level name '{name}' with no definition"),
            )
        })
    }

    /// [`Compiler::own_slot`], `None` when the module has no such
    /// definition.
    fn own_def_slot(&self, name: Symbol) -> Option<u16> {
        let current = self.unit_stack.last().copied().unwrap_or(self.units.entry);
        let module = self.units.modules[current].id;
        self.own_slots.get(&(module, name)).copied()
    }

    /// The order in which a program's declarations are installed: first
    /// everything that only defines something (imports, types, traits,
    /// trait impls, functions), then the top-level `let`s. Each group
    /// keeps its source order. A top-level initialiser can therefore use
    /// every declaration of the program, wherever it is written.
    fn decls_in_init_order(decls: &[Decl]) -> Vec<&Decl> {
        let (lets, definitions): (Vec<&Decl>, Vec<&Decl>) = decls
            .iter()
            .partition(|decl| matches!(**decl, Decl::Let { .. }));
        definitions.into_iter().chain(lets).collect()
    }

    fn compile_decl(&mut self, decl: &Decl) -> Result<(), Diagnostic> {
        match decl {
            Decl::Fn(fn_decl) => {
                let span = fn_decl.span;
                // Arity is encoded as a `u8` in bytecode. Silently
                // wrapping via `.len() as u8` used to let functions
                // with 256 parameters compile with arity=0; at the
                // call site the VM then treated a stack value as the
                // callee and blew up with "cannot call value of type
                // Int". Reject at compile time instead.
                if fn_decl.params.len() > u8::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        format!(
                            "function '{}' has {} parameters; silt functions are limited to 255",
                            resolve(fn_decl.name),
                            fn_decl.params.len()
                        ),
                    ));
                }
                let arity = fn_decl.params.len() as u8;

                // Push a new context for the function body.
                self.contexts
                    .push(CompileContext::new(resolve(fn_decl.name), arity));

                self.compile_params(&fn_decl.params, span)?;

                // Compile the function body in tail position for TCO.
                self.in_tail_position = true;
                self.compile_expr(&fn_decl.body)?;
                self.in_tail_position = false;

                // Emit Return (may be dead code if body ends with a tail call).
                self.current_chunk().emit_op(Op::Return, span);

                // Pop the context, recovering the compiled function.
                let ctx = self.contexts.pop().ok_or(Diagnostic::error(
                    Code::CompilerBug,
                    span,
                    "compiler bug: missing function context",
                ))?;
                let func = ctx.function;

                // Store the function as a VmClosure constant in the enclosing chunk.
                let vm_closure = Arc::new(VmClosure {
                    function: Arc::new(func),
                    upvalues: vec![],
                });
                let closure_val = Value::VmClosure(vm_closure);
                let fi = self.add_constant(closure_val, span)?;
                self.current_chunk().emit_op_u16(Op::Constant, fi, span);

                let slot = self.own_slot(fn_decl.name, span)?;
                self.current_chunk().emit_op_u16(Op::SetGlobal, slot, span);
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
                match &pattern.kind {
                    PatternKind::Ident(name) => {
                        let slot = self.own_slot(*name, span)?;
                        self.current_chunk().emit_op_u16(Op::SetGlobal, slot, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    _ => {
                        let mut slots = Vec::new();
                        for (name, _, _) in crate::parser::top_level_binders(decl) {
                            slots.push((name, self.own_slot(name, span)?));
                        }
                        self.install_destructured(pattern, &slots, span)?;
                    }
                }
                Ok(())
            }

            Decl::Type(type_decl) => {
                // A record or enum type is described to the VM; its values
                // and descriptor are constants where code names them. Type
                // aliases are transparent at the typechecker / canonicaliser
                // layer and emit no runtime artefacts.
                if matches!(type_decl.body, TypeBody::Alias(_)) {
                    return Ok(());
                }
                let Some(id) = self.declared_type(type_decl.name) else {
                    return Err(checker_missed(
                        type_decl.span,
                        &format!("the type '{}' with no definition", type_decl.name),
                    ));
                };
                self.type_info(id);
                Ok(())
            }

            Decl::TraitImpl(trait_impl) => {
                // Compile each method into the global slot of the method
                // of the impl's type: the canonical head of its target, as
                // the typechecker keys impls (`Range` is `List`, an alias
                // is the type it stands for), so the VM finds it by the
                // type of the receiver.
                // An impl whose target names no type (`trait Display
                // for a`) is never dispatched to: it has no code.
                let (Some(ty), Some(t)) = (self.impl_type(trait_impl), self.impl_trait(trait_impl))
                else {
                    return Ok(());
                };
                let type_name = self.type_info(ty).name.clone();

                for method in &trait_impl.methods {
                    let span = method.span;
                    if method.params.len() > u8::MAX as usize {
                        return Err(Diagnostic::error(
                            Code::CompileLimit,
                            span,
                            format!(
                                "trait method '{}.{}' has {} parameters; silt functions are limited to 255",
                                trait_impl.target_type,
                                method.name,
                                method.params.len()
                            ),
                        ));
                    }
                    let arity = method.params.len() as u8;
                    let qualified_name = format!("{type_name}.{}", method.name);

                    self.contexts
                        .push(CompileContext::new(qualified_name, arity));

                    self.compile_params(&method.params, span)?;

                    self.compile_expr(&method.body)?;
                    self.current_chunk().emit_op(Op::Return, span);

                    let ctx = self.contexts.pop().ok_or(Diagnostic::error(
                        Code::CompilerBug,
                        span,
                        "compiler bug: missing trait method context",
                    ))?;
                    let func = ctx.function;
                    let vm_closure = Arc::new(VmClosure {
                        function: Arc::new(func),
                        upvalues: vec![],
                    });
                    let closure_val = Value::VmClosure(vm_closure);
                    let fi = self.add_constant(closure_val, span)?;
                    self.current_chunk().emit_op_u16(Op::Constant, fi, span);

                    let slot = self
                        .globals
                        .method(Some(t), ty, &resolve(method.name))
                        .ok_or_else(|| checker_missed(span, "an impl method with no slot"))?;
                    self.current_chunk().emit_op_u16(Op::SetGlobal, slot, span);
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

    /// Install the names a top-level `let` with the destructuring pattern
    /// `pattern` binds, whose value is on the stack: each binder goes to
    /// its global slot in `slots`. The pattern is bound as a block's `let`
    /// would bind it, then each local is copied to its global.
    fn install_destructured(
        &mut self,
        pattern: &crate::ast::Pattern,
        slots: &[(Symbol, u16)],
        span: Span,
    ) -> Result<(), Diagnostic> {
        self.begin_scope();
        let val_slot = self.add_local(intern("__let_val__"), span)?;
        self.current_chunk()
            .emit_op_u16(Op::SetLocal, val_slot, span);
        self.compile_pattern_bind_checked(pattern, span)?;
        for (name, global) in slots {
            let slot = self.resolve_local(*name).ok_or_else(|| {
                checker_missed(span, &format!("the binder '{name}' of a top-level let"))
            })?;
            self.current_chunk().emit_op_u16(Op::GetLocal, slot, span);
            self.current_chunk()
                .emit_op_u16(Op::SetGlobal, *global, span);
            self.current_chunk().emit_op(Op::Pop, span);
        }
        self.current_chunk().emit_op(Op::Unit, span);
        self.end_scope_with_result(false, span)?;
        self.current_chunk().emit_op(Op::Pop, span);
        Ok(())
    }

    // ── Import compilation ─────────────────────────────────────────

    /// An import of a module of the program compiles the module, once,
    /// before the importer's code. A builtin module has no code.
    fn compile_import(&mut self, target: &ImportTarget, span: Span) -> Result<(), Diagnostic> {
        let module = match target {
            ImportTarget::Module(m) | ImportTarget::Items(m, _) | ImportTarget::Alias(m, _, _) => {
                *m
            }
        };
        let importer = self.unit_stack.last().copied().unwrap_or(self.units.entry);
        let Some(&target) = self.units.modules[importer].imports.get(&module) else {
            return Ok(());
        };
        self.compile_file_module(target, module, span)
    }

    /// Compile the module `target`, imported as `written`, into the
    /// current compilation unit, where it is first imported (at `span`):
    /// its declarations run in a function of their own,
    /// `<module:written>`, so runtime errors carry a frame that
    /// identifies the source file.
    fn compile_file_module(
        &mut self,
        target: usize,
        written: Symbol,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if self.compiled_modules.contains(&target) || self.units.earlier.installed.contains(&target)
        {
            return Ok(());
        }
        self.compiled_modules.insert(target);
        let program = self.units.modules[target].program.clone();
        self.unit_stack.push(target);
        let result = if self.units.modules[target].host.is_empty() {
            self.compile_file_module_inner(written, &program, span)
        } else {
            self.compile_host_module(target, &program, span)
        };
        self.unit_stack.pop();
        result
    }

    /// Install the functions of the host module `target`, one per
    /// signature of `program`, in their global slots.
    fn compile_host_module(
        &mut self,
        target: usize,
        program: &Program,
        span: Span,
    ) -> Result<(), Diagnostic> {
        for decl in &program.decls {
            let Decl::Fn(f) = decl else {
                continue;
            };
            let Some(function) = self.units.modules[target].host.get(&f.name).cloned() else {
                return Err(checker_missed(
                    span,
                    &format!("a host signature without a function: '{}'", f.name),
                ));
            };
            let fi = self.add_constant(Value::HostFn(function), span)?;
            self.current_chunk().emit_op_u16(Op::Constant, fi, span);
            let slot = self.own_slot(f.name, span)?;
            self.current_chunk().emit_op_u16(Op::SetGlobal, slot, span);
            self.current_chunk().emit_op(Op::Pop, span);
        }
        Ok(())
    }

    /// Inner implementation of file module compilation: the declarations
    /// of `program`, the module imported as `written` at `span`.
    fn compile_file_module_inner(
        &mut self,
        written: Symbol,
        program: &Program,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let init_name = format!("<module:{written}>");
        self.contexts.push(CompileContext::new(init_name, 0));

        for decl in Self::decls_in_init_order(&program.decls) {
            self.compile_decl(decl)?;
        }

        // Close the module init function and call it inline. Code silt
        // adds itself carries the import statement's span, so anything
        // that blames it points back to the import site.
        self.current_chunk().emit_op(Op::Unit, span);
        self.current_chunk().emit_op(Op::Return, span);
        let init_ctx = self.contexts.pop().ok_or(Diagnostic::error(
            Code::CompilerBug,
            span,
            "compiler bug: missing module init context",
        ))?;
        let init_closure = Arc::new(VmClosure {
            function: Arc::new(init_ctx.function),
            upvalues: vec![],
        });
        let ci = self.add_constant(Value::VmClosure(init_closure), span)?;
        self.current_chunk().emit_op_u16(Op::Constant, ci, span);
        self.current_chunk().emit_op(Op::Call, span);
        self.current_chunk().emit_u8(0, span);
        self.current_chunk().emit_op(Op::Pop, span);
        Ok(())
    }

    // ── Statements ────────────────────────────────────────────────

    fn compile_stmt(&mut self, stmt: &Stmt, is_last: bool) -> Result<(), Diagnostic> {
        match stmt {
            Stmt::Let { pattern, value, .. } => {
                // The bound value is NOT the block result, so it must never
                // inherit the enclosing tail flag (set by `ExprKind::Block`
                // for the last statement). A leaked flag would compile a
                // call value as `TailCall + Return`, replacing the frame and
                // dead-coding the binding (and the implicit Unit result).
                // Only `Stmt::Expr` legitimately inherits tail position.
                self.in_tail_position = false;
                self.compile_expr(value)?;
                let span = value.span;

                match &pattern.kind {
                    PatternKind::Ident(name) => {
                        // The value just pushed becomes the local.
                        let slot = self.add_local(*name, span)?;
                        self.current_chunk().emit_op_u16(Op::SetLocal, slot, span);
                        if is_last {
                            self.current_chunk().emit_op(Op::Unit, span);
                        }
                    }
                    _ => {
                        // General pattern destructuring for let bindings.
                        // The value stays in the frame as a hidden local and
                        // the pattern's names are bound from it.
                        let val_slot = self.add_local(intern("__let_val__"), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::SetLocal, val_slot, span);
                        self.compile_pattern_bind_checked(pattern, span)?;

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
                // Compile condition, jump to else if false.
                // The condition is not the block result — clear any leaked
                // tail flag so a call condition isn't compiled as
                // `TailCall + Return` (which would dead-code the
                // JumpIfFalse and silently skip the else/panic arm).
                self.in_tail_position = false;
                self.compile_expr(condition)?;
                let else_jump = self
                    .current_chunk()
                    .emit_jump(Op::JumpIfFalse, condition.span);

                // Condition was true — skip else block
                let end_jump = self.current_chunk().emit_jump(Op::Jump, condition.span);

                // Else block: condition was false
                self.patch_jump(else_jump, condition.span)?;
                self.compile_expr(else_body)?;
                // The else body must diverge (return, panic or loop(...)).
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
                // Compile the scrutinee. It is not the block result — clear
                // any leaked tail flag so a call scrutinee isn't compiled as
                // `TailCall + Return` (which would dead-code the pattern
                // test and the else arm, leaking the raw scrutinee as the
                // function's return value).
                self.in_tail_position = false;
                self.compile_expr(expr)?;
                let span = expr.span;

                // The value stays in the frame as a hidden local: the test
                // peeks it and the pattern's names are bound from it.
                let val_slot = self.add_local(intern("__when_val__"), span)?;
                self.current_chunk()
                    .emit_op_u16(Op::SetLocal, val_slot, span);

                let fail_jumps = self.compile_pattern_test(pattern, span)?;
                let matched_jump = self.current_chunk().emit_jump(Op::Jump, span);

                // Pattern didn't match. A failed test of a nested pattern
                // leaves the sub-values it was looking at above the value;
                // drop them. The else body is compiled before the pattern's
                // names exist, so a name it uses is the one of the enclosing
                // scope. The else body diverges (the typechecker requires
                // it), so control never reaches the bindings from here.
                for fj in fail_jumps {
                    self.patch_jump(fj, span)?;
                }
                self.current_chunk()
                    .emit_op_u16(Op::GetLocal, val_slot, span);
                self.current_chunk().emit_op_u16(Op::Slide, val_slot, span);
                self.compile_expr(else_body)?;
                self.current_chunk().emit_op(Op::Pop, span); // pop else result

                // Pattern matched — bind variables
                self.patch_jump(matched_jump, span)?;
                self.compile_pattern_bind(pattern, span)?;

                if is_last {
                    self.current_chunk().emit_op(Op::Unit, span);
                }
                Ok(())
            }
        }
    }

    // ── Expressions ───────────────────────────────────────────────

    /// Emit the call sequence for a callee + `argc` arguments already on
    /// the stack: `TailCall argc; Return` in tail position (the frame is
    /// replaced, so nothing after may execute), `Call argc` otherwise.
    /// Single home for the tail/non-tail emission shape — every plain
    /// `Op::Call`-capable site (normal calls, qualified-variant calls,
    /// module-qualified calls, both pipe shapes) must route through here.
    fn emit_call(&mut self, argc: u8, tail: bool, span: Span) {
        if tail {
            self.current_chunk().emit_op(Op::TailCall, span);
            self.current_chunk().emit_u8(argc, span);
            self.current_chunk().emit_op(Op::Return, span);
        } else {
            self.current_chunk().emit_op(Op::Call, span);
            self.current_chunk().emit_u8(argc, span);
        }
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), Diagnostic> {
        let span = expr.span;
        let tail = self.in_tail_position;
        self.in_tail_position = false;

        match &expr.kind {
            ExprKind::Int(n) => {
                let idx = self.add_constant(Value::Int(*n), span)?;
                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
            }

            ExprKind::Float(n) => {
                let idx = self.add_constant(Value::Float(*n), span)?;
                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
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
                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
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
                        self.compile_operands([&**left, &**right])?;
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

                self.end_scope_with_result(tail, span)?;
            }

            ExprKind::Ident(_) if let Some(variant) = self.variant_value(expr) => {
                let idx = self.add_constant(variant, span)?;
                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
            }

            // A type used as a value is its descriptor.
            ExprKind::Ident(_) if let Some(descriptor) = self.type_value(expr.res) => {
                let idx = self.add_constant(descriptor, span)?;
                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
            }

            ExprKind::Ident(_) if let Some(def) = self.value_def(expr.res) => {
                self.emit_global_value(def, span)?;
            }

            ExprKind::Ident(name) => {
                if let Some(slot) = self.resolve_local(*name) {
                    self.current_chunk().emit_op_u16(Op::GetLocal, slot, span);
                } else if let Some(idx) = self.resolve_upvalue(*name, span)? {
                    self.current_chunk().emit_op(Op::GetUpvalue, span);
                    self.current_chunk().emit_u8(idx, span);
                } else {
                    return Err(checker_missed(
                        span,
                        &format!("the unresolved name '{name}'"),
                    ));
                }
            }

            ExprKind::Call(callee, args) => {
                // Argument count is encoded as a `u8` in all four
                // call emission paths below (CallBuiltin, CallMethod,
                // a global's Call, plain Call). Wrapping via
                // `.len() as u8` used to let a 256-argument call
                // compile with argc=0, and the VM would then
                // misinterpret an unrelated stack value as the
                // callee. Reject at compile time here — the method-
                // call path adds the receiver so the limit is 254
                // explicit arguments in that case.
                if args.len() > u8::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        format!(
                            "call has {} arguments; silt calls are limited to 255",
                            args.len()
                        ),
                    ));
                }
                if let Some(variant) = self.variant_value(callee) {
                    // A variant's constructor: `Circle(r)`,
                    // `Shape.Circle(r)`, `channel.Message(v)`,
                    // `m.Shape.Circle(r)`.
                    let idx = self.add_constant(variant, span)?;
                    self.current_chunk().emit_op_u16(Op::Constant, idx, span);
                    self.compile_operands_above(1, args)?;
                    let argc = args.len() as u8;
                    self.emit_call(argc, tail, span);
                } else if let Some(builtin_name) = self.builtin_module_function(callee) {
                    // A builtin module's function: `list.map(...)`.
                    self.check_decode_target(&builtin_name, args.last(), span)?;
                    self.compile_operands(args)?;
                    let argc = args.len() as u8;
                    let name_idx = self.add_constant(Value::String(builtin_name), span)?;
                    self.current_chunk()
                        .emit_op_u16(Op::CallBuiltin, name_idx, span);
                    self.current_chunk().emit_u8(argc, span);
                } else if let ExprKind::FieldAccess(receiver, method, _) = &callee.kind {
                    if self.builtin_trait_method_of_builtin_type(callee) && !args.is_empty() {
                        // `Int.display(1)`: a builtin trait's method of a
                        // builtin type, which is native, not a global; the
                        // first argument is the receiver.
                        self.compile_operands(args)?;
                        self.emit_call_method(*method, args.len() as u8, callee.res, span)?;
                    } else if let Some(slot) = self.qualified_type_member(callee)? {
                        // `Pt.make(1)`, `m.Pt.make(1)`: a method reached
                        // through its type.
                        self.current_chunk().emit_op_u16(Op::GetGlobal, slot, span);
                        self.compile_operands_above(1, args)?;
                        let argc = args.len() as u8;
                        self.emit_call(argc, tail, span);
                    } else if let Some(def) = self.value_def(callee.res) {
                        // A module's function: `m.f(1)`.
                        self.emit_global_value(def, span)?;
                        self.compile_operands_above(1, args)?;
                        let argc = args.len() as u8;
                        self.emit_call(argc, tail, span);
                    } else {
                        // Method call on a value: expr.method(args)
                        // Compile receiver as first argument. The
                        // receiver takes one slot of the 255-argument
                        // budget so the explicit-arg cap is 254 here.
                        if args.len() >= u8::MAX as usize {
                            return Err(Diagnostic::error(
                                Code::CompileLimit,
                                span,
                                format!(
                                    "method call has {} arguments (plus receiver); silt calls are limited to 255",
                                    args.len()
                                ),
                            ));
                        }
                        self.compile_operands(std::iter::once(&**receiver).chain(args))?;
                        let argc = (args.len() + 1) as u8; // receiver + args
                        self.emit_call_method(*method, argc, callee.res, span)?;
                    }
                } else {
                    // Normal function call. A decoder imported by name
                    // (`import json.{ parse }`) is checked like
                    // `json.parse(..)`.
                    if let Some(builtin_name) = self.builtin_function(callee.res) {
                        self.check_decode_target(&builtin_name, args.last(), span)?;
                    }
                    self.compile_operands(std::iter::once(&**callee).chain(args))?;
                    let argc = args.len() as u8;
                    self.emit_call(argc, tail, span);
                }
            }

            // A variant: `EnumName.Variant`, `time.Monday`, `m.Color.Red`.
            ExprKind::FieldAccess(..) if let Some(variant) = self.variant_value(expr) => {
                let idx = self.add_constant(variant, span)?;
                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
            }

            // `m.Pt` used as a value: the type's descriptor.
            ExprKind::FieldAccess(..) if let Some(descriptor) = self.type_value(expr.res) => {
                let idx = self.add_constant(descriptor, span)?;
                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
            }

            // `Int.display` as a value: the function `{ a -> a.display() }`
            // (`{ a, b -> a.compare(b) }` for the two-argument methods).
            ExprKind::FieldAccess(_, method, _)
                if self.builtin_trait_method_of_builtin_type(expr) =>
            {
                let names: &[&str] = match resolve(*method).as_str() {
                    "compare" | "equal" => &["__self__", "__other__"],
                    _ => &["__self__"],
                };
                let ident = |name: &str| Expr::new(ExprKind::Ident(intern(name)), span);
                let call = Expr::new(
                    ExprKind::Call(
                        Box::new(Expr::new(
                            ExprKind::FieldAccess(Box::new(ident(names[0])), *method, span),
                            span,
                        )),
                        names[1..].iter().map(|n| ident(n)).collect(),
                    ),
                    span,
                );
                let lambda = Expr::new(
                    ExprKind::Lambda {
                        params: names
                            .iter()
                            .map(|n| crate::ast::Param {
                                kind: crate::ast::ParamKind::Data,
                                pattern: crate::ast::Pattern::new(
                                    crate::ast::PatternKind::Ident(intern(n)),
                                    span,
                                ),
                                ty: None,
                            })
                            .collect(),
                        body: Box::new(call),
                    },
                    span,
                );
                self.compile_expr(&lambda)?;
            }

            ExprKind::FieldAccess(..) if let Some(slot) = self.qualified_type_member(expr)? => {
                self.current_chunk().emit_op_u16(Op::GetGlobal, slot, span);
            }

            // `m.f`, `m.limit`, `list.map` used as a value.
            ExprKind::FieldAccess(..) if let Some(def) = self.value_def(expr.res) => {
                self.emit_global_value(def, span)?;
            }

            ExprKind::FieldAccess(expr, field, _) => {
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
                    self.current_chunk()
                        .emit_op_u16(Op::GetField, name_idx, span);
                }
            }

            ExprKind::StringInterp(parts) => {
                // The StringConcat part count is encoded as a `u8` in
                // bytecode. Silently wrapping via the `u8` counter used
                // to let a 300-segment interpolation compile with
                // count=44 (and panic the compiler outright in debug
                // builds); the VM then popped the wrong number of stack
                // values and produced garbled output. Reject at compile
                // time instead, mirroring the call/tuple/record guards.
                if parts.len() > u8::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        format!(
                            "string interpolation has {} segments; silt string interpolations are limited to 255",
                            parts.len()
                        ),
                    ));
                }
                // Every part stays on the stack until `StringConcat`.
                let base = self.ctx().height;
                let mut count: u8 = 0;
                for part in parts {
                    match part {
                        StringPart::Literal(s) => {
                            let idx = self.add_constant(Value::String(s.clone()), span)?;
                            self.current_chunk().emit_op_u16(Op::Constant, idx, span);
                        }
                        StringPart::Expr(e) => {
                            self.compile_expr(e)?;
                            self.current_chunk().emit_op(Op::DisplayValue, span);
                        }
                    }
                    self.ctx_mut().height += 1;
                    count += 1;
                }
                self.ctx_mut().height = base;
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

            ExprKind::Lambda { params, body, .. } => {
                if params.len() > u8::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        format!(
                            "closure has {} parameters; silt functions are limited to 255",
                            params.len()
                        ),
                    ));
                }
                let arity = params.len() as u8;

                // Push a new context for the lambda body.
                self.contexts
                    .push(CompileContext::new("<lambda>".into(), arity));

                self.compile_params(params, span)?;

                // Compile the lambda body in tail position for TCO.
                self.in_tail_position = true;
                self.compile_expr(body)?;
                self.in_tail_position = false;
                self.current_chunk().emit_op(Op::Return, span);

                let ctx = self.contexts.pop().ok_or(Diagnostic::error(
                    Code::CompilerBug,
                    span,
                    "compiler bug: missing lambda context",
                ))?;
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
                    self.current_chunk().emit_op_u16(Op::Constant, fi, span);
                } else {
                    // Has upvalues: emit MakeClosure with descriptors.
                    self.current_chunk().emit_op_u16(Op::MakeClosure, fi, span);
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
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        "tuple cannot have more than 255 elements",
                    ));
                }
                self.compile_operands(elems)?;
                self.current_chunk().emit_op(Op::MakeTuple, span);
                self.current_chunk().emit_u8(elems.len() as u8, span);
            }

            ExprKind::List(elems) => {
                // The MakeList / MakeMap / MakeSet opcodes encode their
                // element count in a u16 operand. Anything larger would
                // silently wrap and the VM would `truncate` the stack by a
                // completely wrong number, leaving orphaned values that
                // corrupt every subsequent operation (B2). Reject oversized
                // literals at compile time with a clear error — the same
                // shape as the `u8`-bounded tuple/record checks above.
                if elems.len() > u16::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        format!(
                            "list literal too large: {} elements (max {})",
                            elems.len(),
                            u16::MAX
                        ),
                    ));
                }
                let has_spread = elems.iter().any(|e| matches!(e, ListElem::Spread(_)));
                if !has_spread {
                    // Fast path: no spreads, just compile all singles
                    self.compile_operands(elems.iter().filter_map(|elem| match elem {
                        ListElem::Single(e) => Some(e),
                        ListElem::Spread(_) => None,
                    }))?;
                    let count = elems.len() as u16;
                    self.current_chunk().emit_op_u16(Op::MakeList, count, span);
                } else {
                    // Spread path: group consecutive singles into segments,
                    // compile each spread, and ListConcat them together.
                    // `single_count` is a u16 and could wrap on >65535
                    // consecutive singles between spreads even when the
                    // outer `elems.len()` bound above catches the overall
                    // literal. Use a usize accumulator and check the bound
                    // on every increment.
                    //
                    // While an element is compiled the stack holds the list
                    // accumulated so far (if any) and the singles not yet
                    // collected; the frame height counts them.
                    let base = self.ctx().height;
                    let mut have_accumulated = false;
                    let mut single_count: usize = 0;

                    for elem in elems {
                        self.ctx_mut().height = base + usize::from(have_accumulated) + single_count;
                        match elem {
                            ListElem::Single(e) => {
                                self.compile_expr(e)?;
                                single_count += 1;
                                if single_count > u16::MAX as usize {
                                    return Err(Diagnostic::error(
                                        Code::CompileLimit,
                                        span,
                                        format!(
                                            "list literal too large: more than {} consecutive \
                                             singleton elements between spreads",
                                            u16::MAX
                                        ),
                                    ));
                                }
                            }
                            ListElem::Spread(e) => {
                                // Flush any pending singles as a MakeList
                                if single_count > 0 {
                                    self.current_chunk().emit_op_u16(
                                        Op::MakeList,
                                        single_count as u16,
                                        span,
                                    );
                                    if have_accumulated {
                                        self.current_chunk().emit_op(Op::ListConcat, span);
                                    }
                                    have_accumulated = true;
                                    single_count = 0;
                                    self.ctx_mut().height = base + 1;
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
                    self.ctx_mut().height = base;
                    // Flush any trailing singles
                    if single_count > 0 {
                        self.current_chunk()
                            .emit_op_u16(Op::MakeList, single_count as u16, span);
                        if have_accumulated {
                            self.current_chunk().emit_op(Op::ListConcat, span);
                        }
                    } else if !have_accumulated {
                        // Edge case: empty list with spreads (shouldn't happen, but be safe)
                        self.current_chunk().emit_op_u16(Op::MakeList, 0, span);
                    }
                }
            }

            ExprKind::Map(pairs) => {
                // MakeMap pair count is emitted as u16 — reject oversized
                // literals at compile time so the VM never sees a wrapped
                // count. See the B2 comment on the list path above.
                if pairs.len() > u16::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        format!(
                            "map literal too large: {} pairs (max {})",
                            pairs.len(),
                            u16::MAX
                        ),
                    ));
                }
                self.compile_operands(pairs.iter().flat_map(|(k, v)| [k, v]))?;
                let pair_count = pairs.len() as u16;
                self.current_chunk()
                    .emit_op_u16(Op::MakeMap, pair_count, span);
            }

            ExprKind::SetLit(elems) => {
                // MakeSet count is emitted as u16 — reject oversized
                // literals at compile time (B2).
                if elems.len() > u16::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        format!(
                            "set literal too large: {} elements (max {})",
                            elems.len(),
                            u16::MAX
                        ),
                    ));
                }
                self.compile_operands(elems)?;
                let count = elems.len() as u16;
                self.current_chunk().emit_op_u16(Op::MakeSet, count, span);
            }

            ExprKind::Range(start, end) => {
                self.compile_operands([&**start, &**end])?;
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

            // The literal's type is the one the resolver resolved it to,
            // written `Pt { .. }` or `util.Pt { .. }`.
            ExprKind::RecordCreate {
                module: _,
                name_span: _,
                name,
                fields,
            } => {
                if fields.len() > u8::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        "record cannot have more than 255 fields",
                    ));
                }
                // Push field values in order
                let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                self.compile_operands(fields.iter().map(|(_, val)| val))?;
                let ty = self.record_type(expr.res, *name, span)?;
                let type_name_idx = self.add_constant(Value::TypeDescriptor(ty), span)?;
                self.current_chunk()
                    .emit_op_u16(Op::MakeRecord, type_name_idx, span);
                self.current_chunk().emit_u8(field_names.len() as u8, span);
                for fname in &field_names {
                    let field_idx = self.add_constant(Value::String(resolve(*fname)), span)?;
                    self.current_chunk().emit_u16(field_idx, span);
                }
            }

            ExprKind::RecordUpdate { expr, fields } => {
                if fields.len() > u8::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        "record update cannot have more than 255 fields",
                    ));
                }
                let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                self.compile_operands(
                    std::iter::once(&**expr).chain(fields.iter().map(|(_, val)| val)),
                )?;
                self.current_chunk().emit_op(Op::RecordUpdate, span);
                self.current_chunk().emit_u8(field_names.len() as u8, span);
                for fname in &field_names {
                    let field_idx = self.add_constant(Value::String(resolve(*fname)), span)?;
                    self.current_chunk().emit_u16(field_idx, span);
                }
            }

            ExprKind::AnonRecord { spread, fields } => {
                if fields.len() > u8::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        "anon record literal cannot have more than 255 fields",
                    ));
                }
                if let Some(base) = spread {
                    // Extend op: compile base, then RecordUpdate-style merge
                    // (RecordUpdate already supports adding new fields too).
                    //
                    // Round 83 originally emitted a sibling `RecordUpdateAnon`
                    // opcode that rebranded the result's `type_name` to
                    // `"<anon>"` so a spread of a nominal record would
                    // compare equal to an anon-record literal of the same
                    // shape. Round 85 follow-up removed the sibling opcode:
                    // rounds 84-85 made `Value::PartialEq` / `Value::Ord` /
                    // `Value::Hash` treat `<anon>` as a wildcard on either
                    // side, which closes the equality, ordering, and
                    // hashing surfaces uniformly without a runtime
                    // rebrand. Locks: tests/lang/round83_anonrec_spread_eq_tests.rs
                    // (PartialEq), tests/typecheck/round85_anonrec_hash_ord_contract_tests.rs
                    // (Hash + Ord + Set contract).
                    let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                    self.compile_operands(
                        std::iter::once(&**base).chain(fields.iter().map(|(_, val)| val)),
                    )?;
                    self.current_chunk().emit_op(Op::RecordUpdate, span);
                    self.current_chunk().emit_u8(field_names.len() as u8, span);
                    for fname in &field_names {
                        let field_idx = self.add_constant(Value::String(resolve(*fname)), span)?;
                        self.current_chunk().emit_u16(field_idx, span);
                    }
                } else {
                    // Closed anon record literal: same encoding as nominal
                    // RecordCreate but with the anonymous record type,
                    // which every run-time record-type check accepts (see
                    // `bytecode::record_type_matches`).
                    let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                    self.compile_operands(fields.iter().map(|(_, val)| val))?;
                    let anon = crate::typeinfo::builtin_type(crate::typeinfo::ty::ANON_RECORD);
                    let type_name_idx =
                        self.add_constant(Value::TypeDescriptor(anon.clone()), span)?;
                    self.current_chunk()
                        .emit_op_u16(Op::MakeRecord, type_name_idx, span);
                    self.current_chunk().emit_u8(field_names.len() as u8, span);
                    for fname in &field_names {
                        let field_idx = self.add_constant(Value::String(resolve(*fname)), span)?;
                        self.current_chunk().emit_u16(field_idx, span);
                    }
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
                    .ok_or_else(|| checker_missed(span, "a `loop(...)` outside a loop"))?;
                let first_slot = loop_info.first_slot;
                let loop_start = loop_info.loop_start;
                let expected = loop_info.binding_count as usize;
                if args.len() != expected {
                    return Err(checker_missed(
                        span,
                        "a `loop(...)` with the wrong number of arguments",
                    ));
                }
                // Defence in depth: `binding_count` is already a u8 so
                // `expected <= 255` — but keep the limit explicit so a
                // future refactor that widens `binding_count` doesn't
                // silently reintroduce a wrap.
                if args.len() > u8::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        format!(
                            "`loop(...)` has {} arguments; silt loops are limited to 255 bindings",
                            args.len()
                        ),
                    ));
                }

                self.compile_operands(args)?;
                self.current_chunk().emit_op(Op::Recur, span);
                self.current_chunk().emit_u8(args.len() as u8, span);
                self.current_chunk().emit_u16(first_slot, span);

                // Emit JumpBack to loop start.
                let current_offset = self.current_chunk().len();
                // JumpBack operand is how far back to jump from after the operand.
                let jump_back_dist = current_offset + 3 - loop_start; // +3 for opcode + u16
                // Mirror `Chunk::patch_jump`: the operand is a `u16`, so a
                // loop body larger than 65_535 bytes of bytecode would wrap
                // and jump to a garbage offset. Reject it cleanly.
                jumpback_fits_u16(jump_back_dist, span)?;
                self.current_chunk()
                    .emit_op_u16(Op::JumpBack, jump_back_dist as u16, span);
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
    ) -> Result<(), Diagnostic> {
        // ── Guardless match (no scrutinee) ───────────────────────
        let Some(scrutinee) = scrutinee else {
            return self.compile_guardless_match(arms, span, tail);
        };

        // Compile the scrutinee and keep it in the frame as a hidden local.
        // This lets us GetLocal it for each arm's test and binding.
        self.compile_expr(scrutinee)?;
        self.begin_scope();
        let scrutinee_slot = self.add_local(intern("__scrutinee__"), span)?;
        self.current_chunk()
            .emit_op_u16(Op::SetLocal, scrutinee_slot, span);
        // Frame height at the start of every arm: everything up to and
        // including the scrutinee.
        let arm_height = self.ctx().height;

        let mut end_jumps = Vec::new();

        for (i, arm) in arms.iter().enumerate() {
            // 1. Push scrutinee for testing. An arm that did not match
            //    lands here with values still above the scrutinee: the
            //    copy its test looked at, sub-values of a nested pattern,
            //    or the names it bound before its guard failed. The slide
            //    drops them and keeps the fresh copy.
            self.current_chunk()
                .emit_op_u16(Op::GetLocal, scrutinee_slot, span);
            if i > 0 {
                self.emit_slide(arm_height, span)?;
            }

            // 2. Test the pattern (value is on TOS, tests peek it)
            let fail_jumps = self.compile_pattern_test(&arm.pattern, span)?;

            // 3. Pop the test copy
            self.current_chunk().emit_op(Op::Pop, span);

            // 4. Begin a scope for this arm's bindings
            self.begin_scope();

            // 5. Push scrutinee again and bind pattern variables
            self.current_chunk()
                .emit_op_u16(Op::GetLocal, scrutinee_slot, span);
            // Register this GetLocal'd copy as a hidden local
            let bind_copy = self.add_local(intern("__bind_src__"), span)?;
            self.current_chunk()
                .emit_op_u16(Op::SetLocal, bind_copy, span);
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

            // The arm's bindings stay under its result until the end of
            // the match, where one slide serves every arm.
            self.end_scope();

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
        self.current_chunk()
            .emit_op_u16(Op::Constant, msg_idx, span);
        self.current_chunk().emit_op(Op::Panic, span);

        let result_height = self.end_scope();

        // Patch all end jumps to here
        for ej in end_jumps {
            self.patch_jump(ej, span)?;
        }

        // Every arm arrives with its result on top of the scrutinee and of
        // whatever the arm left in the frame. Move the result to where the
        // scrutinee was.
        if !tail {
            self.emit_slide(result_height, span)?;
        }

        Ok(())
    }

    /// Compile a guardless match: `match { cond1 -> body1, ... }`
    fn compile_guardless_match(
        &mut self,
        arms: &[MatchArm],
        span: Span,
        tail: bool,
    ) -> Result<(), Diagnostic> {
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

        // No arm matched — panic. The typechecker requires a final `_`
        // arm, so like the scrutinee form's non-exhaustive panic this is
        // only a backstop.
        let msg_idx = self.add_constant(
            Value::String("non-exhaustive match: no condition was true".into()),
            span,
        )?;
        self.current_chunk()
            .emit_op_u16(Op::Constant, msg_idx, span);
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
    ) -> Result<(), Diagnostic> {
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
                // The piped value takes one slot, so the explicit-arg
                // cap is 254 here. Reject before the `+1` can wrap.
                if args.len() >= u8::MAX as usize {
                    return Err(Diagnostic::error(
                        Code::CompileLimit,
                        span,
                        format!(
                            "pipe call has {} arguments (plus piped value); silt calls are limited to 255",
                            args.len()
                        ),
                    ));
                }
                if let Some(builtin_name) = self.builtin_module_function(callee) {
                    // With a piped value the type argument of a decoding
                    // builtin is still the last explicit argument.
                    self.check_decode_target(&builtin_name, args.last(), span)?;
                    // Builtins: val on stack first, then args
                    self.compile_operands(std::iter::once(left).chain(args))?;
                    let argc = (args.len() + 1) as u8;
                    let name_idx = self.add_constant(Value::String(builtin_name), span)?;
                    self.current_chunk()
                        .emit_op_u16(Op::CallBuiltin, name_idx, span);
                    self.current_chunk().emit_u8(argc, span);
                } else {
                    if let Some(builtin_name) = self.builtin_function(callee.res) {
                        self.check_decode_target(&builtin_name, args.last(), span)?;
                    }
                    // Non-builtin: callee first, then val, then args
                    self.compile_operands([&**callee, left].into_iter().chain(args))?;
                    let argc = (args.len() + 1) as u8;
                    self.emit_call(argc, tail, span);
                }
            }
            _ => {
                // val |> f: callee first, then val
                self.compile_operands([right, left])?;
                self.emit_call(1, tail, span);
            }
        }
        Ok(())
    }

    // ── Loop compilation ─────────────────────────────────────────

    fn compile_loop(
        &mut self,
        bindings: &[(Symbol, Span, Expr)],
        body: &Expr,
        span: Span,
    ) -> Result<(), Diagnostic> {
        // `binding_count` is stored in `LoopInfo` as a `u8`, so more
        // than 255 bindings would silently wrap and cause `recur`
        // arity mismatches to be misreported. Reject up front.
        if bindings.len() > u8::MAX as usize {
            return Err(Diagnostic::error(
                Code::CompileLimit,
                span,
                format!(
                    "loop has {} bindings; silt loops are limited to 255",
                    bindings.len()
                ),
            ));
        }

        self.begin_scope();

        // The bindings occupy the slots from the current frame height on;
        // `Recur` writes the new values there and cuts the frame back to
        // just above them. With no bindings that is the frame as it is
        // now, with every enclosing local still in place.
        let first_slot = frame_slot(self.ctx().height, span)?;

        // Compile initial values; each stays on the stack as its binding.
        for (name, _, init) in bindings {
            self.compile_expr(init)?;
            let slot = self.add_local(*name, span)?;
            self.current_chunk().emit_op_u16(Op::SetLocal, slot, span);
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
        // or fell through with the final value on the stack, above the
        // bindings.
        self.end_scope_with_result(false, span)
    }

    // ── Helper: what names resolve to ────────────────────────────

    /// The name of `ty` as values of it print: its name, qualified by its
    /// module ([`ModuleUnit::qualifier`]) when two types of the
    /// program's modules have it.
    fn display_type_name(&self, ty: TypeRef) -> String {
        if crate::defs::builtin_types()
            .get(ty.id.0.0 as usize)
            .is_some()
            || !self.program_clashes.contains(&ty.name)
        {
            return resolve(ty.name);
        }
        let module = self.units.defs.get(ty.id.0).module;
        match self.units.modules.iter().find(|unit| unit.id == module) {
            Some(unit) => format!("{}.{}", unit.qualifier, ty.name),
            None => resolve(ty.name),
        }
    }

    /// The run-time description of the type `id`, which values of the
    /// type carry; described once per compilation, and given to the VM.
    fn type_info(&self, id: crate::defs::TypeId) -> Arc<TypeInfo> {
        if crate::defs::builtin_types().get(id.0.0 as usize).is_some() {
            return crate::typeinfo::builtin_type(id).clone();
        }
        if let Some(info) = self.types.borrow().get(id) {
            return info.clone();
        }
        let defs = &self.units.defs;
        let def = *defs.get(id.0);
        let ty = TypeRef { id, name: def.name };
        let variants = defs.variants(id.0);
        let mut nested = Vec::new();
        let shape = if !variants.is_empty() {
            Shape::Enum(
                variants
                    .iter()
                    .map(|v| {
                        let variant = defs.get(*v);
                        let arity = match variant.kind {
                            crate::defs::DefKind::Variant { arity, .. } => arity,
                            _ => 0,
                        };
                        VariantInfo {
                            name: resolve(variant.name),
                            arity,
                        }
                    })
                    .collect(),
            )
        } else {
            let fields = self.declared_record_fields(def.span).unwrap_or_default();
            Shape::Record(
                fields
                    .iter()
                    .map(|f| {
                        let field_type = self
                            .describe_field_type(&f.ty, &mut Vec::new(), &mut Vec::new())
                            .unwrap_or_else(|_| FieldType::Unsupported(render_type_expr(&f.ty)));
                        collect_records(&field_type, &mut nested);
                        (resolve(f.name), field_type)
                    })
                    .collect(),
            )
        };
        let info = Arc::new(TypeInfo {
            id,
            name: self.display_type_name(ty),
            shape,
        });
        self.types.borrow_mut().insert(info.clone());
        // The record types of its fields, which a decoder builds too.
        for record in nested {
            self.type_info(record);
        }
        info
    }

    /// The declaration of the type declared with its name at `span`.
    fn type_declaration(&self, span: Span) -> Option<&crate::ast::TypeDecl> {
        let programs = self
            .units
            .modules
            .iter()
            .map(|unit| &unit.program)
            .chain(self.units.earlier.programs.iter());
        for program in programs {
            for decl in &program.decls {
                if let Decl::Type(td) = decl
                    && td.name_span == span
                {
                    return Some(td);
                }
            }
        }
        None
    }

    /// The fields of the record type declared with its name at `span`.
    fn declared_record_fields(&self, span: Span) -> Option<Vec<RecordField>> {
        match &self.type_declaration(span)?.body {
            TypeBody::Record(fields) => Some(fields.clone()),
            _ => None,
        }
    }

    /// The variant a resolution names (a constructor pattern, a variant
    /// used as a value). With no resolution (the derived impls of the
    /// builtin types, which the builtin environment makes) it is a
    /// builtin variant, named by its name: builtin variant names are
    /// unique among the builtins.
    fn variant_tag(&self, res: Option<crate::defs::Res>, name: Symbol) -> Option<Tag> {
        if let Some(res) = res {
            let crate::defs::Res::Def(id) = res else {
                return None;
            };
            let crate::defs::DefKind::Variant { ty, ordinal, .. } = self.units.defs.get(id).kind
            else {
                return None;
            };
            return Some(Tag::new(self.type_info(ty), ordinal));
        }
        let name = resolve(name);
        module::builtin_enum_variants()
            .iter()
            .find(|(_, variants)| variants.contains(&name.as_str()))
            .and_then(|(ty, _)| crate::typeinfo::builtin_type_named(ty))
            .and_then(|ty| Tag::named(ty, &name))
    }

    /// The tag of the variant a constructor pattern names.
    fn pattern_tag(
        &self,
        res: Option<crate::defs::Res>,
        name: Symbol,
        span: Span,
    ) -> Result<Tag, Diagnostic> {
        self.variant_tag(res, name)
            .ok_or_else(|| checker_missed(span, &format!("the unresolved variant '{name}'")))
    }

    /// The descriptor of the type a record literal or pattern names.
    fn record_type(
        &self,
        res: Option<crate::defs::Res>,
        name: Symbol,
        span: Span,
    ) -> Result<Arc<TypeInfo>, Diagnostic> {
        match self.res_type(res) {
            Some(ty) => Ok(self.type_info(ty.id)),
            None => crate::typeinfo::builtin_type_named(&resolve(name))
                .cloned()
                .ok_or_else(|| checker_missed(span, &format!("the unresolved type '{name}'"))),
        }
    }

    /// The type a resolution names, if it names a record or enum type.
    fn res_type(&self, res: Option<crate::defs::Res>) -> Option<TypeRef> {
        let Some(crate::defs::Res::Def(id)) = res else {
            return None;
        };
        let def = self.units.defs.get(id);
        matches!(def.kind, crate::defs::DefKind::Type(_)).then_some(TypeRef {
            id: crate::defs::TypeId(id),
            name: def.name,
        })
    }

    /// The value of a type used as a value, if the resolution names a
    /// type: a primitive type's descriptor, or the descriptor of a record,
    /// enum or container type.
    fn type_value(&self, res: Option<crate::defs::Res>) -> Option<Value> {
        let ty = self.res_type(res)?;
        let name = resolve(ty.name);
        if crate::defs::builtin_types()
            .get(ty.id.0.0 as usize)
            .is_some()
            && module::BUILTIN_PRIMITIVE_NAMES.contains(&name.as_str())
        {
            return Some(Value::PrimitiveDescriptor(name));
        }
        Some(Value::TypeDescriptor(self.type_info(ty.id)))
    }

    /// The definition a resolution names, if it names a value with a
    /// global: a top-level function or `let`, a host function, or a
    /// builtin function or constant.
    fn value_def(&self, res: Option<crate::defs::Res>) -> Option<crate::defs::DefId> {
        let Some(crate::defs::Res::Def(id)) = res else {
            return None;
        };
        matches!(
            self.units.defs.get(id).kind,
            crate::defs::DefKind::Fn | crate::defs::DefKind::Let | crate::defs::DefKind::Host
        )
        .then_some(id)
    }

    /// The qualified name (`list.map`, `println`) of the builtin function
    /// or constant a resolution names.
    fn builtin_function(&self, res: Option<crate::defs::Res>) -> Option<String> {
        let def = self.units.defs.get(self.value_def(res)?);
        if !def.module.is_builtin() {
            return None;
        }
        Some(match def.module.builtin_name() {
            Some(module) => format!("{module}.{}", def.name),
            None => resolve(def.name),
        })
    }

    /// If the callee is a builtin module's function (`list.map`, or
    /// `l.map` after `import list as l`), its qualified name: the call is
    /// a `CallBuiltin`.
    fn builtin_module_function(&self, callee: &Expr) -> Option<String> {
        match &callee.kind {
            ExprKind::FieldAccess(..) => self.builtin_function(callee.res),
            _ => None,
        }
    }

    /// Push the value of the definition `def`: its global, or for a
    /// builtin function or constant, the function or the constant.
    fn emit_global_value(&mut self, def: crate::defs::DefId, span: Span) -> Result<(), Diagnostic> {
        if let Some(name) = self.builtin_function(Some(crate::defs::Res::Def(def))) {
            let value = module::builtin_constant_value(&name).unwrap_or(Value::BuiltinFn(name));
            let idx = self.add_constant(value, span)?;
            self.current_chunk().emit_op_u16(Op::Constant, idx, span);
            return Ok(());
        }
        let slot = self.globals.def(def).ok_or_else(|| {
            checker_missed(
                span,
                &format!(
                    "the definition '{}' with no global",
                    self.units.defs.get(def).name
                ),
            )
        })?;
        self.current_chunk().emit_op_u16(Op::GetGlobal, slot, span);
        Ok(())
    }

    /// The type the module being compiled declares as `name`.
    fn declared_type(&self, name: Symbol) -> Option<crate::defs::TypeId> {
        let current = self.unit_stack.last().copied().unwrap_or(self.units.entry);
        let defs = &self.units.defs;
        let unit = self.units.modules.get(current)?;
        defs.of_module(unit.id)
            .iter()
            .copied()
            .find(|id| {
                let def = defs.get(*id);
                def.name == name && matches!(def.kind, crate::defs::DefKind::Type(_))
            })
            .map(crate::defs::TypeId)
    }

    /// The type the impl `ti` is for, as the checker keys impls and the
    /// VM dispatches: the canonical head of its target (`Range` is
    /// `List`, an alias is the type it stands for). `None` for a target
    /// that names no type (`trait Display for a`).
    fn impl_type(&self, ti: &crate::ast::TraitImpl) -> Option<crate::defs::TypeId> {
        let written = match ti.target_res {
            Some(crate::defs::Res::Def(id)) => Some(TypeRef {
                id: crate::defs::TypeId(id),
                name: self.units.defs.get(id).name,
            }),
            _ => {
                let name = resolve(ti.target_type);
                let name = if name == "()" { "Unit" } else { name.as_str() };
                crate::defs::builtin_type_id(name).map(|id| TypeRef {
                    id,
                    name: ti.target_type,
                })
            }
        };
        Some(canonical_head(self.resolver(), written?).id)
    }

    /// The trait the impl `ti` is of. The derived impls of the builtin
    /// types, which the builtin environment makes, name a builtin trait
    /// unresolved.
    fn impl_trait(&self, ti: &crate::ast::TraitImpl) -> Option<crate::defs::TraitId> {
        match ti.trait_res {
            Some(crate::defs::Res::Def(id)) => Some(crate::defs::TraitId(id)),
            _ => crate::defs::builtin_trait_id(&resolve(ti.trait_name)),
        }
    }

    /// The trait a method call's resolution names: the trait of the
    /// method the checker resolved the call to. `None` when it is not
    /// known where the call is compiled.
    fn res_trait(&self, res: Option<crate::defs::Res>) -> Option<crate::defs::TraitId> {
        let Some(crate::defs::Res::Def(id)) = res else {
            return None;
        };
        match self.units.defs.get(id).kind {
            crate::defs::DefKind::Trait(t) => Some(t),
            _ => None,
        }
    }

    /// Emit `CallMethod` of `method` with `argc` values (the receiver
    /// first) on the stack, for the trait the call's resolution `res`
    /// names.
    fn emit_call_method(
        &mut self,
        method: Symbol,
        argc: u8,
        res: Option<crate::defs::Res>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let method_idx = self.add_constant(Value::String(resolve(method)), span)?;
        let trait_operand = match self.res_trait(res) {
            Some(t) => {
                let name = self.units.defs.get(t.0).name;
                self.globals
                    .trait_index(t, resolve(name))
                    .ok_or_else(|| too_many_globals(span))?
            }
            None => crate::bytecode::NO_TRAIT,
        };
        self.current_chunk()
            .emit_op_u16(Op::CallMethod, method_idx, span);
        self.current_chunk().emit_u8(argc, span);
        self.current_chunk().emit_u16(trait_operand, span);
        Ok(())
    }

    /// Whether `callee` is `T.method` for a builtin type (`Int`, `List`,
    /// `io.IoError`, ...) and a method of a builtin trait (Display,
    /// Compare, Equal, Hash, Error), which the VM implements natively for
    /// it, or as a derived impl it dispatches to.
    fn builtin_trait_method_of_builtin_type(&self, callee: &Expr) -> bool {
        let ExprKind::FieldAccess(obj, method, _) = &callee.kind else {
            return false;
        };
        if !matches!(
            resolve(*method).as_str(),
            "display" | "compare" | "equal" | "hash" | "message"
        ) {
            return false;
        }
        let Some(crate::defs::Res::Def(id)) = obj.res else {
            return false;
        };
        let def = self.units.defs.get(id);
        def.module.is_builtin() && def.is_type()
    }

    /// The global slot of `T.method` or `m.T.method`, a method of a type
    /// reached through the type, as the resolver resolved `T` / `m.T`:
    /// the method of the impls for the type's canonical head. `None` when
    /// `expr` is not such an access.
    fn qualified_type_member(&self, expr: &Expr) -> Result<Option<u16>, Diagnostic> {
        let ExprKind::FieldAccess(obj, field, _) = &expr.kind else {
            return Ok(None);
        };
        if !matches!(obj.kind, ExprKind::FieldAccess(..) | ExprKind::Ident(_))
            || self.variant_value(expr).is_some()
        {
            return Ok(None);
        }
        let Some(crate::defs::Res::Def(id)) = obj.res else {
            return Ok(None);
        };
        let def = self.units.defs.get(id);
        if !def.is_type() {
            return Ok(None);
        }
        let ty = canonical_head(
            self.resolver(),
            TypeRef {
                id: crate::defs::TypeId(id),
                name: def.name,
            },
        );
        self.globals
            .method(self.res_trait(expr.res), ty.id, &resolve(*field))
            .map(Some)
            .ok_or_else(|| {
                checker_missed(
                    expr.span,
                    &format!("the method '{}.{field}' with no impl", def.name),
                )
            })
    }

    /// The value of the variant `expr` names, as the resolver resolved
    /// it (`Red`, `Color.Red`, `m.Red`, `m.Color.Red`): a nullary variant
    /// is the value, any other its constructor. Two enums may have
    /// variants of one name, so a variant is not looked up by its name.
    fn variant_value(&self, expr: &Expr) -> Option<Value> {
        let tag = self.variant_tag(Some(expr.res?), intern(""))?;
        Some(match tag.arity() {
            0 => Value::Variant(tag, Vec::new()),
            _ => Value::VariantConstructor(tag),
        })
    }

    // ── Record field types for the json / toml decoders ──────────

    /// The type of the record field `te` as the decoders see it, by what
    /// its names resolved to.
    ///
    /// `Err` carries the part of `te` no decoder exists for, as written.
    /// `open_aliases` holds the aliases being expanded (an alias that
    /// leads back to itself has no decoder). The record types the field
    /// type refers to are added to `records`.
    fn describe_field_type(
        &self,
        te: &TypeExpr,
        open_aliases: &mut Vec<crate::defs::DefId>,
        records: &mut Vec<crate::defs::TypeId>,
    ) -> Result<FieldType, String> {
        const NO_ARGS: &[TypeExpr] = &[];
        let args: &[TypeExpr] = match &te.kind {
            TypeExprKind::Named { .. } => NO_ARGS,
            TypeExprKind::Generic { args, .. } => args.as_slice(),
            TypeExprKind::Tuple(elems) if !elems.is_empty() => {
                let mut parts = Vec::with_capacity(elems.len());
                for elem in elems {
                    parts.push(self.describe_field_type(elem, open_aliases, records)?);
                }
                return Ok(FieldType::Tuple(parts));
            }
            _ => return Err(render_type_expr(te)),
        };
        let unsupported = || Err(render_type_expr(te));
        let Some(crate::defs::Res::Def(id)) = te.res else {
            // A type variable, or a name that resolved to nothing.
            return unsupported();
        };
        let def = *self.units.defs.get(id);
        match def.kind {
            crate::defs::DefKind::TypeAlias => {
                let Some(decl) = self.type_declaration(def.span) else {
                    return unsupported();
                };
                let TypeBody::Alias(target) = &decl.body else {
                    return unsupported();
                };
                if decl.params.len() != args.len() || open_aliases.contains(&id) {
                    return unsupported();
                }
                let target = substitute_type_params(target, &decl.params, args);
                open_aliases.push(id);
                let described = self.describe_field_type(&target, open_aliases, records);
                open_aliases.pop();
                described
            }
            crate::defs::DefKind::Type(ty) if def.module.is_builtin() => {
                // A builtin type, by its id. A range type is described
                // like the list type it is the same type as.
                let name = crate::defs::builtin_types()[ty.0.0 as usize].0;
                match (name, args) {
                    ("Int", []) => Ok(FieldType::Int),
                    ("Float", []) => Ok(FieldType::Float),
                    ("String", []) => Ok(FieldType::String),
                    ("Bool", []) => Ok(FieldType::Bool),
                    _ if ty == crate::typeinfo::ty::DATE && args.is_empty() => Ok(FieldType::Date),
                    _ if ty == crate::typeinfo::ty::TIME && args.is_empty() => Ok(FieldType::Time),
                    _ if ty == crate::typeinfo::ty::DATE_TIME && args.is_empty() => {
                        Ok(FieldType::DateTime)
                    }
                    ("List" | "Range", [elem]) => Ok(FieldType::List(Box::new(
                        self.describe_field_type(elem, open_aliases, records)?,
                    ))),
                    ("Option", [inner]) => Ok(FieldType::Option(Box::new(
                        self.describe_field_type(inner, open_aliases, records)?,
                    ))),
                    ("Map", [key, value]) => {
                        // The keys of a JSON object or a TOML table are
                        // strings.
                        let key_type = self.describe_field_type(key, open_aliases, &mut Vec::new());
                        if !matches!(key_type, Ok(FieldType::String)) {
                            return unsupported();
                        }
                        Ok(FieldType::Map(Box::new(self.describe_field_type(
                            value,
                            open_aliases,
                            records,
                        )?)))
                    }
                    // Set, Channel, functions, the other builtin types.
                    _ => unsupported(),
                }
            }
            // A non-generic record type of the program.
            crate::defs::DefKind::Type(ty)
                if args.is_empty()
                    && self.type_declaration(def.span).is_some_and(|decl| {
                        decl.params.is_empty() && matches!(decl.body, TypeBody::Record(_))
                    }) =>
            {
                records.push(ty);
                Ok(FieldType::Record(ty))
            }
            // Enums, generic records, ...
            _ => unsupported(),
        }
    }

    /// The first field that cannot be decoded, in the record type
    /// `record` or in a record type nested in it. `None` if there is none.
    fn undecodable_field(
        &self,
        record: crate::defs::TypeId,
        seen: &mut HashSet<crate::defs::TypeId>,
    ) -> Option<UndecodableField> {
        if !seen.insert(record) {
            return None;
        }
        let def = *self.units.defs.get(record.0);
        let fields = self.declared_record_fields(def.span)?;
        for field in &fields {
            let mut nested = Vec::new();
            match self.describe_field_type(&field.ty, &mut Vec::new(), &mut nested) {
                Err(part) => {
                    return Some(UndecodableField {
                        record: resolve(def.name),
                        field: resolve(field.name),
                        field_type: render_type_expr(&field.ty),
                        part,
                    });
                }
                Ok(_) => {
                    for nested_record in nested {
                        if let Some(found) = self.undecodable_field(nested_record, seen) {
                            return Some(found);
                        }
                    }
                }
            }
        }
        None
    }

    /// Check the type argument of a call of a decoding builtin
    /// (`json.parse(text, T)`, `toml.parse(text, T)` and their `_list` /
    /// `_map` forms), as the resolver resolved it. If `T` is a record type
    /// of the program, every field the decoder would have to fill must
    /// have a decoder; otherwise the call is a compile error that names
    /// the field and its type. An enum, a builtin container type, or a
    /// primitive type given to a decoder that only decodes records is a
    /// compile error as well. When the type argument is a variable (a
    /// `type a` parameter) the type is only known at run time, where the
    /// decoders report the same problems.
    fn check_decode_target(
        &self,
        builtin_name: &str,
        type_arg: Option<&Expr>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if !DECODING_BUILTINS.contains(&builtin_name) {
            return Ok(());
        }
        let Some(type_arg) = type_arg else {
            return Ok(());
        };
        let type_name = match &type_arg.kind {
            ExprKind::Ident(name) | ExprKind::FieldAccess(_, name, _) => resolve(*name),
            _ => return Ok(()),
        };
        let Some(ty) = self.res_type(type_arg.res) else {
            return Ok(());
        };
        let def = *self.units.defs.get(ty.id.0);
        if !def.module.is_builtin() {
            let enum_type = !self.units.defs.variants(ty.id.0).is_empty();
            if enum_type {
                return Err(Diagnostic::error(
                    Code::InvalidConstruct,
                    span,
                    format!(
                        "`{builtin_name}` cannot decode `{type_name}`: it is an enum type, and enums have no decoder"
                    ),
                )
                .with_help("decode into a record type"));
            }
            let Some(found) = self.undecodable_field(ty.id, &mut HashSet::new()) else {
                return Ok(());
            };
            let owner = if found.record == resolve(def.name) {
                String::new()
            } else {
                format!(" of `{}`, a record type nested in it,", found.record)
            };
            let part = if found.part == found.field_type {
                "which has no decoder".to_string()
            } else {
                format!("and `{}` has no decoder", found.part)
            };
            return Err(Diagnostic::error(
                Code::InvalidConstruct,
                span,
                format!(
                    "`{builtin_name}` cannot decode `{type_name}`: field `{}`{owner} has type `{}`, {part}",
                    found.field, found.field_type
                ),
            )
            .with_help(DECODABLE_TYPES_HELP));
        }
        if !crate::typeinfo::builtin_type(ty.id).variants().is_empty() {
            return Err(Diagnostic::error(
                Code::InvalidConstruct,
                span,
                format!(
                    "`{builtin_name}` cannot decode `{type_name}`: it is an enum type, and enums have no decoder"
                ),
            )
            .with_help("decode into a record type"));
        }
        // Builtin types, by their declaration. `json.parse`,
        // `json.parse_map` and `toml.parse_map` also decode the primitive
        // types; every other decoder, and every decoder given a container
        // type such as `List`, needs a record type.
        let builtin = crate::defs::builtin_types()[ty.id.0.0 as usize].0;
        let is_primitive = module::BUILTIN_PRIMITIVE_NAMES.contains(&builtin);
        let is_container = module::BUILTIN_GENERIC_CONTAINER_NAMES.contains(&builtin);
        let decodes_primitives = matches!(
            builtin_name,
            "json.parse" | "json.parse_map" | "toml.parse_map"
        );
        if is_container || (is_primitive && !decodes_primitives) {
            let accepted = if decodes_primitives {
                "Int, Float, String, Bool, or a record type"
            } else {
                "a record type"
            };
            return Err(Diagnostic::error(
                Code::InvalidConstruct,
                span,
                format!("`{builtin_name}` cannot decode `{type_name}`: its type argument must be {accepted}"),
            )
            .with_help(
                "declare a record type with a field of the type you want, and decode into the record",
            ));
        }
        Ok(())
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

    /// Add a constant to the current chunk, converting overflow to `Diagnostic`.
    fn add_constant(&mut self, value: Value, span: Span) -> Result<u16, Diagnostic> {
        self.current_chunk()
            .add_constant(value)
            .map_err(|msg| Diagnostic::error(Code::CompileLimit, span, msg))
    }

    /// Patch a jump in the current chunk, converting overflow to `Diagnostic`.
    fn patch_jump(&mut self, patch_offset: usize, span: Span) -> Result<(), Diagnostic> {
        self.current_chunk()
            .patch_jump(patch_offset)
            .map_err(|msg| Diagnostic::error(Code::CompileLimit, span, msg))
    }

    fn begin_scope(&mut self) {
        let ctx = self.ctx_mut();
        ctx.scope_depth += 1;
        ctx.scope_starts.push(ctx.height);
    }

    /// Leave the innermost scope: forget its locals and set the frame
    /// height back to what it was when the scope began. Returns that
    /// height. Emits nothing; the values of the scope's locals are still
    /// in the frame, and the caller decides where they are dropped (see
    /// `end_scope_with_result` and `emit_slide`).
    fn end_scope(&mut self) -> usize {
        let ctx = self.ctx_mut();
        let depth = ctx.scope_depth;
        // Pop locals belonging to the scope we are leaving.
        while ctx.locals.last().is_some_and(|l| l.depth >= depth) {
            ctx.locals.pop();
        }
        ctx.scope_depth -= 1;
        let start = ctx
            .scope_starts
            .pop()
            .expect("internal compiler error: end_scope without begin_scope");
        ctx.height = start;
        start
    }

    /// Leave the innermost scope when its result is on top of the stack,
    /// above the scope's locals, and drop the locals from under it. In
    /// tail position (`tail`) the result is returned at once and the
    /// frame goes with it, so nothing is emitted.
    fn end_scope_with_result(&mut self, tail: bool, span: Span) -> Result<(), Diagnostic> {
        let end = self.ctx().height;
        let start = self.end_scope();
        if end > start && !tail {
            self.emit_slide(start, span)?;
        }
        Ok(())
    }

    /// Emit `Slide`: the value on top of the stack becomes the value in
    /// slot `height`, and everything that was above that slot is dropped.
    /// Afterwards the frame holds `height` values plus that one.
    fn emit_slide(&mut self, height: usize, span: Span) -> Result<(), Diagnostic> {
        let slot = frame_slot(height, span)?;
        self.current_chunk().emit_op_u16(Op::Slide, slot, span);
        Ok(())
    }

    /// Make the value on top of the stack a local named `name`. Its slot
    /// is the current frame height, which is where that value is.
    /// (For a parameter the value is the argument the caller pushed.)
    fn add_local(&mut self, name: Symbol, span: Span) -> Result<u16, Diagnostic> {
        let slot = frame_slot(self.ctx().height, span)?;
        let ctx = self.ctx_mut();
        let depth = ctx.scope_depth;
        ctx.locals.push(Local { name, depth, slot });
        ctx.height += 1;
        Ok(slot)
    }

    /// Compile `operands` left to right so that their values are on the
    /// stack, in order, for the instruction the caller emits next. While
    /// an operand is compiled, the ones before it are counted in the
    /// frame height, so a local introduced by the operand (by a `match`,
    /// a block with `let`, a `loop`) gets a slot above them.
    fn compile_operands<'a>(
        &mut self,
        operands: impl IntoIterator<Item = &'a Expr>,
    ) -> Result<(), Diagnostic> {
        self.compile_operands_above(0, operands)
    }

    /// `compile_operands` for a construct that has just pushed `pending`
    /// values of its own, which stay on the stack below the operands (a
    /// callee or a receiver loaded from a global).
    fn compile_operands_above<'a>(
        &mut self,
        pending: usize,
        operands: impl IntoIterator<Item = &'a Expr>,
    ) -> Result<(), Diagnostic> {
        let base = self.ctx().height;
        self.ctx_mut().height = base + pending;
        for operand in operands {
            self.compile_expr(operand)?;
            self.ctx_mut().height += 1;
        }
        self.ctx_mut().height = base;
        Ok(())
    }

    /// Register a function's parameters as locals and destructure those
    /// written as patterns. The arguments are already in the frame, in
    /// slots `0..params.len()`.
    fn compile_params(&mut self, params: &[Param], span: Span) -> Result<(), Diagnostic> {
        let mut destructured = Vec::new();
        for (i, param) in params.iter().enumerate() {
            match &param.pattern.kind {
                PatternKind::Ident(name) => {
                    self.add_local(*name, span)?;
                }
                _ => {
                    let slot = self.add_local(intern(&format!("__param_{i}__")), span)?;
                    destructured.push((slot, &param.pattern));
                }
            }
        }
        for (slot, pattern) in destructured {
            // Bind the pattern's names from a copy of the argument above
            // the parameters.
            self.current_chunk().emit_op_u16(Op::GetLocal, slot, span);
            let copy = self.add_local(intern("__param_copy__"), span)?;
            self.current_chunk().emit_op_u16(Op::SetLocal, copy, span);
            self.compile_pattern_bind_checked(pattern, span)?;
        }
        Ok(())
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

    /// Resolve a variable as an upvalue by walking enclosing compile contexts.
    ///
    /// If the variable is found as a local in an enclosing scope, it is captured
    /// as an upvalue (is_local = true). If the enclosing scope already has it as
    /// an upvalue, it is chained through (is_local = false, transitive capture).
    fn resolve_upvalue(&mut self, name: Symbol, span: Span) -> Result<Option<u8>, Diagnostic> {
        let current_idx = self.contexts.len() - 1;
        if current_idx == 0 {
            return Ok(None); // Top-level script has no enclosing scope.
        }
        self.resolve_upvalue_in(name, current_idx, span)
    }

    fn resolve_upvalue_in(
        &mut self,
        name: Symbol,
        context_index: usize,
        span: Span,
    ) -> Result<Option<u8>, Diagnostic> {
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
            // Upvalues are captured by value (Silt is immutable); the
            // local itself needs no open/closed tracking — see
            // VmClosure doc in src/bytecode.rs.
            let index = if slot > u8::MAX as u16 {
                return Err(Diagnostic::error(
                    Code::CompileLimit,
                    span,
                    format!("cannot capture local in slot {slot} as upvalue (max slot 255)"),
                ));
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
                span,
            )?));
        }

        // Not a local in the enclosing scope -- try recursively as an upvalue.
        if let Some(parent_upvalue_idx) = self.resolve_upvalue_in(name, enclosing_idx, span)? {
            // The enclosing scope has it as an upvalue. Chain it.
            return Ok(Some(self.add_upvalue(
                context_index,
                UpvalueDesc {
                    is_local: false,
                    index: parent_upvalue_idx,
                },
                span,
            )?));
        }

        Ok(None)
    }

    /// Add an upvalue descriptor to a context, deduplicating. Returns
    /// `Err` if the context already holds the maximum of 255 upvalues;
    /// the bytecode format addresses upvalues with a single byte *and*
    /// stores the count as a `u8`, so the last legal index is 254 and
    /// the max count is 255. Anything beyond would either truncate the
    /// index to zero (`256 as u8 == 0`) or wrap `upvalue_count` (and
    /// the emitted `MakeClosure` count byte) to zero while still
    /// writing 2N operand bytes after it — those bytes would then be
    /// reinterpreted as bytecode at runtime. Mirrors the sibling
    /// bounds-check in `resolve_upvalue_in`'s "captured slot > 255"
    /// path so both hard limits surface as `Diagnostic` rather than
    /// panics or silent miscompiles.
    fn add_upvalue(
        &mut self,
        context_index: usize,
        desc: UpvalueDesc,
        span: Span,
    ) -> Result<u8, Diagnostic> {
        let ctx = &mut self.contexts[context_index];
        // Check if we already have this exact upvalue.
        for (i, existing) in ctx.upvalues.iter().enumerate() {
            if existing.is_local == desc.is_local && existing.index == desc.index {
                return Ok(i as u8);
            }
        }
        let index = ctx.upvalues.len();
        if index >= u8::MAX as usize {
            return Err(Diagnostic::error(
                Code::CompileLimit,
                span,
                format!(
                    "too many upvalues: closure captures more than {} values (max)",
                    u8::MAX as usize
                ),
            ));
        }
        ctx.upvalues.push(desc);
        ctx.function.upvalue_count = ctx.upvalues.len() as u8;
        Ok(index as u8)
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Op;

    /// The units of a program with no modules, for a compiler that
    /// compiles nothing.
    fn no_modules() -> ProgramUnits {
        ProgramUnits {
            modules: Vec::new(),
            entry: 0,
            defs: Arc::new(crate::typechecker::names::new_def_table()),
            earlier: EarlierCells::default(),
            resolver: Arc::new(Resolver::new()),
        }
    }

    /// Compile declarations (no main call) through a session and return
    /// all functions.
    fn compile(input: &str) -> Vec<Function> {
        crate::session::testing::compile_decls_str(input)
            .unwrap_or_else(|e| panic!("{e:?}"))
            .functions
    }

    /// Compile expecting an error, return the error.
    fn compile_err(input: &str) -> Diagnostic {
        crate::session::testing::compile_decls_str(input)
            .err()
            .and_then(|errors| errors.into_iter().next())
            .expect("a compile error")
    }

    /// The names of a program's global slots, in slot order.
    fn global_names(program: &crate::session::Program) -> Vec<String> {
        (0..program.globals.len())
            .map(|slot| program.globals.name(slot as u16).to_string())
            .collect()
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
        let fns = compile("fn main() { 4.25 }");
        let main = find_fn(&fns, "main");
        assert!(
            main.chunk
                .constants
                .iter()
                .any(|c| matches!(c, Value::Float(f) if (*f - 4.25).abs() < f64::EPSILON))
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
        for name in ["add", "sub", "main"] {
            find_fn(&fns, name);
        }
        // Each function is installed in a global slot named after it.
        let program = crate::session::testing::compile_decls_str(
            "fn add(a, b) { a + b }\nfn sub(a, b) { a - b }\nfn main() { add(1, 2) }",
        )
        .unwrap();
        let names = global_names(&program);
        for name in ["add", "sub", "main"] {
            assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
        }
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
        let fns = compile("fn main() { let f = { x -> x + 1 }\n f(5) }");
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
    { x -> x + n }
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
        let main = find_fn(&fns, "main");
        // A nullary variant is a Variant value, a constant where it is
        // used; the enum's description lists every variant.
        assert!(main.chunk.constants.iter().any(
            |c| matches!(c, Value::Variant(tag, fields) if tag.name() == "Red" && fields.is_empty())
        ));
        let Some(Value::Variant(tag, _)) = main.chunk.constants.first() else {
            panic!("main's first constant is the variant");
        };
        let names: Vec<&str> = tag
            .ty()
            .variants()
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(names, ["Red", "Green", "Blue"]);
    }

    #[test]
    fn test_compile_enum_variant_constructors() {
        let fns = compile(
            r#"
type Shape { Circle(Float), Rect(Float, Float) }
fn main() { Circle(1.0) }
"#,
        );
        let main = find_fn(&fns, "main");
        // A variant with fields is its constructor, a constant where it
        // is used; the enum's description has each variant's arity.
        let Some(Value::VariantConstructor(tag)) = main.chunk.constants.first() else {
            panic!("main's first constant is the constructor");
        };
        assert_eq!((tag.name(), tag.arity()), ("Circle", 1));
        assert_eq!(tag.ty().variants()[1].name, "Rect");
        assert_eq!(tag.ty().variants()[1].arity, 2);
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
        find_fn(&fns, "Color.display");
        // The method is installed in the global slot of `display` of
        // `Color`.
        let program = crate::session::testing::compile_decls_str(
            "type Color { Red }\ntrait Display for Color {\n    fn display(self) -> String { \"color\" }\n}\n",
        )
        .unwrap();
        assert!(global_names(&program).iter().any(|n| n == "Color.display"));
    }

    // ── Import gating ──────────────────────────────────────────────

    #[test]
    fn test_import_gating_success() {
        compile(
            r#"
import list
fn main() {
    list.length([1, 2])
}
"#,
        );
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

    // ── compile_program vs compile_declarations ────────────────────

    #[test]
    fn test_compile_program_calls_main() {
        let fns = crate::session::testing::compile_str("fn main() { 42 }")
            .unwrap()
            .functions;
        let script = &fns[0];
        // compile_program emits GetGlobal main, Call 0, Return
        assert!(has_op(&script.chunk, Op::GetGlobal));
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

    // ── Selective import compilation ────────────────────────────────

    #[test]
    fn test_compile_selective_import() {
        let fns = compile(
            r#"
import list.{ length, map }
fn main() { length([1, 2]) }
"#,
        );
        assert!(!find_fn(&fns, "main").chunk.code.is_empty());
    }

    #[test]
    fn test_compile_aliased_import() {
        let fns = compile(
            r#"
import list as l
fn main() { l.length([1]) }
"#,
        );
        let main = find_fn(&fns, "main");
        assert!(has_string_constant(&main.chunk, "list.length"));
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
            if let Value::VmClosure(cl) = c
                && cl.function.name == "<lambda>"
            {
                return Some(&cl.function);
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
    when let Some(v) = x else { return 0 }
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

    // ── `loop(...)` outside a loop is an error ─────────────────────

    // ── Record field metadata ──────────────────────────────────────

    #[test]
    fn test_compile_record_field_metadata() {
        let (mut session, entry) = crate::session::testing::session_with(&[(
            "main.silt",
            "type User { name: String, age: Int }",
        )]);
        let program = session
            .compile(entry, crate::session::Entry::Tests { filter: None })
            .unwrap();
        // The record's fields, in declaration order, are in its type's
        // description.
        let user = program
            .types
            .iter()
            .find(|ty| ty.name == "User")
            .expect("User is described");
        let fields: Vec<&str> = user.fields().iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(fields, ["name", "age"]);
    }

    #[test]
    fn test_compile_record_field_descriptors() {
        let source = r#"
type R {
    a: Ids,
    m: Map(String, Int),
    t: (Int, String),
    p: Pair(Bool),
    s: Set(Int),
    inner: Inner,
}
type Inner { x: Int }
type Ids = List(Int)
type Pair(a) = (a, a)
"#;
        let (mut session, entry) = crate::session::testing::session_with(&[("main.silt", source)]);
        let program = session
            .compile(entry, crate::session::Entry::Tests { filter: None })
            .unwrap();
        let r = program.types.iter().find(|ty| ty.name == "R").expect("R");
        let inner = program
            .types
            .iter()
            .find(|ty| ty.name == "Inner")
            .expect("the record type of a field is described too");
        let described: Vec<String> = r.fields().iter().map(|(_, t)| format!("{t:?}")).collect();
        assert_eq!(
            described,
            [
                "List(Int)",
                "Map(Int)",
                "Tuple([Int, String])",
                "Tuple([Bool, Bool])",
                "Unsupported(\"Set(Int)\")",
                format!("Record({:?})", inner.id).as_str(),
            ]
        );
    }

    #[test]
    fn test_decode_target_with_undecodable_field_is_rejected() {
        let err = compile_err(
            r#"
import json
type Bag { items: Set(Int) }
fn main() { json.parse("x", Bag) }
"#,
        );
        assert!(
            err.message.contains("json.parse")
                && err.message.contains("`items`")
                && err.message.contains("Set(Int)"),
            "expected the error to name the call, the field and its type, got: {}",
            err.message
        );
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

    // ── Audit regression: JumpBack operand bounds check (V4) ───────
    //
    // The `JumpBack` operand is a `u16`, so a loop body larger than
    // 65_535 bytes of bytecode would wrap and branch to a garbage
    // offset. `Chunk::patch_jump` already checks this for forward
    // jumps; the matching `Recur` emitter used to cast blindly with
    // `as u16`. The fix threads the distance through
    // `jumpback_fits_u16`; this test exercises that helper directly so
    // the bounds-check is locked without having to synthesize a
    // >64KB loop body.
    /// A program with more top-level definitions than a `u16` slot can
    /// name is a compile error at the first definition that does not
    /// fit. Checking 65,537 definitions takes minutes, so the program
    /// here is small and the slots before it are taken already, as an
    /// earlier REPL entry would have taken them.
    #[test]
    fn test_more_than_65536_globals_rejected() {
        let source = "fn main() { helper() }\nfn helper() { 1 }\n";
        let (mut session, file) = crate::session::testing::session_with(&[("main.silt", source)]);
        assert!(!session.analyze(file).has_errors());
        let mut units = session.program_units(session.module_of(file), false);
        for k in 0..=u16::MAX as u32 - 1 {
            let taken = units
                .earlier
                .globals
                .add_def(crate::defs::DefId(u32::MAX - k), format!("taken{k}"));
            assert!(taken.is_some());
        }
        let err = match Compiler::for_program(units) {
            Ok(_) => panic!("65,537 globals must not compile"),
            Err(err) => err,
        };
        assert_eq!(err.code, Code::CompileLimit, "{}", err.message);
        assert!(
            err.message
                .contains("more than 65536 top-level definitions")
        );
        // `main` took the last slot; `helper` does not fit.
        assert_eq!(
            &source[err.span.start as usize..err.span.end as usize],
            "helper"
        );
    }

    #[test]
    fn test_jumpback_overflow_rejected() {
        use crate::source::Span;

        // A distance that exactly fits must pass.
        assert!(super::jumpback_fits_u16(u16::MAX as usize, Span::BUILTIN).is_ok());

        // One beyond the limit must produce a Diagnostic and not a
        // panic/wrap.
        let err = super::jumpback_fits_u16(u16::MAX as usize + 1, Span::BUILTIN)
            .expect_err("expected u16 overflow to be rejected");
        assert!(
            err.message.contains("loop body too large"),
            "expected loop-body-too-large error, got: {}",
            err.message
        );

        // Way beyond the limit also.
        assert!(super::jumpback_fits_u16(usize::MAX, Span::BUILTIN).is_err());
    }

    // ── Audit regression: add_upvalue >255 upvalues (B5) ────────────
    //
    // The bytecode addresses upvalues with a single byte AND stores
    // `function.upvalue_count` as `u8`, AND the `MakeClosure` opcode
    // emits a single-byte count followed by 2N descriptor operand
    // bytes. Together these mean the hard limit is 255 (not 256):
    // pushing a 256th upvalue would leave `ctx.upvalues.len() == 256`,
    // which truncates to `0u8` both in `upvalue_count` and in the
    // count byte emitted before the descriptor operands. Those
    // descriptor bytes would then be interpreted as bytecode at
    // runtime — a silent miscompile worse than a panic.
    //
    // This test locks the bounds check by pre-filling a compile
    // context with 255 upvalues and verifying that the 256th attempt
    // returns `Err`, NOT `Ok(0u8)`.
    #[test]
    fn test_add_upvalue_rejects_over_255() {
        use crate::bytecode::UpvalueDesc;
        use crate::source::Span;

        let mut compiler = Compiler::for_program(no_modules()).unwrap();
        // Push an outer (script) context plus the function context we'll
        // be adding upvalues into; `add_upvalue` expects `context_index`
        // to be valid.
        compiler
            .contexts
            .push(CompileContext::new("<script>".into(), 0));
        compiler
            .contexts
            .push(CompileContext::new("inner".into(), 0));
        let inner_idx = 1usize;

        // Register exactly 255 distinct upvalues (indices 0..=254) as
        // locals captured from the enclosing scope. These must all
        // succeed.
        for i in 0..u8::MAX {
            let desc = UpvalueDesc {
                is_local: true,
                index: i,
            };
            let result = compiler.add_upvalue(inner_idx, desc, Span::BUILTIN);
            assert!(
                result.is_ok(),
                "upvalue {i} (of 255) should be accepted; got {result:?}"
            );
        }

        // A 256th distinct upvalue must now be rejected — not panic,
        // and critically not silently accepted with a wrapped index.
        // Use `is_local: false` so the dedup check in `add_upvalue`
        // can't collapse it with an existing local-captured entry.
        let overflowing = UpvalueDesc {
            is_local: false,
            index: 0,
        };
        let err = compiler
            .add_upvalue(inner_idx, overflowing, Span::BUILTIN)
            .expect_err("expected 256th upvalue to return Diagnostic");
        assert!(
            err.message.contains("too many upvalues"),
            "expected too-many-upvalues error, got: {}",
            err.message
        );

        // And the context must still hold exactly 255 upvalues — the
        // rejected 256th must NOT have been pushed into ctx.upvalues.
        let ctx = &compiler.contexts[inner_idx];
        assert_eq!(
            ctx.upvalues.len(),
            255,
            "rejected upvalue must not be pushed into ctx.upvalues"
        );
        assert_eq!(
            ctx.function.upvalue_count, 255u8,
            "function.upvalue_count must reflect 255 (not wrapped to 0)"
        );
    }

    // ── Audit regression: add_upvalue accepts exactly 255 (B5) ──────
    //
    // The complementary positive-direction lock for
    // test_add_upvalue_rejects_over_255: fill a context with exactly
    // 255 upvalues and verify `ctx.upvalues.len() == 255` AND
    // `ctx.function.upvalue_count == 255u8`. This pins the upper
    // bound so a future refactor can't silently drop it below 255.
    #[test]
    fn test_add_upvalue_accepts_exactly_255_upvalues() {
        use crate::bytecode::UpvalueDesc;
        use crate::source::Span;

        let mut compiler = Compiler::for_program(no_modules()).unwrap();
        compiler
            .contexts
            .push(CompileContext::new("<script>".into(), 0));
        compiler
            .contexts
            .push(CompileContext::new("inner".into(), 0));
        let inner_idx = 1usize;

        for i in 0..u8::MAX {
            let desc = UpvalueDesc {
                is_local: true,
                index: i,
            };
            let returned = compiler
                .add_upvalue(inner_idx, desc, Span::BUILTIN)
                .unwrap_or_else(|e| {
                    panic!("upvalue {i} (of 255) must be accepted; got {}", e.message)
                });
            assert_eq!(
                returned, i,
                "add_upvalue must return the index at which it stored the desc"
            );
        }

        let ctx = &compiler.contexts[inner_idx];
        assert_eq!(
            ctx.upvalues.len(),
            255,
            "context must hold exactly 255 upvalues"
        );
        assert_eq!(
            ctx.function.upvalue_count, 255u8,
            "function.upvalue_count must be 255 (not wrapped to 0)"
        );
    }
}
