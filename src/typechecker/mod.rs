//! Hindley-Milner type inference and checking for Silt.
//!
//! This module implements Algorithm W-style type inference with:
//! - Type variables and unification
//! - Let-polymorphism (generalization at let bindings)
//! - Exhaustiveness checking for match expressions
//! - Type narrowing after `when` guard statements
//! - Trait constraint checking

mod auto_derive;
mod builtins;
mod env;
mod exhaustiveness;
mod inference;
pub mod names;
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
use env::TypeEnv;
pub use scheme::*;
use solve::PendingWhereConstraint;
use std::rc::Rc;
pub use tables::*;
pub use unify::*;

/// Snapshot of a user type's resolved body, used only by the auto-
/// derive synthesis pass (`synthesize_auto_derive_impls`). Captures the
/// EnumInfo/RecordInfo data in an owned form so the synthesis loop can
/// iterate without holding a borrow on `self.tables.enums` / `self.tables.records`.
enum TypeBodyKind {
    Enum(Vec<VariantInfo>),
    Record(Vec<(Symbol, Type)>),
}

/// Names of builtin traits that the compiler registers automatically
/// with auto-derived impls for every primitive and builtin container.
/// User code cannot redeclare a trait with any of these names — doing
/// so would shadow the compiler's TraitInfo (different method names,
/// different signatures) and produce nonsensical cascade errors when
/// the preregistered impls get revalidated against the user's body.
pub(super) const BUILTIN_TRAIT_NAMES: &[&str] = &["Equal", "Compare", "Hash", "Display", "Error"];

/// The built-in traits a program cannot implement by hand: they are
/// derived structurally (see `reject_sealed_trait_impls`).
pub(super) const SEALED_TRAIT_NAMES: &[&str] = &["Equal", "Compare", "Hash"];

