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
use crate::bytecode::{
    Asm, Const, Emitter, Function, Globals, Label, Segment, UpvalueDesc, VmClosure,
};
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, intern, resolve};
use crate::module;
use crate::source::Span;
use crate::typechecker::decodable::shown;
use crate::typeinfo::{FieldType, Shape, Tag, TypeInfo, TypeTable, VariantInfo};
use crate::types::canonical::{Resolver, canonical_head};
use crate::types::{Type, TypeRef};
use crate::value::{HostFn, Value};

mod dicts;
mod patterns;

// ── Types at run time ───────────────────────────────────────────────
//
// Each record and enum type a program builds values of is described to
// the VM by a `TypeInfo` (see `crate::typeinfo`): its id, its name, its
// variants or its fields. A record's fields, and the fields of an enum's
// variants, carry their types as far as `json.parse(text, T)`,
// `toml.parse(text, T)` and `postgres.query(db, sql, params, T)` need
// them to build a value of `T`, as the checker's tables describe them
// (`Tables::decoded_field_type`); a generic type's are stated over its
// type parameters, which a field of the type at its arguments
// (`b: Box(Int)`) gives. Type aliases are
// replaced by their target. A type no decoder exists for is
// `FieldType::Unsupported`, which makes the decoders return `Err`; it is
// never mapped to some other type. The checker refuses a decoder such
// a type (`Pred::Builtin`), so a checked program never gives a decoder
// one.

// ── Reading a field ──────────────────────────────────────────────────

/// How code reads a field of a record.
#[derive(Clone, Copy)]
enum FieldRead {
    /// At the place the record's type declares the field: the type is a
    /// declared record the compiler knows (the receiver's checked type,
    /// the type a pattern names).
    Slot(usize),
    /// By its name, in the type of the record that is there when the
    /// code runs: an anonymous record, or a record behind an open row
    /// (`{x: T, ...r}`), which a declared record flows into as itself.
    Named(Symbol),
}

// ── Bind destruct kind ───────────────────────────────────────────────

