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
use crate::bytecode::{Chunk, Function, Op, UpvalueDesc, VmClosure};
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

/// A record declaration, kept to describe and check its field types.
struct RecordDecl {
    params: Vec<Symbol>,
    fields: Vec<RecordField>,
}

/// A type alias declaration: `type Name(params) = target`.
struct AliasDecl {
    params: Vec<Symbol>,
    target: TypeExpr,
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
    TypeExpr::new(kind, te.span)
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

// ── Compiler warnings ────────────────────────────────────────────────

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
    /// The prefix of the globals the module's public declarations are
    /// installed under (`<global>.<name>`): unique in the program, so two
    /// modules imported by one name from two packages (an app's
    /// `src/util.silt` and a dependency's own) do not share globals.
    pub global: String,
    /// The module each `import` of this module names, by the module
    /// name written after `import`. Builtin modules are not in it.
    pub imports: HashMap<Symbol, usize>,
    /// For a host module, the function each of its signatures declares,
    /// by name: the module's globals are these functions.
    pub host: HashMap<Symbol, Arc<HostFn>>,
    /// The module each name the module's imports bind stands for: `m`
    /// for `import m`, `n` for `import m as n` (an item import binds no
    /// module name). An alias may be named like another imported module:
    /// `import helper as util` binds `util` to helper, whatever `import
    /// util as u2` imports.
    pub bindings: HashMap<Symbol, usize>,
}

/// The modules of a program, indexed by the session's module ids, and
/// which of them is the entry: the one compiled by `compile_program` or
/// `compile_declarations`. The others are compiled where they are first
/// imported.
#[derive(Default)]
pub struct ProgramUnits {
    pub modules: Vec<ModuleUnit>,
    pub entry: usize,
    /// The definitions the resolver's slots name: a variant is compiled
    /// from its definition. `None` for a compiler with no session.
    pub defs: Option<Arc<crate::defs::DefTable>>,
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
    /// The top-level functions and `let`s of theirs the entry sees.
    pub fns: HashSet<Symbol>,
    pub lets: HashSet<Symbol>,
    /// The global each top-level value of the entry is installed under,
    /// its own and those it sees, where that is not its name: a name
    /// defined again by a later entry gets a global of its own, so the
    /// code of an earlier entry keeps the definition it was checked
    /// against.
    pub globals: HashMap<Symbol, String>,
    /// For each name a top-level `let` of the entry binds again, the
    /// global of the value it had, which the `let`'s initializer reads.
    pub previous: HashMap<Symbol, String>,
    /// The modules installed already: an import of one compiles nothing.
    pub installed: HashSet<usize>,
}

pub struct Compiler {
    contexts: Vec<CompileContext>,
    /// Accumulated compiled functions (one per `Decl::Fn`).
    functions: Vec<Function>,
    /// The modules of the program, from the session. Empty for a
    /// compiler made with [`Compiler::new`], which can only import
    /// builtin modules.
    units: ProgramUnits,
    /// The modules being compiled, innermost last: the importing module
    /// of an `import` met now is the last one, or the entry module.
    unit_stack: Vec<usize>,
    /// Modules already compiled in this compilation unit, so each is
    /// compiled once, where it is first imported.
    compiled_modules: HashSet<usize>,
    /// Warnings emitted during compilation.
    warnings: Vec<Diagnostic>,
    /// Builtin modules that have been explicitly imported in this compilation unit.
    imported_builtin_modules: HashSet<String>,
    /// Aliases for builtin modules: maps the alias name (e.g. "l" from
    /// `import list as l`) to the canonical builtin module name (e.g.
    /// "list"). Used by `extract_builtin_name` and the FieldAccess
    /// codegen to resolve `l.sum` → `CallBuiltin("list.sum", …)` so that
    /// non-curated submodule functions (registered in the typechecker
    /// env / VM dispatcher but absent from `module::builtin_module_functions`)
    /// remain callable through the alias. Mirrors the typechecker's
    /// round-58 prefix-mirror (see src/typechecker/mod.rs:3156) on the
    /// compiler side; without it `l.sum` failed with
    /// "undefined global: l.sum" at runtime even though `list.sum` worked.
    imported_builtin_module_aliases: HashMap<String, String>,
    /// Whether the current expression is in tail position (for TCO).
    in_tail_position: bool,
    /// When compiling inside a file-based module, maps bare function names
    /// to their qualified equivalents so intra-module calls resolve.
    /// Value is (module_name, map_of fn_name -> is_public).
    module_scope: Option<(String, HashMap<String, bool>)>,
    /// For each compiled file-based module, the set of `pub fn` names it
    /// exports. Populated during `compile_file_module_inner`; a name that
    /// is a key here is a module (see `names_function_value`).
    module_public_fns: HashMap<String, HashSet<String>>,
    /// The names of the enum types: the builtin ones, and each `type`
    /// declaration of an enum, collected before the program's code is
    /// compiled. The json / toml decoders read it.
    known_enums: HashSet<String>,
    /// Names of known unit (nullary) enum variants — i.e. those
    /// registered globally as `Value::Variant(name, [])`. Used to gate
    /// the bare-variant method-dispatch rewrite: `Red.display()` must
    /// lower to `GetGlobal("Red"); CallMethod("display", 1)` rather
    /// than the qualified-global `GetGlobal("Red.display")` (which
    /// produces a runtime `undefined global: Red.display`). Only unit
    /// variants are tracked here; payload variants like `Blue(5)` go
    /// through the receiver-expression path naturally because their
    /// receiver is `ExprKind::Call(Blue, [5])`, not `ExprKind::Ident`.
    /// Seeded from `module::builtin_*_enum_variants_with_arity()` so
    /// builtin unit variants (`None`, `IoInterrupted`, etc.) are also
    /// recognised.
    known_unit_variants: HashSet<String>,
    /// Round 94 (module-shadowing): binder names of the entry program's
    /// top-level `let` declarations. These are VALUE globals, so dotted
    /// access on them (`other.year` after `let other = P { .. }`) is
    /// field access, never a module-member lookup — the typechecker
    /// resolves it that way (a value binding shadows a same-named
    /// imported module), and the codegen paths below must agree or the
    /// VM would chase a `GetGlobal("other.year")` that doesn't exist.
    /// Populated by a pre-pass in `compile_program_with_entry` /
    /// `compile_declarations` (NOT in `compile_decl`, so a file
    /// module's own top-level lets don't leak into the consumer's
    /// view). Consulted alongside `resolve_local`/upvalue checks in
    /// `extract_builtin_name`, the Call arm's module-call detection,
    /// and `FieldAccess` codegen.
    top_level_value_globals: HashSet<String>,
    /// Names of the entry program's top-level `fn` declarations, filled
    /// by the same pre-pass as `top_level_value_globals`. A function is a
    /// value, so `double.baz()` with a top-level `fn double` is a method
    /// call on the function, not a call of a member of a module `double`.
    /// Inside a file module the module's own functions are in
    /// `module_scope` instead.
    top_level_fn_names: HashSet<String>,
    /// Lowercase names imported by name (`import json.{ parse }`,
    /// `import util.{ helper }`), per program: the key is the file module
    /// being compiled (`None` for the entry program) and the bare name,
    /// the value the qualified name the import binds (`json.parse`).
    /// Filled by `collect_selective_imports` before the program's code is
    /// compiled. An imported function is a value like a program's own
    /// function, and a call of an imported decoder gets the same
    /// compile-time check as the qualified call.
    selective_imports: HashMap<(Option<String>, String), String>,
    /// Record declarations of every program compiled so far (the entry
    /// program and the file modules it imports), by type name. Filled by
    /// `collect_type_decls` before any code of the program is compiled.
    /// Read to describe record field types for the json / toml decoders.
    record_decls: HashMap<String, RecordDecl>,
    /// Type alias declarations, collected together with `record_decls`.
    alias_decls: HashMap<String, AliasDecl>,
    /// The names of the program's types that two of its types have (two
    /// modules' `Pt`, or a module's own `Result` beside the builtin one):
    /// see [`Compiler::runtime_type_name`].
    clashing_type_names: HashSet<Symbol>,
    /// The names two types of the program's modules have (two modules'
    /// `Pt`, not a builtin type): such a type prints qualified.
    program_clashes: HashSet<Symbol>,
    /// The types described so far, which the VM is given.
    types: RefCell<TypeTable>,
    /// The name the entry's top-level `let` being compiled binds: its
    /// initializer reads the value the name had before (a REPL entry's
    /// `let x = x + 1`).
    initializing: Option<Symbol>,
}

/// Whether `item` is a function or constant of the builtin module
/// `module`, which has a global at run time: a type or a variant of the
/// module has none (a variant is compiled from its definition).
fn builtin_module_function(module: &str, item: Symbol) -> bool {
    let (_, scopes) = crate::typechecker::names::builtins();
    let Some(id) = crate::session::ModuleId::builtin(module) else {
        return false;
    };
    let Some(crate::typechecker::names::Binding::Def(def)) =
        scopes.modules.get(&id).and_then(|e| e.values.get(&item))
    else {
        return false;
    };
    crate::typechecker::names::builtin_def(*def)
        .is_some_and(|d| matches!(d.kind, crate::defs::DefKind::Fn))
}

