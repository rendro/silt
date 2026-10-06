//! AST-to-bytecode compiler for Silt.
//!
//! Walks the AST and emits stack-based bytecode into `Function` objects.
//! Phase 4: full pattern matching compilation for all pattern types,
//! including nested/recursive patterns, or-patterns, guards, ranges,
//! list/tuple/record/map destructuring, pin patterns, when/else,
//! plus all previous features (closures, upvalues, pipes, lambdas).
//!
//! Code is written through the [`Emitter`] only, which narrows operands
//! and keeps the frame's height: a bare narrowing cast is denied here.

#![deny(clippy::cast_possible_truncation)]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast::{
    BinOp, Decl, Expr, ExprKind, ImportTarget, ListElem, MatchArm, Param, PatternKind, Program,
    Stmt, StringPart, TypeBody, UnaryOp,
};
use crate::bytecode::emit::limit_diagnostic;
use crate::bytecode::ops::Limit;
use crate::bytecode::{Asm, Const, Emitter, Function, Globals, Label, UpvalueDesc, VmClosure};
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, intern, resolve};
use crate::module;
use crate::source::Span;
use crate::typeinfo::{FieldType, Shape, Tag, TypeInfo, TypeTable, VariantInfo};
use crate::types::canonical::{Resolver, canonical_head};
use crate::types::{Type, TypeRef};
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

/// A type as messages write it, a record type by its name.
fn shown(ty: &Type) -> String {
    Type::show_all(&[ty], |_, _| None).remove(0)
}

/// A record field that `json.parse` / `toml.parse` cannot decode.
struct UndecodableField {
    /// The record type that declares the field.
    record: String,
    field: String,
    /// The field's type.
    field_type: String,
    /// The part of the field's type that has no decoder.
    part: String,
}

// ── Bind destruct kind ───────────────────────────────────────────────

/// Describes how to destructure a sub-value from a compound pattern.
enum BindDestructKind {
    Variant(usize),
    Tuple(usize),
    List(usize),
    ListRest(usize),
    RecordField(Symbol),
    /// Anonymous record `...rest` capture: produces a new record containing
    /// every field of the parent record except those listed here.
    RecordRest(Vec<Symbol>),
    MapValue(String),
}

// ── Compiler context ──────────────────────────────────────────────────

/// Per-function compilation state.
///
/// The emitter keeps the height of the run-time frame: the number of
/// values the function's frame holds at the current point of the emitted
/// code. It counts parameters, locals (named and hidden), and operands
/// that a construct has evaluated and keeps on the stack while it
/// evaluates the next one (the left side of `+`, a callee, earlier
/// arguments or elements). The rules the compiler follows:
///
/// - Code compiled for an expression at height `h` leaves exactly one
///   more value in the frame, the expression's value in slot `h`
///   (`Compiler::compile_expr` checks it).
/// - A local's slot is where its value is when it is added: the top of
///   the frame (`add_local`), so it is the local's real position.
/// - Where a scope ends or a failed pattern test lands, values nothing
///   names any more may be left in the frame. `Slide` removes them
///   there. The one exception is an expression in tail position: its
///   value is returned at once and the frame is discarded with it.
struct CompileContext {
    emitter: Emitter,
    locals: Vec<Local>,
    scope_depth: usize,
    /// Frame height at the start of every open scope, innermost last.
    scope_starts: Vec<usize>,
    /// Upvalue descriptors for this function/closure.
    upvalues: Vec<UpvalueDesc>,
    /// The loops the code being compiled is in, innermost last.
    loop_stack: Vec<LoopInfo>,
    /// While the names of a pattern are being bound: the number of
    /// locals there were before the pattern bound any. A pin in the
    /// pattern is one of those (see `Compiler::compile_pattern_bind`).
    pattern_floor: Option<usize>,
}

struct LoopInfo {
    /// The slot of the loop's first binding.
    first_slot: usize,
    /// Where a `loop(...)` jumps back to.
    start: Label,
    binding_count: usize,
}

struct Local {
    name: Symbol,
    depth: usize,
    slot: usize,
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

/// A defect in silt: a name that the resolver resolved (every name
/// that resolves to nothing is an error of the analysis, and a program
/// with one is not compiled) but that is no local, upvalue or global the
/// compiler has.
fn name_without_binding(span: Span, name: Symbol) -> Diagnostic {
    Diagnostic::error(
        Code::CompilerBug,
        span,
        format!("compiler bug: the name '{name}' has no binding in the compiled code"),
    )
}

// ── Compiler ──────────────────────────────────────────────────────────

/// A module of the program, as the session hands it to the compiler:
/// parsed and typechecked, with what each of its imports names.
pub struct ModuleUnit {
    /// The module, as the session knows it.
    pub id: crate::session::ModuleId,
    /// The module's declarations, after the typechecker filled them in.
    pub program: Arc<Program>,
    /// The module's top-level `let`s, by the span of each, in the order
    /// the checker decided they are initialised in.
    pub let_order: Vec<Span>,
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
    /// The fields of each record type of the session, with their types
    /// as the checker inferred them, in declaration order.
    pub record_fields: Arc<HashMap<crate::defs::TypeId, Vec<(Symbol, Type)>>>,
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

/// What a `match` has too much of when a failed test or guard cannot
/// jump over an arm.
const MATCH_ARM: &str = "bytes of code in a match arm";

/// A program needs more global slots than the instruction operand can
/// name: the definition at `span` would be its `count`th.
fn too_many_globals(count: usize, span: Span) -> Diagnostic {
    let limit = Limit {
        what: "top-level definitions of a program (functions, `let`s and trait methods)",
        count,
        max: usize::from(u16::MAX) + 1,
    };
    limit_diagnostic(limit, span)
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
    /// builtin traits' default methods and the derived impls of the
    /// builtin types (unless an earlier REPL entry installed them),
    /// then, module by module, each function, `let`, host function and
    /// default method of a trait, and last each impl method. An impl
    /// that leaves a default method out gets no slot for it: its method
    /// is the trait's. Gives the slots of the modules' own functions,
    /// `let`s and host functions by module and name.
    fn assign_slots(
        &self,
        globals: &mut Globals,
    ) -> Result<HashMap<(crate::session::ModuleId, Symbol), u16>, Diagnostic> {
        let mut own = HashMap::new();
        // The default methods of each trait, in the trait's order.
        let mut defaults: HashMap<crate::defs::TraitId, Vec<Symbol>> = HashMap::new();
        let builtin_defaults = crate::typechecker::builtin_default_methods();
        for (t, method) in &builtin_defaults {
            defaults.entry(*t).or_default().push(method.name);
        }
        for unit in &self.units.modules {
            for decl in &unit.program.decls {
                if let Decl::Trait(t) = decl
                    && let Some(id) = self.declared_trait(unit.id, t.name)
                {
                    defaults.entry(id).or_default().extend(
                        t.methods
                            .iter()
                            .filter(|m| !m.is_signature_only)
                            .map(|m| m.name),
                    );
                }
            }
        }
        if !self.builtin_impls_installed {
            for (t, method) in &builtin_defaults {
                let trait_name = self.units.defs.get(t.0).name;
                globals
                    .add_default_method(
                        *t,
                        &resolve(method.name),
                        format!("{trait_name}.{}", method.name),
                    )
                    .ok_or_else(|| too_many_globals(globals.len() + 1, method.span))?;
            }
            for decl in crate::typechecker::builtin_derived_impls().iter() {
                if let Decl::TraitImpl(ti) = decl {
                    self.assign_method_slots(ti, &defaults, globals)?;
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
                    true => format!("{}.{}", unit.qualifier, def.name),
                    false => resolve(def.name),
                };
                let slot = globals
                    .add_def(id, name)
                    .ok_or_else(|| too_many_globals(globals.len() + 1, def.span))?;
                own.insert((unit.id, def.name), slot);
            }
            for decl in &unit.program.decls {
                let Decl::Trait(t) = decl else {
                    continue;
                };
                let Some(id) = self.declared_trait(unit.id, t.name) else {
                    continue;
                };
                for method in t.methods.iter().filter(|m| !m.is_signature_only) {
                    globals
                        .add_default_method(
                            id,
                            &resolve(method.name),
                            format!("{}.{}", t.name, method.name),
                        )
                        .ok_or_else(|| too_many_globals(globals.len() + 1, method.span))?;
                }
            }
        }
        // (An impl may be of a trait of a module that comes later.)
        for (index, unit) in self.units.modules.iter().enumerate() {
            if self.units.earlier.installed.contains(&index) {
                continue;
            }
            for decl in &unit.program.decls {
                if let Decl::TraitImpl(ti) = decl {
                    self.assign_method_slots(ti, &defaults, globals)?;
                }
            }
        }
        Ok(own)
    }

    /// Give a global slot to each method the impl `ti` writes; a default
    /// method of the trait (`defaults`) it leaves out is the trait's.
    fn assign_method_slots(
        &self,
        ti: &crate::ast::TraitImpl,
        defaults: &HashMap<crate::defs::TraitId, Vec<Symbol>>,
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
                .ok_or_else(|| too_many_globals(globals.len() + 1, method.span))?;
        }
        for method in defaults.get(&t).into_iter().flatten() {
            if ti.methods.iter().any(|written| written.name == *method) {
                continue;
            }
            if let Some(slot) = globals.default_method(t, &resolve(*method)) {
                globals.default_for(t, ty, &resolve(*method), slot);
            }
        }
        Ok(())
    }

