//! Canonical type-equality relation.
//!
//! `canonicalize(t)` reduces a [`Type`] to its canonical form. Two types
//! are considered "the same" iff their canonical forms are structurally
//! equal modulo type-var alpha-equivalence. This is the single source
//! of truth for type identity across the typechecker, compiler, and VM.
//!
//! The current reduction set is:
//!
//! - `Type::Range(t) -> Type::List(t)` (Range is a nominal zero-cost
//!   alias of List).
//! - User `type Foo = Bar` alias expansion: a `Type::Generic(name, args)`
//!   whose `name` is registered in the [`Resolver`] alias registry
//!   expands to its stored target with `args` substituted (Phase D).
//! - `Type::AssocProj` reduction: `<T as Trait>::Item` reduces to the
//!   impl's registered binding when the receiver canonicalises to a
//!   concrete head (associated-types phase).
//!
//! ## Phase history (A through D — all live)
//!
//! Phase A originally shipped this module standalone — exposing
//! [`canonicalize`], [`types_equal`], and [`canonical_name`] with unit
//! coverage but no callers. That is no longer true: phase B routed the
//! unifier (`unify` in `src/typechecker/mod.rs`) and the typechecker's
//! `resolve_type_expr` / `type_name_for_impl` through [`canonicalize`];
//! phase C pointed the VM's runtime dispatch (via
//! [`dispatch_type_for_value`]) and the compiler's keying of impl
//! methods (via [`canonical_head`]) at the canonical-name oracle;
//! phase D added the alias registry described below.
//!
//! ## Display vs canonical name
//!
//! [`canonical_name`] is the runtime-dispatch oracle, not a diagnostic
//! renderer. `Range(Int)` displays as `"Range(Int)"` (via
//! `impl Display for Type` in the parent module) but canonicalises to
//! `"List"`. A future `display_name(ty)` helper will preserve the
//! source-level spelling for diagnostics; this module deliberately does
//! not.
//!
//! ## Phase D: user type aliases (and the alias registry)
//!
//! Phase D introduces user-declared type aliases (`type Bytes = List(Int)`
//! and `type Pair(a) = (a, a)`). Aliases are transparent: every mention
//! reduces to the target's canonical form for typechecking, dispatch, and
//! runtime. The alias name is preserved in user-facing diagnostics where
//! the user wrote it (the value-side `Display` of `Type` is unchanged);
//! internally-inferred types continue to spell themselves out (e.g. `let x:
//! Bytes = ...; let y = x` infers `y : List(Int)` for diagnostics on `y`).
//!
//! Phase A's [`canonicalize`] was a pure function with no shared state.
//! Phase D adds a compile-session-scoped alias registry — see
//! [`Resolver::register_alias`] / [`Resolver::lookup_alias`] — that
//! the typechecker populates at decl-processing time and the
//! canonicaliser reads when expanding alias references. Implementation
//! notes:
//!
//! - The registry lives on a [`Resolver`] instance in the checker
//!   tables a compilation session holds (`typechecker::Tables`): every
//!   module's check of the session reads and adds to the one
//!   `Resolver`, so module B importing module A sees A's aliases, and
//!   nothing leaks between sessions.
//! - An alias is keyed by its [`TypeId`]: two modules may each declare
//!   an alias of one name; a trait by its [`TraitKey`]. An associated
//!   type's name is keyed by its resolved `String`, because the interner is `thread_local!`
//!   (see `crate::intern`).
//! - Phase A unit tests in this module continue to pass because they
//!   exercise built-in types only — no aliases registered.
//! - The substitution helper for parametric aliases is the existing
//!   [`crate::types::substitute_vars`] keyed on a `TyVar -> Type` map.
//!   The typechecker assigns one fresh `TyVar` per alias parameter at
//!   registration time so the substitution is straightforward.

use crate::defs::TypeId;
use crate::intern::{Symbol, resolve};
use crate::types::{TraitKey, TyVar, Type, TypeRef};
use crate::value::Value;
use std::collections::HashMap;

/// Resolved type-alias entry stored in the global alias registry.
///
/// Populated by the typechecker when it processes a `TypeBody::Alias`
/// declaration: the target [`crate::ast::TypeExpr`] is resolved to a [`Type`], a
/// fresh `TyVar` is allocated for each alias parameter, and the result
/// is registered here. The canonicaliser then expands an alias
/// reference by substituting the call-site type arguments into the
/// stored `target_param_var_ids` and recursively canonicalising.
///
/// The `TyVar`s used for params here come from the same global TyVar
/// space as the rest of inference; this is fine because they are only
/// ever observed inside the substitution mapping local to one
/// canonicalisation call.
#[derive(Debug, Clone)]
pub struct AliasInfo {
    /// Type-parameter names in source order (e.g. `[a]` for
    /// `type Pair(a) = (a, a)`). Empty for non-parametric aliases.
    pub params: Vec<Symbol>,
    /// `TyVar` ids allocated for each parameter at registration time,
    /// parallel to `params`. The target carries `Type::Var(id)` at
    /// every position where the user wrote the param name; expansion
    /// substitutes through these ids.
    pub param_var_ids: Vec<TyVar>,
    /// The resolved target type: the right-hand side of the alias decl
    /// after `resolve_type_expr`. This is *not* canonicalised here —
    /// the canonicaliser canonicalises after substituting the call-
    /// site args, which lets nested aliases expand correctly.
    pub target: Type,
}

// ── Associated-type bindings registry (Phase: associated types) ──────
//
// Mirrors the alias-registry pattern above. Keys are
// `(trait, target_canonical_head, assoc_name)`, the associated type's
// name resolved to a string (for the `Symbol`-vs-thread-local-interner
// reason).
// The typechecker populates this at impl registration; the
// canonicaliser reads it when reducing `Type::AssocProj` whose
// receiver canonicalises to a concrete head.

#[derive(Debug, Clone)]
pub struct AssocBinding {
    /// The bound type. Stored already-canonicalised, so the reducer
    /// returns it directly with no re-entry into the impl table for
    /// this entry. (Recursive entries — bindings whose value is
    /// itself an `AssocProj` — re-enter through `canonicalize` on the
    /// enclosing type, not through this stored value.)
    pub ty: Type,
    /// The self type of the impl that binds it (`Box(a)`), canonical:
    /// the binding's type variables are the impl's, and stand for the
    /// arguments of the type the projection is taken of.
    pub of: Type,
}

