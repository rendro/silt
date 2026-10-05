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
mod declare_fns;
mod declare_traits;
mod declare_types;
mod derive_gates;
mod derive_synth;
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