    /// The trait the module `module` declares as `name`.
    fn declared_trait(
        &self,
        module: crate::session::ModuleId,
        name: Symbol,
    ) -> Option<crate::defs::TraitId> {
        self.units
            .defs
            .of_module(module)
            .iter()
            .find_map(|&id| match self.units.defs.get(id) {
                def if def.name == name => match def.kind {
                    crate::defs::DefKind::Trait(t) => Some(t),
                    _ => None,
                },
                _ => None,
            })
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
        self.begin_function("<script>".into(), 0, Span::BUILTIN)?;

        self.compile_builtin_derived_impls()?;
        let order = self.units.modules[self.units.entry].let_order.clone();
        for decl in Self::decls_in_init_order(&program.decls, &order) {
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
                self.emit(Asm::GetGlobal { slot }, span)?;
                self.emit(Asm::Call { argc: 0 }, span)?;
            }
            None => {
                self.emit(Asm::Unit, span)?;
            }
        }
        self.emit(Asm::Return, span)?;

        let (script, _) = self.end_function(Span::BUILTIN)?;

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
        self.begin_function("<script>".into(), 0, Span::BUILTIN)?;

        self.compile_builtin_derived_impls()?;
        let order = self.units.modules[self.units.entry].let_order.clone();
        for decl in Self::decls_in_init_order(&program.decls, &order) {
            self.compile_decl(decl)?;
        }

        // Return Unit instead of calling main: code silt adds itself.
        let span = Span::BUILTIN;
        self.emit(Asm::Unit, span)?;
        self.emit(Asm::Return, span)?;

        let (script, _) = self.end_function(Span::BUILTIN)?;
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
        for (t, method) in crate::typechecker::builtin_default_methods() {
            self.compile_default_method(t, &resolve(self.units.defs.get(t.0).name), &method)?;
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

    /// The order in which a module's declarations are installed: first
    /// everything that only defines something (imports, types, traits,
    /// trait impls, functions), in source order; then the top-level
    /// `let`s, in the order the checker decided (`let_order`, by the
    /// span of each: every `let` after the ones its initialiser can
    /// reach). A top-level initialiser can therefore use every
    /// declaration of the program, wherever it is written.
    fn decls_in_init_order<'d>(decls: &'d [Decl], let_order: &[Span]) -> Vec<&'d Decl> {
        let (mut lets, definitions): (Vec<&Decl>, Vec<&Decl>) = decls
            .iter()
            .partition(|decl| matches!(**decl, Decl::Let { .. }));
        // (A `let` the order does not name, which no checked module
        // has, keeps its place after the ones it names.)
        lets.sort_by_key(|decl| match decl {
            Decl::Let { span, .. } => let_order
                .iter()
                .position(|at| at == span)
                .unwrap_or(usize::MAX),
            _ => usize::MAX,
        });
        definitions.into_iter().chain(lets).collect()
    }

    /// Compile the method `method` (an impl's, or a trait's default) as
    /// the function `name`, and leave it on the stack.
    fn compile_method(
        &mut self,
        name: String,
        method: &crate::ast::FnDecl,
    ) -> Result<(), Diagnostic> {
        let span = method.span;
        self.begin_function(name, method.params.len(), span)?;

        self.compile_params(&method.params, span)?;

        // The body is in tail position, like a function's.
        self.in_tail_position = true;
        self.compile_expr(&method.body)?;
        self.in_tail_position = false;
        self.emit(Asm::Return, span)?;

        let (func, _) = self.end_function(span)?;
        let vm_closure = Arc::new(VmClosure {
            function: Arc::new(func),
            upvalues: vec![],
        });
        let closure_val = Value::VmClosure(vm_closure);
        let fi = self.add_constant(closure_val, span)?;
        self.emit(Asm::Constant { k: fi }, span)
    }

    /// Compile the default method `method` of the trait `t`, named
    /// `trait_name`, once, into its global slot: the method of every
    /// impl that leaves it out.
    fn compile_default_method(
        &mut self,
        t: crate::defs::TraitId,
        trait_name: &str,
        method: &crate::ast::FnDecl,
    ) -> Result<(), Diagnostic> {
        let span = method.span;
        self.compile_method(format!("{trait_name}.{}", method.name), method)?;
        let slot = self
            .globals
            .default_method(t, &resolve(method.name))
            .ok_or_else(|| checker_missed(span, "a default method with no slot"))?;
        self.emit(Asm::SetGlobal { slot }, span)?;
        self.emit(Asm::Pop, span)
    }

    fn compile_decl(&mut self, decl: &Decl) -> Result<(), Diagnostic> {
        match decl {
            Decl::Fn(fn_decl) => {
                let span = fn_decl.span;
                // Push a new context for the function body.
                self.begin_function(resolve(fn_decl.name), fn_decl.params.len(), span)?;

                self.compile_params(&fn_decl.params, span)?;

                // Compile the function body in tail position for TCO.
                self.in_tail_position = true;
                self.compile_expr(&fn_decl.body)?;
                self.in_tail_position = false;

                // Emit Return (may be dead code if body ends with a tail call).
                self.emit(Asm::Return, span)?;

                // Pop the context, recovering the compiled function.
                let (func, _) = self.end_function(span)?;

                // Store the function as a VmClosure constant in the enclosing chunk.
                let vm_closure = Arc::new(VmClosure {
                    function: Arc::new(func),
                    upvalues: vec![],
                });
                let closure_val = Value::VmClosure(vm_closure);
                let fi = self.add_constant(closure_val, span)?;
                self.emit(Asm::Constant { k: fi }, span)?;

                let slot = self.own_slot(fn_decl.name, span)?;
                self.emit(Asm::SetGlobal { slot }, span)?;
                self.emit(Asm::Pop, span)?;

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
                        self.emit(Asm::SetGlobal { slot }, span)?;
                        self.emit(Asm::Pop, span)?;
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
                    self.compile_method(format!("{type_name}.{}", method.name), method)?;
                    let slot = self
                        .globals
                        .method(Some(t), ty, &resolve(method.name))
                        .ok_or_else(|| checker_missed(span, "an impl method with no slot"))?;
                    self.emit(Asm::SetGlobal { slot }, span)?;
                    self.emit(Asm::Pop, span)?;
                }
                Ok(())
            }

            Decl::Trait(t) => {
                // A trait declares an interface; each default method it
                // writes is compiled, once.
                let current = self.unit_stack.last().copied().unwrap_or(self.units.entry);
                let module = self.units.modules[current].id;
                let Some(id) = self.declared_trait(module, t.name) else {
                    return Err(checker_missed(
                        t.span,
                        &format!("the trait '{}' with no definition", t.name),
                    ));
                };
                for method in t.methods.iter().filter(|m| !m.is_signature_only) {
                    self.compile_default_method(id, &resolve(t.name), method)?;
                }
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
        self.begin_scope_with_top();
        let val_slot = self.add_local(intern("__let_val__"), span)?;
        self.emit(Asm::SetLocal { slot: val_slot }, span)?;
        self.compile_pattern_bind_checked(pattern, span)?;
        for (name, global) in slots {
            let slot = self.resolve_local(*name).ok_or_else(|| {
                checker_missed(span, &format!("the binder '{name}' of a top-level let"))
            })?;
            self.emit(Asm::GetLocal { slot }, span)?;
            self.emit(Asm::SetGlobal { slot: *global }, span)?;
            self.emit(Asm::Pop, span)?;
        }
        self.emit(Asm::Unit, span)?;
        self.end_scope_with_result(false, span)?;
        self.emit(Asm::Pop, span)?;
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
            self.emit(Asm::Constant { k: fi }, span)?;
            let slot = self.own_slot(f.name, span)?;
            self.emit(Asm::SetGlobal { slot }, span)?;
            self.emit(Asm::Pop, span)?;
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
        self.begin_function(init_name, 0, span)?;

        let current = self.unit_stack.last().copied().unwrap_or(self.units.entry);
        let order = self.units.modules[current].let_order.clone();
        for decl in Self::decls_in_init_order(&program.decls, &order) {
            self.compile_decl(decl)?;
        }

        // Close the module init function and call it inline. Code silt
        // adds itself carries the import statement's span, so anything
        // that blames it points back to the import site.
        self.emit(Asm::Unit, span)?;
        self.emit(Asm::Return, span)?;
        let (init, _) = self.end_function(span)?;
        let init_closure = Arc::new(VmClosure {
            function: Arc::new(init),
            upvalues: vec![],
        });
        let ci = self.add_constant(Value::VmClosure(init_closure), span)?;
        self.emit(Asm::Constant { k: ci }, span)?;
        self.emit(Asm::Call { argc: 0 }, span)?;
        self.emit(Asm::Pop, span)?;
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
                        self.emit(Asm::SetLocal { slot }, span)?;
                        if is_last {
                            self.emit(Asm::Unit, span)?;
                        }
                    }
                    _ => {
                        // General pattern destructuring for let bindings.
                        // The value stays in the frame as a hidden local and
                        // the pattern's names are bound from it.
                        let val_slot = self.add_local(intern("__let_val__"), span)?;
                        self.emit(Asm::SetLocal { slot: val_slot }, span)?;
                        self.compile_pattern_bind_checked(pattern, span)?;

                        if is_last {
                            self.emit(Asm::Unit, span)?;
                        }
                    }
                }