/// Cycle diagnostic returned by [`Resolver::register_assoc_binding`]
/// when the supplied binding RHS would close a cycle back on the
/// triple being registered. Round 76 BROKEN T1 fix.
#[derive(Debug, Clone)]
pub struct AssocBindingCycle {
    /// Trait of the binding under registration.
    pub trait_name: TraitKey,
    /// Canonical target head of the binding under registration.
    pub head: TypeRef,
    /// Associated-type name of the binding under registration.
    pub assoc_name: String,
    /// Triple at which the cycle closes (may equal `(trait_name,
    /// head, assoc_name)` for direct self-reference, or a different
    /// triple for mutual cycles through other registered bindings).
    pub via: (TraitKey, TypeRef, String),
}

/// The key of an associated-type binding: the trait, the canonical head
/// of the impl's target, the associated type's name.
type AssocKey = (TraitKey, TypeRef, String);

/// Compile-session-scoped storage for the alias and associated-type
/// binding registries: one per session, shared by every module's check.
///
/// An alias is keyed by its definition, so two modules' aliases of one
/// name are two entries.
#[derive(Debug, Clone, Default)]
pub struct Resolver {
    aliases: HashMap<TypeId, AliasInfo>,
    assoc_bindings: HashMap<AssocKey, AssocBinding>,
}

impl Resolver {
    /// Allocate a fresh resolver with empty maps.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a user-declared type alias. Called by the typechecker
    /// at decl-processing time. Registering the same alias again (its
    /// module is checked again) replaces the entry.
    pub fn register_alias(&mut self, alias: TypeRef, info: AliasInfo) {
        self.aliases.insert(alias.id, info);
    }

    /// Look up a registered alias. Returns `None` for a type that is
    /// not an alias.
    pub fn lookup_alias(&self, alias: TypeRef) -> Option<AliasInfo> {
        self.aliases.get(&alias.id).cloned()
    }

    /// Remove a registered alias by name. Used by the typechecker when
    /// a cycle is detected closing on an as-yet-unregistered alias:
    /// any earlier alias on the cycle path was registered with a
    /// target that referenced the now-known-cyclic target, so leaving
    /// those entries in the map produces incoherent diagnostics at
    /// later use sites (round 79 LATENT TS-L1).
    pub fn unregister_alias(&mut self, alias: TypeRef) {
        self.aliases.remove(&alias.id);
    }

    /// Forget the associated-type bindings of the impl of `trait_name`
    /// for the type with the head `target_head`: the impl is gone (its
    /// module is checked again, or its REPL entry failed).
    pub fn unregister_assoc_bindings(&mut self, trait_name: TraitKey, target_head: TypeRef) {
        self.assoc_bindings
            .retain(|(tr, head, _), _| !(*tr == trait_name && *head == target_head));
    }

    /// What the registries hold, for a test that compares two states of
    /// a session: each alias and each binding, in a fixed order.
    #[doc(hidden)]
    pub fn fingerprint(&self) -> Vec<String> {
        let mut rows: Vec<String> = self
            .aliases
            .keys()
            .map(|id| format!("alias {id:?}"))
            .chain(
                self.assoc_bindings
                    .iter()
                    .map(|((tr, head, name), binding)| {
                        format!("binding {tr:?} {head:?} {name} = {:?}", binding.ty)
                    }),
            )
            .collect();
        rows.sort();
        rows
    }

    /// Register an `assoc-type` impl binding.
    ///
    /// Called by the typechecker when processing a `TraitImpl`: for
    /// each `type Item = X` binding, the target's canonical head is
    /// computed (so `Range` and `List` collapse to the same key) and
    /// the resolved type is stored. Re-registering the same triple
    /// overwrites the previous entry (matches the alias-registry
    /// convention; the typechecker enforces uniqueness via its
    /// duplicate-impl check, so in practice this only fires once per
    /// triple).
    ///
    /// Round 76 BROKEN T1 cycle protection: refuses to register a
    /// binding whose RHS reduces to a chain of `AssocProj`s that
    /// closes back on the very triple being registered (or any
    /// already-registered triple). Without this guard, a binding like
    /// `type Item = <Int as Container>::Item` (direct self-reference)
    /// or a mutual pair `Foo::T = <Int as Bar>::S` /
    /// `Bar::S = <Int as Foo>::T` would store a self-referential
    /// `AssocProj` and the next `canonicalize` call would recurse
    /// indefinitely (round 76 audit T1 stack overflow). On detection,
    /// returns the offending cycle's head triple so the caller can
    /// emit a typed diagnostic; the binding is *not* inserted.
    #[allow(clippy::result_large_err)]
    pub fn register_assoc_binding(
        &mut self,
        trait_name: TraitKey,
        target_head: TypeRef,
        assoc_name: Symbol,
        ty: Type,
        of: &Type,
    ) -> Result<(), AssocBindingCycle> {
        let head_canon = canonical_head(self, target_head);
        let canon_ty = canonicalize(self, &ty);
        // Cycle detection: walk the canonicalised RHS, following any
        // already-registered assoc bindings, and refuse insertion if
        // we'd close back on the triple under registration. This
        // covers both direct self-reference (`type Item =
        // <Int as Container>::Item`) and mutual cycles through other
        // bindings already in the registry.
        let target_triple = (trait_name, head_canon, resolve(assoc_name));
        let mut visited: std::collections::HashSet<AssocKey> = std::collections::HashSet::new();
        // Treat the triple under registration as already-visited so a
        // direct AssocProj on the RHS that resolves to the same triple
        // is detected immediately.
        visited.insert(target_triple.clone());
        if let Some(cycle) = self.find_assoc_cycle(&canon_ty, &mut visited) {
            return Err(AssocBindingCycle {
                trait_name: target_triple.0,
                head: target_triple.1,
                assoc_name: target_triple.2.clone(),
                via: cycle,
            });
        }
        self.assoc_bindings.insert(
            target_triple,
            AssocBinding {
                ty: canon_ty,
                of: canonicalize(self, of),
            },
        );
        Ok(())
    }