/// The names two or more types of the program have: two modules' types,
/// or a module's type and a builtin type; and of those the names two
/// modules' types have.
fn clashing_type_names(units: &ProgramUnits) -> (HashSet<Symbol>, HashSet<Symbol>) {
    let Some(defs) = &units.defs else {
        return (HashSet::new(), HashSet::new());
    };
    let mut seen: HashMap<Symbol, crate::defs::DefId> = HashMap::new();
    let mut clashing = HashSet::new();
    let mut program = HashSet::new();
    for unit in &units.modules {
        for id in defs.of_module(unit.id) {
            let def = defs.get(*id);
            if !matches!(def.kind, crate::defs::DefKind::Type(_)) {
                continue;
            }
            if seen.insert(def.name, *id).is_some_and(|other| other != *id) {
                program.insert(def.name);
                clashing.insert(def.name);
            }
            if crate::defs::builtin_type_id(&resolve(def.name)).is_some() {
                clashing.insert(def.name);
            }
        }
    }
    (clashing, program)
}

/// The builtin enums, which seed `known_enums`.
fn initial_known_enums() -> HashSet<String> {
    module::builtin_enum_variants()
        .iter()
        .map(|(enum_name, _)| (*enum_name).to_string())
        .collect()
}

/// Seed `known_unit_variants` with all builtin nullary variants. Routes
/// through the arity-aware registries so adding a new builtin variant
/// (whether stdlib-error or prelude) automatically gates the
/// bare-variant method-dispatch rewrite — no edit here is required.
fn initial_known_unit_variants() -> HashSet<String> {
    let mut set = HashSet::new();
    for (_enum_name, variants) in module::builtin_error_enum_variants_with_arity() {
        for (vname, arity) in *variants {
            if *arity == 0 {
                set.insert((*vname).to_string());
            }
        }
    }
    for (_enum_name, variants) in module::builtin_prelude_enum_variants_with_arity() {
        for (vname, arity) in *variants {
            if *arity == 0 {
                set.insert((*vname).to_string());
            }
        }
    }
    set
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

impl Compiler {
    /// Shared constructor body for [`Compiler::new`] and
    /// [`Compiler::for_program`]. The two public constructors differ
    /// only in the modules; everything else is seeded identically here
    /// so the two paths can never drift apart.
    fn build(units: ProgramUnits) -> Self {
        let (clashing_type_names, program_clashes) = clashing_type_names(&units);
        Self {
            contexts: Vec::new(),
            functions: Vec::new(),
            units,
            unit_stack: Vec::new(),
            compiled_modules: HashSet::new(),
            warnings: Vec::new(),
            imported_builtin_modules: HashSet::new(),
            imported_builtin_module_aliases: HashMap::new(),
            in_tail_position: false,
            module_scope: None,
            module_public_fns: HashMap::new(),
            known_enums: initial_known_enums(),
            known_unit_variants: initial_known_unit_variants(),
            top_level_value_globals: HashSet::new(),
            top_level_fn_names: HashSet::new(),
            selective_imports: HashMap::new(),
            record_decls: HashMap::new(),
            alias_decls: HashMap::new(),
            clashing_type_names,
            program_clashes,
            types: RefCell::new(TypeTable::default()),
            initializing: None,
        }
    }

    /// A compiler for a program with no modules but the builtin ones.
    pub fn new() -> Self {
        Self::build(ProgramUnits::default())
    }

    /// A compiler for the modules of a program, as the session analysed
    /// them.
    pub fn for_program(units: ProgramUnits) -> Self {
        Self::build(units)
    }

    /// The alias registries the program was checked with: read via
    /// [`crate::types::canonical::canonical_head`] when emitting
    /// trait-impl global keys, so registration and lookup keys agree
    /// across the typecheck → compile boundary.
    fn resolver(&self) -> &Resolver {
        &self.units.resolver
    }

    /// Returns warnings emitted during compilation.
    pub fn warnings(&self) -> &[Diagnostic] {
        &self.warnings
    }

    /// The types the compiled code builds values of, for the VM.
    pub fn types(&self) -> TypeTable {
        self.types.borrow().clone()
    }

    /// Mark all builtin modules as imported, for the tests below.
    #[cfg(test)]
    fn import_all_builtins(&mut self) {
        for name in crate::module::BUILTIN_MODULES {
            self.imported_builtin_modules.insert(name.to_string());
        }
    }

    // ── Public entry point ────────────────────────────────────────

    /// Compile a full program, returning all functions.
    ///
    /// The first function in the returned `Vec` is the top-level `<script>`,
    /// which ends with `GetGlobal "main" ; Call 0 ; Return`.
    pub fn compile_program(&mut self, program: &Program) -> Result<Vec<Function>, Diagnostic> {
        self.compile_program_with_entry(program, "main")
    }

    /// Compile a full program, dispatching `<script>` to call the global
    /// named `entry_point` (instead of the default `"main"`): a REPL
    /// entry of statements calls the function that holds them, whose
    /// name no program can write.
    pub fn compile_program_with_entry(
        &mut self,
        program: &Program,
        entry_point: &str,
    ) -> Result<Vec<Function>, Diagnostic> {
        // Push a top-level script context.
        self.contexts
            .push(CompileContext::new("<script>".into(), 0));

        // Round 94: record top-level let binders up front so dotted
        // access on them is compiled as field access even when the use
        // site is inside a fn that is compiled before the let decl is
        // reached. See `top_level_value_globals`.
        self.absorb_earlier_cells();
        self.collect_top_level_value_globals(program);
        self.collect_type_decls(program);

        self.compile_builtin_derived_impls()?;
        for decl in Self::decls_in_init_order(&program.decls) {
            self.compile_decl(decl)?;
        }

        // Emit: GetGlobal <entry_point>, Call 0, Return. The call is made
        // for the entry point's declaration and takes its span; without
        // one, silt itself makes the call.
        let span = program
            .decls
            .iter()
            .find_map(|decl| match decl {
                Decl::Fn(f) if resolve(f.name) == entry_point => Some(f.span),
                _ => None,
            })
            .unwrap_or(Span::BUILTIN);
        let name_idx = self.add_constant(Value::String(entry_point.into()), span)?;
        self.current_chunk()
            .emit_op_u16(Op::GetGlobal, name_idx, span);
        self.current_chunk().emit_op(Op::Call, span);
        self.current_chunk().emit_u8(0, span);
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
    /// registers globals and returns Unit.  Useful for test runners and the
    /// REPL where `main()` is not the entry-point.
    pub fn compile_declarations(&mut self, program: &Program) -> Result<Vec<Function>, Diagnostic> {
        self.contexts
            .push(CompileContext::new("<script>".into(), 0));

        // Round 94: same pre-pass as `compile_program_with_entry`.
        self.absorb_earlier_cells();
        self.collect_top_level_value_globals(program);
        self.collect_type_decls(program);

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
    /// program's script: each installs a `<Type>.<method>` global.
    fn compile_builtin_derived_impls(&mut self) -> Result<(), Diagnostic> {
        for decl in crate::typechecker::builtin_derived_impls().iter() {
            self.compile_decl(decl)?;
        }
        Ok(())
    }

    // ── Declarations ──────────────────────────────────────────────

    /// Round 94: pre-pass over the entry program's decls collecting
    /// top-level `let` binder names into `top_level_value_globals`. Also
    /// collects the top-level `fn` names into `top_level_fn_names` and
    /// the program's selective imports (`collect_selective_imports`).
    fn collect_top_level_value_globals(&mut self, program: &Program) {
        for decl in &program.decls {
            match decl {
                Decl::Let { .. } => {
                    for (name, _, _) in crate::parser::top_level_binders(decl) {
                        self.top_level_value_globals.insert(resolve(name));
                    }
                }
                Decl::Fn(fn_decl) => {
                    self.top_level_fn_names.insert(resolve(fn_decl.name));
                }
                _ => {}
            }
        }
        self.collect_selective_imports(program);
    }

    /// What the earlier entries of a REPL session declared, known before
    /// the entry's code is compiled: their types, and which names are
    /// their functions and their `let`s.
    fn absorb_earlier_cells(&mut self) {
        let programs = self.units.earlier.programs.clone();
        for program in &programs {
            self.collect_type_decls(program);
        }
        let earlier = &self.units.earlier;
        self.top_level_fn_names
            .extend(earlier.fns.iter().map(|name| resolve(*name)));
        self.top_level_value_globals
            .extend(earlier.lets.iter().map(|name| resolve(*name)));
    }

    /// The global the entry program's top-level value `name` is installed
    /// under: its name, but for a REPL entry's value that a later entry
    /// defines again (see [`EarlierCells::globals`]). A file module's
    /// names are its own.
    fn top_level_global(&self, name: Symbol) -> String {
        if self.module_scope.is_some() {
            return resolve(name);
        }
        let earlier = &self.units.earlier;
        let global = match self.initializing {
            Some(binder) if binder == name => earlier.previous.get(&name),
            _ => earlier.globals.get(&name),
        };
        global.cloned().unwrap_or_else(|| resolve(name))
    }

    /// Pre-pass recording the lowercase names `program` imports by name
    /// (`import json.{ parse }`) in `selective_imports`, under the
    /// program being compiled (see `current_program`). Uppercase names
    /// are types; `Point.origin()` stays a qualified call.
    fn collect_selective_imports(&mut self, program: &Program) {
        let current = self.current_program();
        for decl in &program.decls {
            let Decl::Import(ImportTarget::Items(module_name, items), _) = decl else {
                continue;
            };
            let mod_str = resolve(*module_name);
            for (item, _) in items {
                let item_str = resolve(*item);
                if item_str.starts_with(|c: char| c.is_lowercase() || c == '_') {
                    self.selective_imports.insert(
                        (current.clone(), item_str.clone()),
                        format!("{mod_str}.{item_str}"),
                    );
                }
            }
        }
    }

    /// The program being compiled: the file module's name, or `None` for
    /// the entry program.
    fn current_program(&self) -> Option<String> {
        self.module_scope.as_ref().map(|(module, _)| module.clone())
    }

    /// Whether the identifier `name`, which is not a local or an upvalue,
    /// names a function of the program being compiled or a value imported
    /// by name, rather than a module. A name that is also a builtin
    /// module, a builtin module alias or a compiled file module keeps
    /// meaning the module.
    fn names_function_value(&self, name: Symbol) -> bool {
        let name_str = resolve(name);
        if module::is_builtin_module(&name_str)
            || self.imported_builtin_module_aliases.contains_key(&name_str)
            || self.module_public_fns.contains_key(&name_str)
        {
            return false;
        }
        let own_function = match &self.module_scope {
            Some((_, fns)) => fns.contains_key(&name_str),
            None => self.top_level_fn_names.contains(&name_str),
        };
        own_function
            || self
                .selective_imports
                .contains_key(&(self.current_program(), name_str))
    }

    /// The qualified builtin name a call of the bare identifier `callee`
    /// calls, when `callee` was imported by name from a builtin module
    /// (`import json.{ parse }` makes `parse` call `json.parse`) and is
    /// not shadowed by a local, an upvalue or a function or top-level
    /// `let` of the program.
    fn selectively_imported_builtin(&self, callee: &Expr) -> Option<String> {
        let ExprKind::Ident(name) = &callee.kind else {
            return None;
        };
        if self.resolve_local(*name).is_some() || self.resolve_upvalue_peek(*name).is_some() {
            return None;
        }
        let name_str = resolve(*name);
        let shadowed = match &self.module_scope {
            Some((_, fns)) => fns.contains_key(&name_str),
            None => {
                self.top_level_fn_names.contains(&name_str)
                    || self.top_level_value_globals.contains(&name_str)
            }
        };
        if shadowed {
            return None;
        }
        let qualified = self
            .selective_imports
            .get(&(self.current_program(), name_str))?;
        let module_name = qualified.split('.').next()?;
        module::is_builtin_module(module_name).then(|| qualified.clone())
    }

    /// Pre-pass over a program's type declarations, run before any of its
    /// code is compiled, so that a use of a type does not depend on where
    /// in the file the type is declared: a function may name
    /// `Color.Red`, and a record field may use an alias, ahead of the
    /// declaration.
    fn collect_type_decls(&mut self, program: &Program) {
        for decl in &program.decls {
            let Decl::Type(type_decl) = decl else {
                continue;
            };
            let type_name = resolve(type_decl.name);
            match &type_decl.body {
                TypeBody::Enum(variants) => {
                    self.known_enums.insert(type_name);
                    for variant in variants {
                        if variant.fields.is_empty() {
                            self.known_unit_variants.insert(resolve(variant.name));
                        }
                    }
                }
                TypeBody::Record(fields) => {
                    self.record_decls.insert(
                        type_name,
                        RecordDecl {
                            params: type_decl.params.clone(),
                            fields: fields.clone(),
                        },
                    );
                }
                TypeBody::Alias(target) => {
                    self.alias_decls.insert(
                        type_name,
                        AliasDecl {
                            params: type_decl.params.clone(),
                            target: target.clone(),
                        },
                    );
                }
            }
        }
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

                let global = self.top_level_global(fn_decl.name);
                let name_idx = self.add_constant(Value::String(global), span)?;
                self.current_chunk()
                    .emit_op_u16(Op::SetGlobal, name_idx, span);
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
                let binder = match &pattern.kind {
                    PatternKind::Ident(name) if self.module_scope.is_none() => Some(*name),
                    _ => None,
                };
                let outer = std::mem::replace(&mut self.initializing, binder);
                let compiled = self.compile_expr(value);
                self.initializing = outer;
                compiled?;

                match &pattern.kind {
                    PatternKind::Ident(name) => {
                        let global = self.top_level_global(*name);
                        let name_idx = self.add_constant(Value::String(global), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::SetGlobal, name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    _ => {
                        let globals: Vec<(Symbol, String)> = crate::parser::top_level_binders(decl)
                            .into_iter()
                            .map(|(name, _, _)| (name, self.top_level_global(name)))
                            .collect();
                        self.install_destructured(pattern, &globals, span)?;
                    }
                }

                Ok(())
            }

            Decl::Type(type_decl) => {
                let span = type_decl.span;
                // Type aliases are transparent at the typechecker /
                // canonicaliser layer and emit no runtime artefacts.
                if matches!(type_decl.body, TypeBody::Alias(_)) {
                    return Ok(());
                }
                let Some(id) = self.declared_type(type_decl.name) else {
                    return Err(checker_missed(
                        span,
                        &format!("the type '{}' with no definition", type_decl.name),
                    ));
                };
                let info = self.type_info(id);
                // The type's descriptor is the global of its name, so it
                // can be passed as a `type a` argument; unless a variant
                // shares the enum's name, which owns the global then.
                let variant_shares_name = match &type_decl.body {
                    TypeBody::Enum(variants) => variants.iter().any(|v| v.name == type_decl.name),
                    _ => false,
                };
                if !variant_shares_name {
                    let val_idx = self.add_constant(Value::TypeDescriptor(info.clone()), span)?;
                    self.current_chunk()
                        .emit_op_u16(Op::Constant, val_idx, span);
                    let name_idx = self.add_constant(Value::String(info.key.clone()), span)?;
                    self.current_chunk()
                        .emit_op_u16(Op::SetGlobal, name_idx, span);
                    self.current_chunk().emit_op(Op::Pop, span);
                }
                if let TypeBody::Enum(variants) = &type_decl.body {
                    self.known_enums.insert(resolve(type_decl.name));
                    for (ordinal, variant) in variants.iter().enumerate() {
                        let vname = resolve(variant.name);
                        let tag = Tag::new(info.clone(), ordinal as u16);
                        let val = if variant.fields.is_empty() {
                            // Track for the bare-variant method-dispatch
                            // rewrite in `Call` codegen — so `Red.display()`
                            // lowers to a value-method call rather than the
                            // qualified-global `GetGlobal("Red.display")`.
                            self.known_unit_variants.insert(vname.clone());
                            Value::Variant(tag, Vec::new())
                        } else {
                            Value::VariantConstructor(tag)
                        };
                        let val_idx = self.add_constant(val, span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::Constant, val_idx, span);
                        let name_idx = self.add_constant(Value::String(vname), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::SetGlobal, name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                }
                Ok(())
            }

            Decl::TraitImpl(trait_impl) => {
                // Compile each method and register as "TypeName.method_name" global.
                //
                // The target type is routed through `canonical_head` so
                // the emitted global key matches the typechecker's
                // registration site (`register_trait_impl` in
                // src/typechecker/mod.rs) and the VM's runtime dispatch
                // name (`Vm::value_type_name_for_dispatch`). The
                // collapse rules — `Range -> List`, `Fun -> Fn`, and
                // user-alias routing (see
                // `src/types/canonical.rs::canonical_head`) — all apply
                // here. For example, a
                // `trait Foo for Range(a) { fn bar(self) { ... } }` impl
                // emits `"List.bar"` here, matches the `"List.bar"` key
                // the typechecker registered, and is found by the VM
                // when dispatching on a `Value::Range` (or `Value::List`)
                // receiver. Without this canonicalisation the compiler
                // would emit `"Range.bar"` while the typechecker
                // registers `"List.bar"`, leaving the impl unreachable.
                let canonical_target = self.impl_target_name(trait_impl);

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
                    let qualified_name = format!("{}.{}", canonical_target, method.name);

                    self.contexts
                        .push(CompileContext::new(qualified_name.clone(), arity));

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

                    let name_idx = self.add_constant(Value::String(qualified_name), span)?;
                    self.current_chunk()
                        .emit_op_u16(Op::SetGlobal, name_idx, span);
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
    /// `pattern` binds, whose value is on the stack: each binder becomes
    /// the global `globals` names for it. The pattern is bound as a block's
    /// `let` would bind it, then each local is copied to its global.
    fn install_destructured(
        &mut self,
        pattern: &crate::ast::Pattern,
        globals: &[(Symbol, String)],
        span: Span,
    ) -> Result<(), Diagnostic> {
        self.begin_scope();
        let val_slot = self.add_local(intern("__let_val__"), span)?;
        self.current_chunk()
            .emit_op_u16(Op::SetLocal, val_slot, span);
        self.compile_pattern_bind_checked(pattern, span)?;
        for (name, global) in globals {
            let slot = self.resolve_local(*name).ok_or_else(|| {
                checker_missed(span, &format!("the binder '{name}' of a top-level let"))
            })?;
            self.current_chunk().emit_op_u16(Op::GetLocal, slot, span);
            let idx = self.add_constant(Value::String(global.clone()), span)?;
            self.current_chunk().emit_op_u16(Op::SetGlobal, idx, span);
            self.current_chunk().emit_op(Op::Pop, span);
        }
        self.current_chunk().emit_op(Op::Unit, span);
        self.end_scope_with_result(false, span)?;
        self.current_chunk().emit_op(Op::Pop, span);
        Ok(())
    }

    // ── Import compilation ─────────────────────────────────────────

    fn compile_import(&mut self, target: &ImportTarget, span: Span) -> Result<(), Diagnostic> {
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
                    // For builtin modules, create aliases: bare "item" -> "module.item".
                    // A variant is a global by its bare name already, and a
                    // type has no value at run time.
                    for (item, _) in items {
                        if !builtin_module_function(&mod_str, *item) {
                            continue;
                        }
                        let qualified = format!("{mod_str}.{item}");
                        let qi = self.add_constant(Value::String(qualified), span)?;
                        self.current_chunk().emit_op_u16(Op::GetGlobal, qi, span);
                        let bare_i =
                            self.add_constant(Value::String(self.top_level_global(*item)), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::SetGlobal, bare_i, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    return Ok(());
                }
                // File-based selective import: compile the module, then alias
                // "module.item" -> bare "item" for each selected name.
                self.compile_file_module(&mod_str, span)?;
                let global = self.imported_global(&mod_str);
                // A type alias and a trait are names for the checker only:
                // they have no value at run time, so nothing is aliased.
                let static_only = self.module_static_names(&mod_str);
                for (item, _) in items {
                    if static_only.contains(item) {
                        continue;
                    }
                    let item_str = resolve(*item);
                    let qualified = format!("{global}.{item_str}");
                    let qi = self.add_constant(Value::String(qualified), span)?;
                    self.current_chunk().emit_op_u16(Op::GetGlobal, qi, span);
                    let bare_i =
                        self.add_constant(Value::String(self.top_level_global(*item)), span)?;
                    self.current_chunk()
                        .emit_op_u16(Op::SetGlobal, bare_i, span);
                    self.current_chunk().emit_op(Op::Pop, span);
                }
                Ok(())
            }
            ImportTarget::Alias(module_name, alias, _) => {
                let mod_str = resolve(*module_name);
                let alias_str = resolve(*alias);
                if module::is_builtin_module(&mod_str) {
                    self.imported_builtin_modules.insert(mod_str.clone());
                    // Record alias → canonical mapping so `l.sum(...)` can be
                    // routed as `CallBuiltin("list.sum", ...)` by
                    // `extract_builtin_name`, and `l.sum` as a value falls
                    // back to `GetGlobal("list.sum")`. This covers
                    // submodule functions registered in the typechecker /
                    // VM dispatcher that aren't in the curated
                    // `builtin_module_functions` list (e.g. list.sum,
                    // string.lines). Without this the curated-only copy
                    // loop below misses them and `l.sum` fails at runtime
                    // with "undefined global: l.sum" even though
                    // `list.sum(...)` works via `CallBuiltin`.
                    self.imported_builtin_module_aliases
                        .insert(alias_str.clone(), mod_str.clone());
                    // Builtin alias: copy all "module.func" globals to "alias.func".
                    let names = module::builtin_module_functions(&mod_str)
                        .into_iter()
                        .chain(module::builtin_module_constants(&mod_str));
                    for func in names {
                        let qualified = format!("{mod_str}.{func}");
                        let qi = self.add_constant(Value::String(qualified), span)?;
                        self.current_chunk().emit_op_u16(Op::GetGlobal, qi, span);
                        let alias_name = format!("{alias_str}.{func}");
                        let ai = self.add_constant(Value::String(alias_name), span)?;
                        self.current_chunk().emit_op_u16(Op::SetGlobal, ai, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                    return Ok(());
                }
                // File module with an alias: the module is compiled under
                // its own unique prefix, and `alias.name` resolves to it
                // (see `module_global`); no globals are named after the
                // alias, so an alias cannot claim another module's names.
                self.compile_file_module(&mod_str, span)?;
                Ok(())
            }
        }
    }

    /// The prefix of the globals of the module the current module imports
    /// as `written`: the module's unique global prefix, or `written`
    /// itself for anything else (a builtin module, an alias).
    fn module_global(&self, written: &str) -> String {
        let importer = self.unit_stack.last().copied().unwrap_or(self.units.entry);
        self.units
            .modules
            .get(importer)
            .and_then(|unit| unit.bindings.get(&intern(written)))
            .map(|&target| self.units.modules[target].global.clone())
            .unwrap_or_else(|| written.to_string())
    }

    /// The names of the type aliases and traits of the module the current
    /// module imports as `written`: they bind no global.
    fn module_static_names(&self, written: &str) -> HashSet<Symbol> {
        let importer = self.unit_stack.last().copied().unwrap_or(self.units.entry);
        let Some(&target) = self
            .units
            .modules
            .get(importer)
            .and_then(|unit| unit.imports.get(&intern(written)))
        else {
            return HashSet::new();
        };
        self.units.modules[target]
            .program
            .decls
            .iter()
            .filter_map(|decl| match decl {
                Decl::Type(t) if matches!(t.body, TypeBody::Alias(_)) => Some(t.name),
                Decl::Trait(t) => Some(t.name),
                _ => None,
            })
            .collect()
    }

    /// The prefix of the globals of the module the current module's
    /// `import module_name ...` names.
    fn imported_global(&self, module_name: &str) -> String {
        let importer = self.unit_stack.last().copied().unwrap_or(self.units.entry);
        self.units
            .modules
            .get(importer)
            .and_then(|unit| unit.imports.get(&intern(module_name)))
            .map(|&target| self.units.modules[target].global.clone())
            .unwrap_or_else(|| module_name.to_string())
    }

    /// Compile a file-based module's declarations into the current compilation
    /// unit. Each public declaration is registered as a global named
    /// `"<global>.decl_name"`, where `<global>` is the module's unique
    /// prefix.
    ///
    /// `module_name` is the import segment as it appears in user code
    /// (e.g. `import calc` → `module_name = "calc"`). Which module it
    /// names, the session decided when it built the module graph; the
    /// compiler looks it up among the imports of the module being
    /// compiled. A module is compiled once, where it is first imported.
    fn compile_file_module(&mut self, module_name: &str, span: Span) -> Result<(), Diagnostic> {
        let importer = self.unit_stack.last().copied().unwrap_or(self.units.entry);
        let target = self
            .units
            .modules
            .get(importer)
            .and_then(|unit| unit.imports.get(&intern(module_name)))
            .copied()
            .ok_or_else(|| {
                Diagnostic::error(
                    Code::ModuleNotFound,
                    span,
                    format!("cannot import module '{module_name}': no project root set"),
                )
            })?;

        if self.compiled_modules.contains(&target) {
            return Ok(());
        }

        let program = self.units.modules[target].program.clone();
        if self.units.earlier.installed.contains(&target) {
            // An earlier REPL entry installed it: only what the code that
            // uses it needs to know is taken.
            self.collect_type_decls(&program);
            let public = program
                .decls
                .iter()
                .filter_map(|decl| match decl {
                    Decl::Fn(f) if f.is_pub => Some(resolve(f.name)),
                    _ => None,
                })
                .collect();
            self.module_public_fns
                .insert(module_name.to_string(), public);
            self.compiled_modules.insert(target);
            return Ok(());
        }
        let global = self.units.modules[target].global.clone();
        if !self.units.modules[target].host.is_empty() {
            let host = self.units.modules[target].host.clone();
            self.compiled_modules.insert(target);
            return self.compile_host_module(module_name, &global, &program, &host, span);
        }
        self.unit_stack.push(target);
        let result = self.compile_file_module_inner(module_name, &global, &program, span);
        self.unit_stack.pop();
        if result.is_ok() {
            self.compiled_modules.insert(target);
        }
        result
    }

    /// Install the functions of a host module as the globals
    /// `<global>.<name>`, one per signature of `program`.
    fn compile_host_module(
        &mut self,
        module_name: &str,
        global: &str,
        program: &Program,
        host: &HashMap<Symbol, Arc<HostFn>>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let mut public_fns = HashSet::new();
        for decl in &program.decls {
            let Decl::Fn(f) = decl else {
                continue;
            };
            let Some(function) = host.get(&f.name) else {
                return Err(checker_missed(
                    span,
                    &format!("a host signature without a function: '{}'", f.name),
                ));
            };
            public_fns.insert(resolve(f.name));
            let fi = self.add_constant(Value::HostFn(function.clone()), span)?;
            self.current_chunk().emit_op_u16(Op::Constant, fi, span);
            let qualified = format!("{global}.{}", f.name);
            let name_idx = self.add_constant(Value::String(qualified), span)?;
            self.current_chunk()
                .emit_op_u16(Op::SetGlobal, name_idx, span);
            self.current_chunk().emit_op(Op::Pop, span);
        }
        self.module_public_fns
            .insert(module_name.to_string(), public_fns);
        Ok(())
    }

    /// Inner implementation of file module compilation: the declarations
    /// of `program`, the module imported as `module_name` at `span`, whose
    /// public declarations become the globals `<global>.<name>`.
    fn compile_file_module_inner(
        &mut self,
        module_name: &str,
        global: &str,
        program: &Program,
        span: Span,
    ) -> Result<(), Diagnostic> {
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

        // Build module scope: every top-level function and `let` of this
        // module. Public ones are registered as "module.name", private ones
        // as "__module__name", so no two modules share a global and the
        // module's own code reads its names there.
        // Save the parent scope first — recursive module compilation (imports) will
        // overwrite it, so we need to restore ours after processing imports.
        let saved_scope = self.module_scope.take();
        let mut all_fn_names: HashMap<String, bool> = HashMap::new();
        let mut let_names: HashSet<String> = HashSet::new();
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => {
                    all_fn_names.insert(resolve(f.name), f.is_pub);
                }
                Decl::Let { is_pub, .. } => {
                    for (name, _, _) in crate::parser::top_level_binders(decl) {
                        all_fn_names.insert(resolve(name), *is_pub);
                        let_names.insert(resolve(name));
                    }
                }
                _ => {}
            }
        }
        // The module's lets are the value globals of the program being
        // compiled now (see `top_level_value_globals`); the importer's are
        // restored at the end.
        let saved_value_globals = std::mem::replace(&mut self.top_level_value_globals, let_names);
        let pub_set: HashSet<String> = all_fn_names
            .iter()
            .filter(|(_, is_pub)| **is_pub)
            .map(|(fn_name, _)| fn_name.clone())
            .collect();
        self.module_public_fns
            .insert(module_name.to_string(), pub_set);
        self.module_scope = Some((global.to_string(), all_fn_names));

        // Wrap module top-level code in a synthetic `<module:name>` function
        // so runtime errors carry a frame that identifies the source file.
        let init_name = format!("<module:{module_name}>");
        self.contexts.push(CompileContext::new(init_name, 0));

        self.collect_type_decls(program);
        self.collect_selective_imports(program);

        // Compile each declaration. Functions get registered as
        // "module_name.fn_name" for public ones, or just compiled (for
        // internal helpers that closures might reference). Synthetic emissions
        // below (Op::SetGlobal, constants, etc.) carry the import statement's
        // span so anything that blames them points back to the import site.
        for decl in Self::decls_in_init_order(&program.decls) {
            match decl {
                Decl::Fn(fn_decl) => {
                    let fn_span = fn_decl.span;
                    if fn_decl.params.len() > u8::MAX as usize {
                        return Err(Diagnostic::error(
                            Code::CompileLimit,
                            fn_span,
                            format!(
                                "imported function '{}' has {} parameters; silt functions are limited to 255",
                                resolve(fn_decl.name),
                                fn_decl.params.len()
                            ),
                        ));
                    }
                    let arity = fn_decl.params.len() as u8;

                    self.contexts
                        .push(CompileContext::new(resolve(fn_decl.name), arity));

                    self.compile_params(&fn_decl.params, fn_span)?;

                    // Compile the body in tail position for TCO, mirroring
                    // the top-level `Decl::Fn` path in `compile_decl`.
                    // Before round 93 this flag was never set here, so
                    // functions in imported modules silently lacked
                    // tail-call elimination and deep recursion blew
                    // MAX_FRAMES. (Enabling it is only safe now that
                    // `compile_stmt` clears the flag for statement-head
                    // sub-expressions — see the round-93 leak fix.)
                    self.in_tail_position = true;
                    self.compile_expr(&fn_decl.body)?;
                    self.in_tail_position = false;
                    self.current_chunk().emit_op(Op::Return, fn_span);

                    let ctx = self.contexts.pop().ok_or(Diagnostic::error(
                        Code::CompilerBug,
                        span,
                        "compiler bug: missing module function context",
                    ))?;
                    let func = ctx.function;

                    let vm_closure = Arc::new(VmClosure {
                        function: Arc::new(func),
                        upvalues: vec![],
                    });
                    let closure_val = Value::VmClosure(vm_closure);
                    let fi = self.add_constant(closure_val, span)?;
                    self.current_chunk().emit_op_u16(Op::Constant, fi, span);

                    if public_fns.contains(&fn_decl.name) {
                        // Register as "module_name.fn_name"
                        let qualified = format!("{global}.{}", fn_decl.name);
                        let name_idx = self.add_constant(Value::String(qualified), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::SetGlobal, name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    } else {
                        // Internal function — still register so closures / calls work,
                        // but under a mangled private name.
                        let private_name = format!("__{global}__{}", fn_decl.name);
                        let name_idx = self.add_constant(Value::String(private_name), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::SetGlobal, name_idx, span);
                        self.current_chunk().emit_op(Op::Pop, span);
                    }
                }
                Decl::Type(type_decl) if public_types.contains(&type_decl.name) => {
                    // Compile the type declaration — registers variants under bare names.
                    self.compile_decl(decl)?;
                    // Also register the type and its variants under
                    // qualified names.
                    if let Some(id) = self.declared_type(type_decl.name) {
                        let info = self.type_info(id);
                        let mut values = vec![(
                            format!("{global}.{}", type_decl.name),
                            Value::TypeDescriptor(info.clone()),
                        )];
                        if let TypeBody::Enum(variants) = &type_decl.body {
                            for (ordinal, variant) in variants.iter().enumerate() {
                                let tag = Tag::new(info.clone(), ordinal as u16);
                                let value = if variant.fields.is_empty() {
                                    Value::Variant(tag, Vec::new())
                                } else {
                                    Value::VariantConstructor(tag)
                                };
                                values.push((format!("{global}.{}", variant.name), value));
                            }
                        }
                        for (qual, value) in values {
                            let val_idx = self.add_constant(value, span)?;
                            self.current_chunk()
                                .emit_op_u16(Op::Constant, val_idx, span);
                            let qual_idx = self.add_constant(Value::String(qual), span)?;
                            self.current_chunk()
                                .emit_op_u16(Op::SetGlobal, qual_idx, span);
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
                Decl::Let {
                    pattern,
                    value,
                    is_pub,
                    span: let_span,
                    ..
                } => {
                    let global_of = |name: Symbol| {
                        if *is_pub {
                            format!("{global}.{name}")
                        } else {
                            format!("__{global}__{name}")
                        }
                    };
                    self.compile_expr(value)?;
                    match &pattern.kind {
                        PatternKind::Ident(name) => {
                            let name_idx =
                                self.add_constant(Value::String(global_of(*name)), *let_span)?;
                            self.current_chunk()
                                .emit_op_u16(Op::SetGlobal, name_idx, *let_span);
                            self.current_chunk().emit_op(Op::Pop, *let_span);
                        }
                        _ => {
                            let globals: Vec<(Symbol, String)> =
                                crate::parser::top_level_binders(decl)
                                    .into_iter()
                                    .map(|(name, _, _)| (name, global_of(name)))
                                    .collect();
                            self.install_destructured(pattern, &globals, *let_span)?;
                        }
                    }
                }
            }
        }

        // Close the module init function and call it inline.
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

        self.module_scope = saved_scope;
        self.top_level_value_globals = saved_value_globals;
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
                        self.warn_if_shadows_module(*name, pattern.span);
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

            // A type of the program used as a value is its descriptor.
            ExprKind::Ident(_) if let Some(ty) = self.program_type(expr.res) => {
                let idx = self.add_constant(Value::TypeDescriptor(self.type_info(ty)), span)?;
                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
            }

            ExprKind::Ident(name) => {
                if let Some(slot) = self.resolve_local(*name) {
                    self.current_chunk().emit_op_u16(Op::GetLocal, slot, span);
                } else if let Some(idx) = self.resolve_upvalue(*name, span)? {
                    self.current_chunk().emit_op(Op::GetUpvalue, span);
                    self.current_chunk().emit_u8(idx, span);
                } else {
                    let name_str = resolve(*name);
                    // If we're inside a module and this name matches a sibling function,
                    // qualify it so intra-module calls resolve correctly.
                    // Public fns: "module.name", private fns: "__module__name".
                    // A type used as a value is its descriptor's global.
                    let resolved_name = if let Some(ty) = self.res_type(expr.res) {
                        self.runtime_type_name(ty)
                    } else if let Some((ref mod_name, ref fn_map)) = self.module_scope {
                        match fn_map.get(&name_str) {
                            Some(true) => format!("{mod_name}.{name_str}"),
                            Some(false) => format!("__{mod_name}__{name_str}"),
                            None => name_str,
                        }
                    } else {
                        self.top_level_global(*name)
                    };
                    let name_idx = self.add_constant(Value::String(resolved_name), span)?;
                    self.current_chunk()
                        .emit_op_u16(Op::GetGlobal, name_idx, span);
                }
            }

            ExprKind::Call(callee, args) => {
                // Argument count is encoded as a `u8` in all four
                // call emission paths below (CallBuiltin, module-
                // qualified Call, CallMethod, plain Call). Wrapping
                // via `.len() as u8` used to let a 256-argument call
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
                // Check if this is a module-qualified builtin call like list.map(...)
                if self.variant_value(callee).is_none()
                    && let Some(builtin_name) = self.extract_builtin_name(callee)?
                {
                    self.check_decode_target(&builtin_name, args.last(), span)?;
                    // Emit arguments first
                    self.compile_operands(args)?;
                    let argc = args.len() as u8;
                    let name_idx = self.add_constant(Value::String(builtin_name), span)?;
                    self.current_chunk()
                        .emit_op_u16(Op::CallBuiltin, name_idx, span);
                    self.current_chunk().emit_u8(argc, span);
                } else if let ExprKind::FieldAccess(receiver, method, _) = &callee.kind {
                    // Check if this is a module-qualified call on a non-local ident.
                    // Round 94: top-level let globals are value bindings too —
                    // they shadow same-named modules exactly like locals do
                    // (the typechecker resolves `other.double(2)` with a
                    // top-level `let other` as a field call on the value).
                    // A function (the program's own, or imported by name)
                    // is a value as well: `double.baz()` calls the method
                    // `baz` on the function `double`.
                    let is_module_call = if let ExprKind::Ident(name) = &receiver.kind {
                        self.resolve_local(*name).is_none()
                            && self.resolve_upvalue_peek(*name).is_none()
                            && !self.top_level_value_globals.contains(&resolve(*name))
                            && !self.names_function_value(*name)
                    } else {
                        false
                    };
                    // A variant's constructor: `Shape.Circle(r)`,
                    // `channel.Message(v)`, `m.Shape.Circle(r)`. Checked
                    // before the module-call path so enum names aren't
                    // confused with missing module imports.
                    if let Some(variant) = self.variant_value(callee) {
                        let idx = self.add_constant(variant, span)?;
                        self.current_chunk().emit_op_u16(Op::Constant, idx, span);
                        self.compile_operands_above(1, args)?;
                        let argc = args.len() as u8;
                        self.emit_call(argc, tail, span);
                    } else if self.builtin_trait_method_of_builtin_type(callee) && !args.is_empty()
                    {
                        // `Int.display(1)`: a builtin trait's method of a
                        // builtin type, which is native, not a global; the
                        // first argument is the receiver.
                        self.compile_operands(args)?;
                        let method_idx =
                            self.add_constant(Value::String(resolve(*method)), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::CallMethod, method_idx, span);
                        self.current_chunk().emit_u8(args.len() as u8, span);
                    } else if let Some(global) = self.qualified_type_member(callee) {
                        let idx = self.add_constant(Value::String(global), span)?;
                        self.current_chunk().emit_op_u16(Op::GetGlobal, idx, span);
                        self.compile_operands_above(1, args)?;
                        let argc = args.len() as u8;
                        self.emit_call(argc, tail, span);
                    } else if let ExprKind::Ident(name) = &receiver.kind
                        && is_module_call
                        && self.known_unit_variants.contains(&resolve(*name))
                    {
                        // Bare unit-variant method call: `Red.display(args)`
                        // where `Red` is a known nullary variant. Push the
                        // variant value as receiver and dispatch via
                        // `CallMethod`, which the VM routes through the
                        // variant's type to find `<EnumName>.display`.
                        // Without this rewrite, the `is_module_call` branch
                        // below would emit `GetGlobal("Red.display")` and
                        // fail at runtime with `undefined global`.
                        let variant_str = resolve(*name);
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
                        match self.variant_value(receiver) {
                            Some(variant) => {
                                let idx = self.add_constant(variant, span)?;
                                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
                            }
                            None => {
                                let var_idx =
                                    self.add_constant(Value::String(variant_str), span)?;
                                self.current_chunk()
                                    .emit_op_u16(Op::GetGlobal, var_idx, span);
                            }
                        }
                        self.compile_operands_above(1, args)?;
                        let argc = (args.len() + 1) as u8; // receiver + args
                        let method_idx =
                            self.add_constant(Value::String(resolve(*method)), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::CallMethod, method_idx, span);
                        self.current_chunk().emit_u8(argc, span);
                    } else if is_module_call {
                        if let ExprKind::Ident(module) = &receiver.kind {
                            // Gate: require import for builtin modules
                            let mod_str = resolve(*module);
                            if module::is_builtin_module(&mod_str)
                                && !self.imported_builtin_modules.contains(&mod_str)
                            {
                                return Err(checker_missed(
                                    span,
                                    &format!("a use of the unimported module '{module}'"),
                                ));
                            }
                            // Module-qualified call on a global module name.
                            let qualified = format!("{}.{method}", self.module_global(&mod_str));
                            let name_idx = self.add_constant(Value::String(qualified), span)?;
                            self.current_chunk()
                                .emit_op_u16(Op::GetGlobal, name_idx, span);
                            self.compile_operands_above(1, args)?;
                            let argc = args.len() as u8;
                            self.emit_call(argc, tail, span);
                        }
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
                        let method_idx =
                            self.add_constant(Value::String(resolve(*method)), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::CallMethod, method_idx, span);
                        self.current_chunk().emit_u8(argc, span);
                    }
                } else {
                    // Normal function call. A decoder imported by name
                    // (`import json.{ parse }`) is checked like
                    // `json.parse(..)`.
                    if let Some(builtin_name) = self.selectively_imported_builtin(callee) {
                        self.check_decode_target(&builtin_name, args.last(), span)?;
                    }
                    self.compile_operands(std::iter::once(&**callee).chain(args))?;
                    let argc = args.len() as u8;
                    self.emit_call(argc, tail, span);
                }
            }

            // A variant: `EnumName.Variant`, `time.Monday`, `m.Color.Red`.
            // Checked ahead of the builtin-module gate so enum names
            // aren't mistaken for missing module imports.
            ExprKind::FieldAccess(..) if let Some(variant) = self.variant_value(expr) => {
                let idx = self.add_constant(variant, span)?;
                self.current_chunk().emit_op_u16(Op::Constant, idx, span);
            }

            // `m.Pt` used as a value: the type's descriptor.
            ExprKind::FieldAccess(..) if let Some(ty) = self.program_type(expr.res) => {
                let idx = self.add_constant(Value::TypeDescriptor(self.type_info(ty)), span)?;
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

            ExprKind::FieldAccess(..) if let Some(global) = self.qualified_type_member(expr) => {
                let idx = self.add_constant(Value::String(global), span)?;
                self.current_chunk().emit_op_u16(Op::GetGlobal, idx, span);
            }

            ExprKind::FieldAccess(expr, field, _) => {
                // Check if this is a module-qualified name like list.map
                // But only if the identifier is NOT a known local or upvalue.
                if let ExprKind::Ident(name) = &expr.kind {
                    // Round 94: a top-level let global is a value binding —
                    // route through the receiver-expression path (GetGlobal +
                    // GetField) like a local, never `GetGlobal("name.field")`.
                    let is_local = self.resolve_local(*name).is_some()
                        || self.resolve_upvalue(*name, span)?.is_some()
                        || self.top_level_value_globals.contains(&resolve(*name));
                    if !is_local {
                        let name_str = resolve(*name);
                        // Gate: require import for builtin modules.
                        if module::is_builtin_module(&name_str)
                            && !self.imported_builtin_modules.contains(&name_str)
                        {
                            return Err(checker_missed(
                                span,
                                &format!("a use of the unimported module '{name}'"),
                            ));
                        }
                        let qualified = format!("{}.{field}", self.module_global(&name_str));
                        let name_idx = self.add_constant(Value::String(qualified), span)?;
                        self.current_chunk()
                            .emit_op_u16(Op::GetGlobal, name_idx, span);
                        return Ok(());
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
                if let Some(builtin_name) = self.extract_builtin_name(callee)? {
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
                    if let Some(builtin_name) = self.selectively_imported_builtin(callee) {
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
            self.warn_if_shadows_module(*name, span);
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

    // ── Helper: qualified variants ───────────────────────────────

    /// The definition `id`. A compiler with no session knows the builtin
    /// definitions only.
    fn def(&self, id: crate::defs::DefId) -> Option<crate::defs::Def> {
        match &self.units.defs {
            Some(defs) => Some(*defs.get(id)),
            None => crate::typechecker::names::builtin_def(id),
        }
    }

    /// The name the impl globals of the type `ty` are installed under:
    /// its name, or for a module's type whose name another type of the
    /// program has too (a module's or a builtin one), the module's global
    /// prefix and the name (`a.Pt`), so that two modules' `Pt` do not
    /// share their impls.
    fn runtime_type_name(&self, ty: TypeRef) -> String {
        self.qualified_type_name(ty, &self.clashing_type_names)
    }

    /// The name of `ty`, qualified by its module's global when `clashes`
    /// has it.
    fn qualified_type_name(&self, ty: TypeRef, clashes: &HashSet<Symbol>) -> String {
        if crate::defs::builtin_types()
            .get(ty.id.0.0 as usize)
            .is_some()
            || !clashes.contains(&ty.name)
        {
            return resolve(ty.name);
        }
        let module = self.def(ty.id.0).map(|def| def.module);
        match self
            .units
            .modules
            .iter()
            .find(|unit| Some(unit.id) == module)
        {
            Some(unit) => format!("{}.{}", unit.global, ty.name),
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
        let Some(def) = self.def(id.0) else {
            unreachable!("a type id names a definition");
        };
        let ty = TypeRef { id, name: def.name };
        let defs = self
            .units
            .defs
            .as_ref()
            .expect("a program type has a session");
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
            name: self.qualified_type_name(ty, &self.program_clashes),
            key: self.runtime_type_name(ty),
            shape,
        });
        self.types.borrow_mut().insert(info.clone());
        // The record types of its fields, which a decoder builds too.
        for record in nested {
            self.type_info(record);
        }
        info
    }

    /// The fields of the record type declared with its name at `span`.
    fn declared_record_fields(&self, span: Span) -> Option<Vec<RecordField>> {
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
                    && let TypeBody::Record(fields) = &td.body
                {
                    return Some(fields.clone());
                }
            }
        }
        None
    }

    /// The variant a resolution names (a constructor pattern, a variant
    /// used as a value). With no resolution (the derived impls of the
    /// builtin types, which the builtin environment makes, and a
    /// compiler with no session) it is a builtin variant, named by its
    /// name: builtin variant names are unique among the builtins.
    fn variant_tag(&self, res: Option<crate::defs::Res>, name: Symbol) -> Option<Tag> {
        if let Some(res) = res {
            let crate::defs::Res::Def(id) = res else {
                return None;
            };
            let def = self.def(id)?;
            let crate::defs::DefKind::Variant { ty, ordinal, .. } = def.kind else {
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
        let def = self.def(id)?;
        matches!(def.kind, crate::defs::DefKind::Type(_)).then_some(TypeRef {
            id: crate::defs::TypeId(id),
            name: def.name,
        })
    }

    /// The type a resolution names, if it names a record or enum type of
    /// the program (not a builtin type).
    fn program_type(&self, res: Option<crate::defs::Res>) -> Option<crate::defs::TypeId> {
        let ty = self.res_type(res)?;
        crate::defs::builtin_types()
            .get(ty.id.0.0 as usize)
            .is_none()
            .then_some(ty.id)
    }

    /// The type the module being compiled declares as `name`.
    fn declared_type(&self, name: Symbol) -> Option<crate::defs::TypeId> {
        let current = self.unit_stack.last().copied().unwrap_or(self.units.entry);
        let defs = self.units.defs.as_ref()?;
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

    /// The name of the type the impl `ti` is for, as the checker keys
    /// impls and the VM dispatches: the canonical head of its target
    /// (`Range` is `List`, an alias is the type it stands for). A target
    /// that names no type (`trait Display for a`) keeps its name.
    fn impl_target_name(&self, ti: &crate::ast::TraitImpl) -> String {
        let written = match ti.target_res {
            Some(crate::defs::Res::Def(id)) => self.def(id).map(|def| TypeRef {
                id: crate::defs::TypeId(id),
                name: def.name,
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
        match written {
            Some(ty) => self.runtime_type_name(canonical_head(self.resolver(), ty)),
            None => resolve(ti.target_type),
        }
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
        let Some(def) = self.def(id) else {
            return false;
        };
        def.module.is_builtin() && def.is_type()
    }

    /// The global of `T.method` or `m.T.method`, a method of a type
    /// reached through the type, as the resolver resolved `T` / `m.T`:
    /// the impl's `<T>.<method>` global, named by the type's run-time
    /// name.
    fn qualified_type_member(&self, expr: &Expr) -> Option<String> {
        let ExprKind::FieldAccess(obj, field, _) = &expr.kind else {
            return None;
        };
        if !matches!(obj.kind, ExprKind::FieldAccess(..) | ExprKind::Ident(_))
            || self.variant_value(expr).is_some()
        {
            return None;
        }
        let Some(crate::defs::Res::Def(id)) = obj.res else {
            return None;
        };
        let def = self.def(id)?;
        if !def.is_type() {
            return None;
        }
        let ty = canonical_head(
            self.resolver(),
            TypeRef {
                id: crate::defs::TypeId(id),
                name: def.name,
            },
        );
        Some(format!("{}.{field}", self.runtime_type_name(ty)))
    }

    /// The value of the variant `expr` names, as the resolver resolved
    /// it (`Red`, `Color.Red`, `m.Red`, `m.Color.Red`): a nullary variant
    /// is the value, any other its constructor. Two enums may have
    /// variants of one name, so a variant is not looked up by its name.
    fn variant_value(&self, expr: &Expr) -> Option<Value> {
        let Some(crate::defs::Res::Def(id)) = expr.res else {
            return None;
        };
        let def = self.def(id)?;
        let crate::defs::DefKind::Variant { ty, ordinal, arity } = def.kind else {
            return None;
        };
        let tag = Tag::new(self.type_info(ty), ordinal);
        Some(match arity {
            0 => Value::Variant(tag, Vec::new()),
            _ => Value::VariantConstructor(tag),
        })
    }

    /// If the callee is a module-qualified builtin (e.g., `list.map`),
    /// return the qualified name. Only returns Some if the ident is NOT a
    /// local/upvalue AND belongs to a known builtin module.
    fn extract_builtin_name(&self, callee: &Expr) -> Result<Option<String>, Diagnostic> {
        if let ExprKind::FieldAccess(expr, field, _) = &callee.kind
            && let ExprKind::Ident(module) = &expr.kind
        {
            // Check if it's a local or upvalue first (round 94: top-level
            // let globals are value bindings and shadow modules the same
            // way — `list.map(x)` with `let list = ...` is a field call).
            let mod_str = resolve(*module);
            if self.resolve_local(*module).is_none()
                && self.resolve_upvalue_peek(*module).is_none()
                && !self.top_level_value_globals.contains(&mod_str)
            {
                if module::is_builtin_module(&mod_str) {
                    if !self.imported_builtin_modules.contains(&mod_str) {
                        return Err(checker_missed(
                            callee.span,
                            &format!("a use of the unimported module '{module}'"),
                        ));
                    }
                    return Ok(Some(format!("{module}.{field}")));
                }
                // Aliased builtin: `import list as l` → `l.sum(...)` must
                // dispatch as `CallBuiltin("list.sum", ...)`. The alias
                // loop in `compile_import` only mirrors the curated
                // `builtin_module_functions` list as globals, so any
                // submodule function registered only in the typechecker
                // / VM dispatcher (list.sum, string.lines, …) would
                // otherwise fail at runtime with "undefined global:
                // l.sum". Rewriting the call at compile time bypasses
                // the global lookup entirely and lets the VM's
                // module-prefix dispatcher do the routing.
                if let Some(canonical) = self.imported_builtin_module_aliases.get(&mod_str) {
                    return Ok(Some(format!("{canonical}.{field}")));
                }
            }
        }
        Ok(None)
    }

    // ── Record field types for the json / toml decoders ──────────

    /// The type of the record field `te` as the decoders see it.
    ///
    /// `Err` carries the part of `te` no decoder exists for, as written.
    /// `open_aliases` holds the aliases being expanded (an alias that
    /// leads back to itself has no decoder). The names of the record
    /// types the field type refers to are added to `records`.
    fn describe_field_type(
        &self,
        te: &TypeExpr,
        open_aliases: &mut Vec<String>,
        records: &mut Vec<String>,
    ) -> Result<FieldType, String> {
        const NO_ARGS: &[TypeExpr] = &[];
        let (name, args): (Symbol, &[TypeExpr]) = match &te.kind {
            TypeExprKind::Named { name, .. } => (*name, NO_ARGS),
            TypeExprKind::Generic { name, args, .. } => (*name, args.as_slice()),
            TypeExprKind::Tuple(elems) if !elems.is_empty() => {
                let mut parts = Vec::with_capacity(elems.len());
                for elem in elems {
                    parts.push(self.describe_field_type(elem, open_aliases, records)?);
                }
                return Ok(FieldType::Tuple(parts));
            }
            _ => return Err(render_type_expr(te)),
        };
        let name_str = resolve(name);

        if let Some(alias) = self.alias_decls.get(&name_str) {
            if alias.params.len() != args.len() || open_aliases.contains(&name_str) {
                return Err(render_type_expr(te));
            }
            let target = substitute_type_params(&alias.target, &alias.params, args);
            open_aliases.push(name_str);
            let described = self.describe_field_type(&target, open_aliases, records);
            open_aliases.pop();
            return described;
        }

        // Builtin types are matched by their canonical name: a range type
        // is described like the list type it is the same type as.
        let canonical = if name_str == "Range" {
            "List"
        } else {
            name_str.as_str()
        };
        match (canonical, args) {
            ("Int", []) => Ok(FieldType::Int),
            ("Float", []) => Ok(FieldType::Float),
            ("String", []) => Ok(FieldType::String),
            ("Bool", []) => Ok(FieldType::Bool),
            ("Date", []) => Ok(FieldType::Date),
            ("Time", []) => Ok(FieldType::Time),
            ("DateTime", []) => Ok(FieldType::DateTime),
            ("List", [elem]) => Ok(FieldType::List(Box::new(self.describe_field_type(
                elem,
                open_aliases,
                records,
            )?))),
            ("Option", [inner]) => Ok(FieldType::Option(Box::new(self.describe_field_type(
                inner,
                open_aliases,
                records,
            )?))),
            ("Map", [key, value]) => {
                // The keys of a JSON object or a TOML table are strings.
                let key_type = self.describe_field_type(key, open_aliases, &mut Vec::new());
                if !matches!(key_type, Ok(FieldType::String)) {
                    return Err(render_type_expr(te));
                }
                Ok(FieldType::Map(Box::new(self.describe_field_type(
                    value,
                    open_aliases,
                    records,
                )?)))
            }
            // A non-generic record type of the program.
            (_, [])
                if name_str.starts_with(|c: char| c.is_uppercase())
                    && !self.known_enums.contains(&name_str)
                    && crate::types::builtins::lookup(&name_str).is_none()
                    && self
                        .record_decls
                        .get(&name_str)
                        .is_none_or(|decl| decl.params.is_empty()) =>
            {
                let Some(ty) = self.res_type(te.res) else {
                    return Err(render_type_expr(te));
                };
                records.push(name_str.clone());
                Ok(FieldType::Record(ty.id))
            }
            // Everything else: type parameters, enums, generic records,
            // Set, Channel, functions, Map with a non-String key, ...
            _ => Err(render_type_expr(te)),
        }
    }

    /// The first field that cannot be decoded, in the record type
    /// `record` or in a record type nested in it. `None` if there is none
    /// or if the compiler has no declaration of `record`.
    fn undecodable_field(
        &self,
        record: &str,
        seen: &mut HashSet<String>,
    ) -> Option<UndecodableField> {
        if !seen.insert(record.to_string()) {
            return None;
        }
        let decl = self.record_decls.get(record)?;
        for field in &decl.fields {
            let mut nested = Vec::new();
            match self.describe_field_type(&field.ty, &mut Vec::new(), &mut nested) {
                Err(part) => {
                    return Some(UndecodableField {
                        record: record.to_string(),
                        field: resolve(field.name),
                        field_type: render_type_expr(&field.ty),
                        part,
                    });
                }
                Ok(_) => {
                    for nested_record in nested {
                        if let Some(found) = self.undecodable_field(&nested_record, seen) {
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
    /// `_map` forms). If `T` names a type declared in the program, every
    /// field the decoder would have to fill must have a decoder;
    /// otherwise the call is a compile error that names the field and its
    /// type. An enum, a builtin container type, or a primitive type given
    /// to a decoder that only decodes records is a compile error as well.
    /// When the type argument is a variable (a `type a` parameter) the
    /// type is only known at run time, where the decoders report the same
    /// problems.
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
        // `binder` is the identifier that makes the argument a variable
        // if it is bound to one.
        let (binder, type_name) = match &type_arg.kind {
            ExprKind::Ident(name) => (*name, *name),
            // `module.Type`: types live in one namespace at run time.
            ExprKind::FieldAccess(receiver, name, _) => match &receiver.kind {
                ExprKind::Ident(module) => (*module, *name),
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        if self.resolve_local(binder).is_some()
            || self.resolve_upvalue_peek(binder).is_some()
            || self.top_level_value_globals.contains(&resolve(binder))
        {
            return Ok(());
        }

        let type_name = resolve(type_name);
        if self.record_decls.contains_key(&type_name) {
            let Some(found) = self.undecodable_field(&type_name, &mut HashSet::new()) else {
                return Ok(());
            };
            let owner = if found.record == type_name {
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
        if self.known_enums.contains(&type_name) {
            return Err(Diagnostic::error(
                Code::InvalidConstruct,
                span,
                format!(
                    "`{builtin_name}` cannot decode `{type_name}`: it is an enum type, and enums have no decoder"
                ),
            )
            .with_help("decode into a record type"));
        }
        // Builtin type names. `json.parse`, `json.parse_map` and
        // `toml.parse_map` also decode the primitive types; every other
        // decoder, and every decoder given a container type such as
        // `List`, needs a record type.
        let is_primitive = module::BUILTIN_PRIMITIVE_NAMES.contains(&type_name.as_str());
        let is_container = module::BUILTIN_GENERIC_CONTAINER_NAMES.contains(&type_name.as_str());
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
                    self.warn_if_shadows_module(*name, param.pattern.span);
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

    /// Emit a warning if `name` shadows a builtin module like `json`, `int`, etc.
    fn warn_if_shadows_module(&mut self, name: Symbol, span: Span) {
        let s = resolve(name);
        if module::is_builtin_module(&s) {
            self.warnings.push(
                Diagnostic::warning(
                    Code::ShadowsModule,
                    span,
                    format!("variable '{s}' shadows the builtin '{s}' module"),
                )
                .with_help(format!("use a different name to access '{s}.*' functions")),
            );
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
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    /// Compile declarations (no main call) and return all functions:
    /// through a session, so that names are resolved, when the program
    /// checks; otherwise by a compiler with no session, which knows the
    /// builtin names only.
    fn compile(input: &str) -> Vec<Function> {
        let (mut session, entry) = crate::session::testing::session_with(&[("main.silt", input)]);
        if !session.analyze(entry).has_errors() {
            return session
                .compile(entry, crate::session::Entry::Tests { filter: None })
                .unwrap_or_else(|e| panic!("{e:?}"))
                .functions;
        }
        let tokens = Lexer::new(crate::source::FileId::default(), input)
            .tokenize()
            .unwrap();
        let program = Parser::new(tokens, input).parse_program().unwrap();
        let mut compiler = Compiler::new();
        compiler.import_all_builtins();
        compiler.compile_declarations(&program).unwrap()
    }

    /// Compile expecting an error, return the error: through a session
    /// when the program checks, by a compiler with no session otherwise.
    fn compile_err(input: &str) -> Diagnostic {
        let (mut session, entry) = crate::session::testing::session_with(&[("main.silt", input)]);
        if !session.analyze(entry).has_errors() {
            return session
                .compile(entry, crate::session::Entry::Tests { filter: None })
                .err()
                .and_then(|errors| errors.into_iter().next())
                .expect("a compile error");
        }
        let tokens = Lexer::new(crate::source::FileId::default(), input)
            .tokenize()
            .unwrap();
        let program = Parser::new(tokens, input).parse_program().unwrap();
        let mut compiler = Compiler::new();
        compiler.compile_declarations(&program).unwrap_err()
    }

    /// Compile without builtin imports (to test import gating).
    fn compile_no_imports(input: &str) -> Result<Vec<Function>, Diagnostic> {
        let tokens = Lexer::new(crate::source::FileId::default(), input)
            .tokenize()
            .unwrap();
        let program = Parser::new(tokens, input).parse_program().unwrap();
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
        let script = &fns[0];
        // Nullary variants are registered as Variant values
        assert!(script.chunk.constants.iter().any(
            |c| matches!(c, Value::Variant(tag, fields) if tag.name() == "Red" && fields.is_empty())
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
        assert!(script.chunk.constants.iter().any(|c| matches!(c, Value::VariantConstructor(tag) if tag.name() == "Circle" && tag.arity() == 1)));
        assert!(script.chunk.constants.iter().any(
            |c| matches!(c, Value::VariantConstructor(tag) if tag.name() == "Rect" && tag.arity() == 2)
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
        // The typechecker reports a module used without an import; the
        // compiler, given such a program anyway, refuses it as a defect.
        let err = compile_err(
            r#"
fn main() {
    list.length([1, 2])
}
"#,
        );
        assert_eq!(err.code, Code::CompilerBug, "{}", err.message);
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
        let tokens = Lexer::new(crate::source::FileId::default(), "fn main() { 42 }")
            .tokenize()
            .unwrap();
        let program = Parser::new(tokens, "fn main() { 42 }")
            .parse_program()
            .unwrap();
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
        let tokens = Lexer::new(
            crate::source::FileId::default(),
            "fn main() { let list = 42\n list }",
        )
        .tokenize()
        .unwrap();
        let program = Parser::new(tokens, "fn main() { let list = 42\n list }")
            .parse_program()
            .unwrap();
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

    #[test]
    fn test_compile_recur_outside_loop() {
        // Reported by the typechecker; a defect if it reaches the compiler.
        let err = compile_err("fn f() { loop(1) }");
        assert_eq!(err.code, Code::CompilerBug, "{}", err.message);
    }

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

        let mut compiler = Compiler::new();
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

        let mut compiler = Compiler::new();
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
