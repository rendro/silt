//! Hindley-Milner type inference and checking for Silt.
//!
//! Inference is by unification, in the order the definitions refer to
//! each other:
//! - Type variables with levels; generalisation of a `let` that binds a
//!   value and of each group of top-level definitions
//! - Annotation variables that are rigid inside their declaration
//! - Exhaustiveness checking for match expressions
//! - Trait bounds: declared, inferred and owed at each use

mod auto_derive;
mod builtin_env;
mod declare_fns;
mod declare_traits;
mod declare_types;
mod derive_gates;
mod derive_synth;
mod env;
mod exhaustiveness;
mod infer;
mod inference;
pub mod names;
mod order;
mod resolve;
mod scheme;
mod show;
mod solve;
mod tables;
mod typeexpr;
mod unify;
// `pub(crate)`: round 93 — `module::sibling_module_suggestion` reuses
// the shared did-you-mean threshold policy for import-path hints.
pub(crate) mod suggest;

pub(super) use std::collections::{BTreeSet, HashMap};

pub(super) use crate::ast::*;
pub(super) use crate::intern::{Symbol, intern, resolve};
pub(super) use crate::source::Span;
pub(super) use crate::types::*;

pub use crate::types::{Scheme, TraitKey, TyVar, Type, TypeRef};

use crate::diagnostic::{Code, Diagnostic, Severity};
pub use builtin_env::*;
use declare_fns::FnSig;
use derive_synth::*;
use env::TypeEnv;
use infer::pattern::collect_pattern_vars;
use solve::{Goal, Origin, Wanted};
use std::rc::Rc;
pub use tables::*;
pub use unify::*;

/// Names of builtin traits that the compiler registers automatically
/// with auto-derived impls for every primitive and builtin container.
/// User code cannot redeclare a trait with any of these names — doing
/// so would shadow the compiler's TraitInfo (different method names,
/// different signatures) and produce nonsensical cascade errors when
/// the preregistered impls get revalidated against the user's body.
pub(super) const BUILTIN_TRAIT_NAMES: &[&str] =
    &["Equal", "Compare", "Hash", "Display", "Error", "Number"];

/// The built-in traits a program cannot implement by hand: they are
/// derived structurally (see `reject_sealed_trait_impls`).
pub(super) const SEALED_TRAIT_NAMES: &[&str] = &["Equal", "Compare", "Hash", "Number"];

/// Subset of [`BUILTIN_TRAIT_NAMES`] that is auto-derived for every
/// primitive and builtin container. `Error` is intentionally excluded:
/// user types and stdlib types must implement `trait Error for ...`
/// explicitly.
pub(super) const BUILTIN_AUTO_DERIVED_TRAIT_NAMES: &[&str] =
    &["Equal", "Compare", "Hash", "Display"];

// ── The type checker ────────────────────────────────────────────────