                Ok(())
            }

            Stmt::Expr(expr) => {
                self.compile_expr(expr)?;
                if !is_last {
                    self.emit(Asm::Pop, expr.span)?;
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
                let else_jump = self.jump_if_false(condition.span)?;

                // Condition was true — skip else block
                let end_jump = self.jump(condition.span)?;

                // Else block: condition was false
                self.bind(else_jump, condition.span)?;
                self.compile_expr(else_body)?;
                // The else body must diverge (return, panic or loop(...)).
                // If it doesn't, we just pop its value and continue.
                self.emit(Asm::Pop, condition.span)?;

                self.bind(end_jump, condition.span)?;

                if is_last {
                    self.emit(Asm::Unit, condition.span)?;
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
                self.emit(Asm::SetLocal { slot: val_slot }, span)?;

                let fail_jumps = self.compile_pattern_test(pattern, span)?;
                let matched_jump = self.jump(span)?;

                // Pattern didn't match. A failed test of a nested pattern
                // leaves the sub-values it was looking at above the value;
                // drop them. The else body is compiled before the pattern's
                // names exist, so a name it uses is the one of the enclosing
                // scope. The else body diverges (the typechecker requires
                // it), so control never reaches the bindings from here.
                for fj in fail_jumps {
                    self.bind(fj, span)?;
                }
                self.emit(Asm::GetLocal { slot: val_slot }, span)?;
                self.emit(Asm::Slide { slot: val_slot }, span)?;
                self.compile_expr(else_body)?;
                self.emit(Asm::Pop, span)?; // pop else result

                // Pattern matched — bind variables
                self.bind(matched_jump, span)?;
                self.compile_pattern_bind(pattern, span)?;

                if is_last {
                    self.emit(Asm::Unit, span)?;
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
    fn emit_call(&mut self, argc: usize, tail: bool, span: Span) -> Result<(), Diagnostic> {
        if tail {
            self.emit(Asm::TailCall { argc }, span)?;
            self.emit(Asm::Return, span)
        } else {
            self.emit(Asm::Call { argc }, span)
        }
    }

    /// Compile `expr`: at height `h`, code that leaves the expression's
    /// value in slot `h` and the frame one value higher. In tail
    /// position the locals of the expression's scopes may still be under
    /// the value, since the frame goes when the value is returned.
    fn compile_expr(&mut self, expr: &Expr) -> Result<(), Diagnostic> {
        let tail = self.in_tail_position;
        let before = self.emitter().height();
        self.compile_expr_kind(expr)?;
        let emitter = self.emitter();
        if !emitter.reachable() {
            // The expression does not return (`return`, `panic`, a
            // `loop(...)`): code after it is dead, and compiled as if
            // the value were there.
            emitter.assume_height(before + 1);
            return Ok(());
        }
        let after = emitter.height();
        if after == before + 1 || (tail && after > before) {
            return Ok(());
        }
        Err(Diagnostic::error(
            Code::CompilerBug,
            expr.span,
            format!(
                "compiler bug: the code of this expression takes the frame of '{}' \
                 from {before} values to {after}",
                emitter.name()
            ),
        ))
    }

    fn compile_expr_kind(&mut self, expr: &Expr) -> Result<(), Diagnostic> {
        let span = expr.span;
        let tail = self.in_tail_position;
        self.in_tail_position = false;

        match &expr.kind {
            ExprKind::Int(n) => {
                let idx = self.add_constant(Value::Int(*n), span)?;
                self.emit(Asm::Constant { k: idx }, span)?;
            }

            ExprKind::Float(n) => {
                let idx = self.add_constant(Value::Float(*n), span)?;
                self.emit(Asm::Constant { k: idx }, span)?;
            }

            ExprKind::Bool(b) => {
                if *b {
                    self.emit(Asm::True, span)?;
                } else {
                    self.emit(Asm::False, span)?;
                }
            }

            ExprKind::StringLit(s, _) => {
                let idx = self.add_constant(Value::String(s.clone()), span)?;
                self.emit(Asm::Constant { k: idx }, span)?;
            }

            ExprKind::Unit => {
                self.emit(Asm::Unit, span)?;
            }

            ExprKind::Binary(left, op, right) => {
                match op {
                    BinOp::And => {
                        // Short-circuit: if left is false, skip right
                        self.compile_expr(left)?;
                        // Duplicate TOS so we can test and still have the value
                        self.emit(Asm::Dup, span)?;
                        let jump = self.jump_if_false(span)?;
                        // Left was truthy, discard it and evaluate right
                        self.emit(Asm::Pop, span)?;
                        self.compile_expr(right)?;
                        self.bind_over(jump, "bytes of code in the right operand of `&&`", span)?;
                    }
                    BinOp::Or => {
                        // Short-circuit: if left is true, skip right
                        self.compile_expr(left)?;
                        // Duplicate TOS so we can test and still have the value
                        self.emit(Asm::Dup, span)?;
                        let jump = self.jump_if_true(span)?;
                        // Left was falsy, discard it and evaluate right
                        self.emit(Asm::Pop, span)?;
                        self.compile_expr(right)?;
                        self.bind_over(jump, "bytes of code in the right operand of `||`", span)?;
                    }
                    _ => {
                        self.compile_operands([&**left, &**right])?;
                        let instruction = match op {
                            BinOp::Add => Asm::Add,
                            BinOp::Sub => Asm::Sub,
                            BinOp::Mul => Asm::Mul,
                            BinOp::Div => Asm::Div,
                            BinOp::Mod => Asm::Mod,
                            BinOp::Eq => Asm::Eq,
                            BinOp::Neq => Asm::Neq,
                            BinOp::Lt => Asm::Lt,
                            BinOp::Gt => Asm::Gt,
                            BinOp::Leq => Asm::Leq,
                            BinOp::Geq => Asm::Geq,
                            BinOp::And | BinOp::Or => unreachable!(),
                        };
                        self.emit(instruction, span)?;
                    }
                }
            }

            ExprKind::Unary(op, operand) => {
                self.compile_expr(operand)?;
                let instruction = match op {
                    UnaryOp::Neg => Asm::Negate,
                    UnaryOp::Not => Asm::Not,
                };
                self.emit(instruction, span)?;
            }

            ExprKind::Block(stmts) => {
                self.begin_scope();

                if stmts.is_empty() {
                    // Empty block evaluates to Unit.
                    self.emit(Asm::Unit, span)?;
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
                self.emit(Asm::Constant { k: idx }, span)?;
            }

            // A type used as a value is its descriptor.
            ExprKind::Ident(_) if let Some(descriptor) = self.type_value(expr.res) => {
                let idx = self.add_constant(descriptor, span)?;
                self.emit(Asm::Constant { k: idx }, span)?;
            }

            ExprKind::Ident(_) if let Some(def) = self.value_def(expr.res) => {
                self.emit_global_value(def, span)?;
            }

            ExprKind::Ident(name) => {
                if let Some(slot) = self.resolve_local(*name) {
                    self.emit(Asm::GetLocal { slot }, span)?;
                } else if let Some(idx) = self.resolve_upvalue(*name) {
                    self.emit(Asm::GetUpvalue { index: idx }, span)?;
                } else {
                    return Err(name_without_binding(span, *name));
                }
            }

            ExprKind::Call(callee, args) => {
                let args: Vec<&Expr> = args.iter().collect();
                self.compile_call(callee, &args, span, tail)?;
            }

            // A variant: `EnumName.Variant`, `time.Monday`, `m.Color.Red`.
            ExprKind::FieldAccess(..) if let Some(variant) = self.variant_value(expr) => {
                let idx = self.add_constant(variant, span)?;
                self.emit(Asm::Constant { k: idx }, span)?;
            }

            // `m.Pt` used as a value: the type's descriptor.
            ExprKind::FieldAccess(..) if let Some(descriptor) = self.type_value(expr.res) => {
                let idx = self.add_constant(descriptor, span)?;
                self.emit(Asm::Constant { k: idx }, span)?;
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
                let mut access = Expr::new(
                    ExprKind::FieldAccess(Box::new(ident(names[0])), *method, span),
                    span,
                );
                access.sel = Some(crate::ast::Selection::Native {
                    tr: self.builtin_method_trait(expr.res, *method, span)?,
                });
                let call = Expr::new(
                    ExprKind::Call(
                        Box::new(access),
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
                self.emit(Asm::GetGlobal { slot }, span)?;
            }

            // `m.f`, `m.limit`, `list.map` used as a value.
            ExprKind::FieldAccess(..) if let Some(def) = self.value_def(expr.res) => {
                self.emit_global_value(def, span)?;
            }

            // A record's field. (Tuple indexing, `t.0`, is rejected by
            // the checker.)
            ExprKind::FieldAccess(expr, field, _) => {
                self.compile_expr(expr)?;
                let name_idx = self.add_constant(Value::String(resolve(*field)), span)?;
                self.emit(Asm::GetField { name: name_idx }, span)?;
            }

            ExprKind::StringInterp(parts) => {
                // Every part stays on the stack until `StringConcat`.
                for part in parts {
                    match part {
                        StringPart::Literal(s) => {
                            let idx = self.add_constant(Value::String(s.clone()), span)?;
                            self.emit(Asm::Constant { k: idx }, span)?;
                        }
                        StringPart::Expr(e) => {
                            self.compile_expr(e)?;
                            self.emit(Asm::DisplayValue, span)?;
                        }
                    }
                }
                self.emit(Asm::StringConcat { count: parts.len() }, span)?;
            }

            ExprKind::Return(maybe_expr) => {
                if let Some(e) = maybe_expr {
                    // Explicit return is always in tail position.
                    self.in_tail_position = true;
                    self.compile_expr(e)?;
                } else {
                    self.emit(Asm::Unit, span)?;
                }
                self.emit(Asm::Return, span)?;
            }

            ExprKind::Match { expr, arms } => {
                self.compile_match(expr.as_deref(), arms, span, tail)?;
            }

            ExprKind::Lambda { params, body, .. } => {
                // Push a new context for the lambda body.
                self.begin_function("<lambda>".into(), params.len(), span)?;

                self.compile_params(params, span)?;

                // Compile the lambda body in tail position for TCO.
                self.in_tail_position = true;
                self.compile_expr(body)?;
                self.in_tail_position = false;
                self.emit(Asm::Return, span)?;

                let (func, upvalue_descs) = self.end_function(span)?;

                let vm_closure = Arc::new(VmClosure {
                    function: Arc::new(func),
                    upvalues: vec![],
                });
                let closure_val = Value::VmClosure(vm_closure);
                let fi = self.add_constant(closure_val, span)?;

                if upvalue_descs.is_empty() {
                    // No upvalues: just push the constant directly.
                    self.emit(Asm::Constant { k: fi }, span)?;
                } else {
                    // Has upvalues: emit MakeClosure with descriptors.
                    self.emit(
                        Asm::MakeClosure {
                            f: fi,
                            captures: &upvalue_descs,
                        },
                        span,
                    )?;
                }
            }

            ExprKind::Tuple(elems) => {
                self.compile_operands(elems)?;
                self.emit(Asm::MakeTuple { count: elems.len() }, span)?;
            }

            ExprKind::List(elems) => {
                let has_spread = elems.iter().any(|e| matches!(e, ListElem::Spread(_)));
                if !has_spread {
                    // Fast path: no spreads, just compile all singles
                    self.compile_operands(elems.iter().filter_map(|elem| match elem {
                        ListElem::Single(e) => Some(e),
                        ListElem::Spread(_) => None,
                    }))?;
                    self.emit(Asm::MakeList { count: elems.len() }, span)?;
                } else {
                    // Spread path: group consecutive singles into segments,
                    // compile each spread, and ListConcat them together.
                    //
                    // While an element is compiled the stack holds the list
                    // accumulated so far (if any) and the singles not yet
                    // collected.
                    let mut have_accumulated = false;
                    let mut single_count: usize = 0;

                    for elem in elems {
                        match elem {
                            ListElem::Single(e) => {
                                self.compile_expr(e)?;
                                single_count += 1;
                            }
                            ListElem::Spread(e) => {
                                // Flush any pending singles as a MakeList
                                if single_count > 0 {
                                    self.emit(
                                        Asm::MakeList {
                                            count: single_count,
                                        },
                                        span,
                                    )?;
                                    if have_accumulated {
                                        self.emit(Asm::ListConcat, span)?;
                                    }
                                    have_accumulated = true;
                                    single_count = 0;
                                }
                                // Compile the spread expression (should be a list or range)
                                self.compile_expr(e)?;
                                if have_accumulated {
                                    self.emit(Asm::ListConcat, span)?;
                                } else {
                                    have_accumulated = true;
                                }
                            }
                        }
                    }
                    // Flush any trailing singles
                    if single_count > 0 {
                        self.emit(
                            Asm::MakeList {
                                count: single_count,
                            },
                            span,
                        )?;
                        if have_accumulated {
                            self.emit(Asm::ListConcat, span)?;
                        }
                    } else if !have_accumulated {
                        // Edge case: empty list with spreads (shouldn't happen, but be safe)
                        self.emit(Asm::MakeList { count: 0 }, span)?;
                    }
                }
            }

            ExprKind::Map(pairs) => {
                self.compile_operands(pairs.iter().flat_map(|(k, v)| [k, v]))?;
                self.emit(Asm::MakeMap { pairs: pairs.len() }, span)?;
            }

            ExprKind::SetLit(elems) => {
                self.compile_operands(elems)?;
                self.emit(Asm::MakeSet { count: elems.len() }, span)?;
            }

            ExprKind::Range(start, end) => {
                self.compile_operands([&**start, &**end])?;
                self.emit(Asm::MakeRange, span)?;
            }

            ExprKind::Pipe(left, right) => {
                // val |> f(args) --> f(val, args)
                // val |> f       --> f(val)
                self.compile_pipe(left, right, span, tail)?;
            }

            ExprKind::QuestionMark(inner) => {
                self.compile_expr(inner)?;
                self.emit(Asm::QuestionMark, span)?;
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
                // Push field values in order
                let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                self.compile_operands(fields.iter().map(|(_, val)| val))?;
                let ty = self.record_type(expr.res, *name, span)?;
                let ty = self.add_constant(Value::TypeDescriptor(ty), span)?;
                let fields = self.name_constants(&field_names, span)?;
                self.emit(
                    Asm::MakeRecord {
                        ty,
                        fields: &fields,
                    },
                    span,
                )?;
            }

            ExprKind::RecordUpdate { expr, fields } => {
                let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                self.compile_operands(
                    std::iter::once(&**expr).chain(fields.iter().map(|(_, val)| val)),
                )?;
                let fields = self.name_constants(&field_names, span)?;
                self.emit(Asm::RecordUpdate { fields: &fields }, span)?;
            }

            ExprKind::AnonRecord { spread, fields } => {
                if let Some(base) = spread {
                    // A spread makes a new anonymous record whatever the
                    // base is: the base's fields as an anonymous record
                    // (the rest of the base with no field left out),
                    // then the written fields merged in.
                    self.in_tail_position = false;
                    self.compile_expr(base)?;
                    self.emit(Asm::DestructRecordRest { excluded: &[] }, span)?;
                    let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                    self.compile_operands(fields.iter().map(|(_, val)| val))?;
                    let fields = self.name_constants(&field_names, span)?;
                    self.emit(Asm::RecordUpdate { fields: &fields }, span)?;
                } else {
                    // Closed anon record literal: same encoding as nominal
                    // RecordCreate but with the anonymous record type,
                    // which every run-time record-type check accepts (see
                    // `bytecode::record_type_matches`).
                    let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                    self.compile_operands(fields.iter().map(|(_, val)| val))?;
                    let anon = crate::typeinfo::builtin_type(crate::typeinfo::ty::ANON_RECORD);
                    let ty = self.add_constant(Value::TypeDescriptor(anon.clone()), span)?;
                    let fields = self.name_constants(&field_names, span)?;
                    self.emit(
                        Asm::MakeRecord {
                            ty,
                            fields: &fields,
                        },
                        span,
                    )?;
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
                let first = loop_info.first_slot;
                let start = loop_info.start;
                if args.len() != loop_info.binding_count {
                    return Err(checker_missed(
                        span,
                        "a `loop(...)` with the wrong number of arguments",
                    ));
                }
                self.compile_operands(args)?;
                self.emit(
                    Asm::Recur {
                        argc: args.len(),
                        first,
                    },
                    span,
                )?;
                self.emit(Asm::Jump { to: start }, span)?;
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
        self.begin_scope_with_top();
        let scrutinee_slot = self.add_local(intern("__scrutinee__"), span)?;
        self.emit(
            Asm::SetLocal {
                slot: scrutinee_slot,
            },
            span,
        )?;
        // Frame height at the start of every arm: everything up to and
        // including the scrutinee.
        let arm_height = self.emitter().height();

        let mut end_jumps = Vec::new();

        for (i, arm) in arms.iter().enumerate() {
            // 1. Push scrutinee for testing. An arm that did not match
            //    lands here with values still above the scrutinee: the
            //    copy its test looked at, sub-values of a nested pattern,
            //    or the names it bound before its guard failed. The slide
            //    drops them and keeps the fresh copy.
            self.emit(
                Asm::GetLocal {
                    slot: scrutinee_slot,
                },
                span,
            )?;
            if i > 0 {
                self.emit_slide(arm_height, span)?;
            }

            // 2. Test the pattern (value is on TOS, tests peek it)
            let fail_jumps = self.compile_pattern_test(&arm.pattern, span)?;

            // 3. Pop the test copy
            self.emit(Asm::Pop, span)?;

            // 4. Begin a scope for this arm's bindings
            self.begin_scope();

            // 5. Push scrutinee again and bind pattern variables
            self.emit(
                Asm::GetLocal {
                    slot: scrutinee_slot,
                },
                span,
            )?;
            // Register this GetLocal'd copy as a hidden local
            let bind_copy = self.add_local(intern("__bind_src__"), span)?;
            self.emit(Asm::SetLocal { slot: bind_copy }, span)?;
            self.compile_pattern_bind(&arm.pattern, span)?;

            // 6. Guard (if present)
            let guard_jump = if let Some(guard) = &arm.guard {
                self.compile_expr(guard)?;
                let j = self.jump_if_false(span)?;
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
            let end_jump = self.jump(span)?;
            end_jumps.push(end_jump);

            // 9. Patch failure / guard jumps to here (next arm)
            if let Some(gj) = guard_jump {
                self.bind_over(gj, MATCH_ARM, span)?;
            }
            for fj in fail_jumps {
                self.bind_over(fj, MATCH_ARM, span)?;
            }
        }

        // No arm matched — panic
        let msg_idx = self.add_constant(
            Value::String("non-exhaustive match: no arm matched".into()),
            span,
        )?;
        self.emit(Asm::Constant { k: msg_idx }, span)?;
        self.emit(Asm::Panic, span)?;

        let result_height = self.end_scope();

        // Patch all end jumps to here
        for ej in end_jumps {
            self.bind(ej, span)?;
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
                let fail_jump = self.jump_if_false(span)?;

                self.in_tail_position = tail;
                self.compile_expr(&arm.body)?;
                let end_jump = self.jump(span)?;
                end_jumps.push(end_jump);

                self.bind_over(fail_jump, MATCH_ARM, span)?;
            } else {
                // Wildcard / default arm — always matches
                self.in_tail_position = tail;
                self.compile_expr(&arm.body)?;
                let end_jump = self.jump(span)?;
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
        self.emit(Asm::Constant { k: msg_idx }, span)?;
        self.emit(Asm::Panic, span)?;

        for ej in end_jumps {
            self.bind(ej, span)?;
        }

        Ok(())
    }

    // ── Pipe compilation ─────────────────────────────────────────

    /// Compile the call of `callee` with `args`: `f(a, b)`, and
    /// `a |> f(b)`, which is the same call (`compile_pipe`).
    fn compile_call(
        &mut self,
        callee: &Expr,
        args: &[&Expr],
        span: Span,
        tail: bool,
    ) -> Result<(), Diagnostic> {
        if let Some(variant) = self.variant_value(callee) {
            // A variant's constructor: `Circle(r)`,
            // `Shape.Circle(r)`, `channel.Message(v)`,
            // `m.Shape.Circle(r)`.
            let idx = self.add_constant(variant, span)?;
            self.emit(Asm::Constant { k: idx }, span)?;
            self.compile_operands(args.iter().copied())?;
            let argc = args.len();
            self.emit_call(argc, tail, span)?;
        } else if let Some(builtin_name) = self.builtin_module_function(callee) {
            // A builtin module's function: `list.map(...)`.
            self.check_decode_target(&builtin_name, args.last().copied(), span)?;
            self.compile_operands(args.iter().copied())?;
            let argc = args.len();
            let name_idx = self.add_constant(Value::String(builtin_name), span)?;
            self.emit(
                Asm::CallBuiltin {
                    name: name_idx,
                    argc,
                },
                span,
            )?;
        } else if let ExprKind::FieldAccess(receiver, method, _) = &callee.kind {
            if self.builtin_trait_method_of_builtin_type(callee) && !args.is_empty() {
                // `Int.display(1)`: a builtin trait's method of a
                // builtin type, which is native, not a global; the
                // first argument is the receiver.
                let t = self.builtin_method_trait(callee.res, *method, span)?;
                self.compile_operands(args.iter().copied())?;
                self.emit_call_method(*method, args.len(), t, tail, span)?;
            } else if let Some(slot) = self.qualified_type_member(callee)? {
                // `Pt.make(1)`, `m.Pt.make(1)`: a method reached
                // through its type.
                self.emit(Asm::GetGlobal { slot }, span)?;
                self.compile_operands(args.iter().copied())?;
                let argc = args.len();
                self.emit_call(argc, tail, span)?;
            } else if let Some(def) = self.value_def(callee.res) {
                // A module's function: `m.f(1)`.
                self.emit_global_value(def, span)?;
                self.compile_operands(args.iter().copied())?;
                let argc = args.len();
                self.emit_call(argc, tail, span)?;
            } else {
                // Method call on a value: expr.method(args)
                self.compile_method_call(callee, receiver, *method, args, span, tail)?;
            }
        } else {
            // Normal function call. A decoder imported by name
            // (`import json.{ parse }`) is checked like
            // `json.parse(..)`.
            if let Some(builtin_name) = self.builtin_function(callee.res) {
                self.check_decode_target(&builtin_name, args.last().copied(), span)?;
            }
            self.compile_operands(std::iter::once(callee).chain(args.iter().copied()))?;
            let argc = args.len();
            self.emit_call(argc, tail, span)?;
        }
        Ok(())
    }

    /// Compile `receiver.method(args)`, a call on a value, as the
    /// checker's `Selection` on `callee` says.
    fn compile_method_call(
        &mut self,
        callee: &Expr,
        receiver: &Expr,
        method: Symbol,
        args: &[&Expr],
        span: Span,
        tail: bool,
    ) -> Result<(), Diagnostic> {
        use crate::ast::Selection;
        let with_receiver = std::iter::once(receiver).chain(args.iter().copied());
        match callee.sel {
            // The function a field holds: an ordinary call of it.
            Some(Selection::FieldCall) => {
                self.compile_expr(receiver)?;
                let name = self.add_constant(Value::String(resolve(method)), span)?;
                self.emit(Asm::GetField { name }, span)?;
                self.compile_operands(args.iter().copied())?;
                self.emit_call(args.len(), tail, span)
            }
            // One impl's method: a call of its global, like a function's.
            Some(Selection::Impl { tr, ty })
                if let Some(slot) = self.globals.method(Some(tr), ty, &resolve(method)) =>
            {
                self.emit(Asm::GetGlobal { slot }, span)?;
                self.compile_operands(with_receiver)?;
                self.emit_call(args.len() + 1, tail, span)
            }
            // The method is found where the code runs, by the receiver's
            // type. (An impl with no global is a builtin type's that the
            // VM has natively.)
            Some(
                Selection::Impl { tr, .. } | Selection::Native { tr } | Selection::Dynamic { tr },
            ) => {
                self.compile_operands(with_receiver)?;
                self.emit_call_method(method, args.len() + 1, tr, tail, span)
            }
            Some(Selection::Field) | None => Err(checker_missed(
                span,
                &format!("the call of '{method}' with no selection"),
            )),
        }
    }

    fn compile_pipe(
        &mut self,
        left: &Expr,
        right: &Expr,
        span: Span,
        tail: bool,
    ) -> Result<(), Diagnostic> {
        // `val |> f(args)` is the call `f(val, args)`, and `val |> f` the
        // call `f(val)`.
        match &right.kind {
            ExprKind::Call(callee, args) => {
                // The call `callee(left, args..)`.
                let args: Vec<&Expr> = std::iter::once(left).chain(args).collect();
                self.compile_call(callee, &args, span, tail)?;
            }
            _ => self.compile_call(right, &[left], span, tail)?,
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
        self.begin_scope();

        // The bindings occupy the slots from the current frame height on;
        // `Recur` writes the new values there and cuts the frame back to
        // just above them. With no bindings that is the frame as it is
        // now, with every enclosing local still in place.
        let first_slot = self.emitter().height();

        // Compile initial values; each stays on the stack as its binding.
        for (name, _, init) in bindings {
            self.compile_expr(init)?;
            let slot = self.add_local(*name, span)?;
            self.emit(Asm::SetLocal { slot }, span)?;
        }

        // Record the loop start, where `loop(...)` jumps back to.
        let start = self.label();
        self.bind(start, span)?;

        // Push loop info so Recur knows what to do.
        self.ctx_mut().loop_stack.push(LoopInfo {
            first_slot,
            start,
            binding_count: bindings.len(),
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
            let fields = self.record_fields(id);
            Shape::Record(
                fields
                    .iter()
                    .map(|(name, ty)| {
                        let field_type = self
                            .describe_field_type(ty, &mut Vec::new())
                            .unwrap_or_else(|_| FieldType::Unsupported(shown(ty)));
                        collect_records(&field_type, &mut nested);
                        (resolve(*name), field_type)
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
            self.emit(Asm::Constant { k: idx }, span)?;
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
        self.emit(Asm::GetGlobal { slot }, span)?;
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

    /// The builtin trait of `T.method` for a builtin type `T`
    /// (`Int.display`): the one the checker resolved the access to.
    fn builtin_method_trait(
        &self,
        res: Option<crate::defs::Res>,
        method: Symbol,
        span: Span,
    ) -> Result<crate::defs::TraitId, Diagnostic> {
        self.res_trait(res)
            .or_else(|| crate::defs::builtin_trait_of_method(&resolve(method)))
            .ok_or_else(|| checker_missed(span, &format!("the method '{method}' of no trait")))
    }

    /// Emit `CallMethod` of `method` of the trait `t` with `argc`
    /// values (the receiver first) on the stack; in tail position
    /// (`tail`), `TailCallMethod; Return`.
    fn emit_call_method(
        &mut self,
        method: Symbol,
        argc: usize,
        t: crate::defs::TraitId,
        tail: bool,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let method = self.add_constant(Value::String(resolve(method)), span)?;
        let name = self.units.defs.get(t.0).name;
        let of = self.globals.trait_index(t, resolve(name)).ok_or_else(|| {
            let limit = Limit {
                what: "traits whose methods a program calls",
                count: usize::from(u16::MAX) + 1,
                max: usize::from(u16::MAX),
            };
            limit_diagnostic(limit, span)
        })?;
        if tail {
            self.emit(Asm::TailCallMethod { method, argc, of }, span)?;
            self.emit(Asm::Return, span)
        } else {
            self.emit(Asm::CallMethod { method, argc, of }, span)
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

    /// The fields of the record type `id`, with their types as the
    /// checker inferred them (an alias is the type it stands for).
    fn record_fields(&self, id: crate::defs::TypeId) -> Vec<(Symbol, Type)> {
        let fields = self
            .units
            .record_fields
            .get(&id)
            .cloned()
            .unwrap_or_default();
        fields
            .into_iter()
            .map(|(name, ty)| {
                (
                    name,
                    crate::types::canonical::canonicalize(self.resolver(), &ty),
                )
            })
            .collect()
    }

    /// The type `ty` of a record field as the decoders see it.
    ///
    /// `Err` carries the part of `ty` no decoder exists for. The record
    /// types the field type refers to are added to `records`.
    fn describe_field_type(
        &self,
        ty: &Type,
        records: &mut Vec<crate::defs::TypeId>,
    ) -> Result<FieldType, String> {
        use crate::typeinfo::ty as bt;
        let unsupported = || Err(shown(ty));
        match ty {
            Type::Int => Ok(FieldType::Int),
            Type::Float => Ok(FieldType::Float),
            Type::String => Ok(FieldType::String),
            Type::Bool => Ok(FieldType::Bool),
            // A range type is described like the list type it is the
            // same type as.
            Type::List(elem) | Type::Range(elem) => Ok(FieldType::List(Box::new(
                self.describe_field_type(elem, records)?,
            ))),
            // The keys of a JSON object or a TOML table are strings.
            Type::Map(key, value) if matches!(**key, Type::String) => Ok(FieldType::Map(Box::new(
                self.describe_field_type(value, records)?,
            ))),
            Type::Tuple(elems) if !elems.is_empty() => {
                let mut parts = Vec::with_capacity(elems.len());
                for elem in elems {
                    parts.push(self.describe_field_type(elem, records)?);
                }
                Ok(FieldType::Tuple(parts))
            }
            Type::Generic(t, args) if t.id == bt::OPTION && args.len() == 1 => Ok(
                FieldType::Option(Box::new(self.describe_field_type(&args[0], records)?)),
            ),
            Type::Generic(t, args) if args.is_empty() => self.describe_named_type(*t, ty, records),
            // Set, Channel, functions, generic records, ...
            _ => unsupported(),
        }
    }

    /// A named type with no arguments as the decoders see it: Date, Time,
    /// DateTime, or a non-generic record type of the program.
    fn describe_named_type(
        &self,
        t: TypeRef,
        ty: &Type,
        records: &mut Vec<crate::defs::TypeId>,
    ) -> Result<FieldType, String> {
        use crate::typeinfo::ty as bt;
        match t.id {
            id if id == bt::DATE => Ok(FieldType::Date),
            id if id == bt::TIME => Ok(FieldType::Time),
            id if id == bt::DATE_TIME => Ok(FieldType::DateTime),
            // A non-generic record type of the program.
            id if crate::defs::builtin_types().get(id.0.0 as usize).is_none()
                && self.units.defs.variants(id.0).is_empty()
                && self
                    .type_declaration(self.units.defs.get(id.0).span)
                    .is_some_and(|decl| decl.params.is_empty()) =>
            {
                records.push(id);
                Ok(FieldType::Record(id))
            }
            // Enums, the other builtin types.
            _ => Err(shown(ty)),
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
        for (field, ty) in self.record_fields(record) {
            let mut nested = Vec::new();
            match self.describe_field_type(&ty, &mut nested) {
                Err(part) => {
                    return Some(UndecodableField {
                        record: resolve(def.name),
                        field: resolve(field),
                        field_type: shown(&ty),
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
            if self
                .type_declaration(def.span)
                .is_some_and(|decl| !decl.params.is_empty())
            {
                return Err(Diagnostic::error(
                    Code::InvalidConstruct,
                    span,
                    format!(
                        "`{builtin_name}` cannot decode `{type_name}`: it is a generic record \
                         type, and a decoder builds a value of one type"
                    ),
                )
                .with_help(DECODABLE_TYPES_HELP));
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

    /// The emitter of the function being compiled.
    fn emitter(&mut self) -> &mut Emitter {
        &mut self.ctx_mut().emitter
    }

    /// Start compiling a function named `name` with `params` parameters:
    /// until [`Compiler::end_function`], code is emitted into it.
    fn begin_function(
        &mut self,
        name: String,
        params: usize,
        span: Span,
    ) -> Result<(), Diagnostic> {
        self.contexts.push(CompileContext {
            emitter: Emitter::new(name, params, span)?,
            locals: Vec::new(),
            scope_depth: 0,
            scope_starts: Vec::new(),
            upvalues: Vec::new(),
            loop_stack: Vec::new(),
            pattern_floor: None,
        });
        Ok(())
    }

    /// Finish the function being compiled: its verified code, and the
    /// values it captures from the function around it.
    fn end_function(&mut self, span: Span) -> Result<(Function, Vec<UpvalueDesc>), Diagnostic> {
        let ctx = self.contexts.pop().ok_or(Diagnostic::error(
            Code::CompilerBug,
            span,
            "compiler bug: missing function context",
        ))?;
        let function = ctx.emitter.finish(ctx.upvalues.len())?;
        Ok((function, ctx.upvalues))
    }

    /// Emit the instruction `asm` into the function being compiled.
    fn emit(&mut self, asm: Asm<'_>, span: Span) -> Result<(), Diagnostic> {
        self.emitter().emit(asm, span)
    }

    /// A new label of the function being compiled.
    fn label(&mut self) -> Label {
        self.emitter().label()
    }

    /// Place `label` at the next instruction.
    fn bind(&mut self, label: Label, span: Span) -> Result<(), Diagnostic> {
        self.emitter().bind(label, span)
    }

    /// Place `label` at the next instruction, saying what the
    /// conditional jumps to it go over (see [`Emitter::bind_over`]).
    fn bind_over(
        &mut self,
        label: Label,
        over: &'static str,
        span: Span,
    ) -> Result<(), Diagnostic> {
        self.emitter().bind_over(label, over, span)
    }

    /// Emit a jump to a new label, which the caller binds.
    fn jump(&mut self, span: Span) -> Result<Label, Diagnostic> {
        let to = self.label();
        self.emit(Asm::Jump { to }, span)?;
        Ok(to)
    }

    /// Emit a jump taken when the top value, which it pops, is false,
    /// to a new label, which the caller binds.
    fn jump_if_false(&mut self, span: Span) -> Result<Label, Diagnostic> {
        let to = self.label();
        self.emit(Asm::JumpIfFalse { to }, span)?;
        Ok(to)
    }

    /// [`Compiler::jump_if_false`] for a top value that is true.
    fn jump_if_true(&mut self, span: Span) -> Result<Label, Diagnostic> {
        let to = self.label();
        self.emit(Asm::JumpIfTrue { to }, span)?;
        Ok(to)
    }

    /// Add a constant to the function being compiled.
    fn add_constant(&mut self, value: Value, span: Span) -> Result<Const, Diagnostic> {
        self.emitter().constant(value, span)
    }

    /// The string constants of `names`, in order.
    fn name_constants(&mut self, names: &[Symbol], span: Span) -> Result<Vec<Const>, Diagnostic> {
        names
            .iter()
            .map(|name| self.add_constant(Value::String(resolve(*name)), span))
            .collect()
    }

    /// Open a scope: the locals added from here on are its locals.
    fn begin_scope(&mut self) {
        let ctx = self.ctx_mut();
        ctx.scope_depth += 1;
        ctx.scope_starts.push(ctx.emitter.height());
    }

    /// Open a scope whose first local is the value on top of the frame.
    fn begin_scope_with_top(&mut self) {
        let ctx = self.ctx_mut();
        ctx.scope_depth += 1;
        ctx.scope_starts
            .push(ctx.emitter.height().saturating_sub(1));
    }

    /// Leave the innermost scope: forget its locals. Returns the frame
    /// height at which the scope began. Emits nothing; the values of the
    /// scope's locals are still in the frame, and the caller decides
    /// where they are dropped (see `end_scope_with_result` and
    /// `emit_slide`).
    fn end_scope(&mut self) -> usize {
        let ctx = self.ctx_mut();
        let depth = ctx.scope_depth;
        // Pop locals belonging to the scope we are leaving.
        while ctx.locals.last().is_some_and(|l| l.depth >= depth) {
            ctx.locals.pop();
        }
        ctx.scope_depth -= 1;
        ctx.scope_starts
            .pop()
            .expect("internal compiler error: end_scope without begin_scope")
    }

    /// Leave the innermost scope when its result is on top of the stack,
    /// above the scope's locals, and drop the locals from under it. In
    /// tail position (`tail`) the result is returned at once and the
    /// frame goes with it, so nothing is emitted.
    fn end_scope_with_result(&mut self, tail: bool, span: Span) -> Result<(), Diagnostic> {
        let end = self.emitter().height();
        let start = self.end_scope();
        if end > start + 1 && !tail {
            self.emit_slide(start, span)?;
        }
        Ok(())
    }

    /// Emit `Slide`: the value on top of the stack becomes the value in
    /// slot `height`, and everything that was above that slot is dropped.
    /// Afterwards the frame holds `height` values plus that one.
    fn emit_slide(&mut self, height: usize, span: Span) -> Result<(), Diagnostic> {
        self.emit(Asm::Slide { slot: height }, span)
    }

    /// Make the value on top of the stack a local named `name`. Its slot
    /// is where that value is: the top of the frame.
    fn add_local(&mut self, name: Symbol, span: Span) -> Result<usize, Diagnostic> {
        let ctx = self.ctx_mut();
        let slot = ctx.emitter.height().checked_sub(1).ok_or_else(|| {
            Diagnostic::error(
                Code::CompilerBug,
                span,
                format!("compiler bug: the local '{name}' has no value in the frame"),
            )
        })?;
        let depth = ctx.scope_depth;
        ctx.locals.push(Local { name, depth, slot });
        Ok(slot)
    }

    /// Compile `operands` left to right so that their values are on the
    /// stack, in order, for the instruction the caller emits next.
    fn compile_operands<'a>(
        &mut self,
        operands: impl IntoIterator<Item = &'a Expr>,
    ) -> Result<(), Diagnostic> {
        for operand in operands {
            self.compile_expr(operand)?;
        }
        Ok(())
    }

    /// Register a function's parameters as locals and destructure those
    /// written as patterns. The arguments are already in the frame, in
    /// slots `0..params.len()`.
    fn compile_params(&mut self, params: &[Param], span: Span) -> Result<(), Diagnostic> {
        let mut destructured = Vec::new();
        for (slot, param) in params.iter().enumerate() {
            let name = match &param.pattern.kind {
                PatternKind::Ident(name) => *name,
                _ => {
                    destructured.push((slot, &param.pattern));
                    intern(&format!("__param_{slot}__"))
                }
            };
            let ctx = self.ctx_mut();
            let depth = ctx.scope_depth;
            ctx.locals.push(Local { name, depth, slot });
        }
        for (slot, pattern) in destructured {
            // Bind the pattern's names from a copy of the argument above
            // the parameters.
            self.emit(Asm::GetLocal { slot }, span)?;
            let copy = self.add_local(intern("__param_copy__"), span)?;
            self.emit(Asm::SetLocal { slot: copy }, span)?;
            self.compile_pattern_bind_checked(pattern, span)?;
        }
        Ok(())
    }

    fn resolve_local(&self, name: Symbol) -> Option<usize> {
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
    fn resolve_upvalue(&mut self, name: Symbol) -> Option<usize> {
        let current_idx = self.contexts.len() - 1;
        self.resolve_upvalue_in(name, current_idx)
    }

    fn resolve_upvalue_in(&mut self, name: Symbol, context_index: usize) -> Option<usize> {
        if context_index == 0 {
            return None; // The top-level script has no enclosing scope.
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

        // Upvalues are captured by value (Silt is immutable); the local
        // itself needs no open/closed tracking — see the `VmClosure` doc
        // in src/bytecode/mod.rs.
        let desc = match local_slot {
            Some(index) => UpvalueDesc {
                is_local: true,
                index,
            },
            // Not a local in the enclosing scope: an upvalue of it,
            // chained through.
            None => UpvalueDesc {
                is_local: false,
                index: self.resolve_upvalue_in(name, enclosing_idx)?,
            },
        };
        Some(self.add_upvalue(context_index, desc))
    }

    /// Add an upvalue descriptor to a context, deduplicating, and give
    /// its index. How many a closure can capture, and from how high in
    /// the frame, is the emitter's to say (`MakeClosure`, `GetUpvalue`,
    /// `Emitter::finish`).
    fn add_upvalue(&mut self, context_index: usize, desc: UpvalueDesc) -> usize {
        let ctx = &mut self.contexts[context_index];
        match ctx.upvalues.iter().position(|existing| *existing == desc) {
            Some(index) => index,
            None => {
                ctx.upvalues.push(desc);
                ctx.upvalues.len() - 1
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Chunk, Op};

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
            .map(|slot| {
                program
                    .globals
                    .name(u16::try_from(slot).unwrap())
                    .to_string()
            })
            .collect()
    }

    /// Check if the chunk's code has an instruction of the opcode.
    fn has_op(chunk: &Chunk, op: Op) -> bool {
        chunk.instrs().any(|(_, instr)| instr.op() == op)
    }

    /// Check if a string constant exists in the chunk.
    fn has_string_constant(chunk: &Chunk, s: &str) -> bool {
        chunk
            .constants()
            .iter()
            .any(|c| matches!(c, Value::String(v) if v == s))
    }

    /// Check if an int constant exists in the chunk.
    fn has_int_constant(chunk: &Chunk, n: i64) -> bool {
        chunk
            .constants()
            .iter()
            .any(|c| matches!(c, Value::Int(v) if *v == n))
    }

    /// Find a function by name in the compiled output.
    /// Functions are embedded as VmClosure constants in the script's chunk,
    /// so we search through all constants recursively.
    fn find_fn<'a>(fns: &'a [Function], name: &str) -> &'a Function {
        // First check top-level functions
        for f in fns {
            if f.name() == name {
                return f;
            }
        }
        // Search VmClosure constants in each function's chunk
        for f in fns {
            if let Some(found) = find_fn_in_constants(f.chunk(), name) {
                return found;
            }
        }
        panic!("function '{name}' not found")
    }

    fn find_fn_in_constants<'a>(chunk: &'a Chunk, name: &str) -> Option<&'a Function> {
        for constant in chunk.constants() {
            if let Value::VmClosure(closure) = constant {
                if closure.function.name() == name {
                    return Some(&closure.function);
                }
                // Recurse into nested closures
                if let Some(found) = find_fn_in_constants(closure.function.chunk(), name) {
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
        assert!(has_int_constant(main.chunk(), 42));
        assert!(has_op(main.chunk(), Op::Constant));
        assert!(has_op(main.chunk(), Op::Return));
    }

    #[test]
    fn test_compile_float_literal() {
        let fns = compile("fn main() { 4.25 }");
        let main = find_fn(&fns, "main");
        assert!(
            main.chunk()
                .constants()
                .iter()
                .any(|c| matches!(c, Value::Float(f) if (*f - 4.25).abs() < f64::EPSILON))
        );
    }

    #[test]
    fn test_compile_bool_literals() {
        let fns = compile("fn main() { true }");
        let main = find_fn(&fns, "main");
        assert!(has_op(main.chunk(), Op::True));

        let fns = compile("fn main() { false }");
        let main = find_fn(&fns, "main");
        assert!(has_op(main.chunk(), Op::False));
    }

    #[test]
    fn test_compile_string_literal() {
        let fns = compile(r#"fn main() { "hello" }"#);
        let main = find_fn(&fns, "main");
        assert!(has_string_constant(main.chunk(), "hello"));
    }

    #[test]
    fn test_compile_unit() {
        let fns = compile("fn main() { () }");
        let main = find_fn(&fns, "main");
        assert!(has_op(main.chunk(), Op::Unit));
    }

    // ── Arithmetic & binary operations ─────────────────────────────

    #[test]
    fn test_compile_arithmetic() {
        let fns = compile("fn add(a, b) { a + b }");
        let f = find_fn(&fns, "add");
        assert_eq!(f.arity(), 2);
        assert!(has_op(f.chunk(), Op::Add));

        let fns = compile("fn sub(a, b) { a - b }");
        assert!(has_op(find_fn(&fns, "sub").chunk(), Op::Sub));

        let fns = compile("fn mul(a, b) { a * b }");
        assert!(has_op(find_fn(&fns, "mul").chunk(), Op::Mul));

        let fns = compile("fn div(a, b) { a / b }");
        assert!(has_op(find_fn(&fns, "div").chunk(), Op::Div));

        let fns = compile("fn modulo(a, b) { a % b }");
        assert!(has_op(find_fn(&fns, "modulo").chunk(), Op::Mod));
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
                has_op(f.chunk(), expected_op),
                "missing {expected_op:?} for {expr}"
            );
        }
    }

    #[test]
    fn test_compile_short_circuit_and() {
        let fns = compile("fn f(a, b) { a && b }");
        let f = find_fn(&fns, "f");
        // Short-circuit and uses Dup + JumpIfFalse + Pop
        assert!(has_op(f.chunk(), Op::Dup));
        assert!(has_op(f.chunk(), Op::JumpIfFalse));
    }

    #[test]
    fn test_compile_short_circuit_or() {
        let fns = compile("fn f(a, b) { a || b }");
        let f = find_fn(&fns, "f");
        assert!(has_op(f.chunk(), Op::Dup));
        assert!(has_op(f.chunk(), Op::JumpIfTrue));
    }

    // ── Unary operations ───────────────────────────────────────────

    #[test]
    fn test_compile_negate() {
        let fns = compile("fn f(x) { -x }");
        assert!(has_op(find_fn(&fns, "f").chunk(), Op::Negate));
    }

    #[test]
    fn test_compile_not() {
        let fns = compile("fn f(x) { !x }");
        assert!(has_op(find_fn(&fns, "f").chunk(), Op::Not));
    }

    // ── Variable binding ───────────────────────────────────────────

    #[test]
    fn test_compile_local_variable() {
        let fns = compile("fn f() { let x = 42\n x }");
        let f = find_fn(&fns, "f");
        assert!(has_op(f.chunk(), Op::SetLocal));
        assert!(has_op(f.chunk(), Op::GetLocal));
    }

    #[test]
    fn test_compile_global_let() {
        let fns = compile("let x = 10\nfn main() { x }");
        let script = &fns[0]; // script is first
        assert_eq!(script.name(), "<script>");
        assert!(has_op(script.chunk(), Op::SetGlobal));
    }

    // ── Function compilation ───────────────────────────────────────

    #[test]
    fn test_compile_function_arity() {
        let fns = compile("fn f(a, b, c) { a }");
        let f = find_fn(&fns, "f");
        assert_eq!(f.arity(), 3);
    }

    #[test]
    fn test_compile_function_zero_arity() {
        let fns = compile("fn f() { 42 }");
        let f = find_fn(&fns, "f");
        assert_eq!(f.arity(), 0);
    }

    #[test]
    fn test_compile_multiple_functions() {
        let fns =
            compile("fn add(a, b) { a + b }\nfn sub(a, b) { a - b }\nfn main() { add(1, 2) }");
        // Script + 3 functions (as closures in the script's constant pool)
        assert_eq!(fns[0].name(), "<script>");
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
        assert!(has_op(main.chunk(), Op::Call));
    }

    #[test]
    fn test_compile_tail_call() {
        // The body of a function in tail position should emit TailCall
        let fns = compile("fn f(n) { f(n - 1) }");
        let f = find_fn(&fns, "f");
        assert!(has_op(f.chunk(), Op::TailCall));
    }

    // ── Lambda / closure compilation ───────────────────────────────

    #[test]
    fn test_compile_lambda() {
        let fns = compile("fn main() { let f = { x -> x + 1 }\n f(5) }");
        let main = find_fn(&fns, "main");
        // Lambda is compiled as a VmClosure constant
        assert!(
            main.chunk()
                .constants()
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
        assert!(has_op(f.chunk(), Op::MakeClosure));
    }

    // ── Collection compilation ─────────────────────────────────────

    #[test]
    fn test_compile_list() {
        let fns = compile("fn main() { [1, 2, 3] }");
        let main = find_fn(&fns, "main");
        assert!(has_op(main.chunk(), Op::MakeList));
    }

    #[test]
    fn test_compile_tuple() {
        let fns = compile("fn main() { (1, 2) }");
        let main = find_fn(&fns, "main");
        assert!(has_op(main.chunk(), Op::MakeTuple));
    }

    #[test]
    fn test_compile_map() {
        let fns = compile(r#"fn main() { #{ "a": 1, "b": 2 } }"#);
        let main = find_fn(&fns, "main");
        assert!(has_op(main.chunk(), Op::MakeMap));
    }

    #[test]
    fn test_compile_set() {
        let fns = compile(r#"fn main() { #[1, 2, 3] }"#);
        let main = find_fn(&fns, "main");
        assert!(has_op(main.chunk(), Op::MakeSet));
    }

    #[test]
    fn test_compile_range() {
        let fns = compile("fn main() { 1..10 }");
        let main = find_fn(&fns, "main");
        assert!(has_op(main.chunk(), Op::MakeRange));
    }

    #[test]
    fn test_compile_list_spread() {
        let fns = compile("fn main() { let a = [1, 2]\n [..a, 3] }");
        let main = find_fn(&fns, "main");
        assert!(has_op(main.chunk(), Op::ListConcat));
    }

    // ── String interpolation ───────────────────────────────────────

    #[test]
    fn test_compile_string_interp() {
        let fns = compile(r#"fn greet(name) { "hello {name}" }"#);
        let f = find_fn(&fns, "greet");
        assert!(has_op(f.chunk(), Op::StringConcat));
        assert!(has_op(f.chunk(), Op::DisplayValue));
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
        assert!(has_op(main.chunk(), Op::MakeRecord));
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
        assert!(has_op(main.chunk(), Op::RecordUpdate));
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
        assert!(has_op(main.chunk(), Op::GetField));
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
        assert!(main.chunk().constants().iter().any(
            |c| matches!(c, Value::Variant(tag, fields) if tag.name() == "Red" && fields.is_empty())
        ));
        let Some(Value::Variant(tag, _)) = main.chunk().constants().first() else {
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
        let Some(Value::VariantConstructor(tag)) = main.chunk().constants().first() else {
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
        assert!(has_op(f.chunk(), Op::TestEqual));
        assert!(has_op(f.chunk(), Op::JumpIfFalse));
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
        assert!(has_op(f.chunk(), Op::TestEqual));
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
        assert!(has_op(f.chunk(), Op::TestTag));
        assert!(has_op(f.chunk(), Op::DestructVariant));
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
        assert!(has_op(f.chunk(), Op::TestTupleLen));
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
        assert!(has_op(f.chunk(), Op::TestListMin));
        assert!(has_op(f.chunk(), Op::TestListExact));
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
        assert!(has_op(f.chunk(), Op::TestIntRange));
    }

    #[test]
    fn test_compile_match_record_pattern() {
        let fns = compile(
            r#"
type Point { x: Int, y: Int }
fn f(p) {
    match p {
        Point { x: 0, y } -> y
        Point { x, y } -> x + y
    }
}
"#,
        );
        let f = find_fn(&fns, "f");
        assert!(has_op(f.chunk(), Op::TestRecordTag));
        assert!(has_op(f.chunk(), Op::DestructRecordField));
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
        assert!(has_op(f.chunk(), Op::JumpIfFalse));
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
        assert!(has_op(f.chunk(), Op::Gt));
        assert!(has_op(f.chunk(), Op::JumpIfFalse));
    }

    // ── Pipe compilation ───────────────────────────────────────────

    #[test]
    fn test_compile_pipe_to_function() {
        let fns = compile("fn double(x) { x * 2 }\nfn main() { 5 |> double }");
        let main = find_fn(&fns, "main");
        // Pipe in tail position emits TailCall
        assert!(has_op(main.chunk(), Op::TailCall));
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
        assert!(has_op(main.chunk(), Op::CallBuiltin));
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
        assert!(has_op(main.chunk(), Op::Recur));
        // The jump back to the start of the loop.
        assert!(has_op(main.chunk(), Op::Jump));
    }

    // ── Question mark ──────────────────────────────────────────────

    #[test]
    fn test_compile_question_mark() {
        let fns = compile("fn f(x: Option(Int)) { Some(x?) }");
        let f = find_fn(&fns, "f");
        assert!(has_op(f.chunk(), Op::QuestionMark));
    }

    // ── Return statement ───────────────────────────────────────────

    #[test]
    fn test_compile_explicit_return() {
        let fns = compile("fn f(x) { return 42 }");
        let f = find_fn(&fns, "f");
        assert!(has_op(f.chunk(), Op::Return));
        assert!(has_int_constant(f.chunk(), 42));
    }

    #[test]
    fn test_compile_return_unit() {
        let fns = compile("fn f() { return }");
        let f = find_fn(&fns, "f");
        assert!(has_op(f.chunk(), Op::Unit));
        assert!(has_op(f.chunk(), Op::Return));
    }

    // ── Blocks ─────────────────────────────────────────────────────

    #[test]
    fn test_compile_empty_block() {
        let fns = compile("fn f() { { } }");
        let f = find_fn(&fns, "f");
        // Empty block evaluates to Unit
        assert!(has_op(f.chunk(), Op::Unit));
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
        assert!(has_op(f.chunk(), Op::SetLocal));
        assert!(has_op(f.chunk(), Op::GetLocal));
        assert!(has_op(f.chunk(), Op::Add));
    }

    // ── Type ascription ────────────────────────────────────────────

    #[test]
    fn test_compile_ascription_is_transparent() {
        // Ascription compiles to just the inner expression
        let fns = compile("fn f() { 42 as Int }");
        let f = find_fn(&fns, "f");
        assert!(has_int_constant(f.chunk(), 42));
        assert!(has_op(f.chunk(), Op::Constant));
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
        assert!(has_op(main.chunk(), Op::CallBuiltin));
        assert!(has_string_constant(main.chunk(), "list.length"));
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
fn shown(x: a) -> String where a: Display { x.display() }
fn field(r: {f: Fn(Int) -> Int}) -> Int { r.f(1) }
fn main() {
    let f = Foo { x: 1 }
    f.display()
}
"#,
        );
        // The method of an impl the checker selected: a call of its
        // global, a tail call here.
        let main = find_fn(&fns, "main");
        assert!(!has_op(main.chunk(), Op::CallMethod));
        assert!(has_op(main.chunk(), Op::GetGlobal));
        assert!(has_op(main.chunk(), Op::TailCall));
        // The method of a bounded variable is found where the code runs:
        // in tail position, to run in the caller's frame.
        let shown = find_fn(&fns, "shown");
        assert!(has_op(shown.chunk(), Op::TailCallMethod));
        // A field that holds a function is read and called.
        let field = find_fn(&fns, "field");
        assert!(has_op(field.chunk(), Op::GetField));
        assert!(!has_op(field.chunk(), Op::CallMethod));
    }

    // ── compile_program vs compile_declarations ────────────────────

    #[test]
    fn test_compile_program_calls_main() {
        let fns = crate::session::testing::compile_str("fn main() { 42 }")
            .unwrap()
            .functions;
        let script = &fns[0];
        // compile_program emits GetGlobal main, Call 0, Return
        assert!(has_op(script.chunk(), Op::GetGlobal));
        assert!(has_op(script.chunk(), Op::Call));
    }

    #[test]
    fn test_compile_declarations_returns_unit() {
        let fns = compile("fn main() { 42 }");
        let script = &fns[0];
        // compile_declarations emits Unit, Return (no main call)
        assert!(has_op(script.chunk(), Op::Unit));
        assert!(has_op(script.chunk(), Op::Return));
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
        assert!(!find_fn(&fns, "main").chunk().code().is_empty());
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
        assert!(has_string_constant(main.chunk(), "list.length"));
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
        let lambda = main.chunk().constants().iter().find_map(|c| {
            if let Value::VmClosure(cl) = c
                && cl.function.name() == "<lambda>"
            {
                return Some(&cl.function);
            }
            None
        });
        assert!(lambda.is_some(), "expected lambda in main's constants");
        let lambda = lambda.unwrap();
        assert!(has_op(lambda.chunk(), Op::DestructTuple));
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
        assert!(has_op(f.chunk(), Op::TestMapHasKey));
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
        assert!(has_op(f.chunk(), Op::TestTag));
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
        assert!(has_op(f.chunk(), Op::JumpIfFalse));
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
            .chunk()
            .code()
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
        assert!(has_op(f.chunk(), Op::Dup));
        assert!(has_op(f.chunk(), Op::Eq));
    }

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
        assert_eq!(
            err.message,
            "too many top-level definitions of a program (functions, `let`s and trait methods): \
             65537 (the limit is 65536)"
        );
        // `main` took the last slot; `helper` does not fit.
        assert_eq!(
            &source[err.span.start as usize..err.span.end as usize],
            "helper"
        );
    }
}
