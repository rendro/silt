//! Hindley-Milner type inference and checking for Silt.
//!
//! This module implements Algorithm W-style type inference with:
//! - Type variables and unification
//! - Let-polymorphism (generalization at let bindings)
//! - Exhaustiveness checking for match expressions
//! - Type narrowing after `when` guard statements
//! - Trait constraint checking

mod auto_derive;
mod builtin_env;
mod builtins;
mod declare_fns;
mod declare_traits;
mod declare_types;
mod deferred;
mod derive_gates;
mod derive_synth;
mod env;
mod exhaustiveness;
mod infer;
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
pub use builtin_env::*;
use declare_traits::impl_method_key;
use derive_synth::*;
use env::TypeEnv;
pub use scheme::*;
use solve::PendingWhereConstraint;
use std::rc::Rc;
pub use tables::*;
pub use unify::*;

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
                        infer::pattern::BindingSite::Let,
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
        self.check_decl_bodies(&mut program.decls, &mut env);

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
                self.check_decl_bodies(&mut program.decls, &mut env);
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
    pub(super) fn check_decl_bodies(&mut self, decls: &mut [Decl], env: &mut TypeEnv) {
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