#[derive(Clone)]
pub struct TypeChecker {
    /// Accumulated type errors.
    pub errors: Vec<Diagnostic>,
    /// Tracks the types of bindings in the enclosing `loop` (if any),
    /// so that `recur` arity and types can be validated.
    pub(super) loop_binding_types: Option<Vec<Type>>,
    /// The bounds in scope: for each annotation variable of a declaration
    /// whose body was or is being checked (by the variable it is in the
    /// declaration's scheme), the traits its `where` clauses declare and
    /// their supertraits, each with its trait arguments (`[Int]` for
    /// `where a: TryInto(Int)`), as the declaration's scheme writes them.
    /// A rigid variable has the methods of these.
    pub(super) bounds: HashMap<TyVar, Vec<(TraitKey, Vec<Type>)>>,
    /// The expected return type of the enclosing function (if any).
    pub(super) current_return_type: Option<Type>,
    /// The types of the module being checked that have a written
    /// `Display` impl, known before the impls are registered: such a
    /// type has `Display` by that impl, not by its structure.
    pub(super) display_written: std::collections::HashSet<TypeRef>,
    /// The (place, type) pairs a missing trait is reported for.
    pub(super) lacking: std::collections::HashSet<(Span, String)>,
    /// The row variables of the signatures of the module's functions
    /// (each by the variable it is in the function's scheme).
    pub(super) sig_rows: std::collections::HashSet<TyVar>,
    /// Those of them a body spreads a record over, or binds the rest
    /// of: no declared record may stand for one (`Pred::Anon`).
    pub(super) anon_rows: std::collections::HashSet<TyVar>,
    /// What stands for a signature's row variable whose body may not be
    /// checked yet: decided when the module's bodies are
    /// (`settle_rows`).
    pub(super) anon_waiting: Vec<(TyVar, Type, Origin)>,
    /// Whether the expression checked next is the callee of a call:
    /// `x.m` there is a method call, anywhere else a field.
    pub(super) callee_position: bool,
    /// The receiver and the name of the callee just checked, when it is
    /// `x.m` and the type of `x` is unknown: `check_call` lets the call
    /// wait (`Goal::Select`).
    pub(super) unknown_receiver: Option<(Type, Symbol)>,
    /// Spans of `?` uses in the current fn/lambda body (round 93). When the
    /// body/return-type unify fails on a Result/Option return that `?`
    /// itself demanded, the diagnostic points back at the `?` site instead
    /// of leaving a bare header-located mismatch. Saved/restored around
    /// each fn body and lambda body, like `current_return_type`.
    pub(super) current_qmark_spans: Vec<Span>,
    /// Names of function declarations that were synthesized by parser
    /// error recovery (Option B). Populated in register_fn_decl when the
    /// FnDecl has `is_recovery_stub == true`. Used by the `ExprKind::Call`
    /// arm to suppress cascade errors (arity/arg-type) — the real parse
    /// error already told the user what went wrong, so reporting N bogus
    /// "undefined variable 'f'" or "function expects 2 args, got 1"
    /// errors would just be noise.
    pub(super) recovery_stub_names: std::collections::HashSet<Symbol>,
    /// B2: span used by `resolve_type_expr` when reporting arity errors
    /// on user type annotations. Callers set this to the surrounding
    /// declaration's span (e.g. `f.span`) before calling resolve, and
    /// reset it afterward. Defaults to a sentinel zero-span when no
    /// caller has populated it.
    pub(super) current_type_anno_span: Option<Span>,
    /// The predicates the uses checked so far owe (`want`), in the order
    /// they were owed. One is solved once its subject is known; one whose
    /// subject `generalize` quantifies is the scheme's
    /// (`let f = constrained_fn`, `fn wrap(x) { constrained_fn(x) }`).
    pub(super) wanted: Vec<Wanted>,
    /// Where in `wanted` each open generalisation scope starts
    /// (`enter_level`), and where the one `exit_level` just left started.
    pub(super) wanted_marks: Vec<usize>,
    pub(super) closed_mark: usize,
    /// The use that names itself for the scheme instantiated next: a
    /// call (its span, the callee as written), a name used as a value.
    /// `instantiate` takes it.
    pub(super) named_use: Option<Origin>,
    /// In a REPL cell: the files (cells) of the checks taken up from the
    /// earlier cells (`take_up_waiting`).
    pub(super) taken_up: std::collections::HashSet<crate::source::FileId>,
    /// The expression being checked.
    pub(super) at: Span,
    /// The parameter types the closure literal checked next is expected
    /// to have: it is an argument of a call whose callee is known. The
    /// closure takes it.
    pub(super) expected_closure: Option<Vec<Type>>,
    /// The annotation variables of the declaration whose body is being
    /// checked, by name: an annotation in the body that writes one of the
    /// names means the same variable.
    pub(super) sig_names: HashMap<Symbol, Type>,
    /// Each annotation variable of a declaration whose body was or is
    /// being checked, as its body sees it: rigid.
    pub(super) rigid_of: HashMap<TyVar, Type>,
    /// The type variables `let` annotations introduced: no `where`
    /// clause can bound one.
    pub(super) let_vars: std::collections::HashSet<TyVar>,
    /// The annotation variables with a `where` clause whose trait is
    /// unknown (reported): what bounds them is not known, so a method
    /// call or a bound owed on one is not reported as well.
    pub(super) unknown_bounds: std::collections::HashSet<TyVar>,
    /// The annotation variables of the functions of the group being
    /// inferred together, when it has several: each with the functions
    /// (by declaration) it is a variable of. Two of different functions
    /// may turn out to be one variable of the group
    /// (`same_in_group`); `rigid_alias` then says which one each stands
    /// for.
    pub(super) group_rigid: HashMap<TyVar, Vec<usize>>,
    pub(super) rigid_alias: HashMap<TyVar, RigidId>,
    /// The declaration of each top-level `let` of the module, by the
    /// names it binds, and the one whose value is being checked: its
    /// value may not read a `let` declared after it, which has not run
    /// when it does.
    pub(super) let_index: HashMap<Symbol, usize>,
    pub(super) checking_let: Option<usize>,
    /// The signature of each method written in an impl of the module, as
    /// its body sees it, by the impl's type, the method and the trait,
    /// until the body is checked.
    pub(super) impl_sigs: HashMap<(TypeRef, Symbol, TraitKey), FnSig>,
    /// Set by the FieldAccess arm of infer_expr: `true` when the last
    /// FieldAccess resolved via method dispatch (trait method table),
    /// `false` when it resolved via record-field or module-qualified
    /// lookup.  Read by the Call arm immediately after inferring the
    /// callee to decide arity semantics (method call adds implicit self;
    /// field/module calls do not).
    pub(super) last_field_access_was_method: bool,
    /// The trait whose method the field access being inferred calls,
    /// which `infer_expr` records on the access (`Expr::res`): the
    /// compiler keys the call by it.
    pub(super) method_trait: Option<TraitKey>,
    /// The trait the method call being inferred names already: a derived
    /// impl's body calls the builtin trait's method of a field
    /// (`display` of Display), whatever other trait has a method of the
    /// name.
    pub(super) forced_trait: Option<TraitKey>,
    /// The traits of the method calls resolved in the deferred pass, by
    /// the span of the access; `resolve_all_types` records each on its
    /// access.
    pub(super) deferred_method_traits: HashMap<Span, TraitKey>,
    /// The methods the impls of two or more traits provide for one type,
    /// with the traits, where this module sees none or several of the
    /// traits: a call of one is ambiguous.
    pub(super) ambiguous_methods: HashMap<(TypeRef, Symbol), Vec<TraitKey>>,
    /// The traits the module names by its imports (`import m.{ T }`),
    /// and the modules it imports: their traits it sees too.
    pub(super) seen_traits: std::collections::HashSet<crate::defs::DefId>,
    pub(super) seen_modules: std::collections::HashSet<crate::session::ModuleId>,
    /// Trait-orphan check (round 63 item 5): the package symbol whose
    /// source we're currently typechecking. `Some(pkg)` is set by
    /// `check_module` (the entry point the session checks every module
    /// of a program with) so per-package
    /// decls (traits/enums/records) are stamped with the right
    /// `defined_in`. `None` means a host module: treat every decl as
    /// local (sentinel
    /// `__builtin__`) so the orphan rule never trips on a program that
    /// has no package context.
    pub(super) current_package: Option<Symbol>,
    /// The session's definitions, which the resolver's `Res` slots
    /// name. `None` for a checker that has no program (the builtins).
    pub(super) defs: Option<std::sync::Arc<crate::defs::DefTable>>,
    /// The module checked.
    pub(super) module: crate::session::ModuleId,
    /// Its name, for diagnostics.
    pub(super) module_name: Symbol,
    /// The types the module's own declarations declare, by name: what a
    /// declaration (which has no resolution of its own) and a name the
    /// checker writes itself refer to.
    pub(super) own_types: HashMap<Symbol, TypeRef>,
    /// The traits the module's own declarations declare, by name.
    pub(super) own_traits: HashMap<Symbol, TraitKey>,
    /// The module's type declarations that are rejected (a reserved or
    /// builtin type's name): a use of one, or of a variant of one, is
    /// already reported, so it is an error type, silently.
    pub(super) rejected_types: std::collections::HashSet<TypeRef>,
    /// The methods of the impls whose trait or type the resolver resolved
    /// to nothing: a call of one is not reported again as unknown.
    pub(super) unresolved_impl_methods: std::collections::HashSet<Symbol>,
    /// Whether the module is a REPL cell: its `let`s are offered to the
    /// next cell, which may fix what is unknown of their types.
    pub(super) is_cell: bool,
    /// Whether the program is a host module's signatures: its
    /// functions have no bodies to check.
    pub(super) signatures_only: bool,
    /// Whether the declarations read are rows of the builtin registry.
    /// A row's text may say what a program's cannot: the type `Never`,
    /// and a result of a type no parameter fixes (`set.new() -> Set(a)`).
    pub(super) registry_rows: bool,
    /// What the checks of a session share; see [`Tables`]. Moved in
    /// for one module's check and out again after it.
    pub(super) tables: Tables,
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeChecker {
    pub fn new() -> Self {
        TypeChecker {
            errors: Vec::new(),
            loop_binding_types: None,
            bounds: HashMap::new(),
            current_return_type: None,
            display_written: std::collections::HashSet::new(),
            lacking: std::collections::HashSet::new(),
            sig_rows: std::collections::HashSet::new(),
            anon_rows: std::collections::HashSet::new(),
            anon_waiting: Vec::new(),
            callee_position: false,
            unknown_receiver: None,
            current_qmark_spans: Vec::new(),
            recovery_stub_names: std::collections::HashSet::new(),
            current_type_anno_span: None,
            wanted: Vec::new(),
            wanted_marks: Vec::new(),
            closed_mark: 0,
            named_use: None,
            taken_up: std::collections::HashSet::new(),
            at: Span::BUILTIN,
            expected_closure: None,
            sig_names: HashMap::new(),
            rigid_of: HashMap::new(),
            let_vars: std::collections::HashSet::new(),
            unknown_bounds: std::collections::HashSet::new(),
            group_rigid: HashMap::new(),
            rigid_alias: HashMap::new(),
            let_index: HashMap::new(),
            checking_let: None,
            impl_sigs: HashMap::new(),
            last_field_access_was_method: false,
            method_trait: None,
            forced_trait: None,
            deferred_method_traits: HashMap::new(),
            ambiguous_methods: HashMap::new(),
            seen_traits: std::collections::HashSet::new(),
            seen_modules: std::collections::HashSet::new(),
            current_package: None,
            defs: None,
            module: crate::session::ModuleId(0),
            module_name: intern("main"),
            own_types: HashMap::new(),
            own_traits: HashMap::new(),
            rejected_types: std::collections::HashSet::new(),
            unresolved_impl_methods: std::collections::HashSet::new(),
            is_cell: false,
            signatures_only: false,
            registry_rows: false,
            tables: Tables::default(),
        }
    }

    /// Sentinel package symbol used as the `defined_in` for built-in
    /// trait/enum/record entries (and for user decls processed without
    /// an enclosing package, i.e. a check outside a session). Distinct
    /// from any real package name because user package names are
    /// validated against [a-z][a-z0-9_-]* by the manifest layer, so a
    /// double-underscore name cannot collide.
    pub(super) fn builtin_pkg() -> Symbol {
        intern("__builtin__")
    }

    /// Returns the `defined_in` package stamp to record on a decl
    /// processed at the current cursor position: the active
    /// `current_package` if set, otherwise the built-in sentinel.
    pub(super) fn defining_package(&self) -> Symbol {
        self.current_package.unwrap_or_else(Self::builtin_pkg)
    }

    /// The trait the module's own declaration `name` declares; a
    /// builtin trait's declaration (the builtin environment) declares
    /// the builtin trait.
    pub(super) fn own_trait(&self, name: Symbol) -> TraitKey {
        if let Some(t) = self.own_traits.get(&name) {
            return *t;
        }
        let id = crate::defs::builtin_trait_id(&resolve(name))
            .unwrap_or_else(|| panic!("the module declares a trait '{name}'"));
        TraitKey { id, name }
    }

    /// The type the module's own declaration `name` declares.
    pub(super) fn own_type(&self, name: Symbol) -> TypeRef {
        *self
            .own_types
            .get(&name)
            .unwrap_or_else(|| panic!("the module declares a type '{name}'"))
    }

    /// Enter the types the module's declarations declare, from the
    /// definitions the resolver entered for them.
    fn enter_own_types(&mut self) {
        let Some(defs) = &self.defs else {
            return;
        };
        let mut types = HashMap::new();
        let mut traits = HashMap::new();
        for id in defs.of_module(self.module) {
            let def = defs.get(*id);
            let name = def.name;
            match def.kind {
                crate::defs::DefKind::Type(ty) => {
                    types.insert(name, TypeRef { id: ty, name });
                }
                crate::defs::DefKind::TypeAlias => {
                    types.insert(
                        name,
                        TypeRef {
                            id: crate::defs::TypeId(*id),
                            name,
                        },
                    );
                }
                crate::defs::DefKind::Trait(t) => {
                    traits.insert(name, TraitKey { id: t, name });
                }
                _ => {}
            }
        }
        self.own_types = types;
        self.own_traits = traits;
    }

    /// Report each private record or enum type of the module that a
    /// public definition's checked type names: a `pub fn`'s or `pub let`'s
    /// scheme (inferred parts and aliases included), a `pub trait`'s method
    /// signatures; and each private trait a `pub trait` lists as a
    /// supertrait. An importer could reach the value but never name its
    /// type. A private type written in the definition's own annotations is
    /// reported by the resolver already.
    fn report_private_in_schemes(&mut self, program: &Program, env: &TypeEnv) {
        let Some(defs) = self.defs.clone() else {
            return;
        };
        let module = self.module;
        let private_type = |r: &TypeRef| {
            let def = defs.get(r.id.0);
            def.module == module
                && def.vis == crate::defs::Vis::Private
                && matches!(def.kind, crate::defs::DefKind::Type(_))
        };
        let mut found: Vec<(TypeRef, String, Symbol, Span)> = Vec::new();
        let leaks = |this: &Self,
                     ty: &Type,
                     written: &std::collections::HashSet<crate::defs::DefId>,
                     owner: String,
                     owner_name: Symbol,
                     span: Span,
                     found: &mut Vec<(TypeRef, String, Symbol, Span)>| {
            let ty = crate::types::canonical::canonicalize(&this.tables.resolver, &this.apply(ty));
            let mut refs = Vec::new();
            ty.collect_refs(&mut refs);
            let mut seen = std::collections::HashSet::new();
            for r in refs {
                if private_type(&r) && !written.contains(&r.id.0) && seen.insert(r.id) {
                    found.push((r, owner.clone(), owner_name, span));
                }
            }
        };
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) if f.is_pub => {
                    let mut written = std::collections::HashSet::new();
                    for te in f
                        .params
                        .iter()
                        .filter_map(|p| p.ty.as_ref())
                        .chain(&f.return_type)
                    {
                        written_defs(te, &mut written);
                    }
                    if let Some(scheme) = env.lookup(f.name) {
                        let owner = format!("public fn '{}'", f.name);
                        leaks(
                            self,
                            &scheme.ty,
                            &written,
                            owner,
                            f.name,
                            f.name_span,
                            &mut found,
                        );
                    }
                }
                Decl::Let { is_pub: true, .. } => {
                    for (name, span, _) in crate::parser::top_level_binders(decl) {
                        if let Some(scheme) = env.lookup(name) {
                            let none = std::collections::HashSet::new();
                            let owner = format!("public let '{name}'");
                            leaks(self, &scheme.ty, &none, owner, name, span, &mut found);
                        }
                    }
                }
                Decl::Trait(t) if t.is_pub => {
                    let key = self.own_trait(t.name);
                    let Some(info) = self.tables.traits.get(&key).cloned() else {
                        continue;
                    };
                    let none = std::collections::HashSet::new();
                    for (_, method_ty) in &info.methods {
                        let owner = format!("public trait '{}'", t.name);
                        leaks(
                            self,
                            method_ty,
                            &none,
                            owner,
                            t.name,
                            t.name_span,
                            &mut found,
                        );
                    }
                    for sup in &info.supertraits {
                        if let Some(sup_info) = self.tables.traits.get(sup)
                            && sup_info
                                .private_to
                                .is_some_and(|(owner, _)| owner == module)
                        {
                            let def = defs.get(sup.id.0);
                            self.errors.push(
                                Diagnostic::error(
                                    Code::PrivateItem,
                                    t.name_span,
                                    format!(
                                        "private trait '{}' is a supertrait of public trait '{}'",
                                        sup.name, t.name
                                    ),
                                )
                                .with_label(
                                    def.span,
                                    format!("'{}' is declared without `pub`", sup.name),
                                )
                                .with_help(format!(
                                    "mark `{}` `pub`, or drop the `pub` of '{}'",
                                    sup.name, t.name
                                )),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        for (ty, owner, owner_name, span) in found {
            let def = defs.get(ty.id.0);
            self.errors.push(
                Diagnostic::error(
                    Code::PrivateItem,
                    span,
                    format!("private type '{}' in the type of {owner}", ty.name),
                )
                .with_label(def.span, format!("'{}' is declared without `pub`", ty.name))
                .with_help(format!(
                    "mark `{}` `pub`, or drop the `pub` of '{owner_name}'",
                    ty.name
                )),
            );
        }
    }

    /// A REPL cell takes up what the earlier cells left waiting for a
    /// type still unknown: if the cell decides the type, their checks are
    /// its to pass. (They stay the earlier cells' in the session's
    /// tables: a cell that fails is forgotten, and the next one takes
    /// them up again.) A check in the body of a definition nothing can
    /// run any more is not taken up: the cell or an earlier one has
    /// defined its name again, and no definition still in reach refers
    /// to it.
    fn take_up_waiting(&mut self, program: &Program, earlier: &[(Symbol, crate::defs::DefId)]) {
        // What is in reach: the names this cell leaves as they were,
        // what the cell itself refers to, and what those refer to.
        let own = self.cell_decls(program);
        let redefined: Vec<Symbol> = program
            .decls
            .iter()
            .filter(|decl| matches!(decl, Decl::Fn(_) | Decl::Let { .. }))
            .flat_map(crate::parser::top_level_binders)
            .map(|(name, _, _)| name)
            .collect();
        let mut live: std::collections::HashSet<crate::defs::DefId> = earlier
            .iter()
            .filter(|(name, _)| !redefined.contains(name))
            .map(|(_, id)| *id)
            .collect();
        live.extend(own.iter().flat_map(|decl| decl.refers.iter().copied()));
        loop {
            let reached: Vec<crate::defs::DefId> = self
                .tables
                .waiting
                .values()
                .flat_map(|cell| &cell.decls)
                .filter(|decl| decl.defs.iter().any(|id| live.contains(id)))
                .flat_map(|decl| decl.refers.iter().copied())
                .filter(|id| !live.contains(id))
                .collect();
            if reached.is_empty() {
                break;
            }
            live.extend(reached);
        }
        for (module, waiting) in &self.tables.waiting {
            if *module == self.module {
                continue;
            }
            // A check outside every definition is of an expression the
            // cell ran: it waits on.
            let waits = |span: &Span| {
                waiting
                    .decls
                    .iter()
                    .filter(|decl| decl.span.start <= span.start && span.end <= decl.span.end)
                    .all(|decl| decl.defs.iter().any(|id| live.contains(id)))
            };
            self.wanted.extend(
                waiting
                    .wanted
                    .iter()
                    .filter(|w| waits(&w.origin.span))
                    .cloned(),
            );
        }
        self.taken_up = self.waiting_files();
    }

    /// The functions and `let`s of the cell `program`, each with what it
    /// defines and what its body refers to.
    fn cell_decls(&self, program: &Program) -> Vec<CellDecl> {
        let Some(defs) = &self.defs else {
            return Vec::new();
        };
        let mut decls = Vec::new();
        for decl in &program.decls {
            let (span, body) = match decl {
                Decl::Fn(f) => (f.span, &f.body),
                Decl::Let { span, value, .. } => (*span, value),
                _ => continue,
            };
            let names: Vec<Symbol> = crate::parser::top_level_binders(decl)
                .into_iter()
                .map(|(name, _, _)| name)
                .collect();
            let own: Vec<crate::defs::DefId> = defs
                .of_module(self.module)
                .iter()
                .copied()
                .filter(|id| names.contains(&defs.get(*id).name))
                .collect();
            let mut refers = Vec::new();
            order::references_in_expr(body, &mut |res| {
                if let Some(crate::defs::Res::Def(id)) = res
                    && !refers.contains(&id)
                {
                    refers.push(id);
                }
            });
            decls.push(CellDecl {
                span: Span {
                    file: span.file,
                    start: span.start.min(body.span.start),
                    end: span.end.max(body.span.end),
                },
                defs: own,
                refers,
            });
        }
        decls
    }

    /// Leave what this cell's own checks still wait for to the cells
    /// after it.
    fn leave_waiting(&mut self, program: &Program) {
        let earlier = std::mem::take(&mut self.taken_up);
        let own = |span: &Span| !earlier.contains(&span.file);
        let waiting = Waiting {
            decls: self.cell_decls(program),
            wanted: std::mem::take(&mut self.wanted)
                .into_iter()
                .filter(|w| !w.solved && own(&w.origin.span))
                .collect(),
        };
        self.tables.waiting.insert(self.module, waiting);
    }

    /// A check an earlier cell left waiting that this cell fails is this
    /// cell's error: it is reported at the cell, which is the input
    /// dropped, with the earlier cell's check as a label.
    fn report_at_cell(&mut self, program: &Program) {
        // (The cell's first declarations are the session's: the imports
        // of what the earlier cells bind.)
        let Some(at) = program
            .decls
            .iter()
            .map(|decl| match decl {
                Decl::Fn(f) => f.span,
                Decl::Let { span, .. } => *span,
                Decl::Type(t) => t.span,
                Decl::Trait(t) => t.span,
                Decl::TraitImpl(t) => t.span,
                Decl::Import(_, span) => *span,
            })
            .rfind(|span| span.file != Span::BUILTIN.file && !self.taken_up.contains(&span.file))
        else {
            return;
        };
        for error in &mut self.errors {
            if error.span.file == at.file || !self.taken_up.contains(&error.span.file) {
                continue;
            }
            let earlier = std::mem::replace(&mut error.span, at);
            let check = std::mem::take(&mut error.message);
            error.message =
                format!("this input gives a value a type an earlier input does not allow: {check}");
            error.labels.insert(0, (earlier, check));
            error
                .notes
                .push("this input is dropped; the earlier one stands".to_string());
        }
    }

    /// The files of what is waiting: each REPL cell is a file of its own.
    fn waiting_files(&self) -> std::collections::HashSet<crate::source::FileId> {
        self.wanted.iter().map(|w| w.origin.span.file).collect()
    }

    /// Report each top-level `let` whose type the module's check leaves
    /// partly unknown (`let ch = channel.new(1)` when nothing in the
    /// module sends on it). A module's check is where its types are
    /// decided: an importer would fix the rest of a `pub let`, and two
    /// importers could fix it two ways; a private one is reached through
    /// the module's public functions just the same. The declaration is
    /// where it is reported. A REPL cell is exempt: the next cell may
    /// decide it.
    fn report_unknown_let_types(&mut self, program: &Program, env: &TypeEnv) {
        if self.is_cell {
            return;
        }
        for decl in &program.decls {
            let Decl::Let { is_pub, .. } = decl else {
                continue;
            };
            let (what, keyword, why) = match is_pub {
                true => (
                    "public let",
                    "pub let",
                    "a module that imports it cannot decide it",
                ),
                false => ("top-level let", "let", "nothing in the module decides it"),
            };
            for (name, span, _) in crate::parser::top_level_binders(decl) {
                let Some(scheme) = env.lookup(name).cloned() else {
                    continue;
                };
                let ty = self.apply(&scheme.ty);
                if free_vars_in(&ty).iter().all(|v| scheme.vars.contains(v)) {
                    continue;
                }
                self.errors.push(
                    Diagnostic::error(
                        Code::AmbiguousType,
                        span,
                        format!(
                            "the type of {what} '{name}' is not fully known here: {}",
                            self.show_type(&ty)
                        ),
                    )
                    .with_help(format!(
                        "annotate it, e.g. `{keyword} {name}: <type> = ...`: {why}"
                    )),
                );
            }
        }
    }

    // ── Check a full program ────────────────────────────────────────

    /// Check `program` in `env`, the module's top-level scope over the
    /// builtin one, and return that scope. What the module imports is
    /// in the session's tables, under each definition's id: the
    /// resolver wrote on each name what it names.
    pub(super) fn check_program_in(&mut self, program: &mut Program, mut env: TypeEnv) -> TypeEnv {
        self.enter_own_types();

        // First pass: pre-register every type name with a placeholder
        // body. This makes recursive type references (e.g.
        // `type Expr { Add(Expr, Expr), ... }`) resolve during variant /
        // field type resolution — without this, the B3 unknown-type
        // check introduced in round 60 would reject the self-reference
        // because `self.tables.enums` / `self.tables.records` don't contain the name
        // until after the body is processed. The real registration loop
        // below overwrites the placeholders.
        for decl in &program.decls {
            if let Decl::Type(td) = decl {
                let ty = self.own_type(td.name);
                // A declaration `register_type_decl` rejects declares
                // nothing: what names it or its variants is not checked.
                if rejected_type_name(&resolve(td.name)) {
                    self.rejected_types.insert(ty);
                    continue;
                }
                match &td.body {
                    TypeBody::Enum(_) => {
                        let pkg = self.defining_package();
                        self.tables.enums.entry(ty).or_insert_with(|| EnumInfo {
                            variants: Vec::new(),
                            params: td.params.clone(),
                            param_var_ids: Vec::new(),
                            // Placeholder stamp: the real entry overwrites
                            // this in `register_type_decl` below. Stamp the
                            // current package now so a stray orphan check
                            // that races ahead of the real registration
                            // (e.g. a malformed program with an impl
                            // referencing a forward-declared enum) sees a
                            // sensible local-package value rather than the
                            // built-in sentinel.
                            defined_in: pkg,
                        });
                    }
                    TypeBody::Record(_) => {
                        let pkg = self.defining_package();
                        self.tables.records.entry(ty).or_insert_with(|| RecordInfo {
                            fields: Vec::new(),
                            defined_in: pkg,
                        });
                    }
                    TypeBody::Alias(_) => {
                        // Phase D: alias names are pre-registered into
                        // the local fast-path set so a forward reference
                        // (`type A = B; type B = ...`) doesn't trip the
                        // unknown-type guard in `resolve_type_expr_inner`
                        // when the second pass resolves the first
                        // alias's target. The arity is also recorded here
                        // (final value won't change between this pass and
                        // the real registration).
                        self.tables.type_aliases.insert(ty);
                        self.tables.type_alias_arity.insert(ty, td.params.len());
                    }
                }
            }
        }
        // First pass: register all type declarations
        for decl in &program.decls {
            if let Decl::Type(td) = decl {
                self.register_type_decl(td, &mut env);
            }
        }
        // A record's fields are read from the tables wherever a value of
        // the record is used: with every alias of the module declared,
        // an alias a field's type names (declared before or after the
        // record) is written out.
        for decl in &program.decls {
            if let Decl::Type(td) = decl
                && matches!(td.body, TypeBody::Record(_))
            {
                let ty = self.own_type(td.name);
                if let Some(info) = self.tables.records.get(&ty).cloned() {
                    let fields = info
                        .fields
                        .iter()
                        .map(|(n, t)| {
                            (
                                *n,
                                crate::types::canonical::canonicalize(&self.tables.resolver, t),
                            )
                        })
                        .collect();
                    if let Some(info) = self.tables.records.get_mut(&ty) {
                        info.fields = fields;
                    }
                }
            }
        }

        // The declarations and the bodies are checked one level deep:
        // what a signature leaves out and what a body leaves unknown are
        // variables of that level, generalised when the definitions that
        // share them are done.
        self.enter_level();

        // Second pass: the trait declarations, before any impl of them.
        for decl in &program.decls {
            if let Decl::Trait(t) = decl {
                self.register_trait_decl_user(t);
            }
        }
        // Each supertrait is given as many arguments as it has
        // parameters: checked where it is written, once every trait of
        // the module is declared.
        for decl in &program.decls {
            if let Decl::Trait(t) = decl {
                self.check_supertrait_arity(t);
            }
        }

        // 2b: Auto-derive Display/Compare/Equal/Hash for every user
        // enum and record that does not already have a manual impl.
        // Mutates `program.decls`. The synthesized TraitImpls flow
        // through `register_trait_impl` (step 2c below) and the
        // compiler's TraitImpl emit path identical to user-written
        // impls — producing real impl methods with global slots so
        // `Op::CallMethod`'s method lookup finds them at runtime, never
        // falling through to `dispatch_trait_method`.
        // Hand-written impls of the sealed traits are rejected first.
        self.reject_sealed_trait_impls(&mut program.decls);
        self.synthesize_auto_derive_impls(&mut program.decls);

        // 2c: the signatures of the functions and of the impls' methods.
        let mut sigs: Vec<Option<FnSig>> = Vec::with_capacity(program.decls.len());
        for decl in &program.decls {
            sigs.push(match decl {
                Decl::Fn(f) => Some(self.register_fn_decl(f, &mut env)),
                Decl::TraitImpl(ti) => {
                    self.register_trait_impl(ti);
                    None
                }
                _ => None,
            });
        }
        self.select_visible_methods();

        // Validate trait implementations against their declarations
        self.validate_trait_impls();

        for (i, decl) in program.decls.iter().enumerate() {
            if let Decl::Let { pattern, .. } = decl {
                for name in collect_pattern_vars(pattern) {
                    self.let_index.insert(name, i);
                }
            }
        }

        // Third pass: the functions and the top-level `let`s, in the
        // order they refer to each other, callees first. Each group that
        // refers to itself is inferred together and then generalised.
        for component in self.definition_order(&program.decls, &sigs) {
            self.check_component(&mut program.decls, &sigs, &component, &mut env);
        }

        // Fourth pass: the bodies of the impls' methods and of the
        // traits' default methods. Their signatures are complete, so
        // nothing above waited for them.
        self.check_decl_bodies(&mut program.decls, &mut env);
        self.exit_level();

        // Detect unresolved type variables on let-binding values where
        // the user did not provide a type annotation.
        self.check_unresolved_let_types(program);

        // After all passes, resolve any remaining type variables in annotations
        self.resolve_all_types(program);

        // Each impl that leaves a default method out gets the trait's
        // checked body, which the compiler compiles with the impl.
        self.share_default_methods(&mut program.decls);

        self.drop_repeated_errors();
        env
    }

    /// Check one group of top-level definitions that refer to each other
    /// (or one definition alone): bind each with the type its
    /// declaration gives it, check the bodies and the values against
    /// those, and generalise what is left unknown. A function with a
    /// complete signature is bound with its scheme already.
    fn check_component(
        &mut self,
        decls: &mut [Decl],
        sigs: &[Option<FnSig>],
        component: &order::Component,
        env: &mut TypeEnv,
    ) {
        // Inside the group each definition has one type.
        self.group_rigid.clear();
        if component.members.len() > 1 {
            for &i in &component.members {
                for r in sigs[i].iter().flat_map(|sig| &sig.rigid) {
                    self.group_rigid.insert(r.var, vec![i]);
                }
            }
        }
        let mut awaited: Vec<(Symbol, Type)> = Vec::new();
        for &i in &component.members {
            match (&decls[i], &sigs[i]) {
                (Decl::Fn(f), Some(sig)) if !sig.complete => {
                    env.define(f.name, Scheme::mono(sig.ty()));
                }
                // A `let` the group reaches before its value is checked.
                (Decl::Let { pattern, .. }, _) if component.cyclic => {
                    for name in collect_pattern_vars(pattern) {
                        let ty = self.fresh_var();
                        env.define(name, Scheme::mono(ty.clone()));
                        awaited.push((name, ty));
                    }
                }
                _ => {}
            }
        }

        // The `let`s of the group, with whether each is generalised.
        let mut lets: Vec<(Vec<Symbol>, bool, Span)> = Vec::new();
        for &i in &component.members {
            match (&mut decls[i], &sigs[i]) {
                // Parser-recovery stubs are skipped: their empty body is
                // not user code and must not produce diagnostics. A host
                // module's functions have no bodies.
                (Decl::Fn(f), Some(sig)) => {
                    if !f.is_recovery_stub && !self.signatures_only {
                        self.check_body(f, sig, env);
                    }
                }
                (
                    Decl::Let {
                        value,
                        pattern,
                        ty,
                        span,
                        ..
                    },
                    _,
                ) => {
                    let names = collect_pattern_vars(pattern);
                    self.checking_let = Some(i);
                    let is_value =
                        self.check_top_level_let(pattern, ty.as_ref(), value, *span, env);
                    self.checking_let = None;
                    for (name, awaited_ty) in &awaited {
                        if names.contains(name)
                            && let Some(bound) = env.lookup(*name).cloned()
                        {
                            self.unify(&bound.ty, awaited_ty, *span);
                        }
                    }
                    lets.push((names, is_value, *span));
                }
                _ => {}
            }
        }
        // What the bodies left for later: a variable they could not
        // decide may be decided now.
        self.solve_wanted(0);

        // Generalise. What a `let` that is not a value leaves unknown
        // stays unknown for everything that mentions it.
        self.exit_level();
        for (names, is_value, span) in &lets {
            for name in names {
                let Some(bound) = env.lookup(*name).cloned() else {
                    continue;
                };
                if *is_value {
                    continue;
                }
                self.keep_monomorphic(&bound.ty);
                // A function of the group gave it the type of one of
                // its annotation variables (see `TypeChecker::bind`).
                if let Some(r) = rigid_in(&self.apply(&bound.ty)) {
                    self.error(
                        Code::TypeMismatch,
                        format!(
                            "the type variable `{0}` would escape its declaration: \
                             `{name}` is defined outside it and cannot have a type that \
                             mentions `{0}`",
                            r.name
                        ),
                        *span,
                    );
                    env.define(*name, Scheme::mono(Type::Error));
                }
            }
        }
        // A definition's type may mention an annotation variable of
        // another function of the group: the bounds are the group's, each
        // on the variable its own stands for.
        let bounds: Vec<Pred> = component
            .members
            .iter()
            .filter_map(|&i| sigs[i].as_ref())
            .flat_map(|sig| sig.bounds.iter().cloned())
            .map(|pred| {
                let rep = |subject: Type| match subject {
                    Type::Var(var) => Type::Var(self.rigid_rep_var(var)),
                    other => other,
                };
                match pred {
                    Pred::Trait { tr, args, subject } => Pred::Trait {
                        tr,
                        args,
                        subject: rep(subject),
                    },
                    Pred::Anon { row, given } => Pred::Anon {
                        row: rep(row),
                        given: given.map(|var| self.rigid_rep_var(var)),
                    },
                }
            })
            .collect();
        for &i in &component.members {
            if let (Decl::Fn(f), Some(sig)) = (&decls[i], &sigs[i])
                && !sig.complete
            {
                let scheme = self.generalize_fn(&sig.ty(), &bounds);
                env.define(f.name, scheme);
            }
        }
        for (names, is_value, _) in &lets {
            if *is_value {
                for name in names {
                    // (A name the pattern failed to bind may be bound
                    // by something else, with a scheme of its own.)
                    if let Some(bound) = env.lookup(*name).cloned()
                        && bound.vars.is_empty()
                    {
                        let scheme = self.generalize_fn(&bound.ty, &bounds);
                        env.define(*name, scheme);
                    }
                }
            }
        }
        self.group_rigid.clear();
        self.settle_bounds();
        self.enter_level();
    }

    /// Check a top-level `let`: its value against its annotation, and
    /// bind its pattern's names, each with the type the value gives it.
    /// Returns whether the value is a syntactic value: the `let` is then
    /// generalised with the group it is checked in.
    fn check_top_level_let(
        &mut self,
        pattern: &mut Pattern,
        ty: Option<&TypeExpr>,
        value: &mut Expr,
        span: Span,
        env: &mut TypeEnv,
    ) -> bool {
        let is_value = self.is_syntactic_value(value);
        let mut val_ty = self.infer_expr(value, env);
        if let Some(te) = ty {
            // (A type variable the annotation introduces is rigid; a
            // `let` that generalises is general in it, like a function
            // in its signature's.)
            let (declared, _) = self.resolve_let_annotation(te);
            self.unify(&val_ty, &declared, span);
            // A value of unknown type (from a module that failed to
            // load) takes the declared type.
            if matches!(self.apply(&val_ty), Type::Error) {
                val_ty = declared;
            }
        }
        if let PatternKind::Ident(name) = &pattern.kind {
            env.define(*name, Scheme::mono(self.apply(&val_ty)));
        } else {
            // A top-level `let` has no failure branch either:
            // the pattern must be irrefutable.
            self.check_pattern(
                pattern,
                &val_ty,
                env,
                span,
                infer::pattern::PatternMode::Binding(infer::pattern::BindingSite::Let),
            );
        }
        is_value
    }

    /// Keep one of each diagnostic: the same message at the same span
    /// with the same severity is reported once. The passes can reach one
    /// node more than once (the derived impls of a type, which all carry
    /// the type declaration's span, are checked per method), and a
    /// repeated line tells the reader nothing new. Run once, after every
    /// pass, so no pass sees a shortened error list.
    fn drop_repeated_errors(&mut self) {
        let mut seen: std::collections::HashSet<(std::string::String, Span, bool)> =
            std::collections::HashSet::new();
        self.errors
            .retain(|e| seen.insert((e.message.clone(), e.span, e.severity == Severity::Warning)));
    }

    // ── Check declaration bodies ──────────────────────────────────────

    /// Type check the body of every method written in an impl of
    /// `decls`, and of every default method of a trait of `decls`,
    /// against `env`.
    ///
    /// An impl's method is checked against the signature its trait
    /// gives it (`register_trait_impl` keeps it in `impl_sigs`, by the
    /// canonical head of the impl's target: `List` for `Range` and for a
    /// user alias of `List(..)`, `Fn` for `Fun`, `Unit` for `()`).
    ///
    /// A default method is checked once, in its trait: `Self` is rigid
    /// there, bounded by the trait and its supertraits, so the body may
    /// use what those promise and nothing an impl's type happens to
    /// have.
    pub(super) fn check_decl_bodies(&mut self, decls: &mut [Decl], env: &mut TypeEnv) {
        // Each body one level deep: its variables are a declaration's,
        // not an outer value's, and what waits for one of them is dropped
        // with it.
        for decl in decls.iter_mut() {
            match decl {
                Decl::TraitImpl(ti) => {
                    let (Some(target), Some(trait_key)) =
                        (self.impl_target(ti), self.impl_trait(ti))
                    else {
                        continue;
                    };
                    for method in ti.methods.iter_mut() {
                        let Some(sig) = self.impl_sigs.remove(&(target, method.name, trait_key))
                        else {
                            continue;
                        };
                        self.check_method_body(method, &sig, env);
                    }
                }
                Decl::Trait(t) => {
                    for method in t.methods.iter_mut().filter(|m| !m.is_signature_only) {
                        let Some(sig) = self.default_method_sig(t.name, method.name) else {
                            continue;
                        };
                        self.check_method_body(method, &sig, env);
                    }
                }
                _ => {}
            }
        }
    }

    fn check_method_body(&mut self, method: &mut FnDecl, sig: &FnSig, env: &mut TypeEnv) {
        self.enter_level();
        self.check_body(method, sig, env);
        self.solve_wanted(0);
        self.exit_level();
        self.settle_bounds();
    }
}

// ── Helper functions ────────────────────────────────────────────────

/// Whether a type declaration of this name is rejected: `TypeOf`, the
/// type system's own, or a builtin scalar or container type's name.
fn rejected_type_name(name: &str) -> bool {
    name == "TypeOf" || crate::types::builtins::lookup(name).is_some()
}

/// The name of a builtin type; `None` for a type a module declares.
pub(super) fn builtin_type_name(ty: TypeRef) -> Option<&'static str> {
    crate::defs::builtin_types()
        .get(ty.id.0.0 as usize)
        .map(|(name, _)| *name)
}

/// The row variables of the anonymous record types in `ty`, which stand
/// for fields, not for a type.
fn row_tail_vars(ty: &Type, out: &mut Vec<TyVar>) {
    match ty {
        Type::AnonRecord { fields, tail } => {
            if let RowTail::Var(v) = tail {
                out.push(*v);
            }
            fields.values().for_each(|t| row_tail_vars(t, out));
        }
        Type::Generic(_, args) | Type::Tuple(args) => {
            args.iter().for_each(|t| row_tail_vars(t, out))
        }
        Type::Fun(params, ret) => {
            params.iter().for_each(|t| row_tail_vars(t, out));
            row_tail_vars(ret, out);
        }
        Type::List(t) | Type::Range(t) | Type::Set(t) | Type::Channel(t) => row_tail_vars(t, out),
        Type::Map(k, v) => {
            row_tail_vars(k, out);
            row_tail_vars(v, out);
        }
        Type::AssocProj { receiver, .. } => row_tail_vars(receiver, out),
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Var(_)
        | Type::Rigid(_)
        | Type::Error
        | Type::Never => {}
    }
}

/// The definitions the type names written in `te` resolve to.
fn written_defs(te: &TypeExpr, out: &mut std::collections::HashSet<crate::defs::DefId>) {
    if let Some(crate::defs::Res::Def(id)) = te.res {
        out.insert(id);
    }
    match &te.kind {
        TypeExprKind::Generic { args, .. } => args.iter().for_each(|a| written_defs(a, out)),
        TypeExprKind::Tuple(elems) => elems.iter().for_each(|e| written_defs(e, out)),
        TypeExprKind::Function(params, ret) => {
            params.iter().for_each(|p| written_defs(p, out));
            written_defs(ret, out);
        }
        TypeExprKind::AssocProj { receiver, .. } => written_defs(receiver, out),
        TypeExprKind::AnonRecord { fields, .. } => {
            fields.iter().for_each(|(_, t)| written_defs(t, out))
        }
        TypeExprKind::Named { .. } | TypeExprKind::SelfType => {}
    }
}

pub(crate) use crate::types::canonical::{canonical_head, head_of_canon as head_of};

/// What checking one module gives.
pub struct ModuleCheck {
    /// The module's errors and warnings.
    pub diagnostics: Vec<Diagnostic>,
    /// The inferred type of each top-level value the module binds by a
    /// declaration: its functions, its `let`s and the items it imports.
    pub top_level: HashMap<Symbol, Type>,
}

/// The context a module is checked in, from the session.
pub struct ModuleContext<'a> {
    /// The module checked.
    pub module: crate::session::ModuleId,
    /// Its name, for diagnostics.
    pub module_name: Symbol,
    /// A file, a REPL cell, or an embedder's host module, whose
    /// functions are bodiless signatures.
    pub kind: names::ModuleKind,
    /// The package the module belongs to, for the trait-orphan rule
    /// (round 63 item 5).
    pub package: Option<Symbol>,
    /// The module's top-level names, from the resolver.
    pub scope: &'a names::ModuleScope,
    /// For a REPL cell, the values it imports from the earlier cells,
    /// with their definitions. Empty for any other module.
    pub earlier: &'a [(Symbol, crate::defs::DefId)],
    /// The session's definitions; the module is resolved already.
    pub defs: std::sync::Arc<crate::defs::DefTable>,
    /// What the checks of the session share. What the module adds to
    /// them is recorded, see [`Tables::forget`].
    pub tables: &'a mut Tables,
}

/// Check one module, which the resolver has resolved, against the
/// session's tables: the module's types, traits and impls join them,
/// and the scheme of each definition it declares is entered once it is
/// checked.
pub fn check_module(program: &mut Program, context: ModuleContext<'_>) -> ModuleCheck {
    let ModuleContext {
        module,
        module_name,
        kind,
        package,
        scope,
        earlier,
        defs,
        tables,
    } = context;
    tables.forget(module);
    tables.vars.begin(module);
    let before = tables.keys();
    let (mut checker, mut env) = builtin_env().start();
    tables.module_names.insert(module, module_name);
    checker.tables = std::mem::take(tables);
    // A REPL cell that binds a name an earlier cell binds reads the
    // earlier value where its own is not bound yet (`let n = n + 1`):
    // what it imports from the earlier cells is in its scope by name.
    for (name, id) in earlier {
        let def = defs.get(*id);
        let scheme = match def.module.is_builtin() {
            true => builtin_scheme(def),
            false => checker.tables.schemes.get(id).cloned(),
        };
        if let Some(scheme) = scheme {
            env.define(*name, scheme);
        }
    }
    checker.signatures_only = kind == names::ModuleKind::Host;
    checker.is_cell = kind == names::ModuleKind::Cell;
    checker.current_package = package;
    checker.defs = Some(defs);
    checker.module = module;
    checker.module_name = module_name;
    for binding in scope.types.values() {
        if let names::Binding::Def(id) = binding {
            checker.seen_traits.insert(*id);
        }
    }
    for binding in scope.values.values() {
        if let names::Binding::Module(id) = binding {
            checker.seen_modules.insert(*id);
        }
    }
    if checker.is_cell {
        checker.take_up_waiting(program, earlier);
    }
    let env = checker.check_program_in(program, env);
    if checker.is_cell {
        checker.report_at_cell(program);
        checker.leave_waiting(program);
    }
    checker.settle_rows();
    checker.report_private_in_schemes(program, &env);
    checker.report_unknown_let_types(program, &env);
    checker.enter_schemes(&env);
    // The type of each top-level value: the module's own, by name; an
    // imported item, by its definition.
    let mut top_level = HashMap::new();
    for decl in &program.decls {
        let names: Vec<Symbol> = match decl {
            Decl::Fn(f) => vec![f.name],
            Decl::Let { .. } => crate::parser::top_level_binders(decl)
                .into_iter()
                .map(|(name, _, _)| name)
                .collect(),
            Decl::Import(ImportTarget::Items(_, items), _) => {
                items.iter().map(|(item, _)| *item).collect()
            }
            _ => Vec::new(),
        };
        for name in names {
            let scheme = match scope.values.get(&name) {
                Some(names::Binding::Def(id)) => {
                    checker.def_scheme(Some(crate::defs::Res::Def(*id)), &env)
                }
                _ => env.lookup(name).cloned(),
            };
            if let Some(scheme) = scheme {
                top_level.insert(name, checker.apply(&scheme.ty));
            }
        }
    }
    *tables = std::mem::take(&mut checker.tables);
    let rows = tables.added_since(&before);
    tables.rows.insert(module, rows);
    ModuleCheck {
        diagnostics: {
            // In source order: a check that waited for a type is
            // reported where it is, not when it was decided.
            // (Per file, the files in the order they first come up.)
            let mut files: Vec<crate::source::FileId> = Vec::new();
            for d in &checker.errors {
                if !files.contains(&d.span.file) {
                    files.push(d.span.file);
                }
            }
            checker
                .errors
                .sort_by_key(|d| (files.iter().position(|f| *f == d.span.file), d.span.start));
            checker.errors
        },
        top_level,
    }
}

// ── Tests ───────────────────────────────────────────────────────────

/// Shared test helpers used by every submodule test suite in
/// `typechecker/`. Before the round-N dedupe, four near-identical
/// copies of `assert_no_errors` / `assert_has_error` / `check_errors`
/// lived in `mod.rs`, `inference.rs`, `exhaustiveness.rs`,
/// `resolve.rs`, and `builtins.rs` (~128 lines of duplication).
///
/// why: we picked the most-general signatures across those copies.
///   - `assert_has_error(input, expected)` — shortest param name
///     used in 3 of 4 copies; mod.rs used `expected_substring` but
///     the body is byte-identical.
///   - `check_errors` inlines `parse()` + `check()` (the mod.rs
///     copy split them into two helpers; the split had no external
///     callers, so we collapsed it).
///   - Panic messages are preserved in the dominant form
///     ("expected no type errors" / "expected error containing").
#[cfg(test)]
pub(super) mod test_helpers;

#[cfg(test)]
mod size_locks;

#[cfg(test)]
mod tests;