/// Describes how to destructure a sub-value from a compound pattern.
enum BindDestructKind {
    Variant(usize),
    Tuple(usize),
    List(usize),
    ListRest(usize),
    RecordField(FieldRead),
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
    /// The name of the closures written in this code, which is how an
    /// error's call stack lists them: `closure in main` in the function
    /// `main` and in every closure in it, however deep; `closure in
    /// P.show` in a method; `closure in table` in the top-level
    /// `let table = ...`, and `closure at the top level` in one that
    /// binds several names or none. No function has `<...>` for a
    /// closure's name.
    closures: String,
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
    /// For a method of an impl of a trait a program declares: the
    /// method, whose dictionary a call leaves below its arguments
    /// (`Instr::FrameDict`).
    frame_dict: Option<Span>,
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

// ── Type arguments written in full ──────────────────────────────────

/// The type `expr` is the value of, if the checker found it to be one.
fn written_type_of(expr: &Expr) -> Option<&Type> {
    match &expr.ty {
        Some(Type::Generic(head, args)) if head.is_builtin(crate::defs::TYPE_OF) => args.first(),
        _ => None,
    }
}

/// The types alone in the type argument `expr` written in full (`a` in
/// `List(a)`), added to `out`.
fn written_leaves<'e>(expr: &'e Expr, out: &mut Vec<&'e Expr>) {
    match &expr.kind {
        ExprKind::Call(callee, args) => {
            written_leaves(callee, out);
            args.iter().for_each(|arg| written_leaves(arg, out));
        }
        ExprKind::Tuple(elems) => elems.iter().for_each(|elem| written_leaves(elem, out)),
        _ => out.push(expr),
    }
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

/// Whether `expr` reads the local `name`: the body of a closure that
/// calls itself by the name its `let` gives it does. (A read of another
/// local of the name, one bound in `expr`, counts too: it costs the
/// closure a slot it does not use, not a wrong value.)
fn reads_local(expr: &Expr, name: Symbol) -> bool {
    any_expr(expr, &mut |e| {
        matches!(e.kind, ExprKind::Ident(n) if n == name) && e.res == Some(crate::defs::Res::Local)
    })
}

/// Whether `found` holds of `expr` or of an expression in it.
pub(super) fn any_expr(expr: &Expr, found: &mut impl FnMut(&Expr) -> bool) -> bool {
    if found(expr) {
        return true;
    }
    let mut inner = |e: &Expr| any_expr(e, found);
    match &expr.kind {
        ExprKind::Ident(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::StringLit(_)
        | ExprKind::Unit => false,
        ExprKind::StringInterp(parts) => parts
            .iter()
            .any(|part| matches!(part, StringPart::Expr(e) if inner(e))),
        ExprKind::List(elems) => elems.iter().any(|elem| match elem {
            ListElem::Single(e) | ListElem::Spread(e) => inner(e),
        }),
        ExprKind::Map(entries) => entries.iter().any(|(k, v)| inner(k) || inner(v)),
        ExprKind::SetLit(elems) | ExprKind::Tuple(elems) | ExprKind::Recur(elems) => {
            elems.iter().any(&mut inner)
        }
        ExprKind::FieldAccess(e, ..)
        | ExprKind::Unary(_, e)
        | ExprKind::QuestionMark(e)
        | ExprKind::Ascription(e, _) => inner(e),
        ExprKind::Binary(a, _, b) | ExprKind::Pipe(a, b) | ExprKind::Range(a, b) => {
            inner(a) || inner(b)
        }
        ExprKind::Call(callee, args) => inner(callee) || args.iter().any(&mut inner),
        ExprKind::Lambda { body, .. } => inner(body),
        ExprKind::RecordCreate { fields, .. } => fields.iter().any(|(_, e)| inner(e)),
        ExprKind::RecordUpdate { expr, fields } => {
            inner(expr) || fields.iter().any(|(_, e)| inner(e))
        }
        ExprKind::AnonRecord { spread, fields } => {
            spread.as_deref().is_some_and(&mut inner) || fields.iter().any(|(_, e)| inner(e))
        }
        ExprKind::Match { expr, arms } => {
            expr.as_deref().is_some_and(&mut inner)
                || arms
                    .iter()
                    .any(|arm| arm.guard.as_deref().is_some_and(&mut inner) || inner(&arm.body))
        }
        ExprKind::Return(e) => e.as_deref().is_some_and(&mut inner),
        ExprKind::Block(stmts) => stmts.iter().any(|stmt| match stmt {
            Stmt::Let { value, .. } => inner(value),
            Stmt::When {
                expr, else_body, ..
            } => inner(expr) || inner(else_body),
            Stmt::WhenBool {
                condition,
                else_body,
            } => inner(condition) || inner(else_body),
            Stmt::Expr(e) => inner(e),
        }),
        ExprKind::Loop { bindings, body } => {
            bindings.iter().any(|(_, _, e)| inner(e)) || inner(body)
        }
    }
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
pub struct ProgramUnits<'a> {
    pub modules: Vec<ModuleUnit>,
    pub entry: usize,
    /// The definitions the resolver's slots name.
    pub defs: Arc<crate::defs::DefTable>,
    /// For a REPL entry: what the earlier entries left in the VM. Empty
    /// for any other program.
    pub earlier: EarlierCells,
    /// The tables the session's modules were checked into. Read from
    /// them: the type aliases and associated-type bindings (impl
    /// targets are canonicalized with them, as the checker keyed the
    /// impls), and the fields of each record type, with their types as
    /// the checker inferred them, in declaration order.
    pub tables: &'a crate::typechecker::Tables,
}

/// What the earlier entries of a REPL session installed, which the entry
/// being compiled uses but does not install again.
#[derive(Default)]
pub struct EarlierCells {
    /// The modules installed already: an import of one compiles nothing.
    pub installed: HashSet<usize>,
    /// The global slots the earlier entries took. The entry's own
    /// definitions get new ones.
    pub globals: Globals,
}

pub struct Compiler<'a> {
    contexts: Vec<CompileContext>,
    /// Accumulated compiled functions (one per `Decl::Fn`).
    functions: Vec<Function>,
    /// The modules of the program, from the session.
    units: ProgramUnits<'a>,
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
    /// The name a local `let` gives the closure compiled next, its
    /// value.
    closure_name: Option<Symbol>,
    /// The default methods of the builtin traits that this program
    /// installs (the first REPL entry, or a program; a later entry finds
    /// them installed), each with its trait: `Error.message`.
    builtin_defaults: Vec<(crate::defs::TraitId, crate::ast::FnDecl)>,
    /// The body of the trait's default method being compiled, if one is:
    /// `Self` there is its impl's type (`DictType`).
    default_body: Option<Span>,
    /// The definitions that take dictionaries, by the span that names
    /// them (`crate::evidence::Evidence::Param`), with what they take.
    owners: HashMap<Span, crate::evidence::Shape>,
    /// While the callee of a call is compiled: the call's span. A
    /// definition with bounds called is passed its dictionaries as its
    /// first arguments (no `Bind`); a `let` general in a bound makes its
    /// value where the call is.
    callee_call: Option<Span>,
    /// The dictionary of the impl of each method of an impl whose header
    /// asks nothing, by the method's body: its template's place
    /// (`Compiler::own_dicts_of`).
    own_dicts: HashMap<Span, u32>,
    /// Each name a `let` general in a bound binds with a pattern of
    /// several names: the places of its bounds among the value's
    /// (`crate::ast::Program::pattern_lets`).
    pattern_lets: HashMap<Span, Vec<u16>>,
}

/// The names two or more types of the program's modules have (two
/// modules' `Pt`): such a type prints qualified by its module's name.
fn program_type_clashes(units: &ProgramUnits<'_>) -> HashSet<Symbol> {
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

/// What a method reached through its type is (`Compiler::type_member`).
enum TypeMember {
    /// The method of an impl: its global.
    Slot(u16),
    /// The method of an impl of a trait a program declares, which a call
    /// reaches through the impl's dictionary.
    Dict,
    /// The VM's own method of the trait.
    Native(crate::defs::TraitId),
}

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

/// A program names more methods than the operand of a method call can
/// tell apart: the call, or the impl that leaves a default method to
/// its trait, at `span` names one more.
fn too_many_method_names(span: Span) -> Diagnostic {
    let limit = Limit {
        what: "names of methods a program calls",
        count: usize::from(u16::MAX) + 2,
        max: usize::from(u16::MAX) + 1,
    };
    limit_diagnostic(limit, span)
}

impl<'a> Compiler<'a> {
    /// A compiler for the modules of a program, as the session analysed
    /// them. Every definition of the program that has a global slot gets
    /// it now, so code can use a definition compiled after it.
    pub fn for_program(units: ProgramUnits<'a>) -> Result<Self, Diagnostic> {
        let program_clashes = program_type_clashes(&units);
        let mut globals = units.earlier.globals.clone();
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
            closure_name: None,
            builtin_defaults: Vec::new(),
            default_body: None,
            owners: HashMap::new(),
            own_dicts: HashMap::new(),
            pattern_lets: HashMap::new(),
            callee_call: None,
        };
        compiler.owners = (compiler.units.modules.iter())
            .flat_map(|unit| unit.program.dict_owners.iter().copied())
            .collect();
        compiler.pattern_lets = (compiler.units.modules.iter())
            .flat_map(|unit| unit.program.pattern_lets.iter().cloned())
            .collect();
        compiler.builtin_defaults = compiler.assign_builtin_defaults(&mut globals)?;
        compiler.own_slots = compiler.assign_slots(&mut globals)?;
        compiler.enter_templates(&mut globals)?;
        compiler.own_dicts = compiler.own_dicts_of(&globals);
        compiler.globals = globals;
        Ok(compiler)
    }

    /// Give a global slot to each default method of a builtin trait
    /// (`Error.message`) that an impl of the program leaves out and no
    /// earlier REPL entry installed: the methods this program compiles,
    /// each with its trait.
    fn assign_builtin_defaults(
        &self,
        globals: &mut Globals,
    ) -> Result<Vec<(crate::defs::TraitId, crate::ast::FnDecl)>, Diagnostic> {
        let impls: Vec<&crate::ast::TraitImpl> = (self.units.modules.iter().enumerate())
            .filter(|(index, _)| !self.units.earlier.installed.contains(index))
            .flat_map(|(_, unit)| &unit.program.decls)
            .filter_map(|decl| match decl {
                Decl::TraitImpl(ti) => Some(ti),
                _ => None,
            })
            .collect();
        let mut installs = Vec::new();
        for decl in crate::typechecker::builtin_trait_decls() {
            let Some(t) = crate::defs::builtin_trait_id(&resolve(decl.name)) else {
                continue;
            };
            for method in decl.methods.into_iter().filter(|m| !m.is_signature_only) {
                let name = resolve(method.name);
                let Some(first) = (impls.iter()).find(|ti| {
                    self.impl_trait(ti) == Some(t)
                        && !ti.methods.iter().any(|written| written.name == method.name)
                }) else {
                    continue;
                };
                if globals.default_method(t, &name).is_some() {
                    continue;
                }
                globals
                    .add_default_method(t, &name, format!("{}.{name}", decl.name))
                    .ok_or_else(|| too_many_globals(globals.len() + 1, method.span))?;
                // (Its code is silt's, which has no place in the program:
                // the code that installs it has the place of the first
                // impl that leaves it out.)
                let mut method = method;
                method.span = first.span;
                installs.push((t, method));
            }
        }
        Ok(installs)
    }

    /// Compile the default methods of the builtin traits this program
    /// installs into their slots.
    fn compile_builtin_defaults(&mut self) -> Result<(), Diagnostic> {
        for (t, method) in std::mem::take(&mut self.builtin_defaults) {
            let name = crate::defs::builtin_traits()
                .get((t.0.0 as usize).wrapping_sub(crate::defs::builtin_types().len()))
                .map_or("?", |(name, _)| name);
            self.compile_default_method(t, name, &method)?;
        }
        Ok(())
    }

    /// Give a global slot to each definition the program installs:
    /// module by module, each function, `let`, host function and
    /// default method of a trait, and last each impl method. An impl
    /// that leaves a default method out gets no slot for it: its method
    /// is the trait's. Gives the slots of the modules' own functions,
    /// `let`s and host functions by module and name.
    fn assign_slots(
        &self,
        globals: &mut Globals,
    ) -> Result<HashMap<(crate::session::ModuleId, Symbol), u16>, Diagnostic> {
        let mut own = HashMap::new();
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
                    self.assign_method_slots(ti, globals)?;
                }
            }
        }
        Ok(own)
    }

    /// Give a global slot to each method the impl `ti` writes; a default
    /// method of the trait it leaves out is the trait's, whose slot the
    /// trait's declaration got (in this program, or in an earlier REPL
    /// entry).
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
                .add_method_at(
                    t,
                    ty,
                    ti.args_key,
                    &resolve(method.name),
                    format!("{type_name}.{}", method.name),
                )
                .ok_or_else(|| too_many_globals(globals.len() + 1, method.span))?;
        }
        for (method, slot) in globals.default_methods(t) {
            if !ti
                .methods
                .iter()
                .any(|written| resolve(written.name) == method)
            {
                globals
                    .default_for_at(t, ty, ti.args_key, &method, slot)
                    .ok_or_else(|| too_many_method_names(ti.trait_name_span))?;
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
        self.units.tables.resolver()
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
        self.compile_builtin_defaults()?;
        self.fill_late_dicts(program)?;

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
        // (A REPL entry whose value is general in a bound is called with
        // no dictionaries, unit in their places: it only shows its value,
        // which no type is decided for.)
        let undecided = entry
            .and_then(|def| {
                let name = self.units.defs.get(def).name;
                program.decls.iter().find_map(|decl| match decl {
                    Decl::Fn(f) if f.name == name => self.shape(f.body.span),
                    _ => None,
                })
            })
            .map_or(0, Self::hidden_count);
        match entry.and_then(|def| self.globals.def(def)) {
            Some(slot) => {
                self.emit(Asm::GetGlobal { slot }, span)?;
                for _ in 0..undecided {
                    self.emit(Asm::Unit, span)?;
                }
                self.emit(Asm::Call { argc: undecided }, span)?;
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
        self.compile_builtin_defaults()?;
        self.fill_late_dicts(program)?;

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

    /// The order in which a module's declarations are installed. First
    /// its imports, in the one order of imports
    /// (`crate::ast::import_order`, the order `silt fmt` writes them
    /// in): an import compiles the module it names where it stands, so
    /// that is the order the modules are initialised in, whatever
    /// order the import lines are written in. Then everything else
    /// that only defines something (types, traits, trait impls,
    /// functions), in source order; then the top-level `let`s, in the
    /// order the checker decided (`let_order`, by the span of each:
    /// every `let` after the ones its initialiser can reach). A
    /// top-level initialiser can therefore use every declaration of
    /// the program, wherever it is written.
    fn decls_in_init_order<'d>(decls: &'d [Decl], let_order: &[Span]) -> Vec<&'d Decl> {
        let imports = crate::ast::imports_in_order(decls).into_iter();
        let (mut lets, definitions): (Vec<&Decl>, Vec<&Decl>) = decls
            .iter()
            .filter(|decl| !matches!(**decl, Decl::Import(..)))
            .partition(|decl| matches!(**decl, Decl::Let { .. }));
        // (A `let` the order does not name, which no checked module
        // has, keeps its place after the ones it names.)
        let mut place: HashMap<Span, usize> = HashMap::with_capacity(let_order.len());
        for (i, at) in let_order.iter().enumerate() {
            place.entry(*at).or_insert(i);
        }
        lets.sort_by_key(|decl| match decl {
            Decl::Let { span, .. } => place.get(span).copied().unwrap_or(usize::MAX),
            _ => usize::MAX,
        });
        imports.chain(definitions).chain(lets).collect()
    }

    /// Compile the method `method` (an impl's, or a trait's default) as
    /// the function `name`, and leave it on the stack.
    fn compile_method(
        &mut self,
        name: String,
        method: &crate::ast::FnDecl,
    ) -> Result<(), Diagnostic> {
        let span = method.span;
        let hidden = self.shape(method.body.span).map_or(0, Self::hidden_count);
        self.begin_function_after(name, method.params.len(), hidden, span)?;
        self.hidden_params(method.body.span);

        self.compile_params(&method.params, span)?;
        self.keep_frame_dict(method)?;

        // The body is in tail position, like a function's.
        self.in_tail_position = true;
        self.compile_expr(&method.body)?;
        self.in_tail_position = false;
        self.emit_body_return(span)?;

        let (func, _) = self.end_function(span)?;
        let vm_closure = Arc::new(VmClosure::new(Arc::new(func), vec![]));
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
        let around = self.default_body.replace(method.body.span);
        let compiled = self.compile_method(format!("{trait_name}.{}", method.name), method);
        self.default_body = around;
        compiled?;
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
                // Push a new context for the function body: its hidden
                // parameters first, if it has bounds.
                let tag = fn_decl.body.span;
                let hidden = self.shape(tag).map_or(0, Self::hidden_count);
                self.begin_function_after(
                    resolve(fn_decl.name),
                    fn_decl.params.len(),
                    hidden,
                    span,
                )?;
                self.hidden_params(tag);

                self.compile_params(&fn_decl.params, span)?;

                // Compile the function body in tail position for TCO.
                self.in_tail_position = true;
                self.compile_expr(&fn_decl.body)?;
                self.in_tail_position = false;

                self.emit_body_return(span)?;

                // Pop the context, recovering the compiled function.
                let (func, _) = self.end_function(span)?;

                // Store the function as a VmClosure constant in the enclosing chunk.
                let vm_closure = Arc::new(VmClosure::new(Arc::new(func), vec![]));
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
                // The closures of the initialiser are named after the
                // binding if the `let` binds one name. If it binds
                // several, or none, no name is theirs alone.
                let closures = match crate::parser::top_level_binders(decl).as_slice() {
                    [(name, _, _)] => format!("closure in {name}"),
                    _ => "closure at the top level".to_string(),
                };
                let around = self
                    .contexts
                    .last_mut()
                    .map(|context| std::mem::replace(&mut context.closures, closures));
                // (A `let` general in a bound is the function of its
                // dictionaries that makes its value; of each name of a
                // pattern, the function that makes the name's part.)
                let compiled = match (&pattern.kind, self.shape(value.span)) {
                    (PatternKind::Ident(name), Some(_)) => self.compile_thunk(*name, value, None),
                    (_, Some(_)) => self.install_pattern_thunks(pattern, value, span),
                    _ => self.compile_expr(value),
                };
                if let (Some(context), Some(around)) = (self.contexts.last_mut(), around) {
                    context.closures = around;
                }
                compiled?;
                if self.shape(value.span).is_some()
                    && !matches!(pattern.kind, PatternKind::Ident(_))
                {
                    return Ok(());
                }
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
                // the typechecker keys impls (an alias is the type it
                // stands for), so the VM finds it by the type of the
                // receiver.
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
                        .method_at(t, ty, trait_impl.args_key, &resolve(method.name))
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
    /// The top-level `let` of `pattern`, of several names, to `value`,
    /// which is general in a bound: each name's global is the function
    /// that makes its part (`compile_pattern_thunk`), or the part.
    fn install_pattern_thunks(
        &mut self,
        pattern: &crate::ast::Pattern,
        value: &Expr,
        span: Span,
    ) -> Result<(), Diagnostic> {
        for (name, at) in crate::typechecker::names::binders(pattern) {
            if self.compile_pattern_thunk(pattern, value, name, at)? == 0 {
                self.emit(Asm::Call { argc: 0 }, span)?;
            }
            let slot = self.own_slot(name, span)?;
            self.emit(Asm::SetGlobal { slot }, span)?;
            self.emit(Asm::Pop, span)?;
        }
        Ok(())
    }

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
        let module = target.module();
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
        let init_closure = Arc::new(VmClosure::new(Arc::new(init), vec![]));
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
                let own_name = match (&pattern.kind, &value.kind) {
                    (PatternKind::Ident(name), ExprKind::Lambda { .. }) => Some(*name),
                    _ => None,
                };
                // (A `let` general in a bound is the function of its
                // dictionaries that makes its value.)
                match (&pattern.kind, self.shape(value.span)) {
                    (PatternKind::Ident(name), Some(_)) => {
                        self.compile_thunk(*name, value, own_name)?;
                    }
                    // Each name of a pattern: the function that makes its
                    // part, a local.
                    (_, Some(_)) => {
                        for (name, at) in crate::typechecker::names::binders(pattern) {
                            if self.compile_pattern_thunk(pattern, value, name, at)? == 0 {
                                self.emit(Asm::Call { argc: 0 }, value.span)?;
                            }
                            self.add_local(name, value.span)?;
                        }
                        if is_last {
                            self.emit(Asm::Unit, value.span)?;
                        }
                        return Ok(());
                    }
                    _ => {
                        self.closure_name = own_name;
                        self.compile_expr(value)?;
                    }
                }
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

    /// Emit the `Return` of a function's body, whose value is on the
    /// stack; nothing where the body's code ends in a `Return` that
    /// every way through it takes (it ends in a tail call, a `return`).
    fn emit_body_return(&mut self, span: Span) -> Result<(), Diagnostic> {
        if !self.emitter().ended() {
            self.emit(Asm::Return, span)?;
        }
        Ok(())
    }

    /// Compile `expr`: at height `h`, code that leaves the expression's
    /// value in slot `h` and the frame one value higher. In tail
    /// position the locals of the expression's scopes may still be under
    /// the value, since the frame goes when the value is returned.
    fn compile_expr(&mut self, expr: &Expr) -> Result<(), Diagnostic> {
        let tail = self.in_tail_position;
        let before = self.emitter().height();
        if let Some(ty) = self.written_type(expr) {
            let mut leaves = Vec::new();
            written_leaves(expr, &mut leaves);
            self.compile_type_value(ty, &leaves, expr.span)?;
        } else if let Some((row, bounds)) = self.builtin_with_dicts(expr) {
            // (A builtin is no closure to bind its dictionaries to.)
            self.compile_builtin_value(row, bounds, expr.span)?;
        } else {
            self.compile_expr_kind(expr)?;
        }
        // (A use of a definition with bounds loads it last.)
        if let Some(site) = &expr.ev
            && self.builtin_with_dicts(expr).is_none()
            && matches!(expr.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..))
        {
            // It is passed its dictionaries: a `let` that is general in
            // a bound makes its value with them; a function, as a value,
            // has them bound (called, the call passes them).
            if let crate::evidence::Site::Ref(bounds) = &**site
                && !bounds.is_empty()
            {
                if self.makes_its_value(expr) {
                    let n = self.pass_dicts(bounds, expr.span)?;
                    let at = self.callee_call.unwrap_or(expr.span);
                    self.emit(Asm::Call { argc: n }, at)?;
                } else if self.callee_call.is_none() {
                    let n = self.pass_dicts(bounds, expr.span)?;
                    self.emit(Asm::Bind { n }, expr.span)?;
                }
            }
        }
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
                let idx = self.float_constant(*n, span)?;
                self.emit(Asm::Constant { k: idx }, span)?;
            }

            ExprKind::Bool(b) => {
                if *b {
                    self.emit(Asm::True, span)?;
                } else {
                    self.emit(Asm::False, span)?;
                }
            }

            ExprKind::StringLit(s) => {
                let idx = self.add_constant(Value::String(s.clone().into()), span)?;
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

            // `Self` given to a call in a trait's default method: the type
            // of the impl whose dictionary the method runs with.
            ExprKind::Ident(name)
                if *name == intern("Self")
                    && expr.res == Some(crate::defs::Res::Local)
                    && let Some(owner) = self.default_body =>
            {
                self.load_frame_dict(owner, span)?;
                self.emit(Asm::DictType, span)?;
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

            // `Tr.m` as a value: the method of the one impl the checker
            // selected, or the function `{ a, b -> Tr.m(a, b) }`, which
            // finds it where it runs. The program wrote no closure there:
            // the function has the method's name, `Tr.m`.
            // `Tr.m` of a trait a program declares, as a value: the
            // method of the dictionary the evidence names, with it bound.
            ExprKind::FieldAccess(of, method, _)
                if self.res_trait(of.res).is_some() && self.dict_call(expr).is_some() =>
            {
                let (tr, recv, own) = self.dict_call(expr).expect("a call through a dictionary");
                let (recv, own) = (recv.clone(), own.to_vec());
                self.compile_dict_method_value(tr, &recv, &own, *method, span)?;
            }

            ExprKind::FieldAccess(of, method, _) if self.res_trait(of.res).is_some() => {
                let tr = self.res_trait(of.res).expect("a trait");
                if let Some(crate::ast::Selection::Impl { tr, ty }) = expr.sel
                    && let Some(slot) = self.globals.method(tr, ty, &resolve(*method))
                {
                    self.emit(Asm::GetGlobal { slot }, span)?;
                } else {
                    let arity = match &expr.ty {
                        Some(Type::Fun(params, _)) => params.len(),
                        _ => return Err(checker_missed(span, "a trait method of no type")),
                    };
                    let names: Vec<Symbol> =
                        (0..arity).map(|k| intern(&format!("__arg{k}__"))).collect();
                    let ident = |name: Symbol| Expr::new(ExprKind::Ident(name), span);
                    let call = Expr::new(
                        ExprKind::Call(
                            Box::new(expr.clone()),
                            names.iter().map(|n| ident(*n)).collect(),
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
                                        crate::ast::PatternKind::Ident(*n),
                                        span,
                                    ),
                                    ty: None,
                                })
                                .collect(),
                            body: Box::new(call),
                        },
                        span,
                    );
                    let method_name = format!("{}.{method}", self.units.defs.get(tr.0).name);
                    let around = self
                        .contexts
                        .last_mut()
                        .map(|context| std::mem::replace(&mut context.closures, method_name));
                    let compiled = self.compile_expr(&lambda);
                    if let (Some(context), Some(around)) = (self.contexts.last_mut(), around) {
                        context.closures = around;
                    }
                    compiled?;
                }
            }

            // `T.decode` as a value, a method the VM has natively for the
            // type: the function `{ v -> T.decode(v) }`. The program wrote
            // no closure there: the function has the method's name,
            // `T.decode`, as a written method has.
            ExprKind::FieldAccess(of, method, _)
                if let Some(TypeMember::Native(_)) = self.type_member(expr)? =>
            {
                let ty = match &of.kind {
                    ExprKind::Ident(ty) | ExprKind::FieldAccess(_, ty, _) => *ty,
                    _ => return Err(checker_missed(span, "a method of no named type")),
                };
                let method_name = format!("{ty}.{method}");
                let names: &[&str] = &["__value__"];
                let ident = |name: &str| Expr::new(ExprKind::Ident(intern(name)), span);
                let call = Expr::new(
                    ExprKind::Call(Box::new(expr.clone()), vec![ident(names[0])]),
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
                let around = self
                    .contexts
                    .last_mut()
                    .map(|context| std::mem::replace(&mut context.closures, method_name));
                let compiled = self.compile_expr(&lambda);
                if let (Some(context), Some(around)) = (self.contexts.last_mut(), around) {
                    context.closures = around;
                }
                compiled?;
            }

            // `Int.empty` of a program's trait, as a value, or `t.empty` of
            // a `type t` bounded by one: through the dictionary, with it
            // bound. (The type named or held is no value of the method.)
            ExprKind::FieldAccess(_, method, _) if self.dict_call(expr).is_some() => {
                let (tr, recv, own) = self.dict_call(expr).expect("a call through a dictionary");
                let (recv, own) = (recv.clone(), own.to_vec());
                self.compile_dict_method_value(tr, &recv, &own, *method, span)?;
            }

            ExprKind::FieldAccess(..)
                if let Some(TypeMember::Slot(slot)) = self.type_member(expr)? =>
            {
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
                self.emit_get_field(self.receiver_field(expr, *field), span)?;
            }

            ExprKind::StringInterp(parts) => {
                // The value of every expression, or its text
                // (`DisplayValue`), stays on the stack until
                // `Interpolate`; a literal part is a constant it reads
                // where it is.
                let mut segments = Vec::with_capacity(parts.len());
                for part in parts {
                    match part {
                        StringPart::Literal(s) => {
                            let k = self.add_constant(Value::String(s.clone().into()), span)?;
                            segments.push(Segment::Literal(k));
                        }
                        StringPart::Expr(e) => {
                            self.compile_expr(e)?;
                            self.emit(Asm::DisplayValue, span)?;
                            segments.push(Segment::Value);
                        }
                    }
                }
                self.emit(
                    Asm::Interpolate {
                        segments: &segments,
                    },
                    span,
                )?;
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
                let own_name = self.closure_name.take();
                // A closure is named after what it is written in, and
                // so are the closures in it.
                let name = match self.contexts.last() {
                    Some(around) => around.closures.clone(),
                    None => return Err(checker_missed(span, "a closure outside any code")),
                };
                self.begin_function(name.clone(), params.len(), span)?;
                if let Some(closure) = self.contexts.last_mut() {
                    closure.closures = name;
                }

                self.compile_params(params, span)?;

                // A closure that calls itself by the name its `let`
                // gives it has itself in a local of that name, unless a
                // parameter takes the name.
                if let Some(name) = own_name
                    && reads_local(body, name)
                    && !params
                        .iter()
                        .any(|p| Self::pattern_binding_names(&p.pattern).contains(&name))
                {
                    self.emit(Asm::GetClosure, span)?;
                    let slot = self.add_local(name, span)?;
                    self.emit(Asm::SetLocal { slot }, span)?;
                }

                // Compile the lambda body in tail position for TCO.
                self.in_tail_position = true;
                self.compile_expr(body)?;
                self.in_tail_position = false;
                self.emit_body_return(span)?;

                let (func, upvalue_descs) = self.end_function(span)?;

                let vm_closure = Arc::new(VmClosure::new(Arc::new(func), vec![]));
                let closure_val = Value::VmClosure(vm_closure);
                let fi = self.add_constant(closure_val, span)?;

                // A closure is made each time the lambda is evaluated,
                // one that captures nothing too: then it is the
                // evaluating task's own, and the tasks that evaluate
                // one lambda (and call what they made, once for each
                // element of a list) do not all count references to
                // one constant.
                self.emit(
                    Asm::MakeClosure {
                        f: fi,
                        captures: &upvalue_descs,
                    },
                    span,
                )?;
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
                    // RecordCreate but with the type of the anonymous
                    // records of these fields.
                    let field_names: Vec<Symbol> = fields.iter().map(|(n, _)| *n).collect();
                    self.compile_operands(fields.iter().map(|(_, val)| val))?;
                    let mut names: Vec<String> = field_names.iter().map(|n| resolve(*n)).collect();
                    names.sort();
                    let anon = crate::typeinfo::anon_record_type(names.iter().map(|n| &**n));
                    let ty = self.add_constant(Value::TypeDescriptor(anon), span)?;
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
                        to: start,
                    },
                    span,
                )?;
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

        self.compile_expr(scrutinee)?;
        // (A scrutinee that is only held against literals needs none of
        // what follows.)
        if self.compile_literal_match(arms, span, tail)? {
            return Ok(());
        }

        // Keep the scrutinee in the frame as a hidden local. This lets us
        // GetLocal it for each arm's test and binding.
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
            let row = self.builtin_row(&builtin_name, span)?;
            // (One bounded by a trait whose impls are passed is passed
            // their dictionaries first, `list.traverse`'s Applicative.)
            let dicts = match callee.ev.as_deref() {
                Some(crate::evidence::Site::Ref(bounds)) => self.pass_dicts(bounds, span)?,
                _ => 0,
            };
            self.compile_operands(args.iter().copied())?;
            let argc = dicts + args.len();
            let builtin = row.id;
            self.emit(Asm::CallBuiltin { builtin, argc }, span)?;
        } else if let ExprKind::FieldAccess(receiver, method, _) = &callee.kind {
            let member = self.type_member(callee)?;
            if self.res_trait(receiver.res).is_some() {
                // `Tr.m(args)`: the method of the impl the checker
                // selected for the type of the arguments.
                self.compile_trait_call(callee, *method, args, span, tail)?;
            } else if let Some(TypeMember::Native(t)) = member {
                // `T.decode(v)`: the VM's method of the type, which takes
                // no value of it; the type's descriptor chooses it.
                let Some(descriptor) = self.type_value(receiver.res) else {
                    return Err(checker_missed(span, "a method of no named type"));
                };
                let idx = self.add_constant(descriptor, span)?;
                self.emit(Asm::Constant { k: idx }, span)?;
                self.compile_operands(args.iter().copied())?;
                self.emit_call_method(*method, args.len() + 1, t, 0, tail, span)?;
            } else if let (Some(TypeMember::Dict), Some((tr, recv, own))) =
                (&member, self.dict_call(callee))
            {
                // `Pt.make(1)`, `Int.empty()`: a method of a program's
                // trait reached through its type, through the dictionary.
                let (recv, own) = (recv.clone(), own.to_vec());
                self.compile_dict_call(tr, &recv, &own, *method, args, span, tail)?;
            } else if let Some(TypeMember::Slot(slot)) = member {
                // `Pt.make(1)`, `m.Pt.make(1)`: a method reached
                // through its type.
                self.emit(Asm::GetGlobal { slot }, span)?;
                self.compile_operands(args.iter().copied())?;
                let argc = args.len();
                self.emit_call(argc, tail, span)?;
            } else if let Some(def) = self.value_def(callee.res) {
                // A module's function: `m.f(1)`.
                self.emit_global_value(def, span)?;
                let dicts = match (self.makes_its_value(callee), callee.ev.as_deref()) {
                    // (A `let` that is general in a bound: its value.)
                    (true, Some(crate::evidence::Site::Ref(bounds))) => {
                        let n = self.pass_dicts(bounds, span)?;
                        self.emit(Asm::Call { argc: n }, span)?;
                        (0, 0)
                    }
                    _ => self.callee_dicts_of(callee, span, tail)?,
                };
                self.compile_operands(args.iter().copied())?;
                let argc = dicts.0 + args.len();
                self.emit_call_keeping(argc, dicts.1, tail, span)?;
            } else {
                // A field's function, or a method on a type held in a
                // value (`a.empty()` for a `type a`).
                self.compile_method_call(callee, receiver, *method, args, span, tail)?;
            }
        } else {
            // Normal function call: a function with bounds is passed its
            // dictionaries first.
            let around = self.callee_call.replace(span);
            let compiled = self.compile_expr(callee);
            self.callee_call = around;
            compiled?;
            let (dicts, keep) = self.callee_dicts_of(callee, span, tail)?;
            self.compile_operands(args.iter().copied())?;
            let argc = dicts + args.len();
            self.emit_call_keeping(argc, keep, tail, span)?;
        }
        Ok(())
    }

    /// Compile `Tr.method(args)`, a call through a trait, as the
    /// checker's `Selection` on `callee` says: the method of one impl,
    /// a call of its global; else the method found where the code runs,
    /// by the type of the first argument.
    fn compile_trait_call(
        &mut self,
        callee: &Expr,
        method: Symbol,
        args: &[&Expr],
        span: Span,
        tail: bool,
    ) -> Result<(), Diagnostic> {
        use crate::ast::Selection;
        // A method of a trait a program declares: through the dictionary
        // the evidence names, the impl's whether the checker knows it or
        // not.
        if let Some((tr, recv, own)) = self.dict_call(callee) {
            let (recv, own) = (recv.clone(), own.to_vec());
            return self.compile_dict_call(tr, &recv, &own, method, args, span, tail);
        }
        match callee.sel {
            Some(Selection::Impl { tr, ty })
                if let Some(slot) = self.globals.method(tr, ty, &resolve(method)) =>
            {
                self.emit(Asm::GetGlobal { slot }, span)?;
                self.compile_operands(args.iter().copied())?;
                self.emit_call(args.len(), tail, span)
            }
            // Found where the code runs, by the type of the argument of
            // type `Self` (a builtin trait's is the first).
            Some(Selection::Dynamic { tr, at }) if usize::from(at) < args.len() => {
                self.compile_operands(args.iter().copied())?;
                let at = usize::from(at);
                self.emit_call_method(method, args.len(), tr, at, tail, span)
            }
            Some(Selection::Impl { tr, .. } | Selection::Native { tr }) if !args.is_empty() => {
                self.compile_operands(args.iter().copied())?;
                self.emit_call_method(method, args.len(), tr, 0, tail, span)
            }
            _ => Err(checker_missed(
                span,
                &format!("the call of '{method}' through its trait with no selection"),
            )),
        }
    }

    /// Compile `receiver.method(args)` as the checker's `Selection` on
    /// `callee` says: the function a field holds, or, on a type held in
    /// a value (`a.empty()` for a `type a`), the method of the type's
    /// impl, found where the code runs.
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
        // A method of a trait a program declares on a type held in a
        // value (`a.empty()` for a `type a`): through the dictionary of
        // the bound (the value is no argument of it).
        if let Some((tr, recv, own)) = self.dict_call(callee) {
            let (recv, own) = (recv.clone(), own.to_vec());
            return self.compile_dict_call(tr, &recv, &own, method, args, span, tail);
        }
        let with_receiver = std::iter::once(receiver).chain(args.iter().copied());
        match callee.sel {
            // The function a field holds: an ordinary call of it.
            Some(Selection::FieldCall) => {
                self.compile_expr(receiver)?;
                self.emit_get_field(self.receiver_field(receiver, method), span)?;
                self.compile_operands(args.iter().copied())?;
                self.emit_call(args.len(), tail, span)
            }
            // The method is found where the code runs, by the type the
            // descriptor carries.
            Some(Selection::Dynamic { tr, .. }) => {
                self.compile_operands(with_receiver)?;
                self.emit_call_method(method, args.len() + 1, tr, 0, tail, span)
            }
            Some(Selection::Impl { .. } | Selection::Native { .. } | Selection::Field) | None => {
                Err(checker_missed(
                    span,
                    &format!("the call of '{method}' with no selection"),
                ))
            }
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
        // `Recur` writes the new values there, cuts the frame back to
        // just above them and jumps back to the start. With no bindings
        // that is the frame as it is now, with every enclosing local
        // still in place.
        let first_slot = self.emitter().height();

        // The initial values are of the enclosing scope, all of them, as
        // the arguments of a `loop(...)` see the values of the round
        // before: each is compiled before any binding has its name, and
        // stays on the stack as its binding.
        for (_, _, init) in bindings {
            self.compile_expr(init)?;
        }
        let ctx = self.ctx_mut();
        let depth = ctx.scope_depth;
        for (i, (name, _, _)) in bindings.iter().enumerate() {
            ctx.locals.push(Local {
                name: *name,
                depth,
                slot: first_slot + i,
            });
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
        // (A generic type's fields are stated over its parameters.)
        let params: Vec<Type> = (self.units.tables.type_params(ty).into_iter())
            .map(Type::Var)
            .collect();
        let shape = if !variants.is_empty() {
            let variant_fields = self.units.tables.decoded_variant_fields(ty, &[]);
            Shape::Enum(
                variants
                    .iter()
                    .map(|v| {
                        let variant = defs.get(*v);
                        let arity = match variant.kind {
                            crate::defs::DefKind::Variant { arity, .. } => arity,
                            _ => 0,
                        };
                        let declared = variant_fields
                            .iter()
                            .find(|(name, _)| *name == variant.name)
                            .map_or(&[][..], |(_, types)| types.as_slice());
                        let fields = declared
                            .iter()
                            .map(|ty| {
                                let field_type = (self.units.tables)
                                    .decoded_field_type(ty, &params, &mut Vec::new())
                                    .unwrap_or_else(|_| FieldType::Unsupported(shown(ty)));
                                field_type.records(&mut nested);
                                field_type
                            })
                            .collect();
                        VariantInfo {
                            name: resolve(variant.name),
                            arity,
                            fields,
                        }
                    })
                    .collect(),
            )
        } else {
            let fields = self.units.tables.decoded_record_fields(ty, &[]);
            Shape::Record(
                fields
                    .iter()
                    .map(|(name, ty)| {
                        let field_type = (self.units.tables)
                            .decoded_field_type(ty, &params, &mut Vec::new())
                            .unwrap_or_else(|_| FieldType::Unsupported(shown(ty)));
                        field_type.records(&mut nested);
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
        // The record and enum types of its fields, which a decoder builds
        // too.
        for record in nested {
            self.type_info(record);
        }
        info
    }

    /// The variant a resolution names (a constructor pattern, a variant
    /// used as a value).
    fn variant_tag(&self, res: Option<crate::defs::Res>) -> Option<Tag> {
        let crate::defs::Res::Def(id) = res? else {
            return None;
        };
        let crate::defs::DefKind::Variant { ty, ordinal, .. } = self.units.defs.get(id).kind else {
            return None;
        };
        Some(Tag::new(self.type_info(ty), ordinal))
    }

    /// The tag of the variant a constructor pattern names.
    fn pattern_tag(
        &self,
        res: Option<crate::defs::Res>,
        name: Symbol,
        span: Span,
    ) -> Result<Tag, Diagnostic> {
        self.variant_tag(res)
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

    /// How the field `field` of the record `receiver` evaluates to is
    /// read: at its place, if the checker gave the receiver a declared
    /// record type.
    fn receiver_field(&self, receiver: &Expr, field: Symbol) -> FieldRead {
        let declared = match &receiver.ty {
            Some(Type::Generic(ty, _)) => Some(self.type_info(ty.id)),
            _ => None,
        };
        Self::field_read(declared.as_deref(), field)
    }

    /// How the field `field` of a record is read: at its place in the
    /// declared type `ty`, by its name without one.
    fn field_read(ty: Option<&TypeInfo>, field: Symbol) -> FieldRead {
        match ty.and_then(|ty| ty.field_index(&resolve(field))) {
            Some(slot) => FieldRead::Slot(slot),
            None => FieldRead::Named(field),
        }
    }

    /// Replace the record on top of the stack with its field.
    fn emit_get_field(&mut self, field: FieldRead, span: Span) -> Result<(), Diagnostic> {
        match field {
            FieldRead::Slot(index) => self.emit(Asm::GetField { index }, span),
            FieldRead::Named(name) => {
                let name = self.add_constant(Value::String(resolve(name).into()), span)?;
                self.emit(Asm::GetFieldNamed { name }, span)
            }
        }
    }

    /// Push the field of the record on top of the stack.
    fn emit_destruct_field(&mut self, field: FieldRead, span: Span) -> Result<(), Diagnostic> {
        match field {
            FieldRead::Slot(index) => self.emit(Asm::DestructRecordField { index }, span),
            FieldRead::Named(name) => {
                let name = self.add_constant(Value::String(resolve(name).into()), span)?;
                self.emit(Asm::DestructRecordFieldNamed { name }, span)
            }
        }
    }

    /// The type a resolution names, if it names a record or enum type,
    /// or an alias, which is only another name: the type it stands for.
    fn res_type(&self, res: Option<crate::defs::Res>) -> Option<TypeRef> {
        let Some(crate::defs::Res::Def(id)) = res else {
            return None;
        };
        let def = self.units.defs.get(id);
        let ty = TypeRef {
            id: crate::defs::TypeId(id),
            name: def.name,
        };
        match def.kind {
            crate::defs::DefKind::Type(_) => Some(ty),
            crate::defs::DefKind::TypeAlias => Some(canonical_head(self.resolver(), ty)),
            _ => None,
        }
    }

    /// The value of a type used as a value, if the resolution names a
    /// type: a primitive type's descriptor, or the descriptor of a record,
    /// enum or container type.
    fn type_value(&self, res: Option<crate::defs::Res>) -> Option<Value> {
        Some(self.type_descriptor(self.res_type(res)?))
    }

    /// Push the value of the type `ty`, which a type argument written in
    /// full writes (`List(Int)`, `(Int, String)`, `List(a)`): a constant,
    /// or made of the values of the type variables in it, which are
    /// `leaves`, the types alone in the argument.
    fn compile_type_value(
        &mut self,
        ty: &Type,
        leaves: &[&Expr],
        span: Span,
    ) -> Result<(), Diagnostic> {
        if let Some(value) = self.type_constant(ty) {
            let k = self.add_constant(value, span)?;
            return self.emit(Asm::Constant { k }, span);
        }
        if let Type::Var(_) | Type::Rigid(_) = ty {
            let leaf = (leaves.iter())
                .find(|leaf| written_type_of(leaf) == Some(ty))
                .ok_or_else(|| checker_missed(span, "a type variable no type argument gives"))?;
            self.in_tail_position = false;
            return self.compile_expr(leaf);
        }
        let (head, parts) = self
            .type_parts(ty)
            .ok_or_else(|| checker_missed(span, &format!("the type `{}` as a value", shown(ty))))?;
        for part in &parts {
            self.compile_type_value(part, leaves, span)?;
        }
        let ty = self.add_constant(Value::TypeDescriptor(head), span)?;
        self.emit(
            Asm::MakeType {
                ty,
                count: parts.len(),
            },
            span,
        )
    }

    /// The type a type argument written in full gives, as the checker
    /// found it: a type applied to types (`List(Int)`, `List(a)`), a
    /// tuple of types, an alias (`type Pair = (Int, String)`); `None` for
    /// any other expression. (A call of a type is no call of a function:
    /// a type is not callable.)
    fn written_type<'e>(&self, expr: &'e Expr) -> Option<&'e Type> {
        let names_type = |e: &Expr| match e.res {
            Some(crate::defs::Res::Def(id)) => matches!(
                self.units.defs.get(id).kind,
                crate::defs::DefKind::Type(_) | crate::defs::DefKind::TypeAlias
            ),
            _ => false,
        };
        let written = match &expr.kind {
            ExprKind::Call(callee, _) => names_type(callee),
            ExprKind::Tuple(_) => true,
            // (An alias left general, `type AnyBox(a) = Box(a)`, is its
            // head's value.)
            ExprKind::Ident(_) | ExprKind::FieldAccess(..) => {
                matches!(expr.res, Some(crate::defs::Res::Def(id))
                    if self.units.defs.get(id).kind == crate::defs::DefKind::TypeAlias)
                    && written_type_of(expr).is_some_and(|ty| self.type_constant(ty).is_some())
            }
            _ => false,
        };
        written.then(|| written_type_of(expr)).flatten()
    }

    /// The value of the type `ty` if no type variable is in it.
    pub(super) fn type_constant(&self, ty: &Type) -> Option<Value> {
        let primitive = |name: &str| Some(self.type_descriptor(TypeRef::builtin(name)));
        match ty {
            Type::Int => primitive("Int"),
            Type::Float => primitive("Float"),
            Type::String => primitive("String"),
            Type::Bool => primitive("Bool"),
            Type::Unit => primitive("Unit"),
            Type::Generic(t, args) if args.is_empty() => Some(self.type_descriptor(*t)),
            _ => {
                let (head, parts) = self.type_parts(ty)?;
                let args: Vec<Value> = (parts.iter())
                    .map(|part| self.type_constant(part))
                    .collect::<Option<_>>()?;
                Some(Value::TypeDescriptor(TypeInfo::applied(&head, &args)))
            }
        }
    }

    /// The type `ty` applied to types, or the tuple of types, as its head
    /// and the types it is applied to.
    fn type_parts<'t>(&self, ty: &'t Type) -> Option<(Arc<TypeInfo>, Vec<&'t Type>)> {
        let builtin = |name: &str| crate::typeinfo::builtin_type_named(name).cloned();
        match ty {
            Type::List(t) => Some((builtin("List")?, vec![&**t])),
            Type::Set(t) => Some((builtin("Set")?, vec![&**t])),
            Type::Channel(t) => Some((builtin("Channel")?, vec![&**t])),
            Type::Map(k, v) => Some((builtin("Map")?, vec![&**k, &**v])),
            Type::Tuple(ts) if !ts.is_empty() => Some((builtin("Tuple")?, ts.iter().collect())),
            Type::Generic(t, args) if !args.is_empty() => {
                Some((self.type_info(t.id), args.iter().collect()))
            }
            _ => None,
        }
    }

    /// The descriptor of the type `ty`, its value (`type_value`).
    pub(super) fn type_descriptor(&self, ty: crate::types::TypeRef) -> Value {
        let name = resolve(ty.name);
        if crate::defs::builtin_types()
            .get(ty.id.0.0 as usize)
            .is_some()
            && let Some(primitive) = module::BUILTIN_PRIMITIVE_NAMES
                .iter()
                .find(|primitive| **primitive == name)
        {
            return Value::PrimitiveDescriptor(primitive);
        }
        Value::TypeDescriptor(self.type_info(ty.id))
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

    /// The row of the builtin registry that a program names `name`
    /// (`list.map`, `println`).
    fn builtin_row(
        &self,
        name: &str,
        span: Span,
    ) -> Result<&'static crate::builtins::registry::Row, Diagnostic> {
        crate::builtins::registry::registry()
            .named(name)
            .ok_or_else(|| checker_missed(span, &format!("the builtin '{name}', which is none")))
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
            let value = self.builtin_row(&name, span)?.value();
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
    /// VM dispatches: the canonical head of its target (an alias is
    /// the type it stands for). `None` for a target
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

    /// The trait the impl `ti` is of.
    fn impl_trait(&self, ti: &crate::ast::TraitImpl) -> Option<crate::defs::TraitId> {
        match ti.trait_res {
            Some(crate::defs::Res::Def(id)) => Some(crate::defs::TraitId(id)),
            _ => None,
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

    /// Emit `CallMethod` of `method` of the trait `t` with `argc`
    /// values on the stack, found by the type of the one at `at` (the
    /// receiver), which passes on the evidence `site`; in tail position
    /// (`tail`), `TailCallMethod; Return`.
    #[allow(clippy::too_many_arguments)]
    fn emit_call_method(
        &mut self,
        method: Symbol,
        argc: usize,
        t: crate::defs::TraitId,
        at: usize,
        tail: bool,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let method = self
            .globals
            .method_id(&resolve(method))
            .ok_or_else(|| too_many_method_names(span))?;
        let name = self.units.defs.get(t.0).name;
        let of = self.globals.trait_index(t, resolve(name)).ok_or_else(|| {
            let limit = Limit {
                what: "traits whose methods a program calls",
                count: usize::from(u16::MAX) + 2,
                max: usize::from(u16::MAX) + 1,
            };
            limit_diagnostic(limit, span)
        })?;
        let call = match tail {
            true => Asm::TailCallMethod {
                method,
                argc,
                of,
                at,
            },
            false => Asm::CallMethod {
                method,
                argc,
                of,
                at,
            },
        };
        self.emit(call, span)?;
        match tail {
            true => self.emit(Asm::Return, span),
            false => Ok(()),
        }
    }

    /// What `T.method` or `m.T.method` is, a method reached through its
    /// type, as the resolver resolved `T` / `m.T` and the checker the
    /// method's trait: the global of the impl for the type's canonical
    /// head, or, where the impl has none, the VM's own method of the
    /// trait (a structural trait's, a builtin error enum's `message`).
    /// `None` when `expr` is not such an access.
    fn type_member(&self, expr: &Expr) -> Result<Option<TypeMember>, Diagnostic> {
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
        // (The checker wrote the method's trait on the access.)
        let Some(t) = self.res_trait(expr.res) else {
            return Err(checker_missed(
                expr.span,
                &format!("the method '{}.{field}' of no trait", def.name),
            ));
        };
        if crate::defs::has_dictionary(t) {
            return Ok(Some(TypeMember::Dict));
        }
        Ok(Some(
            match self.globals.method(t, ty.id, &resolve(*field)) {
                Some(slot) => TypeMember::Slot(slot),
                None => TypeMember::Native(t),
            },
        ))
    }

    /// The value of the variant `expr` names, as the resolver resolved
    /// it (`Red`, `Color.Red`, `m.Red`, `m.Color.Red`): a nullary variant
    /// is the value, any other its constructor. Two enums may have
    /// variants of one name, so a variant is not looked up by its name.
    fn variant_value(&self, expr: &Expr) -> Option<Value> {
        let tag = self.variant_tag(Some(expr.res?))?;
        Some(match tag.arity() {
            0 => Value::variant(tag, []),
            _ => Value::VariantConstructor(tag),
        })
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
        // (The code of a file's top level, `<script>` or
        // `<module:...>`, is its `let`s, each of which says how its
        // closures are named.)
        let closures = match name.starts_with("<script>") || name.starts_with("<module:") {
            true => "closure at the top level".to_string(),
            false => format!("closure in {name}"),
        };
        self.contexts.push(CompileContext {
            closures,
            emitter: Emitter::new(name, params, span)?,
            locals: Vec::new(),
            scope_depth: 0,
            scope_starts: Vec::new(),
            upvalues: Vec::new(),
            loop_stack: Vec::new(),
            pattern_floor: None,
            frame_dict: None,
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
        // What the code names of the globals is there.
        crate::bytecode::verify::verify_globals(&function, &self.globals).map_err(|error| {
            Diagnostic::error(
                Code::CompilerBug,
                span,
                format!(
                    "compiler bug: function '{}' is malformed {error}",
                    function.name()
                ),
            )
        })?;
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

    /// Emit a jump to a new label, which the caller binds. Where the
    /// jump cannot be reached (after a match arm that ends in a tail
    /// call or a `return`) there is no jump, only the label.
    fn jump(&mut self, span: Span) -> Result<Label, Diagnostic> {
        let to = self.label();
        if self.emitter().reachable() {
            self.emit(Asm::Jump { to }, span)?;
        }
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

    /// The constant of the Float a literal writes. (The lexer refuses a
    /// literal that is no finite number.)
    fn float_constant(&mut self, n: f64, span: Span) -> Result<Const, Diagnostic> {
        let float = crate::value::Float::new(n).ok_or_else(|| {
            Diagnostic::error(
                Code::CompilerBug,
                span,
                format!("compiler bug: the float literal {n} is no finite number"),
            )
        })?;
        self.add_constant(Value::Float(float), span)
    }

    /// The string constants of `names`, in order.
    fn name_constants(&mut self, names: &[Symbol], span: Span) -> Result<Vec<Const>, Diagnostic> {
        names
            .iter()
            .map(|name| self.add_constant(Value::String(resolve(*name).into()), span))
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
    fn compile_operands<'e>(
        &mut self,
        operands: impl IntoIterator<Item = &'e Expr>,
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
        // (After the hidden parameters, if the function has any.)
        let first = self.ctx().emitter.hidden();
        for (slot, param) in (first..).zip(params) {
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

    /// Check if the chunk's code calls the builtin `name` (`list.map`).
    fn calls_builtin(chunk: &Chunk, name: &str) -> bool {
        chunk.instrs().any(|(_, instr)| {
            matches!(instr, crate::bytecode::Instr::CallBuiltin { builtin, .. } if builtin.to_string() == name)
        })
    }

    /// Check if a string constant exists in the chunk.
    fn has_string_constant(chunk: &Chunk, s: &str) -> bool {
        chunk
            .constants()
            .iter()
            .any(|c| matches!(c, Value::String(v) if **v == *s))
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
                .any(|c| matches!(c, Value::Float(f) if f.get() == 4.25))
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
        // The lambda's function is a constant, and a closure of it is
        // made where the lambda is evaluated, though it captures
        // nothing.
        assert!(
            main.chunk()
                .constants()
                .iter()
                .any(|c| matches!(c, Value::VmClosure(_)))
        );
        assert!(has_op(main.chunk(), Op::MakeClosure));
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
        assert!(has_op(f.chunk(), Op::Interpolate));
        assert!(has_op(f.chunk(), Op::DisplayValue));
        // The literal is read in place: no instruction pushes it.
        assert!(!has_op(f.chunk(), Op::Constant));
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

    /// A field of a record whose declared type the checker knows is
    /// read at the place the type declares it, in an expression and in
    /// a pattern that names the type; a field of an anonymous record,
    /// and of a record behind an open row (which a declared record
    /// flows into as itself), by its name.
    #[test]
    fn test_field_reads_at_a_place_or_by_name() {
        use crate::bytecode::Instr;
        let fns = compile(
            r#"
type User { name: String, age: Int }
type Pair(a) { left: a, right: a }
fn age(u: User) -> Int { u.age }
fn right(p: Pair(Int)) -> Int { p.right }
fn called(p: Pair(Fn(Int) -> Int)) -> Int { p.right(1) }
fn bound(u: User) -> Int {
    let User { age, name: _ } = u
    age
}
fn matched(u: User) -> Int {
    match u {
        User { name: "a", age } -> age
        User { age: 3, .. } -> 0
        _ -> 1
    }
}
fn open(r: {age: Int, ...rest}) -> Int { r.age }
fn closed(r: {name: String, age: Int}) -> Int { r.age }
fn inferred(r) { r.age }
fn rest(r: {name: String, age: Int}) -> Int {
    let {age, ...others} = r
    age
}
"#,
        );
        let reads = |name: &str| -> Vec<String> {
            let chunk = find_fn(&fns, name).chunk();
            chunk
                .instrs()
                .filter_map(|(_, instr)| match instr {
                    Instr::GetField { index } => Some(format!("get {index}")),
                    Instr::DestructRecordField { index } => Some(format!("destruct {index}")),
                    Instr::GetFieldNamed { name } => Some(format!("get {}", chunk.string(name))),
                    Instr::DestructRecordFieldNamed { name } => {
                        Some(format!("destruct {}", chunk.string(name)))
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(reads("age"), ["get 1"]);
        assert_eq!(reads("right"), ["get 1"]);
        assert_eq!(reads("called"), ["get 1"]);
        assert_eq!(reads("bound"), ["destruct 1"]);
        assert_eq!(
            reads("matched"),
            ["destruct 0", "destruct 1", "destruct 1"],
            "the tests of `name` and of `age`, then the binding of `age`"
        );
        assert_eq!(reads("open"), ["get age"]);
        assert_eq!(reads("closed"), ["get age"]);
        assert_eq!(reads("inferred"), ["get age"]);
        assert_eq!(reads("rest"), ["destruct age"]);
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
            |c| matches!(c, Value::Variant(variant) if variant.name() == "Red" && variant.fields().is_empty())
        ));
        let Some(Value::Variant(red)) = main.chunk().constants().first() else {
            panic!("main's first constant is the variant");
        };
        let names: Vec<&str> = red
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
        // (One jump on the scrutinee: `compile_literal_match`.)
        let f = find_fn(&fns, "f");
        assert!(has_op(f.chunk(), Op::JumpIfFalse));
        assert!(!has_op(f.chunk(), Op::TestEqual));
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
        // The comparison and its jump are one instruction.
        assert!(has_op(f.chunk(), Op::JumpIfNotGt));
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
        // Guard compiles to a condition + JumpIfFalse, here one
        // instruction that compares and jumps
        assert!(has_op(f.chunk(), Op::JumpIfNotGt));
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
        assert!(calls_builtin(main.chunk(), "list.length"));
    }

    // ── Method call compilation ────────────────────────────────────

    #[test]
    fn test_compile_method_call() {
        let fns = compile(
            r#"
type Foo { x: Int }
trait Show { fn show(self) -> String }
trait Show for Foo {
    fn show(self) -> String { "foo" }
}
fn shown(x: a) -> String where a: Show { Show.show(x) }
fn field(r: {f: Fn(Int) -> Int}) -> Int { r.f(1) }
fn main() {
    let f = Foo { x: 1 }
    Show.show(f)
}
"#,
        );
        // A method of a program's trait where the checker knows the impl,
        // whose header asks nothing: the impl's method is called, its
        // receiver checked, a tail call here.
        let main = find_fn(&fns, "main");
        assert!(!has_op(main.chunk(), Op::CallMethod));
        assert!(has_op(main.chunk(), Op::TailCallChecked));
        // On a bounded variable, through the dictionary the function is
        // given: in tail position, to run in the caller's frame.
        let shown = find_fn(&fns, "shown");
        assert!(has_op(shown.chunk(), Op::TailCallDict));
        assert!(!has_op(shown.chunk(), Op::TailCallMethod));
        // A field that holds a function is read and called.
        let field = find_fn(&fns, "field");
        assert!(has_op(field.chunk(), Op::GetFieldNamed));
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
        assert!(calls_builtin(main.chunk(), "list.length"));
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
                && cl.function.name() == "closure in main"
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
        assert!(has_op(f.chunk(), Op::JumpIfNotGt));
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
        // Pin pattern uses Dup + GetLocal + Eq, and the Eq is one
        // instruction with the jump after it
        assert!(has_op(f.chunk(), Op::Dup));
        assert!(has_op(f.chunk(), Op::JumpIfNotEq));
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

    /// The closure of `fn f`'s one `let g = { … }`.
    fn closure_in_f(input: &str) -> Function {
        find_fn(&compile(input), "closure in f").clone()
    }

    /// A closure that calls itself by the name its `let` gives it takes
    /// itself into a local of that name (`GetClosure`); one that does
    /// not name itself, or whose parameter takes the name, does not.
    #[test]
    fn test_only_a_closure_that_names_itself_gets_itself() {
        let named = closure_in_f(
            "fn f() -> Int {\n  let g = { n ->\n    match n {\n      0 -> 0\n      _ -> g(n - 1)\n    }\n  }\n  g(3)\n}\n",
        );
        assert!(has_op(named.chunk(), Op::GetClosure));
        let plain = closure_in_f("fn f() -> Int {\n  let g = { n -> n + 1 }\n  g(3)\n}\n");
        assert!(!has_op(plain.chunk(), Op::GetClosure));
        let param = closure_in_f("fn f() -> Int {\n  let g = { g -> g + 1 }\n  g(3)\n}\n");
        assert!(!has_op(param.chunk(), Op::GetClosure));
    }
}