    /// Walk a canonicalised type and return the offending triple if any
    /// `AssocProj` it contains transitively follows back to a triple in
    /// `visited`. Used by `register_assoc_binding` to break cycles.
    fn find_assoc_cycle(
        &self,
        ty: &Type,
        visited: &mut std::collections::HashSet<AssocKey>,
    ) -> Option<AssocKey> {
        match ty {
            Type::AssocProj {
                receiver,
                trait_name,
                assoc_name,
            } => {
                if let Some(head) = head_of_canon(receiver) {
                    let head_canon = canonical_head(self, head);
                    let triple = (*trait_name, head_canon, resolve(*assoc_name));
                    if visited.contains(&triple) {
                        return Some(triple);
                    }
                    if let Some(binding) = self.assoc_bindings.get(&triple).cloned() {
                        visited.insert(triple);
                        let res = self.find_assoc_cycle(&binding.ty, visited);
                        if res.is_some() {
                            return res;
                        }
                    }
                }
                self.find_assoc_cycle(receiver, visited)
            }
            Type::List(inner) | Type::Range(inner) | Type::Set(inner) | Type::Channel(inner) => {
                self.find_assoc_cycle(inner, visited)
            }
            Type::Map(k, v) => self
                .find_assoc_cycle(k, visited)
                .or_else(|| self.find_assoc_cycle(v, visited)),
            Type::Tuple(elems) => {
                for e in elems {
                    if let Some(c) = self.find_assoc_cycle(e, visited) {
                        return Some(c);
                    }
                }
                None
            }
            Type::Fun(params, ret) => {
                for p in params {
                    if let Some(c) = self.find_assoc_cycle(p, visited) {
                        return Some(c);
                    }
                }
                self.find_assoc_cycle(ret, visited)
            }
            Type::Generic(_, args) => {
                for a in args {
                    if let Some(c) = self.find_assoc_cycle(a, visited) {
                        return Some(c);
                    }
                }
                None
            }
            Type::AnonRecord { fields, .. } => {
                for t in fields.values() {
                    if let Some(c) = self.find_assoc_cycle(t, visited) {
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
            | Type::Rigid(_)
            | Type::Error
            | Type::Never => None,
        }
    }

    /// Look up an `assoc-type` binding by `(trait, target_head,
    /// assoc_name)`. Returns `None` when no impl has registered the
    /// binding.
    pub fn lookup_assoc_binding(
        &self,
        trait_name: TraitKey,
        target_head: TypeRef,
        assoc_name: Symbol,
    ) -> Option<AssocBinding> {
        let head_canon = canonical_head(self, target_head);
        self.assoc_bindings
            .get(&(trait_name, head_canon, resolve(assoc_name)))
            .cloned()
    }
}

/// Reduce a type to its canonical form.
///
/// Recursive structural walk. The current reduction set is:
///
/// - `Type::Range(t)` -> `Type::List(canonicalize(t))`
/// - `Type::Generic(name, args)` whose `name` is a registered alias ->
///   the alias's stored target with `args` substituted into its
///   parameters, then canonicalised. (A name cannot be declared as
///   both a record and an alias: a declared record's `Generic` head is
///   never an alias.)
/// - `Type::AssocProj` whose receiver canonicalises to a concrete head
///   with a registered impl binding -> that binding's stored type,
///   canonicalised.
///
/// Every other variant is rebuilt structurally with each contained
/// type recursively canonicalised. Primitive variants and type
/// variables are returned unchanged.
pub fn canonicalize(resolver: &Resolver, ty: &Type) -> Type {
    match ty {
        // ── Primary reduction: Range collapses to List ─────────────
        // Range is a nominal zero-cost alias of List in silt
        // (see Type::Range docs in src/types/mod.rs). The typechecker,
        // compiler, and VM all need to treat them as the same type for
        // dispatch and equality; canonicalising at the boundary is the
        // single point where that invariant is enforced.
        Type::Range(inner) => Type::List(Box::new(canonicalize(resolver, inner))),

        // ── Phase D: user-declared aliases ─────────────────────────
        // A `Type::Generic(name, args)` whose `name` is a registered
        // alias expands by substituting `args` into the alias's
        // params and canonicalising the substituted target. This
        // catches both parametric aliases (`type Pair(a) = (a, a);
        // Pair(Int) -> (Int, Int)`) and zero-arity aliases that the
        // typechecker happens to produce as `Generic(name, [])` (e.g.
        // when the user wrote `Bytes` bare).
        Type::Generic(name, args) if resolver.lookup_alias(*name).is_some() => {
            let info = resolver.lookup_alias(*name).expect("checked just above");
            // Canonicalise args first so nested alias references in
            // the args resolve before substitution. The targeted
            // substitution then operates on already-canonical types.
            let canon_args: Vec<Type> = args.iter().map(|t| canonicalize(resolver, t)).collect();
            let substituted = expand_alias(&info, &canon_args);
            canonicalize(resolver, &substituted)
        }

        // ── Compound shapes: structural recursion ──────────────────
        Type::List(inner) => Type::List(Box::new(canonicalize(resolver, inner))),
        Type::Set(inner) => Type::Set(Box::new(canonicalize(resolver, inner))),
        Type::Channel(inner) => Type::Channel(Box::new(canonicalize(resolver, inner))),
        Type::Map(k, v) => Type::Map(
            Box::new(canonicalize(resolver, k)),
            Box::new(canonicalize(resolver, v)),
        ),
        Type::Fun(params, ret) => Type::Fun(
            params.iter().map(|t| canonicalize(resolver, t)).collect(),
            Box::new(canonicalize(resolver, ret)),
        ),
        Type::Tuple(elems) => {
            Type::Tuple(elems.iter().map(|t| canonicalize(resolver, t)).collect())
        }
        Type::Generic(name, args) => Type::Generic(
            *name,
            args.iter().map(|t| canonicalize(resolver, t)).collect(),
        ),

        // ── Anonymous structural records ───────────────────────────
        // Recurse on each field. Tail is preserved as-is — row variables
        // are inference-internal and unification handles their binding.
        Type::AnonRecord { fields, tail } => Type::AnonRecord {
            fields: fields
                .iter()
                .map(|(n, t)| (*n, canonicalize(resolver, t)))
                .collect(),
            tail: tail.clone(),
        },

        // ── Associated-type projection ─────────────────────────────
        // `<T as Trait>::Item` reduces to the impl's binding when the
        // receiver is concrete. If the receiver canonicalises to a
        // type-variable (or to another unreduced AssocProj), the
        // projection itself is canonical: it stays as `AssocProj` and
        // propagates through inference until the variable is solved.
        // Cycle protection: the receiver is canonicalised first, so any
        // alias chain on the receiver collapses before we look up the
        // binding; the binding's stored type was canonicalised at
        // registration time, so re-entering canonicalize here cannot
        // re-trigger this arm on the same projection (it would have a
        // concrete head different from the input).
        Type::AssocProj {
            receiver,
            trait_name,
            assoc_name,
        } => {
            let canon_recv = canonicalize(resolver, receiver);
            // Try to find a head symbol on the canonicalised receiver.
            // Concrete heads -> impl-table lookup. None -> abstract.
            if let Some(head) = head_of_canon(&canon_recv)
                && let Some(binding) = resolver.lookup_assoc_binding(*trait_name, head, *assoc_name)
            {
                // The stored binding was canonicalised at registration
                // time. Canonicalise again here so any nested alias /
                // assoc-projection inside the binding (registered
                // before another alias became known) reduces too. The
                // recursion terminates because the binding's head is
                // not the same as the AssocProj's input head.
                // The binding is written with the impl's type variables
                // (`type Item = a` in `trait C for Box(a)`): each is
                // what the receiver has in its place (`Int` for
                // `Box(Int)`), not the impl's own variable.
                let mut stands = std::collections::HashMap::new();
                let bound = match super::instance_of(&binding.of, &canon_recv, &mut stands) {
                    true => super::substitute_vars(&binding.ty, &stands),
                    false => binding.ty.clone(),
                };
                return canonicalize(resolver, &bound);
            }
            // No binding (or abstract receiver): keep as canonical
            // AssocProj. The typechecker emits a "type does not
            // implement trait" diagnostic at the originating site if
            // the receiver was concrete and no impl matches; the
            // canonicaliser itself stays silent.
            Type::AssocProj {
                receiver: Box::new(canon_recv),
                trait_name: *trait_name,
                assoc_name: *assoc_name,
            }
        }

        // ── Leaf shapes: identity ──────────────────────────────────
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Var(_)
        | Type::Rigid(_)
        | Type::Error
        | Type::Never => ty.clone(),
    }
}

/// Substitute call-site `args` into an alias's stored target.
///
/// `args.len()` is expected to match `info.params.len()` — the
/// typechecker enforces alias-arity at the annotation site. If a
/// caller passes fewer args than params (e.g. the typechecker fell
/// back to `Type::Generic(name, vec![])` for a bare alias name),
/// missing params are left as their original `TyVar`s, which the
/// outer canonicalisation will return as-is (silt's inference treats
/// them as fresh polymorphic variables — same outcome the existing
/// "bare parameterised name" path produces for built-ins like
/// `List`).
fn expand_alias(info: &AliasInfo, args: &[Type]) -> Type {
    let mut mapping: HashMap<TyVar, Type> = HashMap::new();
    for (i, &var_id) in info.param_var_ids.iter().enumerate() {
        if let Some(arg) = args.get(i) {
            mapping.insert(var_id, arg.clone());
        }
    }
    crate::types::substitute_vars(&info.target, &mapping)
}

/// Type identity check.
///
/// Two types are equal iff their canonical forms are structurally
/// equal. Phase A uses `PartialEq` for the structural comparison; this
/// matches the existing conventions in `inference.rs` where the
/// unifier alpha-renames before its own equality checks. Full
/// alpha-equivalence (different fresh ids in structurally identical
/// positions count as equal) is a phase-B+ concern: the unifier will
/// continue to handle var-binding via its substitution map, and
/// [`types_equal`] is only consulted on already-substituted types.
pub fn types_equal(resolver: &Resolver, a: &Type, b: &Type) -> bool {
    canonicalize(resolver, a) == canonicalize(resolver, b)
}

/// Single canonical built-in type name used by the runtime, compiler,
/// and typechecker for dispatch lookup.
///
/// Returns `String` (rather than the `&'static str` the design sketch
/// originally suggested) because a user-declared type's
/// `Type::Generic` carries a runtime-interned [`Symbol`]
/// names whose backing string is owned by the interner pool, not a
/// `'static` literal. Built-in names (`"Int"`, `"List"`, `"Map"`, ...)
/// match the entries in [`crate::types::builtins::BUILTIN_TYPES`]; the
/// parity-lock test in this module asserts that every built-in entry
/// has a corresponding [`Type`] producing the same string.
///
/// For user-defined types the identity *is* the name: a `Record`
/// declared as `type Point { x: Int, y: Int }` canonicalises to
/// `"Point"`, and a parameterised `Type::Generic("Result", [Int, String])`
/// canonicalises to `"Result"` (parameters are stripped because dispatch
/// lookup is by head constructor).
pub fn canonical_name(ty: &Type) -> String {
    match ty {
        // ── Primitives ─────────────────────────────────────────────
        Type::Int => "Int".to_string(),
        Type::Float => "Float".to_string(),
        Type::Bool => "Bool".to_string(),
        Type::String => "String".to_string(),
        Type::Unit => "Unit".to_string(),

        // ── Containers ─────────────────────────────────────────────
        // Range collapses to List per the canonicalisation rule. This
        // is the dispatch oracle `dispatch_type_for_value` consults:
        // returning "Range" here would miss the methods the compiler
        // keys under `List`.
        Type::List(_) | Type::Range(_) => "List".to_string(),
        Type::Map(_, _) => "Map".to_string(),
        Type::Set(_) => "Set".to_string(),
        Type::Channel(_) => "Channel".to_string(),
        Type::Tuple(_) => "Tuple".to_string(),
        Type::Fun(_, _) => "Fn".to_string(),

        Type::Generic(name, _) => crate::intern::resolve(name.name),

        // ── Diagnostic / inference-internal shapes ─────────────────
        // These should never reach a dispatch-name consumer in
        // production code (Var has been substituted, Error has been
        // suppressed, Never is bottom). Return descriptive placeholder
        // strings so an accidental phase-C wiring failure is debug-
        // visible rather than silently producing "" (which collides
        // with the empty-name case in lookup tables).
        Type::Var(_) => "_".to_string(),
        Type::Rigid(r) => r.name.to_string(),
        Type::Error => "_".to_string(),
        Type::Never => "Never".to_string(),
        // An unreduced AssocProj has no dispatch head — it is an
        // abstract type pending receiver resolution. Same placeholder
        // as Var to keep dispatch tables from accidentally keying on a
        // pending projection.
        Type::AssocProj { .. } => "_".to_string(),
        // Anonymous structural records have no nominal name; use a
        // dispatch key of their own, for which no impl is ever
        // registered.
        Type::AnonRecord { .. } => "<anon>".to_string(),
    }
}

/// The type a dispatch table knows `ty` by: the type itself, or for a
/// surface alias of a builtin type, the builtin type, or for a user
/// alias, the head of the type it stands for.
///
/// - `Range` -> `List` (`Range` is a nominal alias of `List`, so
///   Range-targeted impls register under the key both List and Range
///   receivers reach at dispatch time).
/// - `Fun` -> `Fn` (the surface alias of the function-type name — the
///   VM dispatches closures under `"Fn"`, so a `trait T for Fun` impl
///   must register there too).
/// - Registered user aliases route to the canonical head of their
///   target: `type Bytes = List(Int)` collapses to `List`; chained
///   aliases collapse fully via recursion.
///
/// Any other type is its own head. (`()` is not a type name: it is
/// written `Unit` in an impl target.)
pub fn canonical_head(resolver: &Resolver, ty: TypeRef) -> TypeRef {
    if ty.is_builtin("Range") {
        return TypeRef::builtin("List");
    }
    if ty.is_builtin("Fun") {
        return TypeRef::builtin("Fn");
    }
    if let Some(info) = resolver.lookup_alias(ty) {
        let canon_target = canonicalize(resolver, &info.target);
        if let Some(head) = head_of_canon(&canon_target) {
            return canonical_head(resolver, head);
        }
    }
    ty
}

/// The head type of a canonical type: the builtin type of a primitive
/// or container, the named type of a record or `Generic`. Used both by
/// this module's alias-routing logic and by the typechecker's trait-impl
/// registration path.
///
/// Returns `None` for shapes that have no nominal head (raw
/// type-variables, error / never sentinels, associated projections,
/// anonymous records).
pub fn head_of_canon(ty: &Type) -> Option<TypeRef> {
    let builtin = match ty {
        Type::Int => "Int",
        Type::Float => "Float",
        Type::Bool => "Bool",
        Type::String => "String",
        Type::Unit => "Unit",
        Type::List(_) | Type::Range(_) => "List",
        Type::Map(_, _) => "Map",
        Type::Set(_) => "Set",
        Type::Channel(_) => "Channel",
        Type::Tuple(_) => "Tuple",
        Type::Fun(_, _) => "Fn",
        Type::Generic(name, _) => return Some(*name),
        Type::Var(_)
        | Type::Rigid(_)
        | Type::Error
        | Type::Never
        | Type::AssocProj { .. }
        | Type::AnonRecord { .. } => return None,
    };
    Some(TypeRef::builtin(builtin))
}

/// The type a runtime [`Value`] dispatches its methods on: the type the
/// methods of its impls are keyed by. A record, a variant and a type
/// descriptor carry their type, so `Int.default()` and
/// `Todo.decode(...)` route to impls of `Int` / `Todo`.
///
/// The mapping mirrors [`canonical_name`] applied to each `Value`
/// variant's corresponding [`Type`]. Every function-shaped value (a closure, a builtin, a host function, a
/// variant constructor) is an `Fn`: the typechecker types each as
/// `Type::Fun(..)`, so a `trait T for Fn` impl serves them all.
pub fn dispatch_type_for_value(val: &Value) -> TypeId {
    let builtin = |ty: Type| TypeRef::builtin(&canonical_name(&ty)).id;
    match val {
        Value::Variant(variant) => variant.type_id(),
        Value::Record(record) => record.type_id(),
        Value::TypeDescriptor(ty) => ty.id,
        Value::PrimitiveDescriptor(name) => TypeRef::builtin(name).id,
        Value::Int(_) => builtin(Type::Int),
        Value::Float(_) => builtin(Type::Float),
        Value::Bool(_) => builtin(Type::Bool),
        Value::String(_) => builtin(Type::String),
        Value::List(_) => builtin(Type::List(Box::new(Type::Unit))),
        Value::Map(_) => builtin(Type::Map(Box::new(Type::Unit), Box::new(Type::Unit))),
        Value::Set(_) => builtin(Type::Set(Box::new(Type::Unit))),
        Value::Tuple(_) => builtin(Type::Tuple(vec![])),
        Value::Channel(_) => builtin(Type::Channel(Box::new(Type::Unit))),
        Value::VmClosure(_)
        | Value::BuiltinFn(_)
        | Value::HostFn(_)
        | Value::VariantConstructor(..) => builtin(Type::Fun(vec![], Box::new(Type::Unit))),
        Value::Unit => builtin(Type::Unit),
        Value::Bytes(_) => TypeRef::builtin("Bytes").id,
        Value::Handle(_) => TypeRef::builtin("Handle").id,
        Value::TcpListener(_) => TypeRef::builtin("TcpListener").id,
        Value::TcpStream(_) => TypeRef::builtin("TcpStream").id,
    }
}

/// The name of the type [`dispatch_type_for_value`] gives, as messages
/// show it.
pub fn dispatch_type_name(val: &Value) -> String {
    match val {
        Value::Variant(variant) => variant.ty().name.clone(),
        Value::Record(record) => record.ty().name.clone(),
        Value::TypeDescriptor(ty) => ty.name.clone(),
        _ => crate::typeinfo::builtin_type(dispatch_type_for_value(val))
            .name
            .clone(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A type of a module, with an id no definition table hands out.
    fn user_type(name: &str) -> TypeRef {
        let k = name
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
            % 1000;
        TypeRef {
            id: crate::defs::TypeId(crate::defs::DefId(u32::MAX - 1 - k)),
            name: intern::intern(name),
        }
    }
    use crate::intern;
    use crate::types::builtins::{BUILTIN_TYPES, BuiltinKind};

    // Helper: build the smallest Type instance whose head constructor
    // matches a given builtin surface name. Used to parity-lock
    // canonical_name against BUILTIN_TYPES.
    fn type_for_builtin(name: &str) -> Option<Type> {
        match name {
            "Int" => Some(Type::Int),
            "Float" => Some(Type::Float),
            "Bool" => Some(Type::Bool),
            "String" => Some(Type::String),
            "Unit" | "()" => Some(Type::Unit),
            "List" => Some(Type::List(Box::new(Type::Int))),
            "Range" => Some(Type::Range(Box::new(Type::Int))),
            "Map" => Some(Type::Map(Box::new(Type::Int), Box::new(Type::Int))),
            "Set" => Some(Type::Set(Box::new(Type::Int))),
            "Channel" => Some(Type::Channel(Box::new(Type::Int))),
            "Tuple" => Some(Type::Tuple(vec![Type::Int, Type::Int])),
            "Fn" | "Fun" => Some(Type::Fun(vec![Type::Int], Box::new(Type::Int))),
            // Handle is a runtime-only resource type with no Type
            // variant; it does not participate in canonicalisation.
            "Handle" => None,
            _ => None,
        }
    }

    // ── canonicalize: reductions ───────────────────────────────────

    #[test]
    fn canonicalize_range_becomes_list() {
        let r = Type::Range(Box::new(Type::Int));
        let res = Resolver::new();
        assert_eq!(canonicalize(&res, &r), Type::List(Box::new(Type::Int)));
    }

    #[test]
    fn canonicalize_nested_range_in_fun() {
        let f = Type::Fun(
            vec![Type::Range(Box::new(Type::Int))],
            Box::new(Type::Range(Box::new(Type::Bool))),
        );
        let expected = Type::Fun(
            vec![Type::List(Box::new(Type::Int))],
            Box::new(Type::List(Box::new(Type::Bool))),
        );
        let res = Resolver::new();
        assert_eq!(canonicalize(&res, &f), expected);
    }

    #[test]
    fn canonicalize_range_in_tuple() {
        let t = Type::Tuple(vec![
            Type::Range(Box::new(Type::Int)),
            Type::String,
            Type::Range(Box::new(Type::Bool)),
        ]);
        let expected = Type::Tuple(vec![
            Type::List(Box::new(Type::Int)),
            Type::String,
            Type::List(Box::new(Type::Bool)),
        ]);
        let res = Resolver::new();
        assert_eq!(canonicalize(&res, &t), expected);
    }

    #[test]
    fn canonicalize_range_in_list() {
        // List of Range collapses to List of List.
        let t = Type::List(Box::new(Type::Range(Box::new(Type::Int))));
        let expected = Type::List(Box::new(Type::List(Box::new(Type::Int))));
        let res = Resolver::new();
        assert_eq!(canonicalize(&res, &t), expected);
    }

    #[test]
    fn canonicalize_range_in_map_key_and_value() {
        let t = Type::Map(
            Box::new(Type::Range(Box::new(Type::Int))),
            Box::new(Type::Range(Box::new(Type::Bool))),
        );
        let expected = Type::Map(
            Box::new(Type::List(Box::new(Type::Int))),
            Box::new(Type::List(Box::new(Type::Bool))),
        );
        let res = Resolver::new();
        assert_eq!(canonicalize(&res, &t), expected);
    }

    #[test]
    fn canonicalize_range_in_set_and_channel() {
        let s = Type::Set(Box::new(Type::Range(Box::new(Type::Int))));
        let res = Resolver::new();
        assert_eq!(
            canonicalize(&res, &s),
            Type::Set(Box::new(Type::List(Box::new(Type::Int))))
        );
        let c = Type::Channel(Box::new(Type::Range(Box::new(Type::Int))));
        assert_eq!(
            canonicalize(&res, &c),
            Type::Channel(Box::new(Type::List(Box::new(Type::Int))))
        );
    }

    #[test]
    fn canonicalize_range_in_generic_args() {
        let name = TypeRef::builtin("Result");
        let g = Type::Generic(name, vec![Type::Range(Box::new(Type::Int)), Type::String]);
        let expected = Type::Generic(name, vec![Type::List(Box::new(Type::Int)), Type::String]);
        let res = Resolver::new();
        assert_eq!(canonicalize(&res, &g), expected);
    }

    #[test]
    fn canonicalize_deeply_nested_range() {
        // Fn(Map(String, Tuple(Range(Int), Set(Range(Bool))))) -> ...
        let t = Type::Fun(
            vec![Type::Map(
                Box::new(Type::String),
                Box::new(Type::Tuple(vec![
                    Type::Range(Box::new(Type::Int)),
                    Type::Set(Box::new(Type::Range(Box::new(Type::Bool)))),
                ])),
            )],
            Box::new(Type::Unit),
        );
        let expected = Type::Fun(
            vec![Type::Map(
                Box::new(Type::String),
                Box::new(Type::Tuple(vec![
                    Type::List(Box::new(Type::Int)),
                    Type::Set(Box::new(Type::List(Box::new(Type::Bool)))),
                ])),
            )],
            Box::new(Type::Unit),
        );
        let res = Resolver::new();
        assert_eq!(canonicalize(&res, &t), expected);
    }

    #[test]
    fn canonicalize_idempotent() {
        // canonicalize(canonicalize(t)) == canonicalize(t) for a
        // representative cross-section of shapes. Locks in the
        // fixed-point property: the canonical form is the unique
        // representative of an equivalence class, so re-running the
        // reducer must not change it.
        let cases = [
            Type::Int,
            Type::Range(Box::new(Type::Int)),
            Type::List(Box::new(Type::Range(Box::new(Type::Int)))),
            Type::Fun(
                vec![Type::Range(Box::new(Type::Int))],
                Box::new(Type::Range(Box::new(Type::Bool))),
            ),
            Type::Tuple(vec![
                Type::Range(Box::new(Type::Int)),
                Type::Range(Box::new(Type::Bool)),
            ]),
            Type::Map(
                Box::new(Type::Range(Box::new(Type::Int))),
                Box::new(Type::Range(Box::new(Type::String))),
            ),
            Type::Var(7),
            Type::Error,
            Type::Never,
            Type::Unit,
        ];
        let res = Resolver::new();
        for t in &cases {
            let once = canonicalize(&res, t);
            let twice = canonicalize(&res, &once);
            assert_eq!(
                once, twice,
                "canonicalize is not idempotent for {t:?}: once={once:?} twice={twice:?}"
            );
        }
    }

    #[test]
    fn canonicalize_leaves_primitives_unchanged() {
        let res = Resolver::new();
        for t in [Type::Int, Type::Float, Type::Bool, Type::String, Type::Unit] {
            assert_eq!(canonicalize(&res, &t), t);
        }
    }

    #[test]
    fn canonicalize_leaves_special_shapes_unchanged() {
        let res = Resolver::new();
        assert_eq!(canonicalize(&res, &Type::Var(0)), Type::Var(0));
        assert_eq!(canonicalize(&res, &Type::Error), Type::Error);
        assert_eq!(canonicalize(&res, &Type::Never), Type::Never);
    }

    // ── types_equal ────────────────────────────────────────────────

    #[test]
    fn types_equal_range_eq_list() {
        let res = Resolver::new();
        assert!(types_equal(
            &res,
            &Type::Range(Box::new(Type::Int)),
            &Type::List(Box::new(Type::Int))
        ));
        // And symmetrically.
        assert!(types_equal(
            &res,
            &Type::List(Box::new(Type::Int)),
            &Type::Range(Box::new(Type::Int))
        ));
    }

    #[test]
    fn types_equal_range_in_compound_position_eq_list() {
        // Tuple(Range(Int), Bool) == Tuple(List(Int), Bool)
        let a = Type::Tuple(vec![Type::Range(Box::new(Type::Int)), Type::Bool]);
        let b = Type::Tuple(vec![Type::List(Box::new(Type::Int)), Type::Bool]);
        let res = Resolver::new();
        assert!(types_equal(&res, &a, &b));
    }

    #[test]
    fn types_equal_distinct_primitives_not_equal() {
        let res = Resolver::new();
        assert!(!types_equal(&res, &Type::Int, &Type::Float));
        assert!(!types_equal(&res, &Type::Int, &Type::Bool));
        assert!(!types_equal(&res, &Type::String, &Type::Bool));
        assert!(!types_equal(&res, &Type::Unit, &Type::Int));
    }

    #[test]
    fn types_equal_distinct_inner_types_not_equal() {
        let res = Resolver::new();
        assert!(!types_equal(
            &res,
            &Type::List(Box::new(Type::Int)),
            &Type::List(Box::new(Type::String))
        ));
        assert!(!types_equal(
            &res,
            &Type::Range(Box::new(Type::Int)),
            &Type::List(Box::new(Type::Bool))
        ));
    }

    #[test]
    fn types_equal_reflexive() {
        let res = Resolver::new();
        for t in [
            Type::Int,
            Type::Range(Box::new(Type::Int)),
            Type::Fun(vec![Type::Int], Box::new(Type::Bool)),
            Type::Tuple(vec![Type::Int, Type::String]),
            Type::Var(3),
        ] {
            assert!(
                types_equal(&res, &t, &t),
                "types_equal not reflexive for {t:?}"
            );
        }
    }

    #[test]
    fn types_equal_alpha_equivalence_phase_a_uses_structural() {
        // Phase A intentionally uses plain structural equality. The
        // existing unifier in src/typechecker/inference.rs binds vars
        // through its substitution map *before* equality is consulted,
        // so structurally-identical-but-different-id type-vars never
        // reach types_equal in production. Full alpha-equivalence is
        // a phase-B+ concern (tracked in this module's docstring).
        //
        // This test locks in current behaviour: identical TyVar ids
        // compare equal, distinct ids do not.
        let res = Resolver::new();
        assert!(types_equal(&res, &Type::Var(0), &Type::Var(0)));
        assert!(!types_equal(&res, &Type::Var(0), &Type::Var(1)));
    }

    // ── canonical_name ─────────────────────────────────────────────

    #[test]
    fn canonical_name_primitives() {
        assert_eq!(canonical_name(&Type::Int), "Int");
        assert_eq!(canonical_name(&Type::Float), "Float");
        assert_eq!(canonical_name(&Type::Bool), "Bool");
        assert_eq!(canonical_name(&Type::String), "String");
        assert_eq!(canonical_name(&Type::Unit), "Unit");
    }

    #[test]
    fn canonical_name_int_is_int() {
        assert_eq!(canonical_name(&Type::Int), "Int");
    }

    #[test]
    fn canonical_name_range_is_list() {
        // The whole point of canonicalisation: dispatch by canonical
        // name must collapse Range to List. Phase C wires this into
        // the VM; this test is the unit-level invariant.
        assert_eq!(canonical_name(&Type::Range(Box::new(Type::Int))), "List");
    }

    #[test]
    fn canonical_name_containers() {
        assert_eq!(canonical_name(&Type::List(Box::new(Type::Int))), "List");
        assert_eq!(
            canonical_name(&Type::Map(Box::new(Type::Int), Box::new(Type::Bool))),
            "Map"
        );
        assert_eq!(canonical_name(&Type::Set(Box::new(Type::Int))), "Set");
        assert_eq!(
            canonical_name(&Type::Channel(Box::new(Type::Int))),
            "Channel"
        );
        assert_eq!(
            canonical_name(&Type::Tuple(vec![Type::Int, Type::Bool])),
            "Tuple"
        );
        assert_eq!(
            canonical_name(&Type::Fun(vec![Type::Int], Box::new(Type::Bool))),
            "Fn"
        );
    }

    #[test]
    fn canonical_name_user_record_uses_name() {
        let sym = user_type("Point");
        let r = Type::Generic(sym, Vec::new());
        assert_eq!(canonical_name(&r), "Point");
    }

    #[test]
    fn canonical_name_user_generic_uses_name() {
        let sym = TypeRef::builtin("Result");
        let g = Type::Generic(sym, vec![Type::Int, Type::String]);
        // Parameters are stripped: dispatch is by head constructor.
        assert_eq!(canonical_name(&g), "Result");
    }

    #[test]
    fn canonical_name_inference_internals_are_placeholder() {
        // Var/Error use the same `_` placeholder Display uses for
        // unknown/error types. Never has its own name. None of these
        // should reach a real dispatch consumer; the placeholder is
        // for debug visibility if a phase-C wiring bug routes them
        // through.
        assert_eq!(canonical_name(&Type::Var(0)), "_");
        assert_eq!(canonical_name(&Type::Error), "_");
        assert_eq!(canonical_name(&Type::Never), "Never");
    }

    // ── Parity lock against BUILTIN_TYPES ──────────────────────────

    #[test]
    fn canonical_name_covers_every_builtin_with_a_type_variant() {
        // For every entry in BUILTIN_TYPES that maps onto a Type
        // variant, canonical_name on that variant must equal the
        // builtin's surface name (with two documented exceptions:
        // `Range` canonicalises to `"List"`; `()` is the surface
        // alias for `Unit` and shares the `"Unit"` canonical form).
        for b in BUILTIN_TYPES {
            let Some(t) = type_for_builtin(b.name) else {
                continue; // e.g. Handle: no Type variant
            };
            let got = canonical_name(&t);
            let expected = match b.name {
                "Range" => "List",
                "()" => "Unit",
                "Fun" => "Fn", // Fn and Fun are surface aliases for Type::Fun
                other => other,
            };
            assert_eq!(
                got, expected,
                "canonical_name mismatch for builtin {} (kind={:?}): got {got:?}, expected {expected:?}",
                b.name, b.kind
            );
        }
    }

    #[test]
    fn canonical_name_primitive_parity_with_builtin_kind() {
        // Every BUILTIN_TYPES entry tagged as Primitive that maps
        // onto a Type variant produces a canonical_name equal to
        // its surface name (modulo the `()`/`Unit` alias).
        for b in BUILTIN_TYPES
            .iter()
            .filter(|b| b.kind == BuiltinKind::Primitive)
        {
            let Some(t) = type_for_builtin(b.name) else {
                continue;
            };
            let got = canonical_name(&t);
            let expected = if b.name == "()" { "Unit" } else { b.name };
            assert_eq!(got, expected, "primitive parity failed for {}", b.name);
        }
    }

    // ── canonical_head ─────────────────────────────────────────────

    #[test]
    fn canonical_head_collapses_range_to_list() {
        let res = Resolver::new();
        assert_eq!(
            canonical_head(&res, TypeRef::builtin("Range")),
            TypeRef::builtin("List")
        );
    }

    #[test]
    fn canonical_head_round_trips_unrelated_types() {
        let res = Resolver::new();
        let types = ["Int", "List", "Map", "Set", "Tuple"]
            .map(TypeRef::builtin)
            .into_iter()
            .chain([user_type("Foo"), user_type("Bar")]);
        for ty in types {
            assert_eq!(canonical_head(&res, ty), ty, "expected round-trip for {ty}");
        }
    }

    // ── dispatch_type_for_value ────────────────────────────────────

    fn builtin_id(name: &str) -> TypeId {
        TypeRef::builtin(name).id
    }

    #[test]
    fn dispatch_type_for_value_list_is_list() {
        let v = Value::list(vec![]);
        assert_eq!(dispatch_type_for_value(&v), builtin_id("List"));
    }

    #[test]
    fn dispatch_type_for_value_primitives() {
        assert_eq!(dispatch_type_for_value(&Value::Int(0)), builtin_id("Int"));
        assert_eq!(
            dispatch_type_for_value(&Value::Float(0.0)),
            builtin_id("Float")
        );
        assert_eq!(
            dispatch_type_for_value(&Value::Bool(false)),
            builtin_id("Bool")
        );
        assert_eq!(
            dispatch_type_for_value(&Value::String(String::new().into())),
            builtin_id("String")
        );
        assert_eq!(dispatch_type_for_value(&Value::Unit), builtin_id("Unit"));
    }

    #[test]
    fn dispatch_type_for_value_record_uses_carried_type() {
        let v = Value::builtin_record(
            crate::typeinfo::ty::DATE,
            [
                ("year", Value::Int(2024)),
                ("month", Value::Int(1)),
                ("day", Value::Int(2)),
            ],
        );
        assert_eq!(dispatch_type_for_value(&v), crate::typeinfo::ty::DATE);
        assert_eq!(dispatch_type_name(&v), "Date");
    }

    #[test]
    fn dispatch_type_for_value_descriptors_use_carried_type() {
        assert_eq!(
            dispatch_type_for_value(&Value::TypeDescriptor(
                crate::typeinfo::builtin_type(crate::typeinfo::ty::WEEKDAY).clone()
            )),
            crate::typeinfo::ty::WEEKDAY
        );
        assert_eq!(
            dispatch_type_for_value(&Value::PrimitiveDescriptor("Int")),
            builtin_id("Int")
        );
    }

    #[test]
    fn dispatch_type_for_value_variant_uses_its_type() {
        let v = Value::variant(crate::typeinfo::bv::SOME, vec![Value::Int(7)]);
        assert_eq!(dispatch_type_for_value(&v), crate::typeinfo::ty::OPTION);
        assert_eq!(dispatch_type_name(&v), "Option");
    }

    // ── Phase D: alias registry + expansion in canonicalize ──────────

    /// `canonicalize` expands a registered non-parametric alias to its
    /// stored target. Test isolation: each unit test owns a fresh
    /// `Resolver` so cross-test contamination is impossible by
    /// construction (post-refactor).
    #[test]
    fn alias_expansion_simple() {
        let mut res = Resolver::new();
        let name = user_type("CanonTest_Bytes");
        res.register_alias(
            name,
            AliasInfo {
                params: vec![],
                param_var_ids: vec![],
                target: Type::List(Box::new(Type::Int)),
            },
        );
        let ty = Type::Generic(name, vec![]);
        assert_eq!(canonicalize(&res, &ty), Type::List(Box::new(Type::Int)));
    }

    /// Parametric alias: the target's TyVar is substituted with the
    /// call-site argument before canonicalisation.
    #[test]
    fn alias_expansion_parametric() {
        let mut res = Resolver::new();
        let name = user_type("CanonTest_PairOf");
        // `type CanonTest_PairOf(a) = (a, a)` with a hand-rolled
        // TyVar id of 999.
        let var_id: TyVar = 999;
        res.register_alias(
            name,
            AliasInfo {
                params: vec![intern::intern("a")],
                param_var_ids: vec![var_id],
                target: Type::Tuple(vec![Type::Var(var_id), Type::Var(var_id)]),
            },
        );
        let ty = Type::Generic(name, vec![Type::Int]);
        assert_eq!(
            canonicalize(&res, &ty),
            Type::Tuple(vec![Type::Int, Type::Int])
        );
    }

    /// Chained alias: `B = A; A = List(Int)` — `B` canonicalises to
    /// `List(Int)` because the recursive walk inside `canonicalize`
    /// re-enters expansion on the substituted target.
    #[test]
    fn alias_expansion_chained() {
        let mut res = Resolver::new();
        let a = user_type("CanonTest_ChainA");
        let b = user_type("CanonTest_ChainB");
        res.register_alias(
            a,
            AliasInfo {
                params: vec![],
                param_var_ids: vec![],
                target: Type::List(Box::new(Type::Int)),
            },
        );
        res.register_alias(
            b,
            AliasInfo {
                params: vec![],
                param_var_ids: vec![],
                target: Type::Generic(a, vec![]),
            },
        );
        let ty = Type::Generic(b, vec![]);
        assert_eq!(canonicalize(&res, &ty), Type::List(Box::new(Type::Int)));
    }

    /// `canonical_head` follows alias chains to the head constructor —
    /// `Bytes -> List(Int) -> List` registers and dispatches under the
    /// same key as a direct `List` impl.
    #[test]
    fn canonical_head_follows_alias_to_head() {
        let mut res = Resolver::new();
        let name = user_type("CanonTest_Bytes2");
        res.register_alias(
            name,
            AliasInfo {
                params: vec![],
                param_var_ids: vec![],
                target: Type::List(Box::new(Type::Int)),
            },
        );
        assert_eq!(
            canonical_head(&res, name),
            TypeRef::builtin("List"),
            "alias name should route to its target's canonical head"
        );
    }

    /// Two `Resolver` instances do not share alias state — locks the
    /// post-refactor isolation contract. Pre-refactor this would have
    /// failed because the alias registry was a process-global
    /// `RwLock<HashMap>`.
    #[test]
    fn two_resolvers_do_not_share_aliases_unit() {
        let mut a = Resolver::new();
        let b = Resolver::new();
        let name = user_type("CanonTest_IsolatedAlias");
        a.register_alias(
            name,
            AliasInfo {
                params: vec![],
                param_var_ids: vec![],
                target: Type::List(Box::new(Type::Int)),
            },
        );
        assert!(a.lookup_alias(name).is_some());
        assert!(b.lookup_alias(name).is_none());
    }
}