/// Round 93: human adjective for the gated built-in traits, used by
/// the field-aware auto-derive gate's diagnostics ("... which is not
/// comparable").
fn builtin_trait_adjective(trait_sym: TraitKey) -> &'static str {
    match resolve(trait_sym.name).as_str() {
        "Compare" => "comparable",
        "Equal" => "equatable",
        "Hash" => "hashable",
        _ => "supported",
    }
}

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
    /// Active trait constraints for type variables in the current function body.
    /// Maps type variable → list of trait names it must satisfy.
    /// Populated during `check_fn_body` to enable method resolution on constrained vars.
    pub(super) active_constraints: HashMap<TyVar, Vec<TraitKey>>,
    /// Side channel holding trait arguments for parameterized-trait
    /// constraints, e.g. `where a: TryInto(Int)` stores `[Int]` under
    /// the key `(tyvar_of_a, TryInto)`. Populated during
    /// `register_fn_decl` and `register_trait_impl` alongside the
    /// parallel monomorphic `active_constraints`; consumed during
    /// descriptor method resolution to substitute trait params.
    /// Absent for bare `where a: Display` entries.
    pub(super) trait_arg_bindings: HashMap<(TyVar, TraitKey), Vec<Type>>,
    /// The expected return type of the enclosing function (if any).
    pub(super) current_return_type: Option<Type>,
    /// Maps function names to their body-constrained types (populated during check_fn_body).
    pub(super) fn_body_types: HashMap<Symbol, Type>,
    /// Deferred checks for field access on type variables (B4).
    /// Each entry is `(object_type, field_name, result_type, span)`.
    /// Re-examined after all function bodies are inferred: if the object type
    /// is still a Var, we emit an error.
    pub(super) pending_field_accesses: Vec<(Type, Symbol, Type, Span)>,
    /// Deferred checks for numeric operations on type variables (B5 / B2).
    /// Each entry is `(operand_type, op_description, span)`. Re-examined after
    /// all function bodies are inferred: if the operand is still a Var, we
    /// emit an error.
    pub(super) pending_numeric_checks: Vec<(Type, &'static str, Span)>,
    /// Deferred checks for `?` applied to a then-unresolved type variable
    /// (round 93). Each entry is `(inner_type, result_type,
    /// enclosing_return_type, span)`. Re-examined after all function bodies
    /// are inferred: if the inner type resolved to Result/Option, the
    /// enclosing fn/lambda return type is constrained exactly like the
    /// concrete inline path; if it resolved to anything else concrete, the
    /// usual "'?' requires Result or Option" diagnostic fires. Still-Var
    /// inners stay lenient (polymorphic templates — same rationale as
    /// `pending_numeric_checks`).
    pub(super) pending_question_marks: Vec<(Type, Type, Option<Type>, Span)>,
    /// Spans of `?` uses in the current fn/lambda body (round 93). When the
    /// body/return-type unify fails on a Result/Option return that `?`
    /// itself demanded, the diagnostic points back at the `?` site instead
    /// of leaving a bare header-located mismatch. Saved/restored around
    /// each fn body and lambda body, like `current_return_type`.
    pub(super) current_qmark_spans: Vec<Span>,
    /// Set by the exhaustiveness checker when its recursion depth bound is
    /// exceeded during a single `check_exhaustiveness` call. Interior
    /// mutability lets the `&self`-taking `is_useful` recursion record the
    /// event without threading a result type through every recursive call.
    /// Reset at the start of each `check_exhaustiveness` invocation.
    pub(super) exhaustiveness_depth_exceeded: std::cell::Cell<bool>,
    /// The span of the match (or the pattern) the usefulness search is
    /// working on: the patterns the search makes up take it.
    pub(super) exhaustiveness_span: std::cell::Cell<Span>,
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
    /// B4: deferred where-clause obligations seen at call sites where
    /// the type argument stayed an unresolved type variable.
    /// Finalize re-applies the substitution after all bodies are
    /// inferred: if the var resolved to a concrete type with a
    /// matching impl, the obligation is satisfied; if it resolved to
    /// a type variable still not covered by the enclosing fn's
    /// active constraints at the time of the call, a clean
    /// diagnostic is emitted.
    pub(super) pending_where_constraints: Vec<PendingWhereConstraint>,
    /// B4: the instantiated type-variable IDs of the enclosing function's
    /// parameters at the time `check_fn_body` is running. Used to decide
    /// whether a call-site where-constraint is touching the enclosing fn's
    /// own polymorphism (in which case the enclosing fn must declare the
    /// constraint) or a top-level unrelated Var (in which case we leave
    /// the obligation alone — the value will resolve via pass-3 narrowing).
    pub(super) current_fn_param_tyvars: Vec<TyVar>,
    /// Audit round 19: tracks trait constraints on type variables created
    /// by `instantiate_with_constraints`. When a scheme with where-clause
    /// constraints is instantiated, the fresh type variables inherit the
    /// constraints here. `generalize` then consults this map to propagate
    /// constraints into newly created schemes (e.g. `let f = constrained_fn`).
    pub(super) tyvar_trait_constraints: HashMap<TyVar, Vec<TraitKey>>,
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
    /// Round 64 item 6B (annotated polymorphic recursion): names of
    /// `fn` declarations whose signature is fully annotated (every
    /// parameter has an explicit type AND the return type is
    /// declared). Populated by `register_fn_decl`.
    ///
    /// The narrowing pass in `check_program` (which collapses a
    /// scheme's quantified vars after body inference observes them
    /// constrained) is SKIPPED for these fns. Locking the registered
    /// scheme as authoritative permits `instantiate_with_constraints`
    /// at the recursive call site to allocate fresh tyvars on every
    /// invocation — i.e. polymorphic recursion. Without the
    /// annotation, the same body that recurses with concrete
    /// non-polymorphic args would be undecidable to infer (Mycroft
    /// 1984), so silt keeps the existing monomorphic-recursion
    /// behaviour and emits a diagnostic note suggesting the user add
    /// annotations.
    pub(super) fully_annotated_fn_names: std::collections::HashSet<Symbol>,
    /// Round 64 item 6B: name of the function whose body is currently
    /// being type-checked. Set by `check_fn_body_with_name` before
    /// recursing into the body and cleared after. The Call arm
    /// consults this to detect a recursive call site and, if the fn
    /// is NOT in `fully_annotated_fn_names`, attach a helpful note
    /// to any type mismatch diagnostic at that call.
    pub(super) current_fn_name: Option<Symbol>,
    /// Round 64 item 6B: names of fns whose body inference observed a
    /// recursive call (callee == enclosing fn). Populated by the Call
    /// arm of `infer_expr` whenever `callee_fn_name == current_fn_name`.
    /// Used by the narrowing pass to decide whether to lock an
    /// annotated fn's scheme: only fns that actually recurse need
    /// the lock — every other annotated fn keeps the legacy
    /// narrowing behaviour so existing test invariants (e.g.
    /// `fn grab(b: Box) -> Int { b.value }` narrowing the bare `Box`
    /// param to `Int` and then surfacing a "type mismatch" at the
    /// caller) keep firing.
    pub(super) recursive_fn_names: std::collections::HashSet<Symbol>,
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
            active_constraints: HashMap::new(),
            trait_arg_bindings: HashMap::new(),
            current_return_type: None,
            fn_body_types: HashMap::new(),
            pending_field_accesses: Vec::new(),
            pending_numeric_checks: Vec::new(),
            pending_question_marks: Vec::new(),
            current_qmark_spans: Vec::new(),
            exhaustiveness_depth_exceeded: std::cell::Cell::new(false),
            exhaustiveness_span: std::cell::Cell::new(Span::BUILTIN),
            recovery_stub_names: std::collections::HashSet::new(),
            current_type_anno_span: None,
            pending_where_constraints: Vec::new(),
            current_fn_param_tyvars: Vec::new(),
            tyvar_trait_constraints: HashMap::new(),
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
            fully_annotated_fn_names: std::collections::HashSet::new(),
            current_fn_name: None,
            recursive_fn_names: std::collections::HashSet::new(),
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

    /// Report each `pub let` whose type the module's check leaves partly
    /// unknown (`pub let ch = channel.new(1)` before anything sends on
    /// it): an importer would fix the rest, and two importers could fix it
    /// two ways. The declaration is where it is reported, whichever
    /// module is checked first.
    fn report_unknown_pub_let_types(&mut self, program: &Program, env: &TypeEnv) {
        if self.is_cell {
            return;
        }
        for decl in &program.decls {
            if !matches!(decl, Decl::Let { is_pub: true, .. }) {
                continue;
            }
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
                            "the type of public let '{name}' is not fully known here: {}",
                            self.show_type(&ty)
                        ),
                    )
                    .with_help(format!(
                        "annotate it, e.g. `pub let {name}: <type> = ...`: a module that \
                         imports it cannot decide it"
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

        // Second pass: register trait declarations FIRST (so default
        // method bodies are recorded in TraitInfo) before synthesizing
        // missing defaults into trait impls. We split the original
        // single-pass loop into three sub-passes so the synthesis step
        // can mutate `program.decls` after every TraitInfo is known but
        // before any TraitImpl is registered into method_table.
        for decl in &program.decls {
            if let Decl::Trait(t) = decl {
                self.register_trait_decl_user(t);
            }
        }

        // 2b: Synthesize default-method bodies into impls that omitted
        // them. Mutates `program.decls`. After this pass, any impl that
        // "uses the default" looks identical (in the AST) to one that
        // re-typed the default body inline — so signature registration,
        // body checking, dispatch, and code generation all flow through
        // the existing machinery unmodified.
        self.synthesize_default_methods(&mut program.decls);

        // 2b.5: Auto-derive Display/Compare/Equal/Hash for every user
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

        // 2c: Register fn signatures and trait impls (now seeing
        // synthesized methods alongside explicit ones).
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => {
                    self.register_fn_decl(f, &mut env);
                }
                Decl::TraitImpl(ti) => {
                    self.register_trait_impl(ti, &mut env);
                }
                _ => {}
            }
        }
        self.select_visible_methods();

        // Process top-level let bindings (after functions are registered so
        // the value expression can call functions, and before function body
        // checking so functions can reference the constants).
        for i in 0..program.decls.len() {
            if let Decl::Let {
                ref mut value,
                ref pattern,
                ref ty,
                span,
                ..
            } = program.decls[i]
            {
                let is_value = inference::is_syntactic_value(&value.kind);
                let mut val_ty = self.infer_expr(value, &mut env);
                if let Some(te) = ty {
                    // B2: populate the arity-error span hint with the
                    // annotation's own span so diagnostics from
                    // `resolve_type_expr` point at the user-written type,
                    // not a zero-span sentinel. Without this, errors in
                    // `let x: Box(Int) = ...` where `Box` is parameterized
                    // emitted a span-less first error followed by a
                    // duplicate from the subsequent unify.
                    let prev_type_span = self.current_type_anno_span.replace(te.span);
                    let declared =
                        self.resolve_type_expr(te, &mut std::collections::HashMap::new());
                    self.current_type_anno_span = prev_type_span;
                    self.unify(&val_ty, &declared, span);
                    // A value of unknown type (from a module that failed to
                    // load) takes the declared type.
                    if matches!(self.apply(&val_ty), Type::Error) {
                        val_ty = declared;
                    }
                }
                let scheme = if is_value {
                    self.generalize(&env, &val_ty)
                } else {
                    Scheme::mono(self.apply(&val_ty))
                };
                if let PatternKind::Ident(name) = &pattern.kind {
                    env.define(*name, scheme);
                } else {
                    // A top-level `let` has no failure branch either:
                    // the pattern must be irrefutable.
                    self.bind_irrefutable_pattern(
                        pattern,
                        &val_ty,
                        &mut env,
                        span,
                        inference::BindingSite::Let,
                    );
                }
            }
        }

        // Validate trait implementations against their declarations
        self.validate_trait_impls();

        // Third pass: type check function bodies to discover constraints.
        // Recovery stubs (Option B) are skipped: their synthetic empty
        // body is not user code and must not produce "return type
        // mismatch", "unused binding", "unreachable", etc.
        let pre_pass3_error_count = self.errors.len();
        let pre_pass3_field_count = self.pending_field_accesses.len();
        let pre_pass3_numeric_count = self.pending_numeric_checks.len();
        let pre_pass3_qmark_count = self.pending_question_marks.len();
        self.check_decl_bodies(&mut program.decls, &env);

        // Narrow function schemes based on body constraints, then re-check.
        //
        // Invariant (audit-round-36 LATENT doc): when `finalize_deferred_checks`
        // runs below, `pending_field_accesses` / `pending_numeric_checks` /
        // `pending_question_marks` / `pending_where_constraints` must contain
        // EXACTLY the pushes from the
        // most recent body-check pass — not a mix of pass-2 + pass-3 entries.
        // Two paths preserve that:
        //   (1) `any_narrowed == false`: no re-check happens, so pass 3's
        //       pushes ARE the "most recent" pool and finalize consumes them
        //       as-is.
        //   (2) `any_narrowed == true`: the truncate/clear inside the branch
        //       rolls the pools back to their pre-pass-3 baseline before the
        //       re-check repopulates them, so finalize again sees only the
        //       most-recent pass's entries.
        // If a future edit adds a THIRD re-check path it MUST either set
        // `any_narrowed = true` (to go through the truncate branch) or add its
        // own equivalent reset/repopulate pairing, or this invariant breaks
        // and duplicate obligations leak into finalize.
        let body_types: HashMap<Symbol, Type> = std::mem::take(&mut self.fn_body_types);
        // Round 64 item 6B: collect annotated-fn signature mismatches
        // here so they survive the truncate-on-recheck step that runs
        // when SOME OTHER unannotated fn was narrowed in this batch.
        // We append them after the recheck so the user always sees
        // the "polymorphic signature but body pins to concrete type"
        // diagnostic.
        let mut annotated_signature_mismatches: Vec<(Symbol, Span)> = Vec::new();
        if !body_types.is_empty() {
            let mut any_narrowed = false;
            for (name, constrained_type) in &body_types {
                let new_scheme = self.generalize(&env, constrained_type);
                // Preserve where-clause constraints from the original scheme
                //
                // Round 73 B1 (BROKEN, soundness): the gate used to be a
                // bare `vars.len()` count comparison, which silently
                // missed row-poly narrowing where one tyvar (e.g. an
                // unannotated record param) gets pinned by body
                // inference to `AnonRecord{f: β, ...γ}` — a tyvar is
                // consumed AND a row-tail tyvar is introduced, leaving
                // the count equal. Without re-narrowing, deferred
                // field-access checks resolve against the body-pass
                // tyvar (open row) rather than the call-site type, and
                // bogus field accesses leak into runtime as crashes.
                // Now we also fire when the type tree narrowed
                // structurally (Var → concrete head at any position).
                if let Some(original_scheme) = env.lookup(*name).cloned()
                    && (original_scheme.vars.len() != new_scheme.vars.len()
                        || scheme_narrowed(&original_scheme.ty, &new_scheme.ty))
                {
                    // Round 64 item 6B (annotated polymorphic recursion):
                    // a fully-annotated fn's signature is authoritative.
                    // If the body's instantiation would narrow the
                    // scheme — i.e. an annotated polymorphic var was
                    // pinned to a concrete type by body usage (e.g.
                    // `fn f(x: a) -> Int { x + 1 }` pins `a` to Int via
                    // the `+` operator's unification) — that's a
                    // signature mismatch the user should fix. Record
                    // the violation now and emit the diagnostic after
                    // the recheck phase below (see the
                    // `annotated_signature_mismatches` drain). Leaving
                    // the scheme intact (a) surfaces the contradiction
                    // at the user's annotation site without silently
                    // monomorphising it, and (b) preserves the
                    // polymorphic shape so a same-body recursive call
                    // still instantiates afresh — which is the whole
                    // point of annotated poly-recursion.
                    // Lock the scheme only when the fn is BOTH fully
                    // annotated AND actually recursive. Annotated-but-
                    // non-recursive fns keep the legacy narrowing
                    // behaviour so test invariants like "bare-Box
                    // narrowing of `b: Box` to `Box(Int)` surfaces a
                    // type-mismatch at the caller" continue to hold.
                    if self.fully_annotated_fn_names.contains(name)
                        && self.recursive_fn_names.contains(name)
                    {
                        let fn_span = program
                            .decls
                            .iter()
                            .find_map(|d| match d {
                                Decl::Fn(fd) if fd.name == *name => Some(fd.span),
                                _ => None,
                            })
                            .unwrap_or(Span::BUILTIN);
                        annotated_signature_mismatches.push((*name, fn_span));
                        continue;
                    }
                    // Scheme was narrowed — some vars got constrained
                    any_narrowed = true;
                    let mut final_scheme = new_scheme.clone();
                    // BROKEN (round 17 F1): `original_scheme.constraints` uses
                    // the pass-2 tyvars, while `new_scheme.vars` uses fresh
                    // pass-3 tyvars from `instantiate_with_constraints` that
                    // flowed through body inference into `fn_body_types`.
                    // A direct `new_scheme.vars.contains(old_tv)` check never
                    // matches, so constraints were silently dropped and calls
                    // like `use_doublable("text")` slipped through typecheck
                    // and crashed at runtime with "no method doubled for
                    // String". Walk the two `Type` trees structurally in
                    // lockstep to build an old→new tyvar remap, then rewrite
                    // the original constraints through it. Narrowing can only
                    // tighten the scheme (never introduce new vars), so any
                    // original constraint whose old var is still free in the
                    // new scheme maps to a concrete new var.
                    let remap = align_tyvars(&original_scheme.ty, &new_scheme.ty);
                    for (old_tv, trait_name) in &original_scheme.constraints {
                        if let Some(&new_tv) = remap.get(old_tv)
                            && new_scheme.vars.contains(&new_tv)
                            && !final_scheme.constraints.contains(&(new_tv, *trait_name))
                        {
                            final_scheme.constraints.push((new_tv, *trait_name));
                        }
                    }
                    env.define(*name, final_scheme);
                }
            }

            if any_narrowed {
                // Discard pass 3 errors — they'll be re-emitted with better accuracy
                self.errors.truncate(pre_pass3_error_count);
                self.fn_body_types.clear();
                // Also truncate deferred checks back to the pre-pass-3
                // baseline (preserving any obligations recorded by the
                // top-level let inference earlier). They'll be re-collected
                // during the re-check with narrowed schemes.
                self.pending_field_accesses.truncate(pre_pass3_field_count);
                self.pending_numeric_checks
                    .truncate(pre_pass3_numeric_count);
                self.pending_question_marks.truncate(pre_pass3_qmark_count);
                // B4: discard the pending where-clause obligations so
                // the re-check with narrowed schemes re-collects them
                // from scratch. Otherwise stale entries pollute the
                // finalize pass with obligations that belong to
                // pre-narrowed instantiations.
                self.pending_where_constraints.clear();

                // Re-check the bodies with the narrowed schemes.
                self.check_decl_bodies(&mut program.decls, &env);
            }
        }

        // Round 64 item 6B: surface annotated-fn signature
        // mismatches recorded above. These are emitted post-narrowing
        // and post-recheck so the truncate-on-recheck step does not
        // erase them; they're the user-facing "your annotation is
        // inconsistent with the body" diagnostic.
        for (name, fn_span) in annotated_signature_mismatches {
            self.error(
                Code::TypeMismatch,
                format!(
                    "function '{}' has a polymorphic signature but its body uses \
                     a parameter as a concrete type; either add a `where` constraint \
                     (e.g. `where a: Display`) or replace the type variable with \
                     the concrete type the body actually requires",
                    resolve(name)
                ),
                fn_span,
            );
        }

        // Resolve any deferred checks (field-access / numeric ops on type
        // variables) before generating "unresolved type" errors.
        self.finalize_deferred_checks();

        // Fourth pass: detect unresolved type variables on let-binding values
        // where the user did not provide a type annotation.
        self.check_unresolved_let_types(program);

        // After all passes, resolve any remaining type variables in annotations
        self.resolve_all_types(program);

        self.drop_repeated_errors();
        env
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

    /// Type check the body of every function and of every trait-impl
    /// method in `decls` against `env`.
    ///
    /// This is the only body-checking loop. The first body pass of
    /// `check_program`, its re-check after scheme narrowing and the REPL
    /// all call it, so the three cannot disagree on which bodies are
    /// checked or on the key a method is looked up under.
    ///
    /// Parser-recovery stubs are skipped: their empty body is not user
    /// code and must not produce diagnostics.
    ///
    /// `register_trait_impl` registers a method under the canonical name
    /// of the impl's target type (`Range` and a user alias of `List(..)`
    /// collapse to `List`, `Fun` to `Fn`, `()` to `Unit`), both in
    /// `method_table` and as the `impl_method_key` binding in `env`.
    /// The lookup key is therefore built from the canonical name too; a
    /// key built from the name as written would miss the binding for an
    /// impl written against an alias, and the body would go unchecked.
    ///
    /// After a method body is checked, its body-constrained type replaces
    /// the template in `method_table`, so call sites see the concrete
    /// return type. The method's where-clause constraints are re-keyed
    /// from the template's type variables to those of the new type, and
    /// the same mapping is applied to the constraints' trait arguments,
    /// which may mention those variables.
    pub(super) fn check_decl_bodies(&mut self, decls: &mut [Decl], env: &TypeEnv) {
        for decl in decls.iter_mut() {
            if let Decl::Fn(f) = decl
                && !f.is_recovery_stub
                && !self.signatures_only
            {
                self.check_fn_body(f, env);
            }
        }
        for decl in decls.iter_mut() {
            let Decl::TraitImpl(ti) = decl else {
                continue;
            };
            let Some(target) = self.impl_target(ti) else {
                continue;
            };
            for method in ti.methods.iter_mut() {
                let method_name = method.name;
                let key = impl_method_key(target, method_name);
                let Some(ty) = self.check_fn_body_with_name(method, env, key) else {
                    continue;
                };
                let Some(entry) = self.tables.method_table.get_mut(&(target, method_name)) else {
                    continue;
                };
                if !entry.method_constraints.is_empty() {
                    let remap = align_tyvars(&entry.method_type, &ty);
                    let ty_remap: HashMap<TyVar, Type> = remap
                        .iter()
                        .map(|(old, new)| (*old, Type::Var(*new)))
                        .collect();
                    entry.method_constraints = entry
                        .method_constraints
                        .iter()
                        .filter_map(|(old_tv, trait_name, args)| {
                            remap.get(old_tv).map(|&new_tv| {
                                let new_args: Vec<Type> =
                                    args.iter().map(|t| substitute_vars(t, &ty_remap)).collect();
                                (new_tv, *trait_name, new_args)
                            })
                        })
                        .collect();
                }
                entry.method_type = ty;
            }
        }
    }

    // ── Validate trait implementations ────────────────────────────────

    fn validate_trait_impls(&mut self) {
        // (a) An unknown supertrait is reported where the trait is
        // registered (`register_trait_decl_inner`).

        // Validate using method_table + trait_impl_set (the new system).
        let impl_pairs: Vec<(TraitKey, TypeRef)> =
            self.tables.trait_impl_set.iter().cloned().collect();
        for (trait_name, type_name) in &impl_pairs {
            // GAP-2: Prefer the impl block's real span (stored at
            // registration time) over a method span. Fall back to the
            // method table only for auto-derived impls that have no
            // user-visible source site, and to `Span::BUILTIN` for the
            // impls silt declares itself.
            let diag_span = self
                .tables
                .trait_impl_spans
                .get(&(*trait_name, *type_name))
                .copied()
                .or_else(|| {
                    self.tables
                        .method_table
                        .iter()
                        .find(|((t, _), _)| t == type_name)
                        .map(|(_, e)| e.span)
                })
                .unwrap_or(Span::BUILTIN);

            // Check that the trait exists first.
            let Some(trait_info) = self.tables.traits.get(trait_name).cloned() else {
                self.error(
                    Code::UnknownTrait,
                    format!("trait '{trait_name}' is not declared"),
                    diag_span,
                );
                continue;
            };

            // Skip auto-derived impls (builtin traits on all types).
            let is_auto = trait_info
                .methods
                .first()
                .and_then(|(m, _)| self.tables.method_table.get(&(*type_name, *m)))
                .map(|e| e.is_auto_derived)
                .unwrap_or(false);
            if is_auto {
                continue;
            }

            // (b) Supertrait obligation: implementing a trait on a type
            // requires every supertrait to also be implemented for the
            // same type. Auto-derived builtins (Display/Equal/Hash/Compare)
            // do show up in `trait_impl_set`, so this also catches the
            // common case `trait Ordered: Equal { ... }` followed by
            // `trait Ordered for MyType { ... }` where MyType has not
            // overridden Equal — auto-derived counts as implementing.
            //
            // B1 (round 60): when the supertrait reference carries args
            // (`trait Holds(b): Carry(b)` + `impl Holds(Int) for Bag`),
            // resolve those args through the enclosing trait's
            // params→impl-args mapping and require the matching impl to
            // exist with positionally-compatible args. Otherwise
            // `impl Carry(String) for Bag` would silently satisfy the
            // obligation for `impl Holds(Int) for Bag`, causing a
            // runtime method-resolution failure.
            let enclosing_args: Vec<Type> = self
                .tables
                .impl_trait_args
                .get(&(*trait_name, *type_name))
                .cloned()
                .unwrap_or_default();
            for (i, supertrait) in trait_info.supertraits.iter().enumerate() {
                if !self
                    .tables
                    .trait_impl_set
                    .contains(&(*supertrait, *type_name))
                {
                    self.error(Code::MissingTraitImpl,
                        format!(
                            "type '{type_name}' implements '{trait_name}' but does not implement supertrait '{supertrait}'"
                        ),
                        diag_span,
                    );
                    continue;
                }
                // Resolve the supertrait's expected args against the
                // enclosing trait's impl args. Skip when there are no
                // supertrait args declared (bare `: Super` form).
                let arg_exprs = trait_info.supertrait_args.get(i);
                let expected_super_args: Vec<Type> = match arg_exprs {
                    Some(exprs) if !exprs.is_empty() => exprs
                        .iter()
                        .map(|te| {
                            crate::typechecker::inference::resolve_supertrait_arg(
                                te,
                                &trait_info,
                                &enclosing_args,
                            )
                        })
                        .collect(),
                    _ => continue,
                };
                let actual_super_args = self
                    .tables
                    .impl_trait_args
                    .get(&(*supertrait, *type_name))
                    .cloned()
                    .unwrap_or_default();
                let len_ok = actual_super_args.len() == expected_super_args.len();
                let pos_ok = len_ok
                    && expected_super_args
                        .iter()
                        .zip(actual_super_args.iter())
                        .all(|(e, a)| {
                            let e = self.apply(e);
                            let a = self.apply(a);
                            self.trait_arg_compatible(&e, &a)
                        });
                if !pos_ok {
                    let fmt_args = |args: &[Type]| -> String {
                        args.iter()
                            .map(|t| format!("{t}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    self.error(Code::InvalidTraitImpl,
                        format!(
                            "impl {}({}) for {} requires impl {}({}) for {}, but found impl {}({}) for {}",
                            resolve(trait_name.name),
                            fmt_args(&enclosing_args),
                            type_name,
                            resolve(supertrait.name),
                            fmt_args(&expected_super_args),
                            type_name,
                            resolve(supertrait.name),
                            fmt_args(&actual_super_args),
                            type_name,
                        ),
                        diag_span,
                    );
                }
            }

            // Check that all required methods are implemented with correct signature.
            // Round 76 BROKEN T2: substitute the impl's `trait_args`
            // (loaded above as `enclosing_args`) into the trait method
            // template *before* alpha-renaming the method-polymorphic
            // vars. Without this step the trait's PARAMETER tyvars
            // (e.g. `a` in `trait Foo(a) { fn produce(self) -> a }`)
            // get treated like method-polymorphic vars, which silently
            // lets `impl Foo(Int) for String { fn produce(self) ->
            // String }` typecheck — the param var is alpha-renamed to
            // a fresh var that unifies with `String`, erasing the
            // impl's promise that the produced value is an `Int`.
            // Pre-substitution pins the param var to its impl-specific
            // concrete arg so the only remaining free vars are the
            // method's own polymorphism.
            let trait_param_substitution: HashMap<TyVar, Type> =
                if !trait_info.param_var_ids.is_empty()
                    && enclosing_args.len() == trait_info.param_var_ids.len()
                {
                    trait_info
                        .param_var_ids
                        .iter()
                        .zip(enclosing_args.iter())
                        .map(|(&v, t)| (v, self.apply(t)))
                        .collect()
                } else {
                    HashMap::new()
                };
            for (method_name, trait_method_type) in &trait_info.methods {
                let key = (*type_name, *method_name);
                if let Some(entry) = self.tables.method_table.get(&key) {
                    let stored_impl_type = entry.method_type.clone();
                    let impl_span = entry.span;
                    // Instantiate BOTH the impl's stored template and the
                    // trait's declared type with fresh variables so that
                    // unification doesn't permanently bind either — the
                    // stored method_table entries are templates reused by
                    // every lookup site via `instantiate_method_type`.
                    let impl_type = self.instantiate_method_type(&stored_impl_type);
                    // Round 76 BROKEN T2: substitute trait_args first so
                    // only method-polymorphic vars are alpha-renamed.
                    let trait_method_with_args = if trait_param_substitution.is_empty() {
                        trait_method_type.clone()
                    } else {
                        substitute_vars(trait_method_type, &trait_param_substitution)
                    };
                    let fvs = free_vars_in(&trait_method_with_args);
                    let mapping: HashMap<TyVar, Type> =
                        fvs.into_iter().map(|v| (v, self.fresh_var())).collect();
                    let expected = substitute_vars(&trait_method_with_args, &mapping);
                    self.unify(&impl_type, &expected, impl_span);
                } else if !trait_info.default_method_bodies.contains_key(method_name) {
                    // No impl method AND the trait does not provide a
                    // default body — the impl is genuinely missing a
                    // required method. Methods with default bodies are
                    // synthesized into the impl by
                    // `synthesize_default_methods` before this validator
                    // runs the second time, so a missing-with-default
                    // entry here means synthesis hasn't happened yet
                    // (which is the normal pre-synthesis path) — silent
                    // is correct.
                    self.error(
                        Code::InvalidTraitImpl,
                        format!(
                            "trait impl '{}' for '{}' is missing method '{}'",
                            trait_name, type_name, method_name
                        ),
                        diag_span,
                    );
                }
            }
        }
    }

    // ── Register type declarations ──────────────────────────────────

    fn register_type_decl(&mut self, td: &TypeDecl, env: &mut TypeEnv) {
        // BROKEN #1: Reject redefinition of reserved type-system sentinel
        // names. `TypeOf` is used internally as the head of
        // `Type::builtin("TypeOf", [..])` to represent a type
        // descriptor (e.g. the runtime value produced by `Employee` when
        // used as a first-class type argument to `json.parse`). A user
        // declaring `type TypeOf(a) { Foo(a) }` would bind `Foo` as a
        // constructor returning a value structurally indistinguishable
        // from that internal descriptor, which silently typechecks and
        // then fails at runtime with "type argument must be a record
        // type". Reject at declaration time for a clear diagnostic. See
        // the sibling guard for builtin trait names in
        // `register_trait_decl` below.
        let td_name_str = resolve(td.name);
        if td_name_str == "TypeOf" {
            self.error(
                Code::InvalidTypeDeclaration,
                format!("'{td_name_str}' is a reserved type name used by the type system"),
                td.name_span,
            );
            return;
        }
        // Round 80 BROKEN B3: a type-decl whose name shadows a builtin
        // scalar/container type (Int, Float, Bool, String, Unit, List,
        // Range, Map, Set, Channel, Tuple, Fn, Fun, Handle, Bytes,
        // TcpListener, TcpStream — the authoritative list is
        // `BUILTIN_TYPES` in `src/types/builtins.rs`) silently overwrote
        // the builtin binding and then auto-derived Equal/Compare/Hash/
        // Display impls referencing fields that the *builtin* type does
        // not have, producing an unspanned cascade of "unknown method
        // 'x' on type Int" errors. Reject at the declaration site with
        // a single clear diagnostic. Mirrors the variant-shadow error
        // shape below.
        if crate::types::builtins::lookup(td_name_str.as_str()).is_some() {
            self.errors.push(
                Diagnostic::error(
                    Code::InvalidTypeDeclaration,
                    td.name_span,
                    format!("type '{td_name_str}' shadows builtin type '{td_name_str}'"),
                )
                .with_help("choose a different name"),
            );
            return;
        }
        let ty = self.own_type(td.name);
        // B2: populate the span hint used by `resolve_type_expr` for any
        // arity error on field / variant type annotations.
        let prev_type_span = self.current_type_anno_span.replace(td.span);
        // Create a mapping from type param names to placeholder type vars
        let mut param_vars: HashMap<Symbol, Type> = HashMap::new();
        for p in &td.params {
            let tv = self.fresh_var();
            param_vars.insert(*p, tv);
        }

        // Phase D: type aliases follow a parallel registration path
        // — resolve the target with each param bound to a fresh
        // TyVar, detect cycles, then register into the canonical
        // alias registry. Aliases never appear in `enums` /
        // `records`, never auto-derive Equal/Compare/Hash/Display
        // (the user must write impls against the target if they
        // want that), and emit no constructor bindings. Early-return
        // before the auto-derive block at the end of the function.
        if let TypeBody::Alias(target_te) = &td.body {
            self.register_type_alias(td, target_te, &mut param_vars);
            self.current_type_anno_span = prev_type_span;
            return;
        }

        match &td.body {
            TypeBody::Enum(variants) => {
                let mut variant_infos = Vec::new();
                let variant_defs = self.variant_resolutions(ty);

                // Compute the TyVar ids for each type parameter once,
                // before the variant loop (they are the same for every variant).
                let var_ids: Vec<TyVar> = td
                    .params
                    .iter()
                    .map(|p| match &param_vars[p] {
                        Type::Var(v) => *v,
                        _ => unreachable!(),
                    })
                    .collect();

                // G3: detect duplicate variant names within the same enum.
                // Previously `type Color { Red, Green, Red }` compiled
                // silently — the second `Red` overwrote the first's
                // constructor binding and no diagnostic was emitted.
                let mut seen_variants: std::collections::HashSet<Symbol> =
                    std::collections::HashSet::new();
                let mut first_variant: HashMap<Symbol, Span> = HashMap::new();
                for variant in variants {
                    if !seen_variants.insert(variant.name) {
                        let mut d = Diagnostic::error(
                            Code::DuplicateDeclaration,
                            variant.name_span,
                            format!("duplicate variant '{}' in enum '{}'", variant.name, td.name),
                        );
                        if let Some(&first) = first_variant.get(&variant.name) {
                            d = d.with_label(first, "first declared here");
                        }
                        self.errors.push(d);
                    } else {
                        first_variant.insert(variant.name, variant.name_span);
                    }
                }

                for variant in variants {
                    let field_types: Vec<Type> = variant
                        .fields
                        .iter()
                        .map(|te| self.resolve_type_expr(te, &mut param_vars))
                        .collect();

                    variant_infos.push(VariantInfo {
                        name: variant.name,
                        field_types: field_types.clone(),
                    });

                    // Register the constructor in the type environment
                    let type_params: Vec<Type> =
                        td.params.iter().map(|p| param_vars[p].clone()).collect();

                    let result_type = if type_params.is_empty() {
                        Type::Generic(ty, vec![])
                    } else {
                        Type::Generic(ty, type_params)
                    };

                    let scheme = Scheme {
                        vars: var_ids.clone(),
                        ty: if field_types.is_empty() {
                            // No-arg constructor is just a value
                            result_type
                        } else {
                            Type::Fun(field_types, Box::new(result_type))
                        },
                        constraints: vec![],
                        optional_last_param: false,
                    };
                    // The variant's definition's scheme, which a use of it
                    // reads: two enums may have variants of one name.
                    if let Some(crate::defs::Res::Def(id)) = variant_defs.get(&variant.name) {
                        self.tables.schemes.insert(*id, scheme);
                    }
                }

                // Register the enum type name as a value so it can be
                // passed to `type a` parameters (`json.parse(body, Color)`,
                // user-defined decoders, etc.). Mirrors the record path.
                // Skipped when a variant shares the enum's name
                // (e.g. `type Box(T) { Box(T) }`) because the variant
                // constructor is already bound under the same symbol.
                let variant_shares_name = variant_infos.iter().any(|v| v.name == td.name);
                if !variant_shares_name {
                    let enum_ty = if td.params.is_empty() {
                        Type::Generic(ty, vec![])
                    } else {
                        let args: Vec<Type> =
                            td.params.iter().map(|p| param_vars[p].clone()).collect();
                        Type::Generic(ty, args)
                    };
                    let scheme = Scheme {
                        vars: var_ids.clone(),
                        ty: Type::type_of(enum_ty),
                        constraints: vec![],
                        optional_last_param: false,
                    };
                    env.define(td.name, scheme);
                }

                self.tables.enums.insert(
                    ty,
                    EnumInfo {
                        params: td.params.clone(),
                        param_var_ids: var_ids,
                        variants: variant_infos,
                        defined_in: self.defining_package(),
                    },
                );
            }
            TypeBody::Record(fields) => {
                // G2: detect duplicate field names in the same record.
                // Previously `type R { a: Int, a: String }` compiled
                // silently and the first field's type was overwritten
                // by the second at the VM record layout level.
                let mut seen_fields: std::collections::HashSet<Symbol> =
                    std::collections::HashSet::new();
                let mut first_field: HashMap<Symbol, Span> = HashMap::new();
                for f in fields {
                    if !seen_fields.insert(f.name) {
                        let mut d = Diagnostic::error(
                            Code::DuplicateRecordField,
                            f.name_span,
                            format!("duplicate field '{}' in record type '{}'", f.name, td.name),
                        );
                        if let Some(&first) = first_field.get(&f.name) {
                            d = d.with_label(first, "first declared here");
                        }
                        self.errors.push(d);
                    } else {
                        first_field.insert(f.name, f.name_span);
                    }
                }
                let field_types: Vec<(Symbol, Type)> = fields
                    .iter()
                    .map(|f| {
                        let ty = self.resolve_type_expr(&f.ty, &mut param_vars);
                        (f.name, ty)
                    })
                    .collect();

                // Store param_var_ids for parameterized record types
                if !td.params.is_empty() {
                    let var_ids: Vec<TyVar> = td
                        .params
                        .iter()
                        .map(|p| match &param_vars[p] {
                            Type::Var(v) => *v,
                            _ => unreachable!(),
                        })
                        .collect();
                    self.tables.record_param_var_ids.insert(ty, var_ids);
                }

                self.tables.records.insert(
                    ty,
                    RecordInfo {
                        fields: field_types.clone(),
                        defined_in: self.defining_package(),
                    },
                );

                // Register the record type name as a value so it can be
                // passed to a `type a` parameter, e.g. `json.parse(body, Employee)`.
                // The value is a TYPE DESCRIPTOR at runtime (represented
                // as `Value::TypeDescriptor(name)`), so its type must
                // be `TypeOf(Employee)` rather than `Employee` itself —
                // otherwise the typechecker would let users write things
                // like `Employee.field` or use the descriptor as an
                // instance (T2 audit fix; mirrors primitive descriptors).
                //
                // For parameterized records (`type Box(a) { ... }`),
                // fresh type vars are generated for each param so
                // `json.parse(Box, ...)` can unify with a monomorphic
                // instance at the call site.
                let record_ty = Type::Record(ty, field_types);
                let scheme = if td.params.is_empty() {
                    Scheme {
                        vars: vec![],
                        ty: Type::type_of(record_ty),
                        constraints: vec![],
                        optional_last_param: false,
                    }
                } else {
                    // Re-use the param TyVars that parameterize the
                    // record's fields so the descriptor type is
                    // `forall a. TypeOf(Box(a))` — generalizing makes
                    // each call instantiate its own fresh vars.
                    let var_ids: Vec<TyVar> = td
                        .params
                        .iter()
                        .map(|p| match &param_vars[p] {
                            Type::Var(v) => *v,
                            _ => unreachable!(),
                        })
                        .collect();
                    let args: Vec<Type> = td.params.iter().map(|p| param_vars[p].clone()).collect();
                    let generic_record = Type::Generic(ty, args);
                    Scheme {
                        vars: var_ids,
                        ty: Type::type_of(generic_record),
                        constraints: vec![],
                        optional_last_param: false,
                    }
                };
                env.define(td.name, scheme);
            }
            TypeBody::Alias(_) => {
                // Phase D: handled by the early-return path above.
                // This arm is unreachable but kept for exhaustiveness.
                unreachable!("alias decls handled before this match");
            }
        }

        // Auto-derive builtin traits for user-defined types.
        //
        // Round 62 split: non-generic enums/records are now also
        // synthesized as real `TraitImpl` AST nodes by
        // `synthesize_auto_derive_impls`, which routes them through
        // `register_trait_impl` and the compiler emit path so
        // `Op::CallMethod` finds a real impl method at dispatch time. The typecheck-stamp here remains load-bearing
        // for two cases the synthesizer skips:
        //
        //   1. Generic types (`type Box(a) { Foo(a) }`,
        //      `type Pair(a, b) { x: a, y: b }`): synthesis would need
        //      `where a: Compare`-style impl-level clauses propagated
        //      through `target_type_args`; that work is mechanical but
        //      invasive and gated as a follow-up. Today the runtime
        //      `dispatch_trait_method` path handles these.
        //   2. Types whose fields don't all satisfy the trait (e.g. a
        //      record with a `Map` field has no Compare on Map, so
        //      Compare-synthesis would emit a body that fails
        //      typecheck).
        //
        // Round 93: the stamp below is PROVISIONAL for Equal / Compare
        // / Hash. `synthesize_auto_derive_impls` later runs a
        // recursive field-aware eligibility pass
        // (`compute_auto_derive_field_negatives`) and REMOVES the
        // stamp (plus the auto-derived `method_table` entry) for any
        // `(trait, type)` pair whose fields / variant payloads cannot
        // satisfy the trait.
        // Before round 93 the stamp stood unconditionally and `==` /
        // `<` / `.compare()` / `.hash()` on e.g. a record wrapping a
        // `Fn(..)` field laundered into nondeterministic Value-level
        // fallbacks (closure ordering = Arc pointer address). Display
        // remains unconditionally stamped: the runtime `display`
        // fallback (`display_value`) is total and deterministic for
        // every Value shape.
        //
        // For the synthesized cases, the second `trait_impl_set.insert`
        // inside `register_trait_impl` is a no-op (the key is already
        // present), and the duplicate-impl coherence check sees
        // `is_auto_derived: true` from the prior method_table entry
        // and allows the synthesized impl to overwrite it.
        let dummy_span = td.span;
        for trait_name in BUILTIN_AUTO_DERIVED_TRAIT_NAMES {
            self.tables
                .trait_impl_set
                .insert((TraitKey::builtin(trait_name), ty));
        }
        // Register auto-derived method entries
        let builtin_methods: &[(&str, Type)] = &[
            (
                "display",
                Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
            ),
            (
                "equal",
                Type::Fun(
                    vec![self.fresh_var(), self.fresh_var()],
                    Box::new(Type::Bool),
                ),
            ),
            (
                "compare",
                Type::Fun(
                    vec![self.fresh_var(), self.fresh_var()],
                    Box::new(Type::Int),
                ),
            ),
            (
                "hash",
                Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
            ),
        ];
        for (method_name, method_type) in builtin_methods {
            self.tables.method_table.insert(
                (ty, intern(method_name)),
                MethodEntry {
                    method_type: method_type.clone(),
                    span: dummy_span,
                    is_auto_derived: true,
                    trait_name: None,
                    method_constraints: Vec::new(),
                },
            );
        }
        self.current_type_anno_span = prev_type_span;
    }

    /// Phase D: register a `type Foo(...) = <target>` alias.
    ///
    /// Resolves `target_te` to a [`Type`] (with the alias's params bound
    /// to fresh `TyVar`s already populated in `param_vars`), detects
    /// cycles, then writes the entry into the typechecker's session-
    /// scoped [`crate::types::canonical::Resolver`] via
    /// [`crate::types::canonical::Resolver::register_alias`].
    ///
    /// Cycle detection traverses the resolved target looking for any
    /// reference back to the alias being declared (or to another alias
    /// that — transitively — references this one). Implementation: walk
    /// the target's free `Type::Generic` / `Type::Record` heads and ask
    /// the registry whether the head is an alias whose own target
    /// reaches `td.name`. The walk uses an explicit "in-progress" set
    /// keyed on alias name so a chain `A -> B -> A` produces the
    /// expected diagnostic at one of the two ends.
    fn register_type_alias(
        &mut self,
        td: &TypeDecl,
        target_te: &TypeExpr,
        param_vars: &mut HashMap<Symbol, Type>,
    ) {
        // The alias name itself must already be in `type_aliases` (the
        // pre-pass placeholder loop populated it). Mark it as in-
        // progress so any reference back to this alias inside its own
        // target — direct or indirect — is detected as a cycle.
        let alias = self.own_type(td.name);
        self.tables.type_aliases.insert(alias);
        self.tables.type_alias_arity.insert(alias, td.params.len());

        // Round 74 Fix #3: snapshot the declared parameter names BEFORE
        // resolving the target so we can detect undeclared free tyvars
        // — `resolve_type_expr_inner` lazily inserts a fresh `Type::Var`
        // into `param_vars` for any lowercase identifier it encounters,
        // including ones the user forgot to declare in `td.params`.
        // Without this guard, `type AnyList = List(a)` (no `(a)` after
        // `AnyList`) silently allocated one shared TyVar reused across
        // every use site, breaking polymorphism (each site would unify
        // with the FIRST use's element type and reject every other).
        let declared_params: std::collections::HashSet<Symbol> =
            td.params.iter().copied().collect();

        // Resolve the target with this alias's params bound. We use
        // `resolve_type_expr_inner` here (not the public canonicalising
        // wrapper): canonicalisation would eagerly expand any alias
        // already in the registry, collapsing a multi-step chain
        // (`A -> B -> A`) to a one-step self-reference and obscuring
        // the diagnostic. The inner resolver leaves alias references
        // as `Type::Generic(alias_name, args)` so `find_alias_cycle`
        // can walk the full chain by lookup_alias-driven recursion.
        // The outer registration step (`register_alias` below) then
        // stores the un-expanded target — canonicalisation expands at
        // every use site instead.
        let target_ty = self.resolve_type_expr_inner(target_te, param_vars);

        // Round 74 Fix #3: any name `param_vars` gained during
        // resolution that wasn't in `declared_params` is an undeclared
        // free tyvar in the alias target. Emit a clear diagnostic that
        // names the offending identifier and suggests adding it to the
        // alias header. Multiple undeclared names produce one
        // diagnostic per name (sorted for stable output).
        let mut undeclared: Vec<Symbol> = param_vars
            .keys()
            .copied()
            .filter(|name| !declared_params.contains(name))
            .collect();
        undeclared.sort_by_key(|s| resolve(*s));
        for name in undeclared {
            let name_str = resolve(name);
            self.error(Code::InvalidTypeDeclaration,
                format!(
                    "undeclared type parameter '{name_str}' in alias target — did you mean `type {}({name_str}) = ...`?",
                    resolve(td.name)
                ),
                target_te.span,
            );
        }

        // Detect cycles before registering. Build a chain that names
        // every alias visited; if `td.name` appears, report it.
        let mut visiting: Vec<TypeRef> = vec![alias];
        if let Some(cycle) = self.find_alias_cycle(&target_ty, &mut visiting) {
            // Format the cycle as `A -> B -> A` for clarity. `cycle` is
            // the Vec of names from the original `td.name` through
            // each alias in the chain that closes the loop.
            let chain: Vec<String> = cycle.iter().map(|t| resolve(t.name)).collect();
            self.error(
                Code::InvalidTypeDeclaration,
                format!(
                    "type alias '{}' forms a cycle: {}",
                    td.name,
                    chain.join(" -> ")
                ),
                target_te.span,
            );
            // Round 79 LATENT TS-L1: when a cycle is detected closing
            // on the alias under registration (`td.name`), every other
            // alias in the cycle chain was registered earlier with a
            // target that pointed back through the now-known-cyclic
            // path. With `type A = B; type B = A`, A registered first
            // (B not yet visible) with target Generic("B"); B then
            // detects the cycle but A is still in the alias map
            // pretending A→Generic("B") is valid, so a later
            // `let v: A = 42` reports "expected B, got Int" instead of
            // a coherent cycle diagnostic. Walk the chain (excluding
            // td.name itself, which never registered) and unregister
            // each entry so use-site canonicalisation no longer sees
            // a half-built alias path.
            for &cycle_name in &cycle {
                if cycle_name != alias {
                    self.tables.resolver.unregister_alias(cycle_name);
                    self.tables.type_aliases.remove(&cycle_name);
                    self.tables.type_alias_arity.remove(&cycle_name);
                }
            }
            // Also drop this alias's placeholder entries — registration
            // is being skipped, and leaving the name in `type_aliases`
            // (the placeholder set) lets later passes treat A as a
            // valid alias that just happens to have no resolver entry.
            self.tables.type_aliases.remove(&alias);
            self.tables.type_alias_arity.remove(&alias);
            // Skip registration so the canonicaliser doesn't loop on a
            // self-referential expansion at any later use site.
            return;
        }

        // Collect the alias-param TyVar ids in source order.
        let param_var_ids: Vec<TyVar> = td
            .params
            .iter()
            .map(|p| match param_vars.get(p) {
                Some(Type::Var(v)) => *v,
                _ => {
                    // Unreachable: register_type_decl seeded every
                    // td.params entry into `param_vars` as a fresh
                    // Type::Var before reaching this branch.
                    unreachable!("alias param missing from param_vars")
                }
            })
            .collect();

        self.tables.resolver.register_alias(
            alias,
            crate::types::canonical::AliasInfo {
                params: td.params.clone(),
                param_var_ids,
                target: target_ty,
            },
        );
    }

    /// Walk a resolved type and return the visited-alias chain that
    /// closes a cycle, or `None` if no cycle is reachable. The
    /// `visiting` Vec carries the alias names seen so far on this
    /// walk; the head is the alias being declared.
    fn find_alias_cycle(&self, ty: &Type, visiting: &mut Vec<TypeRef>) -> Option<Vec<TypeRef>> {
        match ty {
            Type::Generic(name, args) => {
                if visiting.contains(name) {
                    // Direct or indirect self-reference. Append the
                    // closing name so the diagnostic shows
                    // `A -> B -> A`.
                    let mut chain = visiting.clone();
                    chain.push(*name);
                    return Some(chain);
                }
                if let Some(info) = self.tables.resolver.lookup_alias(*name) {
                    visiting.push(*name);
                    let result = self.find_alias_cycle(&info.target, visiting);
                    visiting.pop();
                    if result.is_some() {
                        return result;
                    }
                }
                for a in args {
                    if let Some(c) = self.find_alias_cycle(a, visiting) {
                        return Some(c);
                    }
                }
                None
            }
            Type::Record(_name, fields) => {
                // Dead-arm cleanup (round 88 item D3): a prior version
                // also tested `visiting.contains(name)` here, but
                // `visiting` is seeded only with alias-decl names and
                // the parser rejects a top-level name bound twice — so
                // an alias name can never collide with a
                // record name. The guard was unreachable. Field
                // recursion below remains: it descends into nested
                // alias references inside record fields, which IS how
                // a real alias cycle can route through a record.
                for (_, t) in fields {
                    if let Some(c) = self.find_alias_cycle(t, visiting) {
                        return Some(c);
                    }
                }
                None
            }
            Type::List(inner) | Type::Range(inner) | Type::Set(inner) | Type::Channel(inner) => {
                self.find_alias_cycle(inner, visiting)
            }
            Type::Map(k, v) => self
                .find_alias_cycle(k, visiting)
                .or_else(|| self.find_alias_cycle(v, visiting)),
            Type::Tuple(elems) => {
                for e in elems {
                    if let Some(c) = self.find_alias_cycle(e, visiting) {
                        return Some(c);
                    }
                }
                None
            }
            Type::Fun(params, ret) => {
                for p in params {
                    if let Some(c) = self.find_alias_cycle(p, visiting) {
                        return Some(c);
                    }
                }
                self.find_alias_cycle(ret, visiting)
            }
            Type::AssocProj { receiver, .. } => self.find_alias_cycle(receiver, visiting),
            Type::AnonRecord { fields, .. } => {
                for t in fields.values() {
                    if let Some(c) = self.find_alias_cycle(t, visiting) {
                        return Some(c);
                    }
                }
                None
            }
            Type::Int
            | Type::Float
            | Type::Bool
            | Type::String
            | Type::Unit
            | Type::Var(_)
            | Type::Error
            | Type::Never => None,
        }
    }

    // ── Register function declarations ──────────────────────────────

    fn register_fn_decl(&mut self, f: &FnDecl, env: &mut TypeEnv) {
        // Recovery-stub special case (Option B): record the name and bind
        // its signature just like a real fn, so downstream references in
        // unrelated code do not cascade into "undefined variable" errors.
        if f.is_recovery_stub {
            self.recovery_stub_names.insert(f.name);
        }
        let mut param_map = HashMap::new();
        let mut param_types = Vec::new();

        // B2: populate the span hint used by `resolve_type_expr` for any
        // arity error on parameter / return type annotations.
        let prev_type_span = self.current_type_anno_span.replace(f.span);
        for param in &f.params {
            let ty = match param.kind {
                ParamKind::Type => {
                    // `type a` parameter — seed `a` as a fresh type variable
                    // in `param_map` so it is in scope for the rest of the
                    // signature and any where clauses. The parameter's own
                    // compile-time type is the runtime type descriptor
                    // `TypeOf(a)`, which unifies with record / primitive
                    // descriptors at call sites.
                    let name = match &param.pattern.kind {
                        PatternKind::Ident(n) => *n,
                        _ => unreachable!("parser guarantees `type` params use an Ident pattern"),
                    };
                    let var = param_map
                        .entry(name)
                        .or_insert_with(|| self.fresh_var())
                        .clone();
                    Type::type_of(var)
                }
                ParamKind::Data => {
                    if let Some(te) = &param.ty {
                        self.resolve_type_expr(te, &mut param_map)
                    } else {
                        self.fresh_var()
                    }
                }
            };
            param_types.push(ty);
        }

        // Binding rule: every lowercase type variable that appears in the
        // user-written return annotation must also be introduced by some
        // parameter (regular annotation or `type a`). Detected by
        // snapshotting `param_map` before resolving the return annotation —
        // any new key added afterwards is a variable that only appears in
        // the return, with no anchor.
        let pre_return_keys: std::collections::HashSet<Symbol> =
            param_map.keys().copied().collect();
        let ret_type = if let Some(te) = &f.return_type {
            let resolved = self.resolve_type_expr(te, &mut param_map);
            for name in param_map.keys() {
                if !pre_return_keys.contains(name) {
                    let n = resolve(*name);
                    self.error(Code::InvalidTypeAnnotation,
                        format!(
                            "type variable '{}' in return type is not introduced by any parameter; \
                             add a `type {}` parameter or anchor it on an existing parameter's type",
                            n, n
                        ),
                        f.span,
                    );
                }
            }
            resolved
        } else {
            self.fresh_var()
        };
        self.current_type_anno_span = prev_type_span;

        let fn_type = Type::Fun(param_types.clone(), Box::new(ret_type));
        let mut scheme = self.generalize(env, &fn_type);
        // Round 64 item 6B (annotated polymorphic recursion): record
        // whether the user's signature is fully annotated. A `Data`
        // parameter is annotated iff it carries an explicit `ty`;
        // `Type` parameters are annotated by construction (the binder
        // itself is the annotation). The return type is annotated iff
        // `return_type` is Some. When both hold for every parameter
        // and the return, the narrowing pass in `check_program` will
        // skip this fn — keeping its scheme polymorphic across all
        // recursive call sites in its own body.
        if !f.is_recovery_stub {
            let all_params_annotated = f
                .params
                .iter()
                .all(|p| matches!(p.kind, ParamKind::Type) || p.ty.is_some());
            if all_params_annotated && f.return_type.is_some() {
                self.fully_annotated_fn_names.insert(f.name);
            }
        }

        // Resolve where clauses to (TyVar, trait_name) using param_map.
        // Type variables must be introduced via explicit type annotations in the signature.
        // Trait args (for parameterized traits like `a: TryInto(b)`) are
        // resolved through `param_map` and stashed in `trait_arg_bindings`
        // so descriptor method resolution can substitute them later.
        for wc in &f.where_clauses {
            let type_param = &wc.type_param;
            let trait_name = &wc.trait_name;
            let trait_args = &wc.trait_args;
            if let Some(ty) = param_map.get(type_param) {
                let resolved = self.apply(ty);
                // An unknown trait is reported when the body is checked.
                if let Type::Var(tv) = resolved
                    && let Some(trait_name) = self.named_trait(wc.trait_res, *trait_name)
                {
                    scheme.constraints.push((tv, trait_name));
                    if !trait_args.is_empty() {
                        let resolved_args: Vec<Type> = trait_args
                            .iter()
                            .map(|te| self.resolve_type_expr(te, &mut param_map))
                            .collect();
                        self.trait_arg_bindings
                            .insert((tv, trait_name), resolved_args);
                    }
                }
            } else {
                let first_param_name = f
                    .params
                    .first()
                    .map(|p| match &p.pattern.kind {
                        PatternKind::Ident(n) => resolve(*n),
                        _ => "_".to_string(),
                    })
                    .unwrap_or_else(|| "_".to_string());
                self.error(Code::InvalidTypeAnnotation,
                    format!(
                        "type variable '{}' in where clause is not introduced in the function signature; \
                         use an explicit type annotation, e.g.: fn {}({}: {}) where {}: {}",
                        type_param, f.name,
                        first_param_name,
                        type_param, type_param, trait_name
                    ),
                    f.span,
                );
            }
        }

        env.define(f.name, scheme);
    }

    // ── Register trait declarations ─────────────────────────────────

    /// User-source entry point for trait declaration registration.
    /// Round 62 (item 3 of type-design improvements): trait registration
    /// was previously two paths — this one for user `trait X { ... }`
    /// declarations, and a hand-rolled `register_builtin_trait_decl` for
    /// Display/Compare/Equal/Hash plus a one-off block for `Error`. The
    /// two drifted: the built-in path didn't run the duplicate-method
    /// check, didn't honor where_clauses or supertraits in general, and
    /// any future feature added here had to be manually mirrored into
    /// the built-in path.
    ///
    /// The unification: built-in traits now synthesize `TraitDecl` AST
    /// nodes in `builtin_trait_decls()` and feed them through
    /// `register_trait_decl_inner` directly (skipping only the
    /// redefinition guard, which is keyed off user input). Future
    /// trait-decl features automatically apply to built-ins.
    fn register_trait_decl_user(&mut self, t: &TraitDecl) {
        // Reject redefinition of builtin trait names. The compiler has
        // already preregistered TraitInfo + auto-derived impls for these
        // names; letting a user `trait Equal { fn eq(self) -> Bool }`
        // overwrite them would produce a cascade of bogus
        // "missing method" errors when validate_trait_impls runs the
        // preregistered impls against the user's new body.
        let trait_name_str = resolve(t.name);
        if BUILTIN_TRAIT_NAMES.contains(&trait_name_str.as_str()) {
            self.error(
                Code::InvalidTraitDeclaration,
                format!("trait '{trait_name_str}' is a builtin trait and cannot be redefined"),
                t.name_span,
            );
            return;
        }
        self.register_trait_decl_inner(t);
    }

    /// Shared trait-registration body. Runs for both user-source decls
    /// (after the redefinition guard in `register_trait_decl_user`) and
    /// built-in synthetic decls (via `builtin_trait_decls`).
    pub(super) fn register_trait_decl_inner(&mut self, t: &TraitDecl) {
        let key = self.own_trait(t.name);
        // The supertraits and the associated types' bounds. A trait name
        // that names no trait is reported here, unless the resolver
        // reported it.
        let mut supertraits: Vec<(TraitKey, Vec<TypeExpr>)> = Vec::new();
        for r in &t.supertraits {
            match self.named_trait(r.res, r.name) {
                Some(sup) => supertraits.push((sup, r.args.clone())),
                None if r.res == Some(crate::defs::Res::Error) => {}
                None => self.error(
                    Code::UnknownTrait,
                    format!("trait '{}' lists unknown supertrait '{}'", t.name, r.name),
                    t.span,
                ),
            }
        }
        let assoc_types: Vec<AssocTypeInfo> = t
            .assoc_types
            .iter()
            .map(|a| AssocTypeInfo {
                name: a.name,
                bounds: a
                    .bounds
                    .iter()
                    .filter_map(|b| Some((self.named_trait(b.res, b.name)?, b.args.clone())))
                    .collect(),
                span: a.span,
            })
            .collect();
        for a in &t.assoc_types {
            for b in &a.bounds {
                if b.res != Some(crate::defs::Res::Error)
                    && self.named_trait(b.res, b.name).is_none()
                {
                    self.error(
                        Code::UnknownTrait,
                        format!(
                            "unknown trait '{}' in bound on associated type '{}::{}'",
                            b.name, t.name, a.name
                        ),
                        a.span,
                    );
                }
            }
        }
        // GAP (round 35 F6): duplicate method names in a trait
        // declaration used to silently overwrite each other in the
        // trait's `methods` Vec (first entry won for method lookup but
        // the second's signature won for any HashMap-based bookkeeping
        // like `default_method_bodies`). Emit a diagnostic per dup.
        {
            let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
            for m in &t.methods {
                if !seen.insert(m.name) {
                    self.error(
                        Code::DuplicateDeclaration,
                        format!("duplicate method '{}' in trait '{}'", m.name, t.name),
                        m.span,
                    );
                }
            }
        }

        // Pre-register a stub TraitInfo for `t` carrying just its
        // supertraits and assoc_types. This lets `resolve_type_expr`
        // resolve `Self::AssocName` projections inside method
        // signatures by walking the supertrait chain — e.g. a `Sub`
        // method declaring `Self::Item` (where `Item` lives on
        // `Super`) needs `self.tables.traits[Sub].supertraits = [Super]` to
        // be visible during method-type resolution. The stub is
        // overwritten with the fully-populated TraitInfo at the end
        // of this function.
        let pre_assoc_types = assoc_types.clone();
        let private_to = (!t.is_pub).then_some((self.module, self.module_name));
        let pre_supertraits: Vec<TraitKey> = supertraits.iter().map(|(s, _)| *s).collect();
        let pkg = self.defining_package();
        self.tables.traits.insert(
            key,
            TraitInfo {
                params: t.params.clone(),
                param_var_ids: Vec::new(),
                supertraits: pre_supertraits,
                supertrait_args: Vec::new(),
                param_where_clauses: Vec::new(),
                methods: Vec::new(),
                default_method_bodies: HashMap::new(),
                assoc_types: pre_assoc_types,
                private_to,
                defined_in: pkg,
            },
        );

        let self_var = self.fresh_var();
        // Allocate a fresh TyVar for each trait-level parameter. These
        // are in scope across every method signature — writing
        // `trait TryInto(b) { fn try_into(self) -> Result(b, Error) }`
        // makes `b` resolve to the same TyVar in the method.
        let trait_param_vars: Vec<(Symbol, Type)> =
            t.params.iter().map(|p| (*p, self.fresh_var())).collect();
        let param_var_ids: Vec<TyVar> = trait_param_vars
            .iter()
            .map(|(_, ty)| match ty {
                Type::Var(v) => *v,
                _ => unreachable!("fresh_var always returns Type::Var"),
            })
            .collect();
        let methods: Vec<(Symbol, Type)> = t
            .methods
            .iter()
            .map(|m| {
                let mut param_map = HashMap::new();
                param_map.insert(intern("Self"), self_var.clone());
                for (name, ty) in &trait_param_vars {
                    param_map.insert(*name, ty.clone());
                }
                let mut param_types = Vec::new();
                for param in &m.params {
                    let ty = match param.kind {
                        ParamKind::Type => {
                            let name = match &param.pattern.kind {
                                PatternKind::Ident(n) => *n,
                                _ => unreachable!(
                                    "parser guarantees `type` params use an Ident pattern"
                                ),
                            };
                            let var = param_map
                                .entry(name)
                                .or_insert_with(|| self.fresh_var())
                                .clone();
                            Type::type_of(var)
                        }
                        ParamKind::Data => {
                            if let Some(te) = &param.ty {
                                self.resolve_type_expr(te, &mut param_map)
                            } else if matches!(&param.pattern.kind,
                                PatternKind::Ident(n) if *n == intern("self"))
                            {
                                // Bare `self` parameter shares the trait's
                                // self_var so any AssocProj on the return
                                // type (which references the same Self
                                // tyvar) reduces correctly when the impl
                                // unifies its concrete self-type into the
                                // param. Without this, the param was a
                                // separate fresh var, leaving the AssocProj's
                                // receiver permanently abstract.
                                self_var.clone()
                            } else {
                                self.fresh_var()
                            }
                        }
                    };
                    param_types.push(ty);
                }
                let ret_type = if let Some(te) = &m.return_type {
                    self.resolve_type_expr(te, &mut param_map)
                } else {
                    self.fresh_var()
                };
                (m.name, Type::Fun(param_types, Box::new(ret_type)))
            })
            .collect();

        // Collect default-bodied methods. Methods whose `is_signature_only`
        // flag is false carry a real (non-placeholder) body and are eligible
        // to be cloned into impls that omit them.
        let default_method_bodies: HashMap<Symbol, FnDecl> = t
            .methods
            .iter()
            .filter(|m| !m.is_signature_only)
            .map(|m| (m.name, (*m).clone()))
            .collect();

        // Trait-level where bounds on params. Only the (param_name,
        // trait_name) shape is kept; trait_args on the bound are not
        // yet honored (reserved for a future extension where bounds
        // can themselves reference other trait args).
        let param_where_clauses: Vec<(Symbol, TraitKey)> = t
            .param_where_clauses
            .iter()
            .filter_map(|wc| {
                Some((
                    wc.type_param,
                    self.named_trait(wc.trait_res, wc.trait_name)?,
                ))
            })
            .collect();

        let supertrait_names: Vec<TraitKey> = supertraits.iter().map(|(s, _)| *s).collect();
        let supertrait_args: Vec<Vec<TypeExpr>> =
            supertraits.into_iter().map(|(_, args)| args).collect();

        // Reject duplicate assoc-type names within the same trait.
        // Mirrors the duplicate-method check above.
        let mut seen_assoc: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
        for a in &t.assoc_types {
            if !seen_assoc.insert(a.name) {
                self.error(
                    Code::DuplicateDeclaration,
                    format!(
                        "duplicate associated type '{}' in trait '{}'",
                        resolve(a.name),
                        resolve(t.name)
                    ),
                    a.span,
                );
            }
        }
        self.tables.traits.insert(
            key,
            TraitInfo {
                params: t.params.clone(),
                param_var_ids,
                supertraits: supertrait_names,
                supertrait_args,
                param_where_clauses,
                methods,
                default_method_bodies,
                assoc_types,
                private_to,
                defined_in: pkg,
            },
        );
    }

    // ── Register trait implementations ──────────────────────────────

    /// Convert a type name Symbol to a Type.
    ///
    /// Round 74 Fix #1: include the `Unit`/`()` arm so a user-declared
    /// `trait T for Unit { ... }` impl receives a `Type::Unit` self_type
    /// rather than the `Type::Generic("Unit", [])` fallback, which never
    /// unifies with the canonical `Type::Unit` receiver.
    fn type_from_name(ty: TypeRef) -> Type {
        match builtin_type_name(ty) {
            Some("Int") => Type::Int,
            Some("Float") => Type::Float,
            Some("Bool") => Type::Bool,
            Some("String") => Type::String,
            Some("Unit") => Type::Unit,
            _ => Type::Generic(ty, vec![]),
        }
    }

    /// For every `Decl::TraitImpl` in `decls`, find missing methods that
    /// the trait provides default bodies for and clone the default
    /// FnDecls into the impl's `methods` vec. Runs between trait-decl
    /// registration and trait-impl registration so the synthesized
    /// methods participate in the normal method_table population /
    /// body-check / compile pipeline as if the user had written them
    /// inline.
    ///
    /// We intentionally mutate the AST (rather than carrying defaults
    /// out-of-band) because every downstream consumer — register_trait_impl,
    /// the pass-3 body checker loop, the compiler's emit-impl-methods
    /// loop — already iterates `ti.methods`. Cloning the default into
    /// the impl is the smallest delta that makes the existing code
    /// "just work".
    /// Reject every hand-written impl of `Equal`, `Compare` or `Hash` and
    /// drop it from `decls`. These traits are sealed: every type gets
    /// them derived structurally from its fields (see
    /// `synthesize_auto_derive_impls`), and `==` / `<` never dispatch to
    /// an impl, so a hand-written one could only disagree with them.
    fn reject_sealed_trait_impls(&mut self, decls: &mut Vec<Decl>) {
        let mut errors = Vec::new();
        decls.retain(|decl| match decl {
            Decl::TraitImpl(ti)
                if !ti.is_auto_derived
                    && self
                        .impl_trait(ti)
                        .is_some_and(|t| SEALED_TRAIT_NAMES.iter().any(|n| t.is_builtin(n))) =>
            {
                errors.push((ti.trait_name, ti.span));
                false
            }
            _ => true,
        });
        for (trait_name, span) in errors {
            self.error(
                Code::InvalidTraitImpl,
                format!(
                    "trait '{trait_name}' cannot be implemented by hand: it is derived \
                     structurally for every type whose fields support it — remove this \
                     impl; Equal, Compare and Hash are derived"
                ),
                span,
            );
        }
    }

    /// Auto-derive `Display`, `Compare`, `Equal`, `Hash` impls for every
    /// user-declared enum or record (`Display` only when the type has no
    /// manual `trait Display for T` impl; the other three are sealed, so
    /// they are always derived). Pushes synthesized [`TraitImpl`] AST nodes
    /// onto `decls` so they flow through the same registration pipeline
    /// (`register_trait_impl`) and the compiler's `Decl::TraitImpl`
    /// emission path as user-written impls.
    ///
    /// Skipped for:
    /// - `Display` on types with a manual `Display` impl (synthesized
    ///   impls carry `is_auto_derived: true`, and the coherence check in
    ///   `register_trait_impl` lets a user `Display` impl override one).
    /// - **Generic** user types (`type Box(a) { Foo(a) }`,
    ///   `type Pair(a, b) { x: a, y: b }`). These keep the prior
    ///   typecheck-stamp + `dispatch_trait_method` behaviour. Synthesis
    ///   of generic-typed bodies requires impl-level
    ///   `where a: Compare`-style clauses on every recursive call site,
    ///   plus correct propagation through `target_type_args` /
    ///   `target_param_names` / `where_clauses`. That work is mechanical
    ///   but invasive; gated as a follow-up. See round-62 audit notes
    ///   for the deferred-work entry.
    /// - The `Alias` body kind (handled separately by
    ///   `register_type_alias`).
    fn synthesize_auto_derive_impls(&mut self, decls: &mut Vec<Decl>) {
        let display_sym = TraitKey::builtin("Display");
        // Scan for user-written `Display` impls so we can skip synthesis
        // for the types the user already covered. Use the canonical
        // target-type symbol so an impl on an alias skips synthesis on
        // every type under the same canonical name.
        let mut user_display_impls: std::collections::HashSet<TypeRef> =
            std::collections::HashSet::new();
        for decl in decls.iter() {
            if let Decl::TraitImpl(ti) = decl
                && !ti.is_auto_derived
                && self.named_trait(ti.trait_res, ti.trait_name) == Some(display_sym)
            {
                if let Some(target) = self.impl_target(ti) {
                    user_display_impls.insert(target);
                }
            }
        }
        // And for the types whose written `Display` impl an import brought
        // in (a module of the package, or an earlier REPL cell): deriving
        // one again would clash with it.
        let display_method = intern("display");
        for ((type_name, method), entry) in &self.tables.method_table {
            if *method == display_method
                && !entry.is_auto_derived
                && entry.trait_name == Some(display_sym)
            {
                user_display_impls.insert(*type_name);
            }
        }

        let compare_sym = TraitKey::builtin("Compare");
        let equal_sym = TraitKey::builtin("Equal");
        let hash_sym = TraitKey::builtin("Hash");

        // Pre-collect type names + body kind so we can check field types
        // against `trait_impl_set` without holding a borrow on
        // `program.decls` while we mutate `self.tables.trait_impl_set` below.
        // (We don't mutate trait_impl_set here, but we do need to call
        // `type_name_for_impl` which takes `&self`.)
        //
        // Both non-generic and generic types are collected. For generic
        // types (`td.params` non-empty) the synth helpers emit a
        // where-clause `where p: <Trait>` for each `p` in `td.params`,
        // so generic-param fields trivially satisfy the trait being
        // synthesized.
        // Each task carries the span its synthesized nodes take: the type
        // declaration's, or `Span::BUILTIN` for a builtin type.
        let mut tasks: Vec<(TypeRef, Vec<Symbol>, TypeBodyKind, Span)> = Vec::new();
        // The types the module declares, which the walk over the
        // builtin types below skips.
        let mut user_decl_type_names: std::collections::HashSet<TypeRef> =
            std::collections::HashSet::new();
        for decl in decls.iter() {
            if let Decl::Type(td) = decl {
                match &td.body {
                    TypeBody::Enum(_) => {
                        let ty = self.own_type(td.name);
                        user_decl_type_names.insert(ty);
                        if let Some(info) = self.tables.enums.get(&ty) {
                            tasks.push((
                                ty,
                                td.params.clone(),
                                TypeBodyKind::Enum(info.variants.clone()),
                                td.span,
                            ));
                        }
                    }
                    TypeBody::Record(_) => {
                        let ty = self.own_type(td.name);
                        user_decl_type_names.insert(ty);
                        if let Some(info) = self.tables.records.get(&ty) {
                            tasks.push((
                                ty,
                                td.params.clone(),
                                TypeBodyKind::Record(info.fields.clone()),
                                td.span,
                            ));
                        }
                    }
                    TypeBody::Alias(_) => {}
                }
            }
        }
        // ── Round 93: field-aware eligibility gate ──────────────────
        // `register_type_decl` pre-stamps Equal/Compare/Hash/Display
        // for EVERY user type. Pre-round-93 the stamp stood even when
        // a field could never satisfy the trait — synthesis was merely
        // SKIPPED, so `==` / `<` / `.compare()` / `.hash()` on e.g. a
        // record wrapping a `Fn(..)` field typechecked and laundered
        // into nondeterministic Value-level fallbacks (closure
        // ordering = Arc pointer address under ASLR). Here we compute
        // honest, recursive, field-aware eligibility and UN-stamp the
        // ineligible pairs (recording a precise reason for
        // diagnostics). Equal/Compare/Hash are sealed (no hand-written
        // impl can exist), so one computation serves both the method
        // calls and the `==` / `<` operand checks in inference.rs.
        // Display is exempt: the runtime display fallback is total and
        // deterministic for every Value shape.
        self.enforce_auto_derive_field_gate(&user_decl_type_names);

        // Built-in enums and records are registered directly into
        // `self.tables.enums` / `self.tables.records` from `register_builtins` and
        // the per-module init paths under `src/typechecker/builtins/`
        // without ever appearing as a top-level `Decl::Type`. Walk
        // both maps to give them the same synth treatment as user
        // types: every built-in `(trait, type)` pair pre-stamped in
        // `trait_impl_set` (see `register_builtin_trait_impls`)
        // receives a synthesized impl method, so `Op::CallMethod`'s
        // method lookup resolves at runtime without falling through to
        // `dispatch_trait_method`.
        //
        // This is the second half of the round-62 work: round 62
        // covered every user enum / record (generic + non-generic);
        // this round extends coverage to built-in enums / records.
        // After this pass, the Variant / Record arms in
        // `dispatch_trait_method` and the corresponding entries in
        // the hash allowlist are unreachable and can be deleted.
        //
        // Iteration order: enums then records, sorted by name within
        // each map, so the synthesized AST is deterministic.
        //
        // Only the builtin types are derived here, besides the module's
        // own: another module's types were derived when it was checked,
        // and their impls are in the session's tables. The builtin types'
        // impls are derived once, with the builtin environment (see
        // `BuiltinEnv::build`), and are not derived again.
        let builtin_derived = &self.tables.builtin_derived;
        let underived_builtin = |ty: &TypeRef| -> bool {
            builtin_type_name(*ty).is_some() && !builtin_derived.contains(ty)
        };
        let mut builtin_enum_names: Vec<TypeRef> = self
            .tables
            .enums
            .iter()
            .filter(|(n, _)| !user_decl_type_names.contains(*n) && underived_builtin(n))
            .map(|(n, _)| *n)
            .collect();
        builtin_enum_names.sort_by_key(|t| resolve(t.name));
        for type_name in builtin_enum_names {
            if let Some(info) = self.tables.enums.get(&type_name) {
                tasks.push((
                    type_name,
                    info.params.clone(),
                    TypeBodyKind::Enum(info.variants.clone()),
                    Span::BUILTIN,
                ));
            }
        }
        let mut builtin_record_names: Vec<TypeRef> = self
            .tables
            .records
            .iter()
            .filter(|(n, _)| !user_decl_type_names.contains(*n) && underived_builtin(n))
            .map(|(n, _)| *n)
            .collect();
        builtin_record_names.sort_by_key(|t| resolve(t.name));
        for type_name in builtin_record_names {
            if let Some(info) = self.tables.records.get(&type_name) {
                // Built-in records are non-generic; the params vec is
                // empty. (Generic built-in records would need their
                // param Symbol names tracked alongside `record_param_var_ids`
                // — extend this branch the day a generic built-in
                // record appears.)
                let params = self
                    .tables
                    .record_param_var_ids
                    .get(&type_name)
                    .map(|ids| {
                        // No symbolic param names are tracked for
                        // built-in records; synth helpers only need a
                        // count + uniqueness, so synthesize fresh
                        // placeholder Symbols. Today this branch is
                        // unreachable because every built-in record
                        // is non-generic.
                        (0..ids.len())
                            .map(|i| intern(&format!("__builtin_rec_param_{i}__")))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                tasks.push((
                    type_name,
                    params,
                    TypeBodyKind::Record(info.fields.clone()),
                    Span::BUILTIN,
                ));
            }
        }

        let mut synthesized: Vec<Decl> = Vec::new();
        for (type_name, type_params, body, decl_span) in tasks {
            let derive = auto_derive::Derive {
                span: decl_span,
                ty: type_name,
                variants: self.variant_resolutions(type_name),
            };
            // Helper closures to scope the synthesis decisions per-trait.
            let key = canonical_head(&self.tables.resolver, type_name);

            // Resolve this type's field types in the form they appear in
            // EnumInfo/RecordInfo (already-resolved Types). Use them to
            // decide which traits to synthesize: a trait whose method
            // body would call `.compare()` / `.hash()` on a field that
            // doesn't satisfy that trait would fail the body-check pass.
            // Skipping synthesis for those cases falls back to the
            // typecheck-stamp behaviour from `register_type_decl`, which
            // is intentionally permissive (the call site only fails if
            // the user actually invokes the method).
            //
            // Recursive same-type references are accepted via
            // self_supports = true: e.g. `type Tree { Leaf, Node(Tree, Tree) }`
            // can derive Compare because the recursive `.compare()` calls
            // resolve to the same synthesized method we're emitting.
            //
            // For generic types, every generic param is treated as
            // supporting the trait we're synthesizing — the synthesized
            // impl carries `where p: <Trait>` for each param, so the
            // trait obligation on the recursive `.compare()` etc. will
            // be satisfied at the impl-instantiation site by the user
            // (or rejected there with a localised error). A field that
            // mentions a generic param via `Type::Var` resolves through
            // the param_var_ids in EnumInfo/RecordInfo.
            let param_var_ids: std::collections::HashSet<TyVar> = match &body {
                TypeBodyKind::Enum(_) => self
                    .tables
                    .enums
                    .get(&type_name)
                    .map(|info| info.param_var_ids.iter().copied().collect())
                    .unwrap_or_default(),
                TypeBodyKind::Record(_) => self
                    .tables
                    .record_param_var_ids
                    .get(&type_name)
                    .map(|ids| ids.iter().copied().collect())
                    .unwrap_or_default(),
            };
            let supports = |_trait_sym: TraitKey, ty: &Type| -> bool {
                // Self-references — same nominal head as the type we're
                // synthesizing for — are always allowed (the recursive
                // body calls the same method we register).
                if let Some(name) = self.type_name_for_impl(ty)
                    && name == type_name
                {
                    return true;
                }
                // Generic-param references resolve to a TyVar whose id
                // appears in `param_var_ids`; treat them as supporting
                // the trait being synthesized (the where-clause covers
                // the obligation at the impl-instantiation site).
                if let Type::Var(v) = self.apply(ty)
                    && param_var_ids.contains(&v)
                {
                    return true;
                }
                self.field_type_supports_trait(_trait_sym, ty)
            };

            let (mut compare_ok, mut equal_ok, mut hash_ok, mut display_ok) =
                (true, true, true, true);
            match &body {
                TypeBodyKind::Enum(variants) => {
                    for v in variants {
                        for field_ty in &v.field_types {
                            compare_ok &= supports(compare_sym, field_ty);
                            equal_ok &= supports(equal_sym, field_ty);
                            hash_ok &= supports(hash_sym, field_ty);
                            display_ok &= supports(display_sym, field_ty);
                        }
                    }
                }
                TypeBodyKind::Record(fields) => {
                    for (_, field_ty) in fields {
                        compare_ok &= supports(compare_sym, field_ty);
                        equal_ok &= supports(equal_sym, field_ty);
                        hash_ok &= supports(hash_sym, field_ty);
                        display_ok &= supports(display_sym, field_ty);
                    }
                }
            }

            // Per-trait policy gate. A `(trait, type)` pair receives
            // a synthesized impl ONLY if it is already present in
            // `trait_impl_set`. For user-declared types,
            // `register_type_decl` pre-stamps all four built-in traits
            // unconditionally (line ~2314), so this gate is a no-op
            // for the user-type path. For built-in types,
            // `register_builtin_trait_impls` stamps only the
            // policy-permitted traits (e.g. Compare is excluded for
            // Option/Result/Tuple/Map/Set, see `non_ordering_traits`),
            // and the gate honours that exclusion — synth would
            // otherwise produce a `Compare:Option` impl that breaks
            // `tests/cli/trait_init_parity_tests.rs`.
            let policy_allows = |trait_sym: TraitKey| -> bool {
                self.tables.trait_impl_set.contains(&(trait_sym, key))
            };

            match body {
                TypeBodyKind::Enum(variants) => {
                    // Convert resolved VariantInfo back into AST EnumVariant
                    // shape (the auto_derive helpers operate on AST forms).
                    // Only .name and .fields.len() matter — the synthesis
                    // just emits .compare() / .hash() / .display() /
                    // .equal() calls on positionally-named bound vars, so
                    // the field TypeExprs are placeholders.
                    let ast_variants: Vec<EnumVariant> = variants
                        .iter()
                        .map(|v| EnumVariant {
                            name: v.name,
                            name_span: decl_span,
                            // Synthesize `Wildcard` placeholder TypeExprs;
                            // the auto_derive helpers only count them.
                            fields: v
                                .field_types
                                .iter()
                                .map(|_| {
                                    TypeExpr::new(
                                        TypeExprKind::Named {
                                            module: None,
                                            name: intern("__synth_placeholder__"),
                                            name_span: decl_span,
                                        },
                                        decl_span,
                                    )
                                })
                                .collect(),
                        })
                        .collect();
                    if display_ok
                        && policy_allows(display_sym)
                        && !user_display_impls.contains(&key)
                    {
                        synthesized.push(Decl::TraitImpl(derive.synth_display_impl_for_enum(
                            type_name.name,
                            &type_params,
                            &ast_variants,
                        )));
                    }
                    if compare_ok && policy_allows(compare_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_compare_impl_for_enum(
                            type_name.name,
                            &type_params,
                            &ast_variants,
                        )));
                    }
                    if equal_ok && policy_allows(equal_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_equal_impl_for_enum(
                            type_name.name,
                            &type_params,
                            &ast_variants,
                        )));
                    }
                    if hash_ok && policy_allows(hash_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_hash_impl_for_enum(
                            type_name.name,
                            &type_params,
                            &ast_variants,
                        )));
                    }
                }
                TypeBodyKind::Record(fields) => {
                    let ast_fields: Vec<RecordField> = fields
                        .iter()
                        .map(|(name, _)| RecordField {
                            name: *name,
                            name_span: decl_span,
                            ty: TypeExpr::new(
                                TypeExprKind::Named {
                                    module: None,
                                    name: intern("__synth_placeholder__"),
                                    name_span: decl_span,
                                },
                                decl_span,
                            ),
                        })
                        .collect();
                    if display_ok
                        && policy_allows(display_sym)
                        && !user_display_impls.contains(&key)
                    {
                        synthesized.push(Decl::TraitImpl(derive.synth_display_impl_for_record(
                            type_name.name,
                            &type_params,
                            &ast_fields,
                        )));
                    }
                    if compare_ok && policy_allows(compare_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_compare_impl_for_record(
                            type_name.name,
                            &type_params,
                            &ast_fields,
                        )));
                    }
                    if equal_ok && policy_allows(equal_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_equal_impl_for_record(
                            type_name.name,
                            &type_params,
                            &ast_fields,
                        )));
                    }
                    if hash_ok && policy_allows(hash_sym) {
                        synthesized.push(Decl::TraitImpl(derive.synth_hash_impl_for_record(
                            type_name.name,
                            &type_params,
                            &ast_fields,
                        )));
                    }
                }
            }
        }
        decls.extend(synthesized);
    }

    /// Conservative check: is the field type `ty` known to satisfy
    /// `trait_name` as recorded in `trait_impl_set`? Returns false on
    /// unresolved tyvars or unknown nominal heads. Used by
    /// `synthesize_auto_derive_impls` to decide whether a record / enum
    /// can have a sound auto-derived impl.
    fn field_type_supports_trait(&self, trait_name: TraitKey, ty: &Type) -> bool {
        let Some(type_name) = self.type_name_for_impl(ty) else {
            return false;
        };
        let canonical = canonical_head(&self.tables.resolver, type_name);
        self.tables
            .trait_impl_set
            .contains(&(trait_name, canonical))
    }

    /// Round 93: compute honest field-aware eligibility for the three
    /// gated built-in traits (Equal / Compare / Hash) over every
    /// user-declared type, then un-stamp `trait_impl_set` /
    /// `method_table` for the ineligible pairs and store the reasons
    /// in `auto_derive_negatives`. See the call site in
    /// `synthesize_auto_derive_impls` for the full rationale.
    fn enforce_auto_derive_field_gate(
        &mut self,
        user_type_names: &std::collections::HashSet<TypeRef>,
    ) {
        let negatives = self.compute_auto_derive_field_negatives(user_type_names);

        // Un-stamp the negatives: drop the provisional `trait_impl_set`
        // entry (so `where a: Trait` obligations and supertrait checks
        // reject honestly) and the provisional auto-derived
        // `method_table` entry (so `.compare()` / `.equal()` / `.hash()`
        // calls are rejected instead of falling through to
        // `dispatch_trait_method`'s Value-level behaviour at runtime).
        for (trait_sym, canon) in negatives.keys() {
            self.tables.trait_impl_set.remove(&(*trait_sym, *canon));
            let method_sym = match resolve(trait_sym.name).as_str() {
                "Equal" => intern("equal"),
                "Compare" => intern("compare"),
                "Hash" => intern("hash"),
                _ => continue,
            };
            // `method_table` is keyed on the declared (un-canonical)
            // type name; for enum/record decls the canonical name is
            // the declared name, but remove under both to be safe.
            self.tables.method_table.remove(&(*canon, method_sym));
            for name in user_type_names {
                if canonical_head(&self.tables.resolver, *name) == *canon {
                    self.tables.method_table.remove(&(*name, method_sym));
                }
            }
        }

        // Clear any stale negatives for the types processed in this
        // run before storing the fresh results (a REPL session or
        // re-check may redefine a type with now-eligible fields; a
        // leftover negative would spuriously reject it).
        let processed: std::collections::HashSet<TypeRef> = user_type_names
            .iter()
            .map(|n| canonical_head(&self.tables.resolver, *n))
            .collect();
        self.tables
            .auto_derive_negatives
            .retain(|(_, canon), _| !processed.contains(canon));
        self.tables.auto_derive_negatives.extend(negatives);
    }

    /// Round 93: fixpoint over the user-declared types computing which
    /// `(trait, type)` pairs canNOT satisfy a gated built-in trait
    /// because of an offending field / variant payload. Returns
    /// `(trait, canonical type name) → full diagnostic message`.
    ///
    /// Termination / recursion notes: each pass may only ADD
    /// negatives and the pair space is finite, so the loop is bounded
    /// by `3 × |types|` passes. Recursive and mutually-recursive
    /// types that are otherwise clean are never added — the walk
    /// reads the CURRENT stamp for nominal heads (coinductive: a
    /// reference cycle with no offending field is sound because
    /// runtime values are finite trees), so `type Tree { leaf: Int,
    /// kids: List(Tree) }` keeps all four traits.
    fn compute_auto_derive_field_negatives(
        &self,
        user_type_names: &std::collections::HashSet<TypeRef>,
    ) -> HashMap<(TraitKey, TypeRef), String> {
        let gated_traits = [
            TraitKey::builtin("Equal"),
            TraitKey::builtin("Compare"),
            TraitKey::builtin("Hash"),
        ];

        // Owned snapshot of each user type's resolved body so the
        // fixpoint can walk without re-borrowing `self.tables.enums` /
        // `self.tables.records`. Sorted by name for deterministic results.
        // (Generic-param fields resolve to `Type::Var`s, which the
        // walker treats as supporting — the synthesized impl's
        // `where p: Trait` clause covers them at instantiation.)
        let mut entries: Vec<(TypeRef, TypeRef, TypeBodyKind)> = Vec::new();
        for name in user_type_names {
            let canon = canonical_head(&self.tables.resolver, *name);
            if let Some(info) = self.tables.enums.get(name) {
                entries.push((*name, canon, TypeBodyKind::Enum(info.variants.clone())));
            } else if let Some(info) = self.tables.records.get(name) {
                entries.push((*name, canon, TypeBodyKind::Record(info.fields.clone())));
            }
        }
        entries.sort_by_key(|(name, ..)| resolve(name.name));

        let mut negatives: HashMap<(TraitKey, TypeRef), String> = HashMap::new();
        loop {
            let mut changed = false;
            for (name, canon, body) in &entries {
                for trait_sym in gated_traits {
                    let key = (trait_sym, *canon);
                    if negatives.contains_key(&key) || !self.tables.trait_impl_set.contains(&key) {
                        continue;
                    }
                    let supports =
                        |fty: &Type| self.gate_field_supports_trait(trait_sym, fty, &negatives, 0);
                    let offending: Option<String> = match body {
                        TypeBodyKind::Record(fields) => fields.iter().find_map(|(fname, fty)| {
                            (!supports(fty)).then(|| {
                                format!(
                                    "field '{}' has type '{}'",
                                    resolve(*fname),
                                    self.apply(fty)
                                )
                            })
                        }),
                        TypeBodyKind::Enum(variants) => variants.iter().find_map(|v| {
                            v.field_types.iter().enumerate().find_map(|(i, fty)| {
                                (!supports(fty)).then(|| {
                                    format!(
                                        "variant '{}' payload #{} has type '{}'",
                                        resolve(v.name),
                                        i + 1,
                                        self.apply(fty)
                                    )
                                })
                            })
                        }),
                    };
                    if let Some(field_desc) = offending {
                        negatives.insert(
                            key,
                            format!(
                                "type '{}' cannot derive '{}': {}, which is not {}",
                                resolve(name.name),
                                resolve(trait_sym.name),
                                field_desc,
                                builtin_trait_adjective(trait_sym),
                            ),
                        );
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        negatives
    }

    /// Round 93: recursive, honest "does this FIELD type satisfy the
    /// gated built-in trait?" check used by the field-aware gate and
    /// by the operator-operand instantiation walk.
    ///
    /// Deliberately permissive arms (over-rejection is the failure
    /// mode to avoid):
    ///   - `Var`: a generic param of the enclosing type (the
    ///     synthesized impl's `where p: Trait` clause covers it at
    ///     the instantiation site) or a not-yet-resolved inference
    ///     var — never provably bad here.
    ///   - `AssocProj`: "maybe valid" exactly like `Var` (round-92
    ///     operand parity).
    ///   - depth cap: give up permissively on absurdly deep types
    ///     rather than risk a stack overflow.
    fn gate_field_supports_trait(
        &self,
        trait_sym: TraitKey,
        ty: &Type,
        negatives: &HashMap<(TraitKey, TypeRef), String>,
        depth: usize,
    ) -> bool {
        if depth > 64 {
            return true;
        }
        let ty = self.apply(ty);
        let recurse = |t: &Type| self.gate_field_supports_trait(trait_sym, t, negatives, depth + 1);
        // Stamp lookup for a nominal/container head, honest w.r.t. the
        // in-progress negatives.
        let head_ok = |head: TypeRef| {
            let canon = canonical_head(&self.tables.resolver, head);
            let key = (trait_sym, canon);
            if negatives.contains_key(&key) {
                return false;
            }
            self.tables.trait_impl_set.contains(&key)
        };
        match &ty {
            Type::Error | Type::Never | Type::Var(_) | Type::AssocProj { .. } => true,
            // Functions support none of Equal/Compare/Hash: the
            // Value-level fallbacks are Arc-pointer identity (equal),
            // Arc-pointer ADDRESS ordering (compare — ASLR-
            // nondeterministic) and a constant tag (hash).
            Type::Fun(..) => false,
            // Channels carry identity-based equality (round 82:
            // `Value::Channel(a) == Value::Channel(b)` iff ids match)
            // but no ordering or hashing through the trait surface.
            Type::Channel(_) => trait_sym == TraitKey::builtin("Equal"),
            Type::List(t) | Type::Range(t) | Type::Set(t) => {
                let head = self
                    .type_name_for_impl(&ty)
                    .expect("container head has canonical name");
                head_ok(head) && recurse(t)
            }
            Type::Map(k, v) => head_ok(TypeRef::builtin("Map")) && recurse(k) && recurse(v),
            Type::Tuple(ts) => head_ok(TypeRef::builtin("Tuple")) && ts.iter().all(recurse),
            // Structural records: Value's PartialEq / Ord / Hash all
            // compare them element-wise (round-85 contracts), so the
            // honest answer is the conjunction over the known fields.
            // Open rows are rejected: the hidden tail could carry
            // anything.
            Type::AnonRecord { fields, tail } => {
                matches!(tail, RowTail::Closed) && fields.values().all(recurse)
            }
            // Nominal heads: the stamp (kept honest by the fixpoint
            // for user types, by registration policy for builtins)
            // decides the head; instantiation args / embedded field
            // types are walked so `Box(Fn(Int) -> Int)` is caught even
            // though `Box(a)` itself is conditionally eligible.
            Type::Record(name, fields) => head_ok(*name) && fields.iter().all(|(_, t)| recurse(t)),
            Type::Generic(name, args) => head_ok(*name) && args.iter().all(recurse),
            // Scalars and anything else: defer to the registered
            // stamp, exactly like the one-level synthesis gate.
            _ => self.field_type_supports_trait(trait_sym, &ty),
        }
    }

    /// Round 93: operator-operand violation check for `==`/`!=`
    /// (`is_equality`) and `<`/`>`/`<=`/`>=` on nominal record / enum
    /// operands. Returns the diagnostic to emit when the operand's
    /// type cannot soundly support the Value-level operation.
    pub(super) fn operand_builtin_trait_violation(
        &self,
        ty: &Type,
        is_equality: bool,
    ) -> Option<String> {
        let trait_sym = if is_equality {
            TraitKey::builtin("Equal")
        } else {
            TraitKey::builtin("Compare")
        };
        let resolved =
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        let (name, args) = match &resolved {
            Type::Generic(name, args) => (*name, args.clone()),
            // Nominal records normally flow as `Type::Generic`, but a
            // `Type::Record` form carries its (instantiated) field
            // types inline — walk them directly.
            Type::Record(name, fields) => {
                let canon = canonical_head(&self.tables.resolver, *name);
                if let Some(msg) = self.tables.auto_derive_negatives.get(&(trait_sym, canon)) {
                    return Some(msg.clone());
                }
                let no_negatives = HashMap::new();
                return fields.iter().find_map(|(fname, fty)| {
                    (!self.gate_field_supports_trait(trait_sym, fty, &no_negatives, 0))
                        .then(|| {
                        format!(
                            "type '{}' cannot derive '{}': field '{}' has type '{}', which is not {}",
                            resolve(name.name),
                            resolve(trait_sym.name),
                            resolve(*fname),
                            self.apply(fty),
                            builtin_trait_adjective(trait_sym),
                        )
                    })
                });
            }
            // Round 97: container HEADS (List/Range/Tuple/Map/Set) pass the
            // structural shape gate in `is_valid_compare_operand`, but the
            // Value-level operation recurses into element types — and the VM
            // fallback for a `Fn`-shaped element is Arc-pointer-address
            // ordering (ASLR-nondeterministic), exactly the bug round 3 fixed
            // for bare `Fn` operands and round 93 fixed for nominal fields.
            // Mirror the round-93 field walk: a container is compare/equality
            // -valid IFF every element / component / value type is itself
            // valid. `gate_field_supports_trait` already does this recursion
            // honestly (and bottoms out at `Type::Fun(..) => false`), so we
            // reuse it and surface the whole container type as the reason.
            Type::List(_) | Type::Range(_) | Type::Tuple(_) | Type::Map(..) | Type::Set(_) => {
                let no_negatives = HashMap::new();
                return (!self.gate_field_supports_trait(trait_sym, &resolved, &no_negatives, 0))
                    .then(|| {
                        format!(
                            "type '{resolved}' cannot derive '{}': element type is not {}",
                            resolve(trait_sym.name),
                            builtin_trait_adjective(trait_sym),
                        )
                    });
            }
            _ => return None,
        };
        let canon = canonical_head(&self.tables.resolver, name);
        if let Some(msg) = self.tables.auto_derive_negatives.get(&(trait_sym, canon)) {
            return Some(msg.clone());
        }
        // Instantiation walk: substitute the concrete type args into
        // the declared field / payload types and re-check. This is
        // the use-site flavour of the declaration-level gate — it
        // catches `Box(Fn(Int) -> Int)` where `type Box(a) { v: a }`
        // is conditionally eligible, while leaving phantom params
        // (`type Tag(a) { name: String }`) unpunished because only
        // the types that actually appear in fields are walked.
        let no_negatives = HashMap::new();
        let check = |fty: &Type| self.gate_field_supports_trait(trait_sym, fty, &no_negatives, 0);
        if let Some(info) = self.tables.records.get(&name) {
            let mapping: HashMap<TyVar, Type> = self
                .tables
                .record_param_var_ids
                .get(&name)
                .filter(|ids| ids.len() == args.len())
                .map(|ids| ids.iter().copied().zip(args.iter().cloned()).collect())
                .unwrap_or_default();
            return info.fields.iter().find_map(|(fname, fty)| {
                let concrete = substitute_vars(fty, &mapping);
                (!check(&concrete)).then(|| {
                    format!(
                        "type '{resolved}' cannot derive '{}': field '{}' has type '{}', which is not {}",
                        resolve(trait_sym.name),
                        resolve(*fname),
                        self.apply(&concrete),
                        builtin_trait_adjective(trait_sym),
                    )
                })
            });
        }
        if let Some(info) = self.tables.enums.get(&name) {
            let mapping: HashMap<TyVar, Type> = if info.param_var_ids.len() == args.len() {
                info.param_var_ids
                    .iter()
                    .copied()
                    .zip(args.iter().cloned())
                    .collect()
            } else {
                HashMap::new()
            };
            return info.variants.iter().find_map(|v| {
                v.field_types.iter().enumerate().find_map(|(i, fty)| {
                    let concrete = substitute_vars(fty, &mapping);
                    (!check(&concrete)).then(|| {
                        format!(
                            "type '{resolved}' cannot derive '{}': variant '{}' payload #{} has type '{}', which is not {}",
                            resolve(trait_sym.name),
                            resolve(v.name),
                            i + 1,
                            self.apply(&concrete),
                            builtin_trait_adjective(trait_sym),
                        )
                    })
                })
            });
        }
        None
    }

    /// Round 93: when a `.equal()` / `.compare()` / `.hash()` call
    /// misses the method table because the field-aware gate removed
    /// the provisional auto-derive entry, surface the precise reason
    /// instead of a generic "unknown field or method".
    pub(super) fn method_auto_derive_violation(
        &self,
        type_name: TypeRef,
        method: Symbol,
    ) -> Option<String> {
        let trait_sym = match resolve(method).as_str() {
            "equal" => TraitKey::builtin("Equal"),
            "compare" => TraitKey::builtin("Compare"),
            "hash" => TraitKey::builtin("Hash"),
            _ => return None,
        };
        let canon = canonical_head(&self.tables.resolver, type_name);
        self.tables
            .auto_derive_negatives
            .get(&(trait_sym, canon))
            .cloned()
    }

    fn synthesize_default_methods(&self, decls: &mut [Decl]) {
        for decl in decls.iter_mut() {
            let Decl::TraitImpl(ti) = decl else {
                continue;
            };
            let Some(trait_info) = self.impl_trait(ti).and_then(|t| self.tables.traits.get(&t))
            else {
                // Unknown trait — let validate_trait_impls / dispatch
                // surface the diagnostic; nothing to synthesize here.
                continue;
            };
            let impl_method_names: std::collections::HashSet<Symbol> =
                ti.methods.iter().map(|m| m.name).collect();
            // Walk methods in the order they appear on the trait so the
            // synthesized FnDecls land in a deterministic order.
            for (method_name, _ty) in &trait_info.methods {
                if impl_method_names.contains(method_name) {
                    continue;
                }
                if let Some(default_fn) = trait_info.default_method_bodies.get(method_name) {
                    ti.methods.push(default_fn.clone());
                }
            }
        }
    }

    /// Apply the trait-orphan rule. Returns `true` when the impl is
    /// allowed, `false` after emitting an error and signalling the
    /// caller to skip the rest of registration. See the call site in
    /// `register_trait_impl` for the rule statement.
    fn check_orphan_rule(&mut self, ti: &TraitImpl, target_type: TypeRef) -> bool {
        let trait_pkg = self
            .impl_trait(ti)
            .and_then(|t| self.tables.traits.get(&t))
            .map(|t| t.defined_in);
        // Compute the head-type package by reconstructing a Type from
        // the impl's target name + args. We use the canonicalised
        // `target_type` symbol because the round-23 GAP #1 unknown-
        // target check has already validated the name; for the orphan
        // walk we just need the head symbol's `defined_in`.
        let head_pkg = if let Some(info) = self.tables.enums.get(&target_type) {
            Some(info.defined_in)
        } else if let Some(info) = self.tables.records.get(&target_type) {
            Some(info.defined_in)
        } else {
            // Built-in head (List/Map/Set/Channel/Range/Tuple/Fn/Int/...):
            // no enum or record registered under this name. Treat as
            // stdlib-owned (`None`) so the trait-local arm can satisfy
            // the rule when a user package writes `trait MyTrait for
            // List(...)`.
            let head_str = builtin_type_name(target_type).unwrap_or_default();
            if crate::types::builtins::is_primitive(head_str)
                || crate::types::builtins::is_container(head_str)
            {
                None
            } else {
                // Truly unknown — the round-23 GAP #1 check already
                // emitted a diagnostic. Don't double-report; allow the
                // orphan walk to fall through.
                return true;
            }
        };

        let builtin = Self::builtin_pkg();
        // A package "p" is local iff it equals the current package.
        // Built-in stamps (`__builtin__` or `None`) are never local on
        // their own — they're stdlib-owned, and the orphan rule
        // requires the OTHER arm to be locally owned. The REPL /
        // scratch script case (current_package = None) treats every
        // arm as local so the rule is effectively disabled there.
        let current_pkg_sym = self.current_package;
        // Returns true when a package stamp identifies the active
        // current_package. None and `__builtin__` are stdlib-owned
        // and return false. The REPL path (current_package=None)
        // short-circuits before reaching here so we don't have to
        // special-case it inside this helper.
        let is_local = |pkg: Option<Symbol>| -> bool {
            let Some(cur) = current_pkg_sym else {
                return true;
            };
            match pkg {
                None => false,
                Some(p) if p == builtin => false,
                Some(p) => p == cur,
            }
        };

        // Unknown trait — the unknown-trait diagnostic fires elsewhere.
        // Treat as local so we don't pile a misleading orphan
        // diagnostic on top of it.
        if trait_pkg.is_none() {
            return true;
        }
        let trait_local = is_local(trait_pkg);
        let type_local = is_local(head_pkg);

        if trait_local || type_local {
            return true;
        }

        // Both arms are foreign. Build a diagnostic that names both
        // packages, the impl's trait, and the impl's target type.
        let trait_pkg_name = trait_pkg
            .map(resolve)
            .unwrap_or_else(|| "(unknown)".to_string());
        let head_pkg_name = head_pkg
            .map(resolve)
            .unwrap_or_else(|| "__builtin__".to_string());
        let current_pkg_name = current_pkg_sym
            .map(resolve)
            .unwrap_or_else(|| "(scratch)".to_string());
        self.error(
            Code::OrphanImpl,
            format!(
                "orphan impl: trait '{}' is from package '{}' and type '{}' is from package '{}'; \
                 either the trait or the type must be defined in the current package '{}'",
                resolve(ti.trait_name),
                trait_pkg_name,
                resolve(ti.target_type),
                head_pkg_name,
                current_pkg_name,
            ),
            ti.span,
        );
        false
    }

    fn register_trait_impl(&mut self, ti: &TraitImpl, env: &mut TypeEnv) {
        // An impl of a trait or for a type the resolver resolved to
        // nothing: it reported why.
        if ti.trait_res == Some(crate::defs::Res::Error)
            || ti.target_res == Some(crate::defs::Res::Error)
        {
            self.unresolved_impl_methods
                .extend(ti.methods.iter().map(|m| m.name));
            return;
        }
        // Phase B: canonicalise the target-type symbol so an impl
        // `trait Foo for Range(a)` registers under the same key
        // (`"List"`) that dispatch lookup will use for both `Range(_)`
        // and `List(_)` receivers. Round 61's
        // dispatch fix collapsed receivers to `List` at runtime; with phase B's `type_name_for_impl`
        // canonicalising at the lookup side too, the impl table
        // would otherwise be unreachable for an explicitly
        // Range-targeted impl. Without this canonicalisation the
        // round-61 lock test
        // `user_trait_method_on_list_dispatches_for_range_receiver`
        // (and its siblings) regresses to "type 'Range' does not
        // implement trait 'Foo'".
        //
        // Round-23 GAP #1: reject trait impls whose target type was never
        // declared: `trait Greet for Widget { ... }` with no `type
        // Widget` would attach methods to a phantom type. A lowercase
        // target (`trait Display for a { ... }`) names a type variable,
        // not a type: there is nothing to register it for.
        if self.names_rejected(ti.target_res, ti.target_type) {
            return;
        }
        let Some(written) = self.named_type(ti.target_res, ti.target_type) else {
            let name_str = resolve(ti.target_type);
            if !name_str.starts_with(|c: char| c.is_lowercase()) {
                self.error(
                    Code::UnknownType,
                    format!("trait impl target '{name_str}' is not a declared type"),
                    ti.span,
                );
            }
            return;
        };
        let target_type = canonical_head(&self.tables.resolver, written);
        let Some(trait_key) = self.impl_trait(ti) else {
            self.error(
                Code::UnknownTrait,
                format!("trait '{}' is not declared", ti.trait_name),
                ti.span,
            );
            return;
        };
        let impl_key = (trait_key, target_type);

        // Coherence check: reject duplicate user-defined impls.
        if self.tables.trait_impl_set.contains(&impl_key) {
            // Allow overriding auto-derived impls (only `Display` can be
            // written by hand: see `reject_sealed_trait_impls`).
            let first_method = ti
                .methods
                .first()
                .map(|m| m.name)
                .unwrap_or_else(|| intern("display"));
            // The impl of this trait already registered, not a method of
            // another trait of one name (`trait Describe for Pt { fn
            // display }` is no impl of Display).
            let existing = self
                .tables
                .trait_methods
                .get(&(target_type, first_method, trait_key))
                .cloned()
                .or_else(|| {
                    self.tables
                        .method_table
                        .get(&(target_type, first_method))
                        .filter(|e| self.entry_trait(e, first_method) == Some(trait_key))
                        .cloned()
                });
            let is_overriding_auto = existing.map(|e| e.is_auto_derived).unwrap_or(true);
            if !is_overriding_auto {
                self.error(
                    Code::DuplicateDeclaration,
                    format!(
                        "duplicate implementation of trait '{}' for type '{}'",
                        ti.trait_name, ti.target_type
                    ),
                    ti.span,
                );
                return;
            }
        }

        // Trait-orphan rule (round 63 item 5): reject `impl Trait for Type`
        // when both the trait and the target type's head are foreign to
        // the current package. Auto-derived synthetic impls are exempt
        // — they're conceptually the stdlib's implementation specialised
        // to a user-supplied type parameter, and the synth pass never
        // races against another package over a built-in head.
        //
        // Built-ins (`__builtin__`) are stdlib-owned and treated as a
        // wild-card counterparty: a user package implementing a built-in
        // trait for one of its own types satisfies the type-local arm,
        // and a user package implementing one of its own traits for a
        // built-in type satisfies the trait-local arm.
        if !ti.is_auto_derived && !self.check_orphan_rule(ti, target_type) {
            // Skip the rest of impl registration on rejection: don't
            // poison `trait_impl_set` / `method_table` with an entry the
            // user wasn't allowed to register, otherwise downstream
            // dispatch would silently route through this orphan.
            return;
        }

        self.tables.trait_impl_set.insert(impl_key);
        // GAP-2: record the impl block's real span for validate_trait_impls
        // to use when reporting missing-method diagnostics.
        self.tables.trait_impl_spans.insert(impl_key, ti.span);

        // Build the impl-level parameter map. For a parameterized target
        // like `trait X for Box(a)`, each lowercase binder in
        // `target_param_names` becomes a fresh type variable that is
        // shared across every method in the impl — so `fn get(self) -> a`
        // and `fn put(self, x: a)` in the same impl refer to the SAME
        // type variable, mirroring the fn-signature convention.
        let mut impl_param_map: HashMap<Symbol, Type> = HashMap::new();
        for &param_name in &ti.target_param_names {
            impl_param_map.insert(param_name, self.fresh_var());
        }

        // Construct the self_type. Three cases:
        //   1. Bare-target form (`trait X for Int`): target_type_args is
        //      empty. For primitive types, fall through to type_from_name.
        //      For parameterized user types, synthesize fresh-var args to
        //      match the record's / enum's arity — otherwise the receiver-
        //      unify step in dispatch_method_entry would fail with arity
        //      mismatch when the caller passes a concrete instantiation
        //      like `Box { value: 42 }` (Generic("Box", [Int])) against a
        //      zero-arg self_type (Generic("Box", [])).
        //   2. Parameterized user type (`trait X for Box(a)`): resolve
        //      each arg through impl_param_map. Arity enforced against
        //      the record's param_var_ids or the enum's declared params.
        //   3. Built-in parameterized form (`trait X for List(a)`):
        //      resolve_type_expr handles List/Map/Set/Channel/Tuple/Fn
        //      already; reuse it.
        //
        // A builtin type, or a record, enum or alias type: a target the
        // checker does not know (a builtin type with no impls, `Tuple`
        // aside) is reported.
        {
            let name_str = resolve(written.name);
            let known = match builtin_type_name(written) {
                Some(builtin) => {
                    crate::types::builtins::is_primitive(builtin)
                        || crate::types::builtins::is_container(builtin)
                        || self.tables.enums.contains_key(&written)
                        || self.tables.records.contains_key(&written)
                }
                None => {
                    self.tables.records.contains_key(&written)
                        || self.tables.enums.contains_key(&written)
                        || self.tables.type_aliases.contains(&written)
                }
            };
            if !known {
                self.error(
                    Code::UnknownType,
                    format!("trait impl target '{name_str}' is not a declared type"),
                    ti.span,
                );
            }
        }

        let self_type = if ti.target_type_args.is_empty() {
            // Phase D: if the bare target is a user alias, synthesize
            // the self-type from the alias's canonical target rather
            // than treating the alias name as a nominal head. Without
            // this, `trait T for Bytes` (where `Bytes = List(Int)`)
            // would produce a `Generic("Bytes", [])` self-type that
            // would never unify with any concrete `List(Int)`
            // receiver. Callers downstream still see the canonical
            // form (`canonical_head` collapses the impl_key
            // to `"List"`), so the dispatch lookup arrives at the
            // right method.
            if let Some(info) = self.tables.resolver.lookup_alias(written) {
                // Build a fresh-var instantiation per alias parameter so
                // the impl methods see polymorphic vars rather than
                // shared template tyvars. For `Bytes = List(Int)` (no
                // params), the substitution is identity and the result
                // is `List(Int)` after canonicalisation. For
                // `Pair(a) = (a, a)`, params get fresh vars and the
                // self-type is `(a', a')`.
                let mut mapping: HashMap<TyVar, Type> = HashMap::new();
                for &var_id in &info.param_var_ids {
                    mapping.insert(var_id, self.fresh_var());
                }
                let substituted = crate::types::substitute_vars(&info.target, &mapping);
                crate::types::canonical::canonicalize(&self.tables.resolver, &substituted)
            } else {
                let user_arity = self
                    .tables
                    .record_param_var_ids
                    .get(&written)
                    .map(|v| v.len())
                    .or_else(|| self.tables.enums.get(&written).map(|e| e.params.len()))
                    .unwrap_or(0);
                if user_arity == 0 {
                    // Bare builtin-container targets (`trait T for List`,
                    // Map/Set/Channel; a bare `Range` target arrives here
                    // as `List` via `canonical_head`) mirror
                    // `resolve_type_expr`'s bare-name annotation
                    // semantics: synthesize a fresh var per element slot
                    // so the self_type unifies with any concrete receiver
                    // (`List(Int)`, `Map(String, Bool)`, ...). Pre-fix
                    // these fell through to `type_from_name`'s
                    // `Generic("List", [])`, which unify's catch-all
                    // rejected against `Type::List(Int)` with the
                    // self-contradictory "type mismatch: expected List,
                    // got List(Int)" — even though the SAME impl
                    // dispatched fine through a where-bound fn
                    // (head-keyed obligation + runtime dispatch). The
                    // fresh vars are per-registration, like the alias /
                    // user-arity branches: `instantiate_method_entry`
                    // refreshes them per call site.
                    //
                    // Variadic `Tuple` has no fresh-var shape; it keeps
                    // the `Generic("Tuple", [])` fallback and is matched
                    // by the bare-`Tuple` wildcard arm in `unify` (same
                    // strategy as `Fn`).
                    match builtin_type_name(target_type) {
                        Some("List") => Type::List(Box::new(self.fresh_var())),
                        Some("Set") => Type::Set(Box::new(self.fresh_var())),
                        Some("Channel") => Type::Channel(Box::new(self.fresh_var())),
                        Some("Map") => {
                            Type::Map(Box::new(self.fresh_var()), Box::new(self.fresh_var()))
                        }
                        // Use the canonicalised target name so the
                        // self_type built here matches the `method_table`
                        // registration key (also canonicalised). Without
                        // this, `trait T for Fun` produces a self_type of
                        // `Generic("Fun", [])` while the impl_key is
                        // `("T", "Fn")` — and the dispatch unify of
                        // `Type::Fun(_, _)` against `Generic("Fun", [])`
                        // misses the `(Type::Fun, Generic("Fn", []))` arm
                        // we added in `unify`. Round 71 follow-up TYPE-3
                        // canonical-name unification.
                        _ => Self::type_from_name(target_type),
                    }
                } else {
                    let args: Vec<Type> = (0..user_arity).map(|_| self.fresh_var()).collect();
                    Type::Generic(target_type, args)
                }
            }
        } else {
            // Arity check. Covers user-declared record/enum targets via
            // record_param_var_ids / self.tables.enums, AND builtin parameterized
            // containers (List, Set, Channel, Map) whose arities are fixed
            // by the language. Without the builtin arm, `trait X for List(a, b)`
            // fell through to `_ => Type::Generic("List", [a, b])` below,
            // silently producing a phantom 2-arg List type with no diagnostic.
            // Derive fixed-arity builtin entries from the authoritative
            // table. Variadic shapes (`Tuple`, `Fn`, `Fun`, `Handle`)
            // carry `arity: None` and are intentionally skipped — they
            // do not participate in this trait-impl arity check.
            let builtin_arity: Option<(usize, &'static str)> = builtin_type_name(written)
                .and_then(crate::types::builtins::lookup)
                .filter(|b| b.kind == crate::types::builtins::BuiltinKind::Container)
                .and_then(|b| b.arity.map(|a| (a as usize, "builtin")));
            // Round 74 Fix #2: include user-declared type aliases in the
            // arity table. Without this, `trait Show for Pair(a)` where
            // `type Pair(a) = (a, a)` skipped the arity check (the alias
            // is not a record / enum / builtin container) and fell
            // through to the `_ => Type::Generic("Pair", [tv])` arm at
            // the bottom — producing an impl whose self_type never
            // unifies with any concrete `(Int, Int)` receiver.
            let alias_arity: Option<(usize, &'static str)> = self
                .tables
                .type_alias_arity
                .get(&written)
                .copied()
                .map(|a| (a, "alias"));
            let expected_arity = self
                .tables
                .record_param_var_ids
                .get(&written)
                .map(|v| (v.len(), "record"))
                .or_else(|| {
                    self.tables
                        .enums
                        .get(&written)
                        .map(|e| (e.params.len(), "enum"))
                })
                .or(alias_arity)
                .or(builtin_arity);
            if let Some((expected, kind)) = expected_arity
                && expected != ti.target_type_args.len()
            {
                self.error(Code::ArityMismatch,
                    format!(
                        "type argument count mismatch for {kind} '{}' in trait impl: expected {expected}, got {}",
                        resolve(written.name),
                        ti.target_type_args.len()
                    ),
                    ti.span,
                );
            }
            // Resolve through a dedicated Generic form so the head symbol
            // is preserved alongside the impl-level tyvar args.
            let resolved_args: Vec<Type> = ti
                .target_type_args
                .iter()
                .map(|arg_te| self.resolve_type_expr(arg_te, &mut impl_param_map))
                .collect();
            // Round 74 Fix #2: parametric alias as trait-impl target —
            // expand the alias by substituting `resolved_args` through
            // the alias's stored target. Mirrors the non-parametric
            // alias path at line ~5617 (which calls
            // `self.tables.resolver.lookup_alias(written)` and walks
            // `info.target` with each `param_var_ids[i]` mapped to a
            // fresh tyvar). Here we map `param_var_ids[i]` → the
            // user-supplied type-arg at the same index.
            //
            // Without this, `trait Show for Pair(a)` (with
            // `type Pair(a) = (a, a)`) produced
            // `Type::Generic("Pair", [tv])` as the self_type. A
            // concrete `(1, 2)` receiver typed as `(Int, Int)` —
            // canonicalised by `resolve_type_expr` of the annotation
            // `Pair(Int)` to `(Int, Int)` — would not unify with the
            // phantom `Generic("Pair", _)`.
            if let Some(info) = self.tables.resolver.lookup_alias(written) {
                let mut mapping: HashMap<TyVar, Type> = HashMap::new();
                for (i, &var_id) in info.param_var_ids.iter().enumerate() {
                    if let Some(arg_ty) = resolved_args.get(i) {
                        mapping.insert(var_id, arg_ty.clone());
                    }
                }
                let substituted = crate::types::substitute_vars(&info.target, &mapping);
                crate::types::canonical::canonicalize(&self.tables.resolver, &substituted)
            } else {
                match builtin_type_name(written).unwrap_or_default() {
                    "List" if resolved_args.len() == 1 => {
                        Type::List(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Range" if resolved_args.len() == 1 => {
                        Type::Range(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Set" if resolved_args.len() == 1 => {
                        Type::Set(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Channel" if resolved_args.len() == 1 => {
                        Type::Channel(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Map" if resolved_args.len() == 2 => {
                        let mut iter = resolved_args.into_iter();
                        Type::Map(
                            Box::new(iter.next().unwrap()),
                            Box::new(iter.next().unwrap()),
                        )
                    }
                    _ => Type::Generic(written, resolved_args),
                }
            }
        };

        // Record the impl's full self type under the canonical head key so
        // `verify_trait_obligation` can compare an obligated type's
        // positional args against the impl's — closing the alias-expansion
        // soundness hole where `trait Total for Bytes2` (with
        // `type Bytes2 = List(Int)`) satisfied a where-bound for ANY
        // `List(T)`. Overwrites are fine: coherence rejects duplicate user
        // impls above, and the one permitted overwrite (a user Display
        // impl overriding the auto-derived one) should win here too.
        self.tables
            .impl_self_types
            .insert((trait_key, target_type), self_type.clone());

        // Resolve impl-level where clauses (e.g. `trait X for Box(a) where
        // a: Show`) to `(TyVar, trait)` pairs against the impl_param_map.
        // These apply to every method in the impl and are appended to
        // both the method's scheme (so active_constraints in the body see
        // them during check_fn_body_with_name) and its MethodEntry (so
        // external call sites defer the obligation via pending_where).
        //
        // Multi-trait bounds (`where a: Show + Hash`) arrive pre-flattened
        // from parse_where_clauses_opt as separate (tv, trait) entries
        // sharing a type_var, so the resolution loop handles both forms
        // with a single path.
        let mut impl_level_constraints: Vec<(TyVar, TraitKey, Vec<Type>)> = Vec::new();
        // Parallel structure used to populate self.tables.impl_constraints below so
        // that call-site constraint resolution can recursively verify the
        // impl's own where clauses against the actual concrete type
        // arguments at the call site.
        //
        // Round 101 BROKEN: the stored index MUST live in the index space
        // the consumer uses. `verify_trait_obligation` resolves an
        // obligation via `type_args_of(resolved_receiver).get(idx)` — the
        // positional args of the CANONICAL EXPANDED type. For direct
        // targets (`Box(a)`, `Map(k, v)`) that space coincides with
        // `target_param_names` order, but for ALIAS targets it does not:
        // with `type Named(a) = Map(String, a)`, param `a` is at param
        // position 0 but EXPANDED slot 1, so indexing by param position
        // verified the key slot (`String`) instead of `a` — both false
        // rejects ("'String' does not implement 'Marked'" on a valid
        // program) and false accepts (a `where a: Display` bound checked
        // against `String` while the actual value type was `Fn`). Compute
        // the index as the position of the param's tyvar within the
        // expanded self_type's positional args so both sides of the table
        // agree. A param that never surfaces as a top-level positional
        // slot (Tuple/Fn alias targets, occurrences nested deeper than
        // one wrapper) gets no entry — the same effective behavior as
        // before, where `args.get(idx)` returned `None` at verify time
        // and the obligation was skipped.
        let expanded_self_args = self.type_args_of(&self_type);
        let mut impl_obligations_by_index: Vec<(usize, TraitKey, Vec<Type>)> = Vec::new();
        for wc in &ti.where_clauses {
            // A bound the resolver resolved to nothing: it reported why.
            if wc.trait_res == Some(crate::defs::Res::Error) {
                continue;
            }
            let type_param = &wc.type_param;
            let trait_args = &wc.trait_args;
            let Some(trait_key) = self
                .named_trait(wc.trait_res, wc.trait_name)
                .filter(|t| self.tables.traits.contains_key(t))
            else {
                self.error(
                    Code::UnknownTrait,
                    format!(
                        "unknown trait '{}' in where clause on trait impl '{} for {}'",
                        resolve(wc.trait_name),
                        resolve(ti.trait_name),
                        resolve(ti.target_type)
                    ),
                    ti.span,
                );
                continue;
            };
            let trait_name = &trait_key;
            // Round 101: the bound's arity must match the trait's
            // declared param count in BOTH directions (see
            // check_where_bound_arity). Skip registering the malformed
            // bound — a length-mismatched arg list would sail past
            // verify_trait_obligation's equal-length zip guard and
            // degrade to a bare "implements the trait" check.
            if !self.check_where_bound_arity(*trait_name, trait_args.len(), ti.span) {
                continue;
            }
            // Resolve the bound's trait args through the impl's
            // param_map so any lowercase tyvars from the impl header
            // bind to the impl's fresh tyvars. For concrete args
            // (e.g. `where a: Conv(Int)`) this is the identity walk.
            // Empty when the bound trait has no args.
            let resolved_bound_args: Vec<Type> = trait_args
                .iter()
                .map(|te| self.resolve_type_expr(te, &mut impl_param_map))
                .collect();
            match impl_param_map.get(type_param) {
                Some(ty) => {
                    let resolved = self.apply(ty);
                    if let Type::Var(tv) = resolved {
                        impl_level_constraints.push((tv, *trait_name, resolved_bound_args.clone()));
                        // Record the bound's args under (tv, trait) so
                        // the call-site `bound_args` lookup in
                        // `dispatch_method_entry` finds them when the
                        // impl method gets dispatched. `instantiate_with_constraints`
                        // / `instantiate_method_entry` propagate these
                        // entries to fresh tyvars at each call site.
                        if !resolved_bound_args.is_empty() {
                            self.trait_arg_bindings
                                .insert((tv, *trait_name), resolved_bound_args.clone());
                        }
                        // Round 101 BROKEN: index in the EXPANDED-args
                        // space (see the `expanded_self_args` comment
                        // above), NOT the `target_param_names` space —
                        // the two diverge for alias targets.
                        if let Some(idx) = expanded_self_args
                            .iter()
                            .position(|slot| matches!(slot, Type::Var(v) if *v == tv))
                        {
                            impl_obligations_by_index.push((idx, *trait_name, resolved_bound_args));
                        }
                    }
                    // If resolved is concrete (shouldn't happen — impl_param_map
                    // only inserts fresh Var entries) treat it as a tautology
                    // and register no positional obligation.
                }
                None => {
                    self.error(Code::InvalidTypeAnnotation,
                        format!(
                            "type variable '{}' in impl-level where clause is not declared in the target type arguments; \
                             declare it as a target parameter: `trait {} for {}({}, ...)`",
                            resolve(*type_param),
                            resolve(ti.trait_name),
                            resolve(ti.target_type),
                            resolve(*type_param)
                        ),
                        ti.span,
                    );
                }
            }
        }
        if !impl_obligations_by_index.is_empty() {
            self.tables
                .impl_constraints
                .insert((trait_key, target_type), impl_obligations_by_index);
        }

        // GAP (round 35 F5): extraneous trait-impl methods — methods on
        // the impl whose names aren't declared in the trait — used to
        // get silently registered into the method_table. Reject each
        // method whose name is not in the trait's declared method list.
        //
        // GAP (round 35 F6): duplicate method names within a single
        // trait impl used to silently overwrite the earlier definition
        // in the method_table. Track a seen-set and reject the second
        // (and subsequent) occurrences.
        // Validate trait_args against the trait's declared parameter
        // count. `trait Foo(a, b)` must be implemented as `trait Foo(X, Y) for T`;
        // a parameterless trait must be `trait Foo for T` (no args).
        // When the count matches and the trait declared param-level
        // where bounds, verify that each supplied arg satisfies the
        // declared bounds — concrete types are checked now via
        // `verify_trait_obligation`; unresolved tyvars are added as
        // impl-level constraints so body checking sees them.
        let trait_info_clone = self.tables.traits.get(&trait_key).cloned();
        if let Some(trait_info) = &trait_info_clone {
            if ti.trait_args.len() != trait_info.params.len() {
                let expected = trait_info.params.len();
                self.error(
                    Code::ArityMismatch,
                    format!(
                        "trait '{}' expects {} {}, got {} in impl for '{}'",
                        resolve(ti.trait_name),
                        expected,
                        inference::plural(expected, "type argument", "type arguments"),
                        ti.trait_args.len(),
                        resolve(ti.target_type),
                    ),
                    ti.span,
                );
            } else if !ti.trait_args.is_empty() {
                // Resolve each supplied trait arg through impl_param_map
                // so lowercase names bind to the same fresh tyvars used
                // by the impl's methods. Stash for later verification by
                // `verify_trait_obligation` (closes the trait-args
                // soundness hole: `where a: TryInto(Int)` against a
                // `trait TryInto(Float) for String` impl must now reject).
                let resolved_trait_args: Vec<Type> = ti
                    .trait_args
                    .iter()
                    .map(|te| self.resolve_type_expr(te, &mut impl_param_map))
                    .collect();
                self.tables
                    .impl_trait_args
                    .insert((trait_key, target_type), resolved_trait_args.clone());
                if !trait_info.param_where_clauses.is_empty() {
                    for (param_name, bound_trait) in &trait_info.param_where_clauses {
                        let Some(idx) = trait_info.params.iter().position(|p| p == param_name)
                        else {
                            continue;
                        };
                        let Some(arg_ty) = resolved_trait_args.get(idx) else {
                            continue;
                        };
                        let applied = self.apply(arg_ty);
                        match &applied {
                            Type::Var(_) => {
                                // Deferred — the impl's own where clause path
                                // will propagate it; skip here.
                            }
                            _ => {
                                // Parameterless sub-bound: `trait Foo(a) where a: Display`
                                // — no args to thread.
                                self.verify_trait_obligation(*bound_trait, &[], &applied, ti.span);
                            }
                        }
                    }
                }
            }
        }

        // ── Associated-type bindings ───────────────────────────────
        //
        // For each `type Name = T` in the impl:
        //   - reject duplicate bindings;
        //   - resolve the bound type through impl_param_map (so impl-
        //     level type-vars are visible);
        //   - register the binding into the canonical assoc-binding
        //     registry under (trait_name, target_canonical_head, name)
        //     so the canonicaliser can reduce `<T as Trait>::Name`.
        // After processing the impl-supplied bindings, verify that
        // every assoc-type the trait declares has a binding here, and
        // that each binding satisfies the trait's declared bounds.
        let trait_assoc_types: Vec<AssocTypeInfo> = self
            .tables
            .traits
            .get(&trait_key)
            .map(|info| info.assoc_types.clone())
            .unwrap_or_default();
        // Index the impl's bindings by name for lookup + duplicate check.
        let mut impl_binding_map: HashMap<Symbol, Type> = HashMap::new();
        let mut seen_binding: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
        for binding in &ti.assoc_type_bindings {
            if !seen_binding.insert(binding.name) {
                self.error(
                    Code::DuplicateDeclaration,
                    format!(
                        "duplicate associated-type binding '{}' in impl of '{}' for '{}'",
                        resolve(binding.name),
                        resolve(ti.trait_name),
                        resolve(ti.target_type),
                    ),
                    binding.span,
                );
                continue;
            }
            // Reject bindings whose name is not declared in the trait.
            // (Trait info may be missing if the trait itself was
            // unknown — in that case the per-impl unknown-trait error
            // already fired and we silently skip the binding.)
            let known = trait_assoc_types.iter().any(|a| a.name == binding.name);
            if (!trait_assoc_types.is_empty() || self.tables.traits.contains_key(&trait_key))
                && self.tables.traits.contains_key(&trait_key)
                && !known
            {
                self.error(
                    Code::InvalidTraitImpl,
                    format!(
                        "associated type '{}' is not declared in trait '{}'",
                        resolve(binding.name),
                        resolve(ti.trait_name),
                    ),
                    binding.span,
                );
                continue;
            }
            let resolved = self.resolve_type_expr(&binding.ty, &mut impl_param_map);
            // Register into the canonical assoc-binding registry. The
            // registry keys on the canonical target head (so Range and
            // List collapse), parallel to method_table's
            // canonical_head routing above. Round 76 BROKEN T1:
            // the registry refuses self- or mutually-referential
            // bindings that would otherwise drive `canonicalize` into
            // infinite recursion (stack overflow). Skip the
            // `impl_binding_map` insertion on cycle so the per-method
            // signature check below reports a clean "missing required
            // assoc-type" rather than walking through a poisoned entry.
            match self.tables.resolver.register_assoc_binding(
                trait_key,
                target_type,
                binding.name,
                resolved.clone(),
            ) {
                Ok(()) => {
                    impl_binding_map.insert(binding.name, resolved);
                }
                Err(cycle) => {
                    let via_msg = if (cycle.via.0, cycle.via.1, cycle.via.2.as_str())
                        == (cycle.trait_name, cycle.head, cycle.assoc_name.as_str())
                    {
                        String::new()
                    } else {
                        format!(
                            " (cycle via <{} as {}>::{})",
                            cycle.via.1, cycle.via.0, cycle.via.2
                        )
                    };
                    self.error(Code::InvalidTraitImpl,
                        format!(
                            "associated type binding for <{} as {}>::{} is self-referential{via_msg}",
                            cycle.head, cycle.trait_name, cycle.assoc_name,
                        ),
                        binding.span,
                    );
                }
            }
        }
        // Verify each declared assoc-type has a binding and that the
        // binding satisfies any declared bounds.
        for assoc in &trait_assoc_types {
            let Some(bound_ty) = impl_binding_map.get(&assoc.name) else {
                self.error(
                    Code::InvalidTraitImpl,
                    format!(
                        "impl of '{}' for '{}' is missing required associated type '{}'",
                        resolve(ti.trait_name),
                        resolve(ti.target_type),
                        resolve(assoc.name),
                    ),
                    ti.span,
                );
                continue;
            };
            let applied = self.apply(bound_ty);
            for (bound_trait, bound_args) in &assoc.bounds {
                if !self.tables.traits.contains_key(bound_trait) {
                    self.error(
                        Code::UnknownTrait,
                        format!(
                            "unknown trait '{}' in bound on associated type '{}::{}'",
                            resolve(bound_trait.name),
                            resolve(ti.trait_name),
                            resolve(assoc.name),
                        ),
                        assoc.span,
                    );
                    continue;
                }
                // Resolve bound trait args at the impl site so any
                // lowercase tyvars from the impl are visible.
                let resolved_bound_args: Vec<Type> = bound_args
                    .iter()
                    .map(|te| self.resolve_type_expr(te, &mut impl_param_map))
                    .collect();
                // For type-variable bindings (e.g. `type Item = a` in a
                // parameterized impl), defer the obligation to the
                // pending-where path — verify_trait_obligation already
                // accepts a Var receiver and emits a clean diagnostic
                // for unresolved cases at finalize time.
                self.verify_trait_obligation(
                    *bound_trait,
                    &resolved_bound_args,
                    &applied,
                    assoc.span,
                );
            }
        }

        let trait_method_names: Option<std::collections::HashSet<Symbol>> = self
            .tables
            .traits
            .get(&trait_key)
            .map(|info| info.methods.iter().map(|(n, _)| *n).collect());
        let mut seen_impl_methods: std::collections::HashSet<Symbol> =
            std::collections::HashSet::new();
        for method in &ti.methods {
            if !seen_impl_methods.insert(method.name) {
                self.error(
                    Code::DuplicateDeclaration,
                    format!(
                        "duplicate method '{}' in trait impl '{} for {}'",
                        method.name, ti.trait_name, ti.target_type
                    ),
                    method.span,
                );
            }
            if let Some(names) = &trait_method_names
                && !names.contains(&method.name)
            {
                self.error(
                    Code::InvalidTraitImpl,
                    format!(
                        "method '{}' is not declared in trait '{}'",
                        method.name, ti.trait_name
                    ),
                    method.span,
                );
            }
        }

        let self_sym = intern("self");
        for method in &ti.methods {
            // Seed the method's param_map with both the impl-level target
            // tyvars AND the Self alias, so the method signature and body
            // see `a` as a concrete TyVar and `self` / `Self` resolve to
            // the parameterized self_type.
            let mut param_map = impl_param_map.clone();
            param_map.insert(intern("Self"), self_type.clone());
            let mut param_types = Vec::new();
            for (i, param) in method.params.iter().enumerate() {
                let ty = match param.kind {
                    ParamKind::Type => {
                        let name = match &param.pattern.kind {
                            PatternKind::Ident(n) => *n,
                            _ => {
                                unreachable!("parser guarantees `type` params use an Ident pattern")
                            }
                        };
                        let var = param_map
                            .entry(name)
                            .or_insert_with(|| self.fresh_var())
                            .clone();
                        Type::type_of(var)
                    }
                    ParamKind::Data => {
                        if let Some(te) = &param.ty {
                            self.resolve_type_expr(te, &mut param_map)
                        } else if i == 0
                            && matches!(&param.pattern.kind, PatternKind::Ident(n) if *n == self_sym)
                        {
                            // Bare `self` parameter in a trait impl: type it as the
                            // target type so field/method accesses on `self` are
                            // properly checked against the impl's target.
                            self_type.clone()
                        } else {
                            self.fresh_var()
                        }
                    }
                };
                param_types.push(ty);
            }
            let ret_type = if let Some(te) = &method.return_type {
                self.resolve_type_expr(te, &mut param_map)
            } else {
                self.fresh_var()
            };

            let fn_type = Type::Fun(param_types, Box::new(ret_type));

            // Two traits may each provide a method of one name for one
            // type (two modules' `Show` for `Int`): each impl is kept, by
            // its trait, and a call means the one whose trait the module
            // of the call sees (`select_visible_methods`); a call that
            // sees both is ambiguous.

            // Collect constraints for this method:
            //   (a) every impl-level constraint, verbatim (they reference
            //       impl_param_map TyVars which are also visible to the
            //       method's fn_type because param_map was cloned from
            //       impl_param_map);
            //   (b) every method-level `where` clause, resolved through
            //       the method's param_map — which sees BOTH impl-level
            //       binders (from the clone) AND method-local type annos.
            //
            // Method-level where clauses on trait-impl methods were
            // silently ignored by prior rounds — `register_trait_impl`
            // never consulted `method.where_clauses`. The impl-level
            // follow-up folds that latent gap into the same code path.
            let mut method_constraints = impl_level_constraints.clone();
            for wc in &method.where_clauses {
                // A bound the resolver resolved to nothing: it reported why.
                if wc.trait_res == Some(crate::defs::Res::Error) {
                    continue;
                }
                let type_param = &wc.type_param;
                let trait_args = &wc.trait_args;
                let Some(trait_key) = self
                    .named_trait(wc.trait_res, wc.trait_name)
                    .filter(|t| self.tables.traits.contains_key(t))
                else {
                    self.error(
                        Code::UnknownTrait,
                        format!(
                            "unknown trait '{}' in where clause on '{}.{}'",
                            resolve(wc.trait_name),
                            resolve(ti.target_type),
                            resolve(method.name)
                        ),
                        method.span,
                    );
                    continue;
                };
                let trait_name = &trait_key;
                // Round 101: bound arity must match the trait's declared
                // param count (see check_where_bound_arity — it dedupes
                // against the identical diagnostic the method body's
                // check_fn_body_with_name pass emits for the same span).
                if !self.check_where_bound_arity(*trait_name, trait_args.len(), method.span) {
                    continue;
                }
                // Resolve the bound's trait args through the method's
                // param_map (which sees both the impl-level binders
                // and any method-local type annos). Empty for
                // parameterless trait bounds. Without storing these
                // alongside the (tv, trait) pair, downstream
                // `verify_trait_obligation` would fall back to the
                // bare "trait implemented" check — letting
                // `where a: Conv(Int)` accept any `Conv(*) for ...`.
                let resolved_bound_args: Vec<Type> = trait_args
                    .iter()
                    .map(|te| self.resolve_type_expr(te, &mut param_map))
                    .collect();
                match param_map.get(type_param) {
                    Some(ty) => {
                        let resolved = self.apply(ty);
                        if let Type::Var(tv) = resolved {
                            method_constraints.push((tv, *trait_name, resolved_bound_args.clone()));
                            if !resolved_bound_args.is_empty() {
                                self.trait_arg_bindings
                                    .insert((tv, *trait_name), resolved_bound_args.clone());
                            }
                        }
                    }
                    None => {
                        // Give the user the full "declare it in the sig or
                        // target" hint — this is the same spirit as the
                        // register_fn_decl error at mod.rs:5276.
                        self.error(Code::InvalidTypeAnnotation,
                            format!(
                                "type variable '{}' in where clause on '{}.{}' is not declared in the impl target \
                                 arguments or in the method's parameter annotations",
                                resolve(*type_param),
                                resolve(ti.target_type),
                                resolve(method.name)
                            ),
                            method.span,
                        );
                    }
                }
            }

            // Populate method_table. Store BOTH the raw template type
            // AND the collected constraints so receiver-method dispatch
            // sites can instantiate both through a shared substitution
            // via instantiate_method_entry, then push the instantiated
            // constraints into pending_where_constraints for the
            // finalize-pass check.
            // GAP-1: store the per-method span (not `ti.span`, which
            // points at the impl block header) so the
            // `validate_trait_impls` signature-mismatch unify error
            // lands on the offending method's signature line.
            // The method of another trait this impl's method shares its
            // name with, for its type, is kept by its trait: a builtin
            // trait's method of a builtin type (`display` of Int) has no
            // trait in the method table.
            if let Some(existing) = self
                .tables
                .method_table
                .get(&(target_type, method.name))
                .cloned()
                && let Some(existing_trait) = existing.trait_name.or_else(|| {
                    crate::defs::builtin_trait_of_method(&resolve(method.name))
                        .and_then(|t| self.trait_key(t.0))
                })
                && existing_trait != trait_key
            {
                self.tables
                    .trait_methods
                    .entry((target_type, method.name, existing_trait))
                    .or_insert(MethodEntry {
                        trait_name: Some(existing_trait),
                        ..existing
                    });
            }
            let entry = MethodEntry {
                method_type: fn_type.clone(),
                span: method.span,
                is_auto_derived: ti.is_auto_derived,
                trait_name: Some(trait_key),
                method_constraints: method_constraints.clone(),
            };
            if !ti.is_auto_derived {
                self.tables
                    .trait_methods
                    .insert((target_type, method.name, trait_key), entry.clone());
            }
            self.tables
                .method_table
                .insert((target_type, method.name), entry);

            // Bind the method in the module's scope under its impl key
            // (`impl_method_key`), where `check_decl_bodies` checks its
            // body; attach the same constraints to the scheme so the
            // body's check_fn_body_with_name sees them as active. The key
            // is built from the canonical target (`List` for `Range`).
            let key = impl_method_key(target_type, method.name);
            let mut scheme = self.generalize(env, &fn_type);
            for (tv, trait_name, _trait_args) in &method_constraints {
                if !scheme.constraints.contains(&(*tv, *trait_name)) {
                    scheme.constraints.push((*tv, *trait_name));
                }
            }
            env.define(key, scheme);
        }
    }

    /// Round 60 G1, extended round 101: a where-clause bound must
    /// supply exactly the trait's declared number of type arguments.
    /// Returns `true` when the arity matches (or the trait is unknown
    /// — the caller has already reported that).
    ///
    /// Both directions are soundness-relevant, not just hygiene: the
    /// parameterized-trait verification in `verify_trait_obligation`
    /// only runs its round-58 positional arg-compatibility zip when
    /// `impl_args.len() == bound_trait_args.len()`, so a
    /// length-mismatched bound like `where a: Cast(Int, String)` on a
    /// one-param `trait Cast(to)` silently degraded to a bare
    /// "implements Cast" check — matching (and dispatching through!)
    /// any `Cast(*)` impl. Zero args on a parameterized trait
    /// additionally leaves the implied params unresolved (the original
    /// round-60 direction).
    ///
    /// Callers: the fn-level where-clause loop in
    /// `check_fn_body_with_name`, and the impl-level and method-level
    /// where-clause loops in `register_trait_impl`. Method-level
    /// bounds pass through BOTH the registration site and the body
    /// check, so the emit dedupes on (message, span) to keep the
    /// diagnostic single.
    pub(super) fn check_where_bound_arity(
        &mut self,
        trait_name: TraitKey,
        got: usize,
        span: Span,
    ) -> bool {
        let Some(info) = self.tables.traits.get(&trait_name) else {
            return true;
        };
        let n = info.params.len();
        if got == n {
            return true;
        }
        let msg = format!(
            "trait '{}' expects {} {} in bound, got {}",
            resolve(trait_name.name),
            n,
            inference::plural(n, "type argument", "type arguments"),
            got
        );
        if !self
            .errors
            .iter()
            .any(|e| e.message == msg && e.span == span)
        {
            self.error(Code::ArityMismatch, msg, span);
        }
        false
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
        Type::Record(_, fields) => fields.iter().for_each(|(_, t)| row_tail_vars(t, out)),
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

/// The key an impl's method is bound under in its module's scope, where
/// its body is checked: the method of that type, two types of one name
/// (a module's own `Pt` and an imported one) apart.
fn impl_method_key(target: TypeRef, method: Symbol) -> Symbol {
    intern(&format!("{}#{}.{method}", target.name, target.id.0.0))
}

pub(crate) use crate::types::canonical::{canonical_head, head_of_canon as head_of};

/// Collect the set of variable names bound by a pattern.
pub(super) fn collect_pattern_vars(pat: &Pattern) -> Vec<Symbol> {
    match &pat.kind {
        PatternKind::Ident(name) => vec![*name],
        PatternKind::Tuple(pats) => pats.iter().flat_map(collect_pattern_vars).collect(),
        PatternKind::List(pats, rest) => {
            let mut vars: Vec<Symbol> = pats.iter().flat_map(collect_pattern_vars).collect();
            if let Some(rest_pat) = rest {
                vars.extend(collect_pattern_vars(rest_pat));
            }
            vars
        }
        PatternKind::Constructor { args: pats, .. } => {
            pats.iter().flat_map(collect_pattern_vars).collect()
        }
        PatternKind::Record { fields, .. } => {
            let mut vars: Vec<Symbol> = Vec::new();
            for (field_name, _, sub_pat) in fields {
                if let Some(p) = sub_pat {
                    vars.extend(collect_pattern_vars(p));
                } else {
                    // Shorthand field `{ x }` binds `x`
                    vars.push(*field_name);
                }
            }
            vars
        }
        PatternKind::AnonRecord { fields, rest } => {
            let mut vars: Vec<Symbol> = Vec::new();
            for (field_name, _, sub_pat) in fields {
                if let Some(p) = sub_pat {
                    vars.extend(collect_pattern_vars(p));
                } else {
                    vars.push(*field_name);
                }
            }
            if let Some((r, _)) = rest {
                vars.push(*r);
            }
            vars
        }
        PatternKind::Or(alts) => {
            // Return vars from first alt (they should all be the same after validation)
            alts.first().map(collect_pattern_vars).unwrap_or_default()
        }
        PatternKind::Map(entries) => entries
            .iter()
            .flat_map(|(_, p)| collect_pattern_vars(p))
            .collect(),
        PatternKind::Wildcard
        | PatternKind::Int(_)
        | PatternKind::Float(_)
        | PatternKind::Bool(_)
        | PatternKind::StringLit(..)
        | PatternKind::Range(_, _)
        | PatternKind::FloatRange(_, _)
        | PatternKind::Pin(_) => vec![],
    }
}

/// Synthesize `TraitDecl` AST nodes for the five built-in traits.
///
/// Round 62 (item 3 of type-design improvements) — built-in trait
/// registration was previously a separate code path that hand-rolled
/// `TraitInfo` entries directly, bypassing the duplicate-method check,
/// supertrait/where-clause processing, and any future feature added to
/// `register_trait_decl`. This function returns the same `TraitDecl`
/// shape a parser would produce for hand-written user code, so the
/// unified `register_trait_decl_inner` pipeline handles built-ins and
/// user traits identically.
///
/// The five built-ins:
/// - `Display`: `fn display(self) -> String` (signature only).
/// - `Compare`: `fn compare(self, other) -> Int` (signature only). The
///   second parameter is left untyped so `register_trait_decl_inner`
///   allocates a fresh TyVar — matching the pre-unification shape that
///   used `Type::Fun([fresh, fresh], Int)`.
/// - `Equal`:   `fn equal(self, other) -> Bool` (signature only).
/// - `Hash`:    `fn hash(self) -> Int` (signature only).
/// - `Error: Display { fn message(self) -> String { self.display() } }`.
///   Carries a real default body so `synthesize_default_methods` can
///   clone `self.display()` into impls that omit `message`.
fn builtin_trait_decls() -> Vec<TraitDecl> {
    let dummy_span = Span::BUILTIN;
    let self_sym = intern("self");
    let other_sym = intern("other");

    fn self_param(self_sym: Symbol, span: Span) -> Param {
        Param {
            kind: ParamKind::Data,
            pattern: Pattern::new(PatternKind::Ident(self_sym), span),
            ty: None,
        }
    }
    fn other_param(other_sym: Symbol, span: Span) -> Param {
        // Leave `other`'s type as None so register_trait_decl_inner
        // allocates a fresh TyVar — matches the pre-unification
        // `Type::Fun([fresh_var(), fresh_var()], ret)` shape exactly.
        Param {
            kind: ParamKind::Data,
            pattern: Pattern::new(PatternKind::Ident(other_sym), span),
            ty: None,
        }
    }
    fn unit_body(span: Span) -> Expr {
        Expr::new(ExprKind::Unit, span)
    }
    fn named_ret(name: &str, span: Span) -> Option<TypeExpr> {
        Some(TypeExpr::new(
            TypeExprKind::Named {
                module: None,
                name: intern(name),
                name_span: span,
            },
            span,
        ))
    }
    fn sig_only_method(name: &str, params: Vec<Param>, ret: &str, span: Span) -> FnDecl {
        FnDecl {
            name: intern(name),
            params,
            return_type: named_ret(ret, span),
            where_clauses: Vec::new(),
            body: unit_body(span),
            is_pub: true,
            span,
            // Synthesized built-in: no source identifier — mirror `span`.
            name_span: span,
            is_recovery_stub: false,
            is_signature_only: true,
            doc: None,
        }
    }

    // Error.message default body: `self.display()`
    let error_default_body = {
        let self_ident = Expr::new(ExprKind::Ident(self_sym), dummy_span);
        let field_access = Expr::new(
            ExprKind::FieldAccess(Box::new(self_ident), intern("display"), dummy_span),
            dummy_span,
        );
        Expr::new(
            ExprKind::Call(Box::new(field_access), Vec::new()),
            dummy_span,
        )
    };
    let error_message_fn = FnDecl {
        name: intern("message"),
        params: vec![self_param(self_sym, dummy_span)],
        return_type: named_ret("String", dummy_span),
        where_clauses: Vec::new(),
        body: error_default_body,
        is_pub: true,
        span: dummy_span,
        // Synthesized built-in `Error.message`: no source ident — mirror span.
        name_span: dummy_span,
        is_recovery_stub: false,
        is_signature_only: false,
        doc: None,
    };

    vec![
        // trait Display { fn display(self) -> String }
        TraitDecl {
            name: intern("Display"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "display",
                vec![self_param(self_sym, dummy_span)],
                "String",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Compare { fn compare(self, other) -> Int }
        TraitDecl {
            name: intern("Compare"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "compare",
                vec![
                    self_param(self_sym, dummy_span),
                    other_param(other_sym, dummy_span),
                ],
                "Int",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Equal { fn equal(self, other) -> Bool }
        TraitDecl {
            name: intern("Equal"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "equal",
                vec![
                    self_param(self_sym, dummy_span),
                    other_param(other_sym, dummy_span),
                ],
                "Bool",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Hash { fn hash(self) -> Int }
        TraitDecl {
            name: intern("Hash"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "hash",
                vec![self_param(self_sym, dummy_span)],
                "Int",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Error: Display { fn message(self) -> String { self.display() } }
        TraitDecl {
            name: intern("Error"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: vec![TraitRef {
                module: None,
                name: intern("Display"),
                args: Vec::new(),
                span: dummy_span,
                res: None,
            }],
            param_where_clauses: Vec::new(),
            methods: vec![error_message_fn],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
    ]
}

/// Register built-in trait declarations (Display/Compare/Equal/Hash/Error)
/// and their auto-derived impls for primitives and builtin containers.
///
/// This is the single source of truth for derive policy. The builtin
/// environment every check starts from is built with it, so no two
/// checks diverge on which types implement which traits.
///
/// Round 62 (item 3 of type-design improvements): the trait-decl
/// registration step now flows through the same code path as user
/// `trait X { ... }` declarations. See `builtin_trait_decls` for the
/// synthesized AST nodes and `register_trait_decl_inner` for the
/// shared body. The auto-derive policy below remains imperative — it's
/// policy (which types pre-stamp Display/Compare/Equal/Hash impls), not
/// declaration.
///
/// Derive policy:
/// - `Int`, `Float`, `Bool`, `String`, `()`, `List` get all four
///   built-in traits (Equal, Compare, Hash, Display).
/// - `Tuple`, `Map`, `Set` get Equal/Hash/Display only — the VM's
///   `compare()` (src/vm/arithmetic.rs) does not support ordering for
///   these, so registering Compare would type-check code that then
///   panics at runtime.
/// - `Option`, `Result` get Equal/Hash/Display only. They wrap generic
///   parameters; the auto-derived methods are stored as polymorphic
///   templates and instantiated at each call site. Compare is
///   excluded because ordering on Variants is limited to same-name
///   variants at runtime.
pub(super) fn register_builtin_trait_impls(checker: &mut TypeChecker) {
    // ── Register built-in trait declarations through the unified
    //    register_trait_decl_inner pipeline. Same code path user
    //    `trait X { fn ... }` declarations take, minus the
    //    BUILTIN_TRAIT_NAMES redefinition check (which is keyed off
    //    user input and lives on register_trait_decl_user). ─────────
    for td in builtin_trait_decls() {
        checker.register_trait_decl_inner(&td);
    }

    // ── Register auto-derived impls ─────────────────────────────
    // Error is intentionally excluded from auto-derive: user code and
    // stdlib must `trait Error for XyzError { ... }` explicitly. Only
    // Equal/Compare/Hash/Display are auto-derived for the built-in
    // types below.
    let all_auto_traits: &[&str] = BUILTIN_AUTO_DERIVED_TRAIT_NAMES;
    let non_ordering_traits: &[&str] = &["Equal", "Hash", "Display"];

    // Primitives + List: all four auto-derived traits.
    register_auto_derived_impls_for(
        checker,
        // Round 75 TYPE-3 LATENT: canonical key for the unit type is
        // "Unit" (matches canonical_name(Type::Unit) and
        // dispatch_type_for_value(Value::Unit)); an impl target `()`
        // names it too.
        &["Int", "Float", "Bool", "String", "Unit"],
        all_auto_traits,
    );
    register_auto_derived_impls_for(checker, &["List"], all_auto_traits);
    // Tuple/Map/Set: Equal/Hash/Display only.
    register_auto_derived_impls_for(checker, &["Tuple", "Map", "Set"], non_ordering_traits);
    // Option/Result: Equal/Hash/Display only (generic wrappers, stored
    // as polymorphic templates).
    register_auto_derived_impls_for(checker, &["Option", "Result"], non_ordering_traits);

    // ── Built-in enums + records that flow through synth ────────────
    //
    // Round-62 follow-up: extend auto-derive coverage from primitives
    // and the four parametric containers to every built-in enum and
    // record registered in `register_builtins`. Each entry below
    // pre-stamps `trait_impl_set` for the policy-permitted traits so
    // `synthesize_auto_derive_impls` knows which (trait, type) pairs
    // are allowed to receive a synthesized impl method,
    // and `field_type_supports_trait` returns true for fields that
    // reference these types (e.g. `Option(DateTime)` on `FileStat`).
    //
    // Trait selection mirrors the existing patterns:
    //   - All four traits where every variant / field is orderable.
    //   - Equal/Hash/Display only when a field type lacks Compare
    //     (e.g. records carrying a `Map` field — `Response`/`Request`).
    //   - Skip entirely for types whose fields can't satisfy any of
    //     the four (e.g. `ChannelOp` carries `Channel(_)`); the synth
    //     pass's `field_type_supports_trait` gate would block synth
    //     anyway and a stamp without a runtime impl would surface as
    //     a misleading "no method" error.
    //
    // The synth pass (`synthesize_auto_derive_impls`) discovers these
    // types via a uniform walk over `self.tables.enums` / `self.tables.records` and
    // emits the same `TraitImpl` AST that user-declared types receive,
    // producing real impl methods at compile time. After this round,
    // `Op::CallMethod` always finds an impl method for built-in
    // enum/record receivers, and the Variant /
    // Record arms in `dispatch_trait_method` are dead.

    // Built-in enums — non-generic, all four traits.
    //
    // Round 73 L4 (LATENT, dead-code dedup): the stdlib error-enum
    // names are no longer hand-rolled here; they're sourced from
    // `module::builtin_error_enum_variants_with_arity()` — the
    // single authoritative registry. Adding/renaming a typed-error
    // enum no longer requires a parallel-array edit at this site.
    let error_enum_names: Vec<&'static str> =
        crate::module::builtin_error_enum_variants_with_arity()
            .iter()
            .map(|(name, _)| *name)
            .collect();
    let mut all_enum_names: Vec<&'static str> = vec!["Step", "ChannelResult", "Method", "Weekday"];
    // Stdlib error enums: Display + Error are already registered in
    // `errors.rs`; re-stamping with all_auto_traits adds the missing
    // Equal/Compare/Hash without disturbing the existing entries
    // (insert is idempotent).
    all_enum_names.extend(error_enum_names);
    register_auto_derived_impls_for(checker, &all_enum_names, all_auto_traits);

    // Built-in records — Date/Time/DateTime/Duration/Instant/FileStat
    // and Weekday are already stamped by `register_auto_derived_impls_for`
    // calls in `time.rs` / `fs.rs` (kept there so the per-module file
    // owns its derive policy). Re-stamping is harmless if any drift
    // appears here.

    // HTTP records carry a `Map(String, String)` headers field. Map
    // has no Compare (see line 4359), so Compare is excluded.
    register_auto_derived_impls_for(checker, &["Response", "Request"], non_ordering_traits);

    // ChannelOp variants carry `Channel(_)` which has no Compare /
    // Equal / Hash / Display impl. No stamps; the synth pass walks
    // `self.tables.enums` and skips this entry because every trait fails
    // the field-support gate.

    // Bytes: Display only. The generic `dispatch_trait_method` arm at
    // src/vm/dispatch.rs:309 routes `display` to `display_value`, and
    // `Value::Bytes` already has a runtime Display impl
    // (`format_bytes_preview` at src/value.rs:1364 — short hex preview
    // + length, e.g. `bytes(de ad be ef, length: 4)`). Equal exists as
    // `bytes.eq(a, b)` but is not auto-derived through the trait
    // surface; Compare / Hash are intentionally omitted (Bytes is an
    // opaque resource, not an ordered key type — users wanting to
    // compare or hash should `bytes.to_hex` first).
    register_auto_derived_impls_for(checker, &["Bytes"], &["Display"]);

    // TcpListener / TcpStream are registered in BUILTIN_TYPES so the
    // trait-impl-target gate gives an orphan-rule rejection (rather
    // than "type not declared") if a user tries to add their own
    // impls, but no built-in trait impls are stamped — they remain
    // unprintable opaque resources.
}

/// Register auto-derived trait impls and method-table entries for a
/// group of types against a set of trait names. Shared helper used by
/// `register_builtin_trait_impls` and the `time` builtin module so
/// the set of derived methods stays consistent.
///
/// The four built-in trait methods are always considered. A method is
/// registered only when its parent trait appears in `trait_names`:
/// - `display` ← Display
/// - `equal`   ← Equal
/// - `compare` ← Compare
/// - `hash`    ← Hash
pub(super) fn register_auto_derived_impls_for(
    checker: &mut TypeChecker,
    type_names: &[&str],
    trait_names: &[&str],
) {
    let dummy_span = Span::BUILTIN;
    let has_display = trait_names.contains(&"Display");
    let has_equal = trait_names.contains(&"Equal");
    let has_compare = trait_names.contains(&"Compare");
    let has_hash = trait_names.contains(&"Hash");
    for type_name in type_names {
        for trait_name in trait_names {
            checker
                .tables
                .trait_impl_set
                .insert((TraitKey::builtin(trait_name), TypeRef::builtin(type_name)));
        }
        // Build method entries only for traits in `trait_names`.
        let mut methods: Vec<(&str, Type)> = Vec::with_capacity(4);
        if has_display {
            methods.push((
                "display",
                Type::Fun(vec![checker.fresh_var()], Box::new(Type::String)),
            ));
        }
        if has_equal {
            methods.push((
                "equal",
                Type::Fun(
                    vec![checker.fresh_var(), checker.fresh_var()],
                    Box::new(Type::Bool),
                ),
            ));
        }
        if has_compare {
            methods.push((
                "compare",
                Type::Fun(
                    vec![checker.fresh_var(), checker.fresh_var()],
                    Box::new(Type::Int),
                ),
            ));
        }
        if has_hash {
            methods.push((
                "hash",
                Type::Fun(vec![checker.fresh_var()], Box::new(Type::Int)),
            ));
        }
        for (method_name, method_type) in &methods {
            checker.tables.method_table.insert(
                (TypeRef::builtin(type_name), intern(method_name)),
                MethodEntry {
                    method_type: method_type.clone(),
                    span: dummy_span,
                    is_auto_derived: true,
                    trait_name: None,
                    method_constraints: Vec::new(),
                },
            );
        }
    }
}

impl TypeChecker {
    /// For each type and method name that the impls of two or more traits
    /// provide, put in the method table the one the module checked
    /// means: the one whose trait the module sees, by `scope`: a trait it
    /// declares, a trait it names by an import, a trait of a module it
    /// imports, or a builtin trait. Where it sees none or several of the
    /// traits, a call is ambiguous (`ambiguous_methods`). Run once the
    /// module's impls are registered.
    fn select_visible_methods(&mut self) {
        let mut providers: HashMap<(TypeRef, Symbol), Vec<TraitKey>> = HashMap::new();
        for (ty, method, t) in self.tables.trait_methods.keys() {
            providers.entry((*ty, *method)).or_default().push(*t);
        }
        let seen: std::collections::HashSet<TraitKey> = providers
            .values()
            .flatten()
            .copied()
            .filter(|t| self.sees_trait(*t))
            .collect();
        let sees = |t: &TraitKey| seen.contains(t);
        self.ambiguous_methods.clear();
        for ((ty, method), mut traits) in providers {
            if traits.len() < 2 {
                continue;
            }
            traits.sort_by_key(|t| self.show_trait(*t));
            let seen: Vec<TraitKey> = traits.iter().copied().filter(|t| sees(t)).collect();
            match seen.as_slice() {
                [t] => {
                    let entry = self.tables.trait_methods[&(ty, method, *t)].clone();
                    self.tables.method_table.insert((ty, method), entry);
                }
                [] => {
                    self.ambiguous_methods.insert((ty, method), traits);
                }
                _ => {
                    self.ambiguous_methods.insert((ty, method), seen);
                }
            }
        }
    }

    /// Keep what the check learned of each written impl method (its
    /// body's type) for the modules checked later.
    fn keep_trait_methods(&mut self) {
        for ((ty, method), entry) in &self.tables.method_table {
            if let Some(t) = entry.trait_name
                && let Some(kept) = self.tables.trait_methods.get_mut(&(*ty, *method, t))
            {
                *kept = entry.clone();
            }
        }
    }
}

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
    let env = checker.check_program_in(program, env);
    checker.keep_trait_methods();
    checker.report_private_in_schemes(program, &env);
    checker.report_unknown_pub_let_types(program, &env);
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
        diagnostics: checker.errors,
        top_level,
    }
}

// ── The builtin environment ─────────────────────────────────────────

/// What every check starts from: the checker and the environment once
/// the builtin functions, types, traits and derived impls are registered.
/// Built once per thread and shared by every check made on it (see
/// [`builtin_env`]).
struct BuiltinEnv {
    /// A checker with no tables: what each check starts from.
    checker: TypeChecker,
    /// The builtin types, traits, impls and type variables: what a
    /// session's tables start from.
    tables: Tables,
    /// The scope of the builtin names: the parent of every program's
    /// top-level scope.
    root: Rc<TypeEnv>,
    /// The derived impls of the builtin types (`Display`, `Equal`, ...
    /// for `IoError`, `Weekday`, ...), checked: a program compiles them
    /// once, see [`builtin_derived_impls`].
    impls: Rc<Vec<Decl>>,
}

impl BuiltinEnv {
    fn build() -> Self {
        let mut checker = TypeChecker::new();
        let mut env = TypeEnv::new();
        // With no `current_package`, every builtin decl is stamped with
        // the `__builtin__` sentinel by `defining_package()`, which the
        // orphan rule relies on: `trait Display for List(a)` in user code
        // must not look trait-local.
        checker.register_builtins(&mut env);
        register_builtin_trait_impls(&mut checker);
        // Each builtin variant is bound as `Enum.Variant` too, which is
        // what a resolved use of it reads.
        let mut variants: Vec<(Symbol, Scheme)> = Vec::new();
        for (enum_name, info) in &checker.tables.enums {
            for variant in &info.variants {
                if let Some(scheme) = env.lookup(variant.name) {
                    variants.push((
                        intern(&format!("{enum_name}.{}", variant.name)),
                        scheme.clone(),
                    ));
                }
            }
        }
        for (name, scheme) in variants {
            env.define(name, scheme);
        }
        // Every builtin scheme is generalized, so the bodies below are
        // checked without walking the builtin scope for free variables.
        env.closed = env.free_vars(&checker).is_empty();
        // Derive the builtin types' impls once, as a check of a program
        // with no declarations would, and check their bodies. They are
        // registered in a scope over the builtin one, so the scopes their
        // bodies open share the builtin scope instead of copying it, and
        // what they bind is then moved into the builtin scope.
        let root = Rc::new(env);
        let mut scope = TypeEnv::child_of(root.clone());
        let mut impls = Vec::new();
        checker.synthesize_auto_derive_impls(&mut impls);
        for decl in &impls {
            if let Decl::TraitImpl(ti) = decl {
                if let Some(target) = checker.impl_target(ti) {
                    checker.tables.builtin_derived.insert(target);
                }
                checker.register_trait_impl(ti, &mut scope);
            }
        }
        checker.check_decl_bodies(&mut impls, &scope);
        checker.finalize_deferred_checks();
        debug_assert!(
            checker.errors.is_empty(),
            "the builtin derived impls check: {:?}",
            checker.errors
        );
        checker.errors.clear();
        let bindings = std::mem::take(&mut scope.bindings);
        drop(scope);
        let mut env = Rc::try_unwrap(root).expect("no scope over the builtin scope is left");
        env.bindings.extend(bindings);
        env.closed = false;
        env.closed = env.free_vars(&checker).is_empty();
        let tables = std::mem::take(&mut checker.tables);
        BuiltinEnv {
            checker,
            tables,
            root: Rc::new(env),
            impls: Rc::new(impls),
        }
    }

    /// A fresh checker, with no tables, and an empty top-level scope over
    /// the builtins.
    fn start(&self) -> (TypeChecker, TypeEnv) {
        (self.checker.clone(), TypeEnv::child_of(self.root.clone()))
    }
}

thread_local! {
    /// The builtin environment of this thread, with the interner
    /// generation it was built in. Symbols are per thread, and
    /// `intern::reset` invalidates them, so the cache is too.
    static BUILTIN_ENV: std::cell::RefCell<Option<(u64, Rc<BuiltinEnv>)>> =
        const { std::cell::RefCell::new(None) };
}

/// The derived impls of the builtin types, checked. Every program
/// compiles them once (`Compiler::compile_program`), so a method call on
/// a builtin type's value finds its method.
pub fn builtin_derived_impls() -> Rc<Vec<Decl>> {
    builtin_env().impls.clone()
}

/// Whether the builtin scope binds `name` (`int.parse`).
pub(super) fn builtin_env_has(name: Symbol) -> bool {
    builtin_env().root.bindings.contains_key(&name)
}

/// The builtin environment, built on first use and after each
/// `intern::reset`.
fn builtin_env() -> Rc<BuiltinEnv> {
    let generation = crate::intern::generation();
    if let Some(env) = BUILTIN_ENV.with(|cell| match &*cell.borrow() {
        Some((built_in, env)) if *built_in == generation => Some(env.clone()),
        _ => None,
    }) {
        return env;
    }
    let env = Rc::new(BuiltinEnv::build());
    BUILTIN_ENV.with(|cell| *cell.borrow_mut() = Some((generation, env.clone())));
    env
}

/// The scheme of the builtin definition `def`: the one the builtin scope
/// binds under its name (`println`, `list.map`, `Weekday.Monday`,
/// `Option`).
fn builtin_scheme(def: &crate::defs::Def) -> Option<Scheme> {
    let key = match def.kind {
        crate::defs::DefKind::Variant { ty, .. } => {
            let enum_name = names::builtin_def(ty.0)?.name;
            format!("{enum_name}.{}", def.name)
        }
        _ => match def.module.builtin_name() {
            Some(module) if !def.is_type() => format!("{module}.{}", def.name),
            _ => resolve(def.name),
        },
    };
    builtin_env().root.lookup(intern(&key)).cloned()
}

/// The builtin names, as the resolver enters them: every name the builtin
/// scope binds, each builtin enum with the arity of each variant, and the
/// builtin traits.
pub(super) struct BuiltinNames {
    pub bindings: Vec<Symbol>,
    pub enums: Vec<(Symbol, Vec<(Symbol, usize)>)>,
    pub traits: Vec<Symbol>,
}

pub(super) fn builtin_names() -> BuiltinNames {
    let env = builtin_env();
    let mut bindings: Vec<Symbol> = env.root.bindings.keys().copied().collect();
    bindings.sort_by_key(|name| resolve(*name));
    let mut enums: Vec<(Symbol, Vec<(Symbol, usize)>)> = env
        .tables
        .enums
        .iter()
        .map(|(name, info)| {
            let variants = info
                .variants
                .iter()
                .map(|v| (v.name, v.field_types.len()))
                .collect();
            (name.name, variants)
        })
        .collect();
    enums.sort_by_key(|(name, _)| resolve(*name));
    let mut traits: Vec<Symbol> = env.tables.traits.keys().map(|t| t.name).collect();
    traits.sort_by_key(|name| resolve(*name));
    BuiltinNames {
        bindings,
        enums,
        traits,
    }
}

/// Return a map of builtin qualified names to their type signature strings.
/// Used by the LSP to show type info in completions.
pub fn builtin_type_signatures() -> std::collections::HashMap<String, String> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut sigs = std::collections::HashMap::new();
    for (name, scheme) in &env.bindings {
        let name_str = resolve(*name);
        if name_str.contains('.') {
            let ty = checker.instantiate(scheme);
            sigs.insert(name_str, format!("{ty}"));
        }
    }
    sigs
}

/// Snapshot every nominal record / enum name registered by
/// `register_builtins`. Used by the round-82 parity test in
/// `tests/typecheck/round82_stdlib_types_registry_tests.rs` to lock the central
/// registry (`module::BUILTIN_STDLIB_TYPE_NAMES`) against runtime
/// state. Routes through a fresh `TypeChecker` so the snapshot reflects
/// every per-module `register` callback's effect on `checker.tables.records`
/// / `checker.tables.enums` — including the `Result`/`Option`/`Step`/
/// `ChannelResult`/`ChannelOp` prelude enums declared directly in
/// `register_builtins` itself.
///
/// Each entry's category (`"record"` vs `"enum"`) is preserved so the
/// test can render a useful diff when the sets diverge.
pub fn registered_builtin_type_names() -> Vec<(String, &'static str)> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for ty in checker.tables.records.keys() {
        out.push((resolve(ty.name), "record"));
    }
    for ty in checker.tables.enums.keys() {
        out.push((resolve(ty.name), "enum"));
    }
    out.sort();
    out
}

/// Return a map of builtin qualified names to their parameter-name lists,
/// indexed in argument order. Sibling registry to
/// `builtin_type_signatures`: signatures carry only types (the rendered
/// `Fn(T1, T2) -> R` form has no `name:` per param), so the LSP
/// `signatureHelp` handler — which needs `ParameterInformation` per
/// arg to drive active-arg highlighting — has nowhere else to look up
/// names.
///
/// Round-71 DX-4 fix (audit): pre-round, `signature_help.rs` emitted
/// `parameters: vec![]` for every builtin call site, so the active-arg
/// highlight was broken across the entire stdlib surface. This
/// registry seeds names for the most-used `list.*`, `string.*`,
/// `map.*`, `set.*`, `io.*` modules. Builtins not present here surface
/// as before with empty parameter info — a follow-up round can extend
/// the coverage.
///
/// Names are deliberately compact (`xs`, `f`, `k`, `v`, `s`, `path`)
/// to mirror the doc comments in `src/typechecker/builtins/*.rs`. A
/// follow-up audit can normalize wording.
pub fn builtin_param_names() -> std::collections::HashMap<&'static str, &'static [&'static str]> {
    let entries: &[(&'static str, &'static [&'static str])] = &[
        // ── list.* ───────────────────────────────────────────────
        ("list.map", &["xs", "f"]),
        ("list.filter", &["xs", "pred"]),
        ("list.fold", &["xs", "init", "f"]),
        ("list.each", &["xs", "f"]),
        ("list.find", &["xs", "pred"]),
        ("list.zip", &["xs", "ys"]),
        ("list.flatten", &["xs"]),
        ("list.sort_by", &["xs", "key"]),
        ("list.flat_map", &["xs", "f"]),
        ("list.filter_map", &["xs", "f"]),
        ("list.any", &["xs", "pred"]),
        ("list.all", &["xs", "pred"]),
        ("list.fold_until", &["xs", "init", "f"]),
        ("list.unfold", &["seed", "f"]),
        ("list.append", &["xs", "x"]),
        ("list.prepend", &["xs", "x"]),
        ("list.concat", &["xs", "ys"]),
        ("list.get", &["xs", "i"]),
        ("list.set", &["xs", "i", "x"]),
        ("list.take", &["xs", "n"]),
        ("list.drop", &["xs", "n"]),
        ("list.enumerate", &["xs"]),
        ("list.head", &["xs"]),
        ("list.tail", &["xs"]),
        ("list.last", &["xs"]),
        ("list.reverse", &["xs"]),
        ("list.sort", &["xs"]),
        ("list.unique", &["xs"]),
        ("list.contains", &["xs", "x"]),
        ("list.length", &["xs"]),
        ("list.group_by", &["xs", "key"]),
        ("list.index_of", &["xs", "x"]),
        ("list.remove_at", &["xs", "i"]),
        ("list.min_by", &["xs", "key"]),
        ("list.max_by", &["xs", "key"]),
        ("list.sum", &["xs"]),
        ("list.sum_float", &["xs"]),
        ("list.product", &["xs"]),
        ("list.product_float", &["xs"]),
        ("list.scan", &["xs", "init", "f"]),
        ("list.intersperse", &["xs", "sep"]),
        // ── string.* ─────────────────────────────────────────────
        ("string.from", &["x"]),
        ("string.split", &["s", "sep"]),
        ("string.join", &["xs", "sep"]),
        ("string.trim", &["s"]),
        ("string.trim_start", &["s"]),
        ("string.trim_end", &["s"]),
        ("string.char_code", &["s"]),
        ("string.from_char_code", &["code"]),
        ("string.contains", &["s", "needle"]),
        ("string.replace", &["s", "from", "to"]),
        ("string.length", &["s"]),
        ("string.byte_length", &["s"]),
        ("string.to_upper", &["s"]),
        ("string.to_lower", &["s"]),
        ("string.starts_with", &["s", "prefix"]),
        ("string.ends_with", &["s", "suffix"]),
        ("string.chars", &["s"]),
        ("string.repeat", &["s", "n"]),
        ("string.index_of", &["s", "needle"]),
        ("string.last_index_of", &["s", "needle"]),
        ("string.split_at", &["s", "i"]),
        ("string.lines", &["s"]),
        ("string.starts_with_at", &["s", "i", "prefix"]),
        ("string.slice", &["s", "start", "end"]),
        ("string.pad_left", &["s", "width", "pad"]),
        ("string.pad_right", &["s", "width", "pad"]),
        ("string.is_empty", &["s"]),
        ("string.is_alpha", &["s"]),
        ("string.is_digit", &["s"]),
        ("string.is_upper", &["s"]),
        ("string.is_lower", &["s"]),
        ("string.is_alnum", &["s"]),
        ("string.is_whitespace", &["s"]),
        // ── map.* ────────────────────────────────────────────────
        ("map.get", &["m", "k"]),
        ("map.set", &["m", "k", "v"]),
        ("map.delete", &["m", "k"]),
        ("map.contains", &["m", "k"]),
        ("map.keys", &["m"]),
        ("map.values", &["m"]),
        ("map.merge", &["m", "other"]),
        ("map.length", &["m"]),
        ("map.filter", &["m", "pred"]),
        ("map.map", &["m", "f"]),
        ("map.entries", &["m"]),
        ("map.from_entries", &["entries"]),
        ("map.each", &["m", "f"]),
        ("map.update", &["m", "k", "default", "f"]),
        // ── set.* ────────────────────────────────────────────────
        ("set.new", &[]),
        ("set.from_list", &["xs"]),
        ("set.to_list", &["s"]),
        ("set.contains", &["s", "x"]),
        ("set.insert", &["s", "x"]),
        ("set.remove", &["s", "x"]),
        ("set.length", &["s"]),
        ("set.union", &["s", "other"]),
        ("set.intersection", &["s", "other"]),
        ("set.difference", &["s", "other"]),
        ("set.symmetric_difference", &["s", "other"]),
        ("set.is_subset", &["s", "other"]),
        ("set.map", &["s", "f"]),
        ("set.filter", &["s", "pred"]),
        ("set.each", &["s", "f"]),
        ("set.fold", &["s", "init", "f"]),
        // ── io.* ─────────────────────────────────────────────────
        ("io.inspect", &["x"]),
        ("io.read_file", &["path"]),
        ("io.write_file", &["path", "contents"]),
        ("io.read_line", &[]),
        ("io.args", &[]),
    ];
    entries.iter().copied().collect()
}

/// Return a map of every built-in name (qualified or bare) to its
/// markdown doc string, for the LSP to render in hover / completion /
/// signature-help. Includes everything registered with
/// `env.define_with_doc` / `env.attach_doc` under
/// `src/typechecker/builtins/` plus the unqualified globals
/// (`println`, `panic`, `Some`, `Ok`, …) registered in
/// `register_builtins` itself. Names without a registered doc are
/// omitted; callers do an `Option<&str>` lookup.
///
/// The map is keyed by the resolved (string) name so LSP code can
/// look up `"list.map"` directly without going through the intern
/// table — symmetric with `builtin_type_signatures`.
pub fn builtin_docs() -> std::collections::HashMap<String, String> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut docs = std::collections::HashMap::new();
    for (name, doc) in &env.builtin_docs {
        docs.insert(resolve(*name), doc.clone());
    }
    docs
}

/// Test-only: the sorted names (qualified or bare) of every
/// function-typed binding registered by `register_builtins`.
#[doc(hidden)]
pub fn builtin_function_names() -> Vec<String> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut out: Vec<String> = env
        .bindings
        .iter()
        .filter(|(_, s)| matches!(s.ty, Type::Fun(_, _)))
        .map(|(name, _)| resolve(*name))
        .collect();
    out.sort();
    out
}

/// Test-only: iterate `(qualified_name, doc)` for every built-in name
/// that has a registered doc. Used by the parity walker
/// (`tests/meta/docs_stdlib_println_parity_tests.rs`) to scan inlined
/// markdown for `\`\`\`silt` fenced blocks with `println(...) --
/// expected` annotations and run them against `silt run`.
#[doc(hidden)]
pub fn iter_builtin_docs() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = builtin_docs().into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Test-only introspection: collect the auto-derived trait-impl and
/// method registrations of the builtin init, for the derive-policy locks
/// in `tests/cli/trait_init_parity_tests.rs`.
///
/// Returns `(trait_impls, method_keys)` where:
/// - `trait_impls` is the set of `"Trait:Type"` pairs registered in
///   `trait_impl_set`.
/// - `method_keys` is the set of `"Type.method"` pairs in
///   `method_table`.
///
/// Stringifies the `Symbol` keys so test code doesn't need access to
/// the crate-private `Symbol`/`intern` types.
#[doc(hidden)]
pub fn __trait_init_fingerprint_check_program() -> (
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
) {
    use std::collections::BTreeSet;
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    register_builtin_trait_impls(&mut checker);
    let trait_impls: BTreeSet<String> = checker
        .tables
        .trait_impl_set
        .iter()
        .map(|(tr, ty)| format!("{}:{}", resolve(tr.name), resolve(ty.name)))
        .collect();
    let method_keys: BTreeSet<String> = checker
        .tables
        .method_table
        .keys()
        .map(|(ty, m)| format!("{}.{}", resolve(ty.name), resolve(*m)))
        .collect();
    (trait_impls, method_keys)
}

/// Test-only introspection for the built-in trait declarations
/// (Display/Compare/Equal/Hash). Returns one tuple per registered
/// trait in the fixed order Display, Compare, Equal, Hash:
///
///   (trait_name, method_name, method_arity, return_type_string,
///    supertrait_args_count, default_method_bodies_count,
///    params_count, supertraits_count, param_where_clauses_count)
///
/// Used by `tests/meta/typechecker_builtin_trait_registration_parity_tests.rs`
/// to lock the semantics of the round-61 dead-code collapse: the four
/// near-identical TraitInfo construction blocks were replaced with a
/// single parameterised helper, and this fingerprint proves the
/// before/after shapes are identical.
#[doc(hidden)]
pub fn __builtin_trait_registration_fingerprint() -> Vec<(
    String,
    String,
    usize,
    String,
    usize,
    usize,
    usize,
    usize,
    usize,
)> {
    let mut checker = TypeChecker::new();
    register_builtin_trait_impls(&mut checker);
    let names = ["Display", "Compare", "Equal", "Hash"];
    let mut out = Vec::new();
    for name in names {
        let info = checker
            .tables
            .traits
            .get(&TraitKey::builtin(name))
            .unwrap_or_else(|| panic!("built-in trait {name} not registered"));
        assert_eq!(
            info.methods.len(),
            1,
            "built-in trait {name} should have exactly one method, got {}",
            info.methods.len()
        );
        let (method_sym, method_ty) = &info.methods[0];
        let (arity, ret_str) = match method_ty {
            Type::Fun(params, ret) => (params.len(), format!("{ret:?}")),
            other => panic!("built-in trait {name} method type is not Fun: {other:?}"),
        };
        out.push((
            name.to_string(),
            resolve(*method_sym),
            arity,
            ret_str,
            info.supertrait_args.len(),
            info.default_method_bodies.len(),
            info.params.len(),
            info.supertraits.len(),
            info.param_where_clauses.len(),
        ));
    }
    out
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
pub(super) mod test_helpers {
    use super::*;

    pub(super) fn check_errors(input: &str) -> Vec<Diagnostic> {
        crate::session::testing::analyze_str(input).1
    }

    pub(super) fn assert_no_errors(input: &str) {
        let errors = check_errors(input);
        let hard: Vec<_> = errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
        assert!(
            hard.is_empty(),
            "expected no type errors, got:\n{}",
            hard.iter()
                .map(|e| format!("  {}", e.message))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    pub(super) fn assert_has_error(input: &str, expected: &str) {
        let errors = check_errors(input);
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "expected error containing '{expected}', got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod size_locks {
    //! Round 50 audit: after removing the never-read informational
    //! fields (`_name`, `_params`) from `EnumInfo`, `RecordInfo`, and
    //! `TraitInfo`, these assertions lock the struct sizes so that
    //! accidentally re-adding a purely informational field (which
    //! would bloat the typechecker HashMaps storing thousands of
    //! these per compilation) fails fast at test time.
    //!
    //! If you INTENTIONALLY add a field, update the expected size
    //! below. If the size changed because the underlying Vec/HashMap
    //! layout changed in a Rust release, that's also fine — bump the
    //! numbers once, and the lock continues to protect against
    //! accidental re-introduction of dead fields.
    use super::{EnumInfo, RecordInfo, TraitInfo};

    #[test]
    fn enum_info_size_locked() {
        // Round 63 item 5 added `defined_in: Symbol` to track the
        // owning package for the trait-orphan rule.
        assert_eq!(
            std::mem::size_of::<EnumInfo>(),
            80,
            "EnumInfo size changed — see module doc"
        );
    }

    #[test]
    fn record_info_size_locked() {
        // Round 63 item 5 added `defined_in: Symbol` to track the
        // owning package for the trait-orphan rule.
        assert_eq!(
            std::mem::size_of::<RecordInfo>(),
            32,
            "RecordInfo size changed — see module doc"
        );
    }

    #[test]
    fn trait_info_size_locked() {
        // Round 63 item 5 added `defined_in: Symbol` to track the
        // owning package for the trait-orphan rule; stage 5 shrank its
        // spans from 24 to 12 bytes.
        assert_eq!(
            std::mem::size_of::<TraitInfo>(),
            232,
            "TraitInfo size changed — see module doc"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::*;
    use super::*;

    // ── Basic type inference ────────────────────────────────────────

    #[test]
    fn test_int_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = 42
  x
}
        "#,
        );
    }

    #[test]
    fn test_float_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = 3.14
  x
}
        "#,
        );
    }

    #[test]
    fn test_string_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = "hello"
  x
}
        "#,
        );
    }

    #[test]
    fn test_bool_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = true
  x
}
        "#,
        );
    }

    #[test]
    fn test_arithmetic() {
        assert_no_errors(
            r#"
fn main() {
  let x = 1 + 2
  let y = x * 3
  y
}
        "#,
        );
    }

    #[test]
    fn test_comparison() {
        assert_no_errors(
            r#"
fn main() {
  let x = 1 < 2
  x
}
        "#,
        );
    }

    #[test]
    fn test_function_call() {
        assert_no_errors(
            r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1, 2)
}
        "#,
        );
    }

    #[test]
    fn test_shadowing() {
        assert_no_errors(
            r#"
fn main() {
  let x = 1
  let x = x + 1
  let x = x * 3
  x
}
        "#,
        );
    }

    // ── List inference ──────────────────────────────────────────────

    #[test]
    fn test_list_inference() {
        assert_no_errors(
            r#"
fn main() {
  let xs = [1, 2, 3]
  xs
}
        "#,
        );
    }

    #[test]
    fn test_empty_list() {
        assert_no_errors(
            r#"
fn main() {
  let xs = []
  xs
}
        "#,
        );
    }

    // ── Tuple inference ─────────────────────────────────────────────

    #[test]
    fn test_tuple_inference() {
        assert_no_errors(
            r#"
fn main() {
  let pair = (1, "hello")
  pair
}
        "#,
        );
    }

    // ── Lambda inference ────────────────────────────────────────────

    #[test]
    fn test_lambda() {
        assert_no_errors(
            r#"
fn main() {
  let double = { x -> x * 2 }
  double(5)
}
        "#,
        );
    }

    // ── Enum types ──────────────────────────────────────────────────

    #[test]
    fn test_enum_type() {
        assert_no_errors(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn area(shape) {
  match shape {
    Circle(r) -> 3.14159 * r * r
    Rect(w, h) -> w * h
  }
}

fn main() {
  area(Circle(5.0))
}
        "#,
        );
    }

    // ── Record types ────────────────────────────────────────────────

    #[test]
    fn test_record_type() {
        assert_no_errors(
            r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn main() {
  let u = User { name: "Alice", age: 30, active: true }
  u.name
}
        "#,
        );
    }

    #[test]
    fn test_record_update() {
        assert_no_errors(
            r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn birthday(user: User) -> User {
  user.{ age: user.age + 1 }
}

fn main() {
  let u = User { name: "Alice", age: 30, active: true }
  let u2 = birthday(u)
  u2.age
}
        "#,
        );
    }

    // ── Match exhaustiveness ────────────────────────────────────────

    #[test]
    fn test_match_exhaustive_with_wildcard() {
        assert_no_errors(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn describe(shape) {
  match shape {
    Circle(r) -> "circle"
    _ -> "other"
  }
}

fn main() {
  describe(Circle(1.0))
}
        "#,
        );
    }

    #[test]
    fn test_match_exhaustive_all_variants() {
        assert_no_errors(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn describe(shape) {
  match shape {
    Circle(r) -> "circle"
    Rect(w, h) -> "rect"
  }
}

fn main() {
  describe(Circle(1.0))
}
        "#,
        );
    }

    #[test]
    fn test_match_non_exhaustive() {
        assert_has_error(
            r#"
type Color {
  Red,
  Green,
  Blue,
}

fn name(c) {
  match c {
    Red -> "red"
    Green -> "green"
  }
}

fn main() {
  name(Red)
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_non_exhaustive_nested_option() {
        // The new Maranget algorithm catches nested patterns.
        // Matching Ok(Some(x)) and Err(e) misses Ok(None).
        assert_has_error(
            r#"
fn handle(r) {
  match r {
    Ok(Some(x)) -> x
    Err(e) -> 0
  }
}
fn main() { handle(Ok(Some(1))) }
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_exhaustive_nested_option() {
        // Full coverage of nested Option inside Result.
        assert_no_errors(
            r#"
fn handle(r) {
  match r {
    Ok(Some(x)) -> x
    Ok(None) -> 0
    Err(e) -> 0
  }
}
fn main() { handle(Ok(Some(1))) }
        "#,
        );
    }

    #[test]
    fn test_match_non_exhaustive_bool_in_tuple() {
        // Tuple of bools: (true, true) and (false, false) misses mixed cases.
        assert_has_error(
            r#"
fn check(pair) {
  match pair {
    (true, true) -> "both"
    (false, false) -> "neither"
  }
}
fn main() { check((true, true)) }
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_exhaustive_bool_tuple() {
        assert_no_errors(
            r#"
fn check(pair) {
  match pair {
    (true, true) -> "both true"
    (true, false) -> "first true"
    (false, _) -> "first false"
  }
}
fn main() { check((true, true)) }
        "#,
        );
    }

    // ── Generic types ───────────────────────────────────────────────

    #[test]
    fn test_option_some_none() {
        assert_no_errors(
            r#"
fn main() {
  let x = Some(42)
  let y = None
  match x {
    Some(n) -> n
    None -> 0
  }
}
        "#,
        );
    }

    #[test]
    fn test_result_ok_err() {
        assert_no_errors(
            r#"
fn main() {
  let x = Ok(42)
  match x {
    Ok(n) -> n
    Err(e) -> 0
  }
}
        "#,
        );
    }

    // ── Question mark operator ──────────────────────────────────────

    #[test]
    fn test_question_mark() {
        assert_no_errors(
            r#"
fn process(x) {
  let val = Ok(x)?
  Ok(val * 2)
}

fn main() {
  match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#,
        );
    }

    // ── When guard (type narrowing) ─────────────────────────────────

    #[test]
    fn test_when_guard() {
        assert_no_errors(
            r#"
fn process(x) {
  when let Ok(value) = Ok(x) else {
    return Err("failed")
  }
  Ok(value * 2)
}

fn main() {
  match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#,
        );
    }

    // ── Boolean when guard ────────────────────────────────────────────

    #[test]
    fn test_when_bool_guard() {
        assert_no_errors(
            r#"
fn check(n) {
  when n > 0 else {
    return "not positive"
  }
  "positive"
}

fn main() {
  check(5)
}
        "#,
        );
    }

    #[test]
    fn test_when_bool_mixed_with_pattern_guard() {
        assert_no_errors(
            r#"
fn process(x) {
  when let Ok(value) = Ok(x) else {
    return Err("failed")
  }
  when value > 0 else {
    return Err("must be positive")
  }
  Ok(value * 2)
}

fn main() {
  match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#,
        );
    }

    // ── Pipe operator ───────────────────────────────────────────────

    #[test]
    fn test_pipe_operator() {
        assert_no_errors(
            r#"
import list
fn main() {
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
}
        "#,
        );
    }

    // ── String interpolation ────────────────────────────────────────

    #[test]
    fn test_string_interpolation() {
        assert_no_errors(
            r#"
fn main() {
  let name = "world"
  let n = 42
  "hello {name}, the answer is {n}"
}
        "#,
        );
    }

    // ── Trait implementation ────────────────────────────────────────

    #[test]
    fn test_trait_impl() {
        assert_no_errors(
            r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle(r={r})"
      Rect(w, h) -> "Rect({w}x{h})"
    }
  }
}

fn main() {
  let s = Circle(5.0)
  s.display()
}
        "#,
        );
    }

    // ── Map literal ─────────────────────────────────────────────────

    #[test]
    fn test_map_literal() {
        assert_no_errors(
            r#"
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  m
}
        "#,
        );
    }

    // ── Integration test programs ───────────────────────────────────

    #[test]
    fn test_fizzbuzz_program() {
        assert_no_errors(
            r#"
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  let results = [
    fizzbuzz(1),
    fizzbuzz(3),
    fizzbuzz(5),
    fizzbuzz(15),
  ]
  results
}
        "#,
        );
    }

    #[test]
    fn test_closures_and_higher_order() {
        assert_no_errors(
            r#"
fn make_adder(n) {
  { x -> x + n }
}

fn main() {
  let add5 = make_adder(5)
  add5(10)
}
        "#,
        );
    }

    #[test]
    fn test_error_handling_pipeline() {
        assert_no_errors(
            r#"
import list
import string
import int
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when let Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when let Some(port_line) = lines |> list.find { l -> string.contains(l, "port=") } else {
    return Err("missing port in config")
  }

  let host = host_line |> string.replace("host=", "")
  let port_result = port_line |> string.replace("port=", "") |> int.parse()
  when let Ok(port) = port_result else {
    return Err("invalid port number")
  }

  Ok("connecting to {host}:{port}")
}

fn main() {
  match parse_config("host=localhost\nport=8080") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }

  match parse_config("host=localhost") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }
}
        "#,
        );
    }

    #[test]
    fn test_match_with_guards() {
        assert_no_errors(
            r#"
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}

fn main() {
  [classify(-5), classify(0), classify(42)]
}
        "#,
        );
    }

    // ── Let-polymorphism ────────────────────────────────────────────

    #[test]
    fn test_let_polymorphism() {
        assert_no_errors(
            r#"
fn identity(x) {
  x
}

fn main() {
  let a = identity(42)
  let b = identity("hello")
  a
}
        "#,
        );
    }

    // ── Unification error ───────────────────────────────────────────

    #[test]
    fn test_type_mismatch_in_binary_op() {
        assert_has_error(
            r#"
fn main() {
  let x = 42 + 1.5
  x
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_bool_op_type_mismatch() {
        assert_has_error(
            r#"
fn main() {
  let x = 42 && true
  x
}
            "#,
            "type mismatch",
        );
    }

    // ── Range ───────────────────────────────────────────────────────

    #[test]
    fn test_range_expression() {
        assert_no_errors(
            r#"
fn main() {
  let r = 1..10
  r
}
        "#,
        );
    }

    // ── Exhaustiveness: guards don't count as covering ──────────────

    #[test]
    fn test_match_guards_with_catch_all() {
        assert_no_errors(
            r#"
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}

fn main() {
  classify(5)
}
        "#,
        );
    }

    // ── Severity tests ─────────────────────────────────────────────

    #[test]
    fn test_type_error_has_error_severity() {
        // A type mismatch should produce Severity::Error.
        //
        // LATENT fix (audit round 36): previously this test only asserted that
        // *some* error with Error severity existed — any unrelated diagnostic
        // with Error severity would satisfy it. Narrow the lock to the specific
        // Int/String mismatch under test: find the diagnostic whose message
        // mentions both "Int" and "String" and assert IT has Error severity.
        // Per the "test must fail on a mutated source" rule for weak-lock
        // strengthenings: if the typechecker regressed to produce the Int/String
        // mismatch as a Warning, this strengthened assertion would fail where
        // the old `any()` check would still pass due to unrelated errors.
        let errors = check_errors(
            r#"
            fn main() {
                let x: Int = "hello"
                x
            }
        "#,
        );
        assert!(!errors.is_empty());
        let mismatch = errors
            .iter()
            .find(|e| e.message.contains("Int") && e.message.contains("String"))
            .unwrap_or_else(|| {
                panic!(
                    "expected an error mentioning both Int and String, got: {:?}",
                    errors.iter().map(|e| &e.message).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            mismatch.severity,
            Severity::Error,
            "Int/String mismatch must be Error severity, got {:?} for message {:?}",
            mismatch.severity,
            mismatch.message
        );
    }

    #[test]
    fn test_valid_program_no_errors() {
        let errors = check_errors(
            r#"
            fn main() {
                let x = 42
                x + 1
            }
        "#,
        );
        let hard_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
        assert!(hard_errors.is_empty());
    }

    #[test]
    fn test_trait_impl_validates_methods() {
        // Complete impl should have no errors about missing methods
        let errors = check_errors(
            r#"
            trait Greet {
                fn greet(self) -> String {
                    "hello"
                }
            }
            trait Greet for User {
                fn greet(self) -> String {
                    "hi"
                }
            }
            type User { name: String }
            fn main() { 0 }
        "#,
        );
        let trait_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("missing method"))
            .collect();
        assert!(
            trait_errors.is_empty(),
            "unexpected trait errors: {:?}",
            trait_errors
        );
    }

    #[test]
    fn test_trait_impl_missing_method() {
        // Both trait methods are abstract (no body) so omitting `detail`
        // in the impl is genuinely missing — not silently filled in by a
        // default. With the default-method feature, a method with a body
        // would be synthesized into the impl rather than reported.
        let errors = check_errors(
            r#"
            trait Showable {
                fn show(self) -> String
                fn detail(self) -> String
            }
            trait Showable for Item {
                fn show(self) -> String { "item" }
            }
            type Item { name: String }
            fn main() { 0 }
        "#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("missing method") && e.message.contains("detail"))
        );
    }

    #[test]
    fn test_trait_impl_unknown_trait() {
        let errors = check_errors(
            r#"
            trait Nonexistent for Thing {
                fn foo(self) -> Int { 0 }
            }
            type Thing { x: Int }
            fn main() { 0 }
        "#,
        );
        assert!(errors.iter().any(|e| e.message.contains("not declared")));
    }

    #[test]
    fn test_builtin_display_trait_exists() {
        // Implementing Display should not produce "trait not declared" error
        let errors = check_errors(
            r#"
            type Color { Red, Blue }
            trait Display for Color {
                fn display(self) -> String {
                    match self {
                        Red -> "red"
                        Blue -> "blue"
                    }
                }
            }
            fn main() { 0 }
        "#,
        );
        let undeclared: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("not declared"))
            .collect();
        assert!(
            undeclared.is_empty(),
            "Display should be a built-in trait: {:?}",
            undeclared
        );
    }

    #[test]
    fn test_where_unknown_trait_warning() {
        let errors = check_errors(
            r#"
            fn show(x) where x: Nonexistent {
                x
            }
            fn main() { 0 }
        "#,
        );
        assert!(errors.iter().any(|e| e.message.contains("Nonexistent")));
    }

    #[test]
    fn test_where_constraint_satisfied() {
        // Should produce no errors about constraints
        let errors = check_errors(
            r#"
            trait Showable {
                fn show(self) -> String { "default" }
            }
            type Color { Red, Blue }
            trait Showable for Color {
                fn show(self) -> String { "color" }
            }
            fn display(x) where x: Showable {
                x
            }
            fn main() {
                display(Red)
            }
        "#,
        );
        let constraint_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("does not implement"))
            .collect();
        assert!(
            constraint_errors.is_empty(),
            "unexpected: {:?}",
            constraint_errors
        );
    }

    #[test]
    fn test_where_constraint_violated() {
        // GAP (round 17 F4): the previous test only bound `where x: Showable`
        // on `x` which never referenced a valid type variable — the
        // constraint-introduction check fired with a suggestion string
        // that happened to contain "Showable", and the test's disjunctive
        // assertion (`contains("does not implement") || contains("Showable")`)
        // matched the wrong branch. It was green against a codebase that
        // completely dropped the "does not implement" check.
        //
        // Pin the real path: declare `display` with a proper typed
        // parameter `x: a where a: Showable`, implement Showable for Int
        // only, then call `display("text")`. Int satisfies; String does
        // not. Must now produce "type 'String' does not implement trait
        // 'Showable'".
        let errors = check_errors(
            r#"
            trait Showable { fn show(self) -> String }
            trait Showable for Int { fn show(self) -> String { "int" } }
            fn display(x: a) -> String where a: Showable { x.show() }
            fn main() { display("text") }
        "#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("does not implement")
                    && e.message.contains("Showable")),
            "expected 'does not implement trait Showable', got: {errors:?}"
        );
    }

    // ── Record types with generic fields (List, Map) ───────────────

    #[test]
    fn test_record_with_list_field() {
        assert_no_errors(
            r#"
type Bag {
  items: List,
  name: String,
}

fn main() {
  let b = Bag { items: [1, 2, 3], name: "test" }
  b.name
}
        "#,
        );
    }

    #[test]
    fn test_record_with_map_field() {
        assert_no_errors(
            r#"
type Config {
  data: Map,
}

fn main() {
  let c = Config { data: #{ "key": "value" } }
  c.data
}
        "#,
        );
    }

    #[test]
    fn test_record_with_list_and_map_fields() {
        assert_no_errors(
            r#"
type Config {
  values: Map,
  errors: List,
}

fn main() {
  let c = Config { values: #{ "a": 1 }, errors: ["err1", "err2"] }
  c.values
}
        "#,
        );
    }

    #[test]
    fn test_record_with_list_field_access() {
        assert_no_errors(
            r#"
type Bag {
  items: List,
}

fn main() {
  let b = Bag { items: [1, 2, 3] }
  b.items
}
        "#,
        );
    }

    // ── Tests for newly registered builtins ────────────────────────

    #[test]
    fn test_list_module_builtins() {
        assert_no_errors(
            r#"
import list
fn main() {
  let xs = [1, 2, 3]
  let ys = list.append(xs, 4)
  let zs = list.concat(xs, ys)
  let head = list.head(xs)
  let tail = list.tail(xs)
  let last = list.last(xs)
  let rev = list.reverse(xs)
  let sorted = list.sort(xs)
  let has = list.contains(xs, 2)
  let n = list.length(xs)
  let taken = list.take(xs, 2)
  let dropped = list.drop(xs, 1)
  let got = list.get(xs, 0)
  let pairs = list.enumerate(xs)
  n
}
        "#,
        );
    }

    #[test]
    fn test_string_module_builtins() {
        assert_no_errors(
            r#"
import string
fn main() {
  let s = "hello world"
  let upper = string.to_upper(s)
  let lower = string.to_lower(s)
  let n = string.length(s)
  let starts = string.starts_with(s, "hello")
  let ends = string.ends_with(s, "world")
  let chars = string.chars(s)
  let repeated = string.repeat(s, 3)
  let idx = string.index_of(s, "world")
  let sliced = string.slice(s, 0, 5)
  let replaced = string.replace(s, "world", "there")
  n
}
        "#,
        );
    }

    #[test]
    fn test_float_module_builtins() {
        assert_no_errors(
            r#"
import float
fn main() {
  let a = 3.14
  let b = 2.71
  let mn = float.min(a, b)
  let mx = float.max(a, b)
  let parsed = float.parse("3.14")
  let rounded = float.round(a)
  let ceiled = float.ceil(a)
  let floored = float.floor(a)
  let abs = float.abs(a)
  rounded
}
        "#,
        );
    }

    #[test]
    fn test_int_module_builtins() {
        assert_no_errors(
            r#"
import int
fn main() {
  let a = 5
  let b = 3
  let mn = int.min(a, b)
  let mx = int.max(a, b)
  let f = int.to_float(a)
  f
}
        "#,
        );
    }

    #[test]
    fn test_map_module_builtins() {
        assert_no_errors(
            r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let got = map.get(m, "a")
  let updated = map.set(m, "c", 3)
  let deleted = map.delete(m, "a")
  let ks = map.keys(m)
  let vs = map.values(m)
  let merged = map.merge(m, #{ "c": 3 })
  ks
}
        "#,
        );
    }

    #[test]
    fn test_io_module_builtins() {
        assert_no_errors(
            r#"
import io
fn main() {
  let result = io.read_file("test.txt")
  let args = io.args()
  args
}
        "#,
        );
    }

    #[test]
    fn test_option_module_builtins() {
        assert_no_errors(
            r#"
import option
fn main() {
  let opt = Some(42)
  let is_s = option.is_some(opt)
  let is_n = option.is_none(opt)
  let val = option.unwrap_or(opt, 0)
  let mapped = option.map(opt, { x -> x + 1 })
  let res = option.to_result(opt, "no value")
  val
}
        "#,
        );
    }

    #[test]
    fn test_result_module_builtins() {
        assert_no_errors(
            r#"
import result
fn main() {
  let r = Ok(42)
  let is_ok = result.is_ok(r)
  let is_err = result.is_err(r)
  is_ok
}
        "#,
        );
    }

    #[test]
    fn test_higher_order_builtins() {
        assert_no_errors(
            r#"
import list
fn main() {
  let xs = [[1, 2], [3, 4], [5]]
  let flat = list.flatten(xs)
  let zipped = list.zip([1, 2, 3], ["a", "b", "c"])
  let sorted = list.sort_by([3, 1, 2], { x -> x })
  flat
}
        "#,
        );
    }

    #[test]
    fn test_len_accepts_string_and_map() {
        assert_no_errors(
            r#"
import list
import string
import map
fn main() {
  let list_len = list.length([1, 2, 3])
  let str_len = string.length("hello")
  let map_len = map.length(#{ "a": 1 })
  list_len + str_len + map_len
}
        "#,
        );
    }

    #[test]
    fn test_assert_ne_builtin() {
        assert_no_errors(
            r#"
import test
fn main() {
  test.assert_ne(1, 2)
}
        "#,
        );
    }

    #[test]
    fn test_channel_new_no_type_error() {
        assert_no_errors(
            r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.close(ch)
  ch
}
        "#,
        );
    }

    #[test]
    fn test_channel_send_mixed_types_is_error() {
        assert_has_error(
            r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.send(ch, "hello")
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_channel_receive_constrains_element_type() {
        assert_no_errors(
            r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  let result = channel.receive(ch)
  result
}
            "#,
        );
    }

    #[test]
    fn test_task_spawn_no_type_error() {
        assert_no_errors(
            r#"
import task
fn main() {
  let h = task.spawn({ -> 42 })
  let result = task.join(h)
  result
}
        "#,
        );
    }

    #[test]
    fn test_map_length_no_type_error() {
        assert_no_errors(
            r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let n = map.length(m)
  n
}
        "#,
        );
    }

    // ── Type narrowing after when/pattern match ────────────────────

    #[test]
    fn test_when_some_narrows_inner_type() {
        // After `when let Some(x) = opt`, x should have the inner type (Int)
        assert_no_errors(
            r#"
fn get_value(opt) {
  when let Some(x) = opt else {
    return 0
  }
  x + 1
}

fn main() {
  get_value(Some(42))
}
            "#,
        );
    }

    #[test]
    fn test_when_ok_narrows_inner_type() {
        // After `when let Ok(v) = result`, v should have the ok type
        assert_no_errors(
            r#"
fn process(result) {
  when let Ok(v) = result else {
    return 0
  }
  v + 10
}

fn main() {
  process(Ok(5))
}
            "#,
        );
    }

    #[test]
    fn test_when_some_used_in_arithmetic() {
        assert_no_errors(
            r#"
fn double_or_zero(opt) {
  when let Some(n) = opt else {
    return 0
  }
  n * 2
}

fn main() {
  double_or_zero(Some(21))
}
            "#,
        );
    }

    // ── Generic type inference ──────────────────────────────────────

    #[test]
    fn test_generic_identity_multiple_types() {
        // A generic function used with multiple types
        assert_no_errors(
            r#"
fn identity(x) {
  x
}

fn main() {
  let a = identity(42)
  let b = identity("hello")
  let c = identity(true)
  a + 1
}
            "#,
        );
    }

    #[test]
    fn test_nested_generic_list_of_options() {
        // List<Option<Int>> — nested generic type
        assert_no_errors(
            r#"
fn main() {
  let xs = [Some(1), Some(2), None]
  xs
}
            "#,
        );
    }

    #[test]
    fn test_generic_function_returning_generic() {
        assert_no_errors(
            r#"
fn wrap(x) {
  Some(x)
}

fn main() {
  let a = wrap(42)
  let b = wrap("hello")
  match a {
    Some(n) -> n
    None -> 0
  }
}
            "#,
        );
    }

    #[test]
    fn test_generic_pair_function() {
        assert_no_errors(
            r#"
fn make_pair(a, b) {
  (a, b)
}

fn main() {
  let p1 = make_pair(1, "hello")
  let p2 = make_pair(true, 3.14)
  p1
}
            "#,
        );
    }

    // ── Recursive functions ─────────────────────────────────────────

    #[test]
    fn test_recursive_function() {
        assert_no_errors(
            r#"
fn factorial(n) {
  match n {
    0 -> 1
    _ -> n * factorial(n - 1)
  }
}

fn main() {
  factorial(5)
}
            "#,
        );
    }

    #[test]
    fn test_recursive_list_function() {
        assert_no_errors(
            r#"
import list
fn sum(xs) {
  match list.head(xs) {
    None -> 0
    Some(h) -> h + sum(list.tail(xs))
  }
}

fn main() {
  sum([1, 2, 3])
}
            "#,
        );
    }

    // ── More exhaustiveness checking ────────────────────────────────

    #[test]
    fn test_match_int_without_wildcard_non_exhaustive() {
        // Matching on Int literal patterns without wildcard should be non-exhaustive
        assert_has_error(
            r#"
fn describe(n) {
  match n {
    0 -> "zero"
    1 -> "one"
  }
}

fn main() {
  describe(2)
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_string_without_wildcard_non_exhaustive() {
        // Matching on String literal patterns without wildcard should be non-exhaustive
        assert_has_error(
            r#"
fn greet(name) {
  match name {
    "alice" -> "hi alice"
    "bob" -> "hi bob"
  }
}

fn main() {
  greet("carol")
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_enum_one_variant_non_exhaustive() {
        // Matching only one variant of a multi-variant enum
        assert_has_error(
            r#"
type Shape {
  Circle(Float),
  Square(Float),
  Triangle(Float, Float),
}

fn area(s) {
  match s {
    Circle(r) -> 3.14 * r * r
  }
}

fn main() {
  area(Circle(5.0))
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_all_guards_non_exhaustive() {
        // Guard arms don't count toward exhaustiveness
        assert_has_error(
            r#"
fn classify(n) {
  match n {
    x when x > 0 -> "positive"
    x when x < 0 -> "negative"
    x when x == 0 -> "zero"
  }
}

fn main() {
  classify(5)
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_int_with_wildcard_exhaustive() {
        // Adding a wildcard makes int matching exhaustive
        assert_no_errors(
            r#"
fn describe(n) {
  match n {
    0 -> "zero"
    1 -> "one"
    _ -> "other"
  }
}

fn main() {
  describe(2)
}
            "#,
        );
    }

    #[test]
    fn test_match_string_with_wildcard_exhaustive() {
        assert_no_errors(
            r#"
fn greet(name) {
  match name {
    "alice" -> "hi alice"
    "bob" -> "hi bob"
    _ -> "hi stranger"
  }
}

fn main() {
  greet("carol")
}
            "#,
        );
    }

    // ── Error cases ─────────────────────────────────────────────────

    #[test]
    fn test_wrong_number_of_arguments() {
        assert_has_error(
            r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1, 2, 3)
}
            "#,
            "argument",
        );
    }

    #[test]
    fn test_too_few_arguments() {
        assert_has_error(
            r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1)
}
            "#,
            "argument",
        );
    }

    #[test]
    fn test_access_nonexistent_record_field() {
        assert_has_error(
            r#"
type Point { x: Int, y: Int }

fn main() {
  let p = Point { x: 1, y: 2 }
  p.z
}
            "#,
            "no field",
        );
    }

    #[test]
    fn test_undefined_variable() {
        assert_has_error(
            r#"
fn main() {
  let x = 1
  y + x
}
            "#,
            "undefined variable",
        );
    }

    #[test]
    fn test_arithmetic_on_string_and_int() {
        // String + Int is rejected: `+` is numeric only
        assert_has_error(
            r#"
fn main() {
  "hello" + 42
}
            "#,
            "requires Int or Float",
        );
    }

    #[test]
    fn test_boolean_and_with_non_bool() {
        assert_has_error(
            r#"
fn main() {
  let x = "hello" && true
  x
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_int_minus_string() {
        assert_has_error(
            r#"
fn main() {
  42 - "hello"
}
            "#,
            "operator '-'",
        );
    }

    // ── Set type inference ──────────────────────────────────────────

    #[test]
    fn test_set_literal_inference() {
        assert_no_errors(
            r#"
fn main() {
  let s = #[1, 2, 3]
  s
}
            "#,
        );
    }

    #[test]
    fn test_empty_set_literal() {
        assert_no_errors(
            r#"
fn main() {
  let s = #[]
  s
}
            "#,
        );
    }

    #[test]
    fn test_set_of_strings() {
        assert_no_errors(
            r#"
fn main() {
  let s = #["hello", "world"]
  s
}
            "#,
        );
    }

    // ── Loop/recur ──────────────────────────────────────────────────

    #[test]
    fn test_loop_basic() {
        assert_no_errors(
            r#"
fn main() {
  loop n = 0 {
    match n > 10 {
      true -> n
      false -> loop(n + 1)
    }
  }
}
            "#,
        );
    }

    #[test]
    fn test_loop_with_accumulator() {
        assert_no_errors(
            r#"
fn main() {
  loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      false -> loop(i + 1, acc + i)
    }
  }
}
            "#,
        );
    }

    #[test]
    fn test_loop_recur_arity_mismatch() {
        // loop has 2 bindings, recur has 1 argument.
        //
        // LATENT fix (audit round 36): previously the assertion was a 2-way
        // substring OR — `contains("binding") || contains("argument")` — so
        // many unrelated diagnostics could satisfy it (e.g. any diagnostic
        // that says "unused binding" or "argument count"). The real message
        // produced by typechecker/inference.rs is
        // `loop has N binding(s), but `loop(...)` supplies M argument(s)`.
        //
        // Strengthening:
        //   - AND-chain specific phrases "loop has" && "`loop(...)` supplies"
        //   - require Severity::Error (GAP #163 established recur arity
        //     mismatch is an Error, not a Warning)
        //
        // Per the "test must fail on a mutated source" rule for weak-lock
        // strengthenings: if the message were reworded, or if the emitter
        // regressed to `self.warning(...)` instead of `self.error(...)`,
        // this strengthened check would fail where the old OR-substring
        // check could still pass. The current code passes both — this is a
        // correct-just-under-locked scenario, so the strengthening is valid.
        let errors = check_errors(
            r#"
fn main() {
  loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      false -> loop(i + 1)
    }
  }
}
            "#,
        );
        let recur_err = errors
            .iter()
            .find(|e| e.message.contains("loop has") && e.message.contains("`loop(...)` supplies"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a recur arity diagnostic containing both \"loop has\" and \
                     \"`loop(...)` supplies\", got: {:?}",
                    errors.iter().map(|e| &e.message).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            recur_err.severity,
            Severity::Error,
            "recur arity mismatch must be Error severity (GAP #163), got {:?} for message {:?}",
            recur_err.severity,
            recur_err.message
        );
    }

    // ── Trait system edge cases ─────────────────────────────────────

    #[test]
    fn test_trait_impl_with_wrong_method_signature() {
        // Both trait methods are declared abstract (no body) so the impl
        // genuinely owes both. Methods with default bodies are now
        // synthesized into impls rather than reported as missing.
        let errors = check_errors(
            r#"
trait Describable {
  fn describe(self) -> String
  fn summary(self) -> String
}

type Widget { label: String }

trait Describable for Widget {
  fn describe(self) -> String { "widget" }
}

fn main() { 0 }
            "#,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("missing method")),
            "expected missing method error, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_trait_unknown_in_impl() {
        assert_has_error(
            r#"
type Foo { x: Int }

trait DoesNotExist for Foo {
  fn bar(self) -> Int { 0 }
}

fn main() { 0 }
            "#,
            "not declared",
        );
    }

    #[test]
    fn test_where_clause_unknown_trait() {
        let errors = check_errors(
            r#"
fn do_thing(x) where x: FakeTrait {
  x
}

fn main() { 0 }
            "#,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("FakeTrait")),
            "expected unknown trait error, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_multiple_trait_impls_for_same_type() {
        // Implementing two different traits for the same type should be fine
        let errors = check_errors(
            r#"
trait Printable {
  fn print(self) -> String { "default" }
}

trait Serializable {
  fn serialize(self) -> String { "default" }
}

type Item { name: String }

trait Printable for Item {
  fn print(self) -> String { "item" }
}

trait Serializable for Item {
  fn serialize(self) -> String { "serialized" }
}

fn main() { 0 }
            "#,
        );
        // Should not produce "not declared" or "missing method" errors
        let bad_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("not declared") || e.message.contains("missing method"))
            .collect();
        assert!(
            bad_errors.is_empty(),
            "unexpected trait errors: {:?}",
            bad_errors
        );
    }

    // ── Ascription (as) ─────────────────────────────────────────────

    #[test]
    fn test_valid_ascription() {
        assert_no_errors(
            r#"
fn main() {
  let x = 42 as Int
  x
}
            "#,
        );
    }

    #[test]
    fn test_ascription_string() {
        assert_no_errors(
            r#"
fn main() {
  let s = "hello" as String
  s
}
            "#,
        );
    }

    #[test]
    fn test_ascription_incompatible_type() {
        assert_has_error(
            r#"
fn main() {
  let x = 42 as String
  x
}
            "#,
            "type mismatch",
        );
    }

    // ── Import-dependent type checking ──────────────────────────────

    #[test]
    fn test_string_module_split() {
        assert_no_errors(
            r#"
import string
fn main() {
  let parts = string.split("a,b,c", ",")
  parts
}
            "#,
        );
    }

    #[test]
    fn test_list_map_and_filter() {
        assert_no_errors(
            r#"
import list
fn main() {
  let xs = [1, 2, 3, 4, 5]
  let doubled = list.map(xs, { x -> x * 2 })
  let evens = list.filter(xs, { x -> x > 2 })
  doubled
}
            "#,
        );
    }

    #[test]
    fn test_map_get_returns_option() {
        assert_no_errors(
            r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let result = map.get(m, "a")
  match result {
    Some(v) -> v
    None -> 0
  }
}
            "#,
        );
    }

    #[test]
    fn test_chained_module_calls() {
        assert_no_errors(
            r#"
import string
import list
fn main() {
  let s = "Hello World"
  let result = s
    |> string.to_lower
    |> string.split(" ")
    |> list.length
  result
}
            "#,
        );
    }

    // ── Additional edge cases ───────────────────────────────────────

    #[test]
    fn test_nested_match_exhaustive() {
        // Nested Result<Option<Int>> fully covered
        assert_no_errors(
            r#"
fn process(r) {
  match r {
    Ok(Some(x)) -> x
    Ok(None) -> -1
    Err(_) -> -2
  }
}

fn main() {
  process(Ok(Some(42)))
}
            "#,
        );
    }

    #[test]
    fn test_enum_match_all_variants_exhaustive() {
        assert_no_errors(
            r#"
type Direction {
  North,
  South,
  East,
  West,
}

fn to_string(d) {
  match d {
    North -> "north"
    South -> "south"
    East -> "east"
    West -> "west"
  }
}

fn main() {
  to_string(North)
}
            "#,
        );
    }

    #[test]
    fn test_record_update_type_checks() {
        assert_no_errors(
            r#"
type Config {
  host: String,
  port: Int,
}

fn main() {
  let c = Config { host: "localhost", port: 8080 }
  let c2 = c.{ port: 9090 }
  c2.host
}
            "#,
        );
    }

    #[test]
    fn test_question_mark_on_non_result() {
        // Using ? on a non-Result/Option type should error
        assert_has_error(
            r#"
fn main() -> Result {
  let x = 42?
  x
}
            "#,
            "requires Result or Option",
        );
    }

    // ── Unification unit tests ─────────────────────────────────────

    #[test]
    fn test_unify_occurs_check() {
        // Unifying Var(0) with List(Var(0)) should produce an infinite type error
        let mut tc = TypeChecker::new();
        let var = tc.fresh_var(); // Type::Var(0)
        let list_of_var = Type::List(Box::new(var.clone()));
        tc.unify(&var, &list_of_var, Span::BUILTIN);
        assert!(
            !tc.errors.is_empty(),
            "occurs check should produce an error"
        );
        assert!(
            tc.errors[0].message.contains("infinite type"),
            "expected 'infinite type' error, got: {}",
            tc.errors[0].message
        );
    }

    #[test]
    fn test_unify_function_arity_mismatch() {
        // Unifying Function([Int], Int) with Function([Int, Int], Int) should error
        let mut tc = TypeChecker::new();
        let fn1 = Type::Fun(vec![Type::Int], Box::new(Type::Int));
        let fn2 = Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int));
        tc.unify(&fn1, &fn2, Span::BUILTIN);
        assert!(
            !tc.errors.is_empty(),
            "function arity mismatch should produce an error"
        );
        assert!(
            tc.errors[0].message.contains("expects") && tc.errors[0].message.contains("argument"),
            "expected arity diagnostic, got: {}",
            tc.errors[0].message
        );
    }

    #[test]
    fn test_unify_basic_var_with_int() {
        // Unifying Var(0) with Int should map Var(0) -> Int
        let mut tc = TypeChecker::new();
        let var = tc.fresh_var(); // Type::Var(0)
        tc.unify(&var, &Type::Int, Span::BUILTIN);
        assert!(tc.errors.is_empty(), "basic unification should not error");
        let resolved = tc.apply(&var);
        assert_eq!(resolved, Type::Int, "Var(0) should resolve to Int");
    }

    #[test]
    fn test_unify_transitive() {
        // Unify Var(0) with Var(1), then Var(1) with String.
        // Resolving Var(0) should yield String.
        let mut tc = TypeChecker::new();
        let var0 = tc.fresh_var(); // Type::Var(0)
        let var1 = tc.fresh_var(); // Type::Var(1)
        tc.unify(&var0, &var1, Span::BUILTIN);
        tc.unify(&var1, &Type::String, Span::BUILTIN);
        assert!(
            tc.errors.is_empty(),
            "transitive unification should not error"
        );
        let resolved = tc.apply(&var0);
        assert_eq!(
            resolved,
            Type::String,
            "Var(0) should transitively resolve to String"
        );
    }

    #[test]
    fn test_unify_list() {
        // Unifying List(Var(0)) with List(Int) should resolve Var(0) to Int
        let mut tc = TypeChecker::new();
        let var = tc.fresh_var(); // Type::Var(0)
        let list_var = Type::List(Box::new(var.clone()));
        let list_int = Type::List(Box::new(Type::Int));
        tc.unify(&list_var, &list_int, Span::BUILTIN);
        assert!(tc.errors.is_empty(), "list unification should not error");
        let resolved = tc.apply(&var);
        assert_eq!(resolved, Type::Int, "Var(0) should resolve to Int");
    }
}
