//! Canonical type-equality relation.
//!
//! `canonicalize(t)` reduces a [`Type`] to its canonical form. Two types
//! are considered "the same" iff their canonical forms are structurally
//! equal modulo type-var alpha-equivalence. This is the single source
//! of truth for type identity across the typechecker, compiler, and VM.
//!
//! Today the only reduction is `Type::Range(t) -> Type::List(t)`. The
//! API generalises so future reductions (user `type Foo = Bar` aliases,
//! ExtFloat-as-Float-with-flag, future shorthand types) plug in here.
//!
//! ## Phase A scope
//!
//! This module is purely additive: it exposes [`canonicalize`],
//! [`types_equal`], and [`canonical_name`] with thorough unit coverage
//! but is not yet wired into any caller. Phase B routes the existing
//! unifier in `src/typechecker/inference.rs` through [`canonicalize`]
//! at its entry points; phase C points the VM's
//! `value_type_name_for_dispatch` and the compiler's trait-impl
//! global-name emission at [`canonical_name`].
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
//! Phase D adds a process-global alias registry — see
//! [`register_alias`] / [`lookup_alias`] / [`clear_aliases`] — that the
//! typechecker populates at decl-processing time and the canonicaliser
//! reads when expanding alias references. Implementation notes:
//!
//! - The registry is `RwLock<HashMap<Symbol, AliasInfo>>`, mirroring the
//!   variant-decl-order registry pattern in `src/value.rs`. Pros: no
//!   signature churn across the rest of the codebase; same architecture
//!   already in use for variant ordinals. Cons: stateful global; tests
//!   that need isolation across test threads must either name aliases
//!   uniquely (recommended; aliases are interned `Symbol`s, so distinct
//!   names never collide) or call [`clear_aliases`].
//! - Phase A unit tests in this module continue to pass because they
//!   exercise built-in types only — no aliases registered.
//! - The substitution helper for parametric aliases is the existing
//!   [`crate::types::substitute_vars`] keyed on a `TyVar -> Type` map.
//!   The typechecker assigns one fresh `TyVar` per alias parameter at
//!   registration time so the substitution is straightforward.

use crate::intern::{Symbol, intern, resolve};
use crate::types::{Type, TyVar};
use crate::value::Value;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// Resolved type-alias entry stored in the global alias registry.
///
/// Populated by the typechecker when it processes a `TypeBody::Alias`
/// declaration: the target [`TypeExpr`] is resolved to a [`Type`], a
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

// Note: the registry is keyed on the resolved `String` rather than
// the `Symbol`. The interner (`crate::intern`) is `thread_local!`,
// so two threads (e.g. parallel test runners) can produce different
// `Symbol` values for the same string. Keying by string sidesteps
// that hazard and matches the cross-process variant-ordinal registry
// pattern in `src/value.rs`, which also uses `String` keys for the
// same reason.
fn alias_registry() -> &'static RwLock<HashMap<String, AliasInfo>> {
    static REG: OnceLock<RwLock<HashMap<String, AliasInfo>>> = OnceLock::new();
    REG.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register a user-declared type alias into the canonicalisation
/// registry. Called by the typechecker at decl-processing time.
/// Re-registering the same name overwrites the previous entry, which
/// matches the duplicate-decl semantics enforced elsewhere
/// (`register_type_decl` already errors on a duplicate name; the
/// overwrite here is a defensive convenience for tests).
pub fn register_alias(name: Symbol, info: AliasInfo) {
    let mut guard = alias_registry().write().unwrap();
    guard.insert(resolve(name), info);
}

/// Look up a registered alias by name. Returns `None` for built-in
/// names and for any user name that has not been registered (which
/// is the common case during a non-alias-bearing typecheck run).
pub fn lookup_alias(name: Symbol) -> Option<AliasInfo> {
    let guard = alias_registry().read().unwrap();
    guard.get(&resolve(name)).cloned()
}

/// Clear every registered alias. Provided for test isolation: tests
/// that instantiate independent typecheckers can call this between
/// runs to avoid cross-test contamination. Production code never
/// needs to call this.
pub fn clear_aliases() {
    let mut guard = alias_registry().write().unwrap();
    guard.clear();
}

// ── Associated-type bindings registry (Phase: associated types) ──────
//
// Mirrors the alias-registry pattern above. Keys are
// `(trait_name, target_canonical_head, assoc_name)` resolved to
// strings (for the same `Symbol`-vs-thread-local-interner reason).
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
}

fn assoc_registry() -> &'static RwLock<HashMap<(String, String, String), AssocBinding>> {
    static REG: OnceLock<RwLock<HashMap<(String, String, String), AssocBinding>>> =
        OnceLock::new();
    REG.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register an `assoc-type` impl binding.
///
/// Called by the typechecker when processing a `TraitImpl`: for each
/// `type Item = X` binding, the target's canonical head is computed
/// (so `Range` and `List` collapse to the same key) and the resolved
/// type is stored. Re-registering the same triple overwrites the
/// previous entry (matches the alias-registry convention; the
/// typechecker enforces uniqueness via its duplicate-impl check, so
/// in practice this only fires once per triple).
pub fn register_assoc_binding(
    trait_name: Symbol,
    target_head: Symbol,
    assoc_name: Symbol,
    ty: Type,
) {
    let head_canon = canonicalize_type_name(target_head);
    let mut guard = assoc_registry().write().unwrap();
    guard.insert(
        (resolve(trait_name), resolve(head_canon), resolve(assoc_name)),
        AssocBinding {
            ty: canonicalize(&ty),
        },
    );
}

/// Look up an `assoc-type` binding by `(trait, target_head, assoc_name)`.
/// Returns `None` when no impl has registered the binding.
pub fn lookup_assoc_binding(
    trait_name: Symbol,
    target_head: Symbol,
    assoc_name: Symbol,
) -> Option<AssocBinding> {
    let head_canon = canonicalize_type_name(target_head);
    let guard = assoc_registry().read().unwrap();
    guard
        .get(&(resolve(trait_name), resolve(head_canon), resolve(assoc_name)))
        .cloned()
}

/// Clear every registered assoc-type binding. Provided for test
/// isolation symmetric with `clear_aliases`.
pub fn clear_assoc_bindings() {
    let mut guard = assoc_registry().write().unwrap();
    guard.clear();
}

/// Reduce a type to its canonical form.
///
/// Recursive structural walk. The current reduction set is:
///
/// - `Type::Range(t)` -> `Type::List(canonicalize(t))`
/// - `Type::Generic(name, args)` or `Type::Record(name, _)` whose
///   `name` is a registered alias -> the alias's stored target with
///   `args` substituted into its parameters, then canonicalised.
///
/// Every other variant is rebuilt structurally with each contained
/// type recursively canonicalised. Primitive variants and type
/// variables are returned unchanged.
pub fn canonicalize(ty: &Type) -> Type {
    match ty {
        // ── Primary reduction: Range collapses to List ─────────────
        // Range is a nominal zero-cost alias of List in silt
        // (see Type::Range docs in src/types/mod.rs). The typechecker,
        // compiler, and VM all need to treat them as the same type for
        // dispatch and equality; canonicalising at the boundary is the
        // single point where that invariant is enforced.
        Type::Range(inner) => Type::List(Box::new(canonicalize(inner))),

        // ── Phase D: user-declared aliases ─────────────────────────
        // A `Type::Generic(name, args)` whose `name` is a registered
        // alias expands by substituting `args` into the alias's
        // params and canonicalising the substituted target. This
        // catches both parametric aliases (`type Pair(a) = (a, a);
        // Pair(Int) -> (Int, Int)`) and zero-arity aliases that the
        // typechecker happens to produce as `Generic(name, [])` (e.g.
        // when the user wrote `Bytes` bare).
        Type::Generic(name, args) if lookup_alias(*name).is_some() => {
            let info = lookup_alias(*name).expect("checked just above");
            // Canonicalise args first so nested alias references in
            // the args resolve before substitution. The targeted
            // substitution then operates on already-canonical types.
            let canon_args: Vec<Type> = args.iter().map(canonicalize).collect();
            let substituted = expand_alias(&info, &canon_args);
            canonicalize(&substituted)
        }

        // ── Compound shapes: structural recursion ──────────────────
        Type::List(inner) => Type::List(Box::new(canonicalize(inner))),
        Type::Set(inner) => Type::Set(Box::new(canonicalize(inner))),
        Type::Channel(inner) => Type::Channel(Box::new(canonicalize(inner))),
        Type::Map(k, v) => Type::Map(Box::new(canonicalize(k)), Box::new(canonicalize(v))),
        Type::Fun(params, ret) => Type::Fun(
            params.iter().map(canonicalize).collect(),
            Box::new(canonicalize(ret)),
        ),
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(canonicalize).collect()),
        Type::Record(name, fields) => Type::Record(
            *name,
            fields
                .iter()
                .map(|(n, t)| (*n, canonicalize(t)))
                .collect(),
        ),
        Type::Generic(name, args) => {
            Type::Generic(*name, args.iter().map(canonicalize).collect())
        }

        // ── Anonymous structural records ───────────────────────────
        // Recurse on each field. Tail is preserved as-is — row variables
        // are inference-internal and unification handles their binding.
        Type::AnonRecord { fields, tail } => Type::AnonRecord {
            fields: fields.iter().map(|(n, t)| (*n, canonicalize(t))).collect(),
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
            let canon_recv = canonicalize(receiver);
            // Try to find a head symbol on the canonicalised receiver.
            // Concrete heads -> impl-table lookup. None -> abstract.
            if let Some(head) = head_symbol_of_canon(&canon_recv) {
                if let Some(binding) = lookup_assoc_binding(*trait_name, head, *assoc_name) {
                    // The stored binding was canonicalised at registration
                    // time. Canonicalise again here so any nested alias /
                    // assoc-projection inside the binding (registered
                    // before another alias became known) reduces too. The
                    // recursion terminates because the binding's head is
                    // not the same as the AssocProj's input head.
                    return canonicalize(&binding.ty);
                }
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
        | Type::ExtFloat
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Var(_)
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
pub fn types_equal(a: &Type, b: &Type) -> bool {
    canonicalize(a) == canonicalize(b)
}

/// Single canonical built-in type name used by the runtime, compiler,
/// and typechecker for dispatch lookup.
///
/// Returns `String` (rather than the `&'static str` the design sketch
/// originally suggested) because user-declared `Type::Record` and
/// `Type::Generic` carry runtime-interned [`Symbol`](crate::intern::Symbol)
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
        Type::ExtFloat => "ExtFloat".to_string(),
        Type::Bool => "Bool".to_string(),
        Type::String => "String".to_string(),
        Type::Unit => "Unit".to_string(),

        // ── Containers ─────────────────────────────────────────────
        // Range collapses to List per the canonicalisation rule. This
        // is the dispatch oracle the VM's value_type_name_for_dispatch
        // (phase C) will consult: returning "Range" here would miss
        // the qualified-global lookup the compiler emits under the
        // "List.<m>" key.
        Type::List(_) | Type::Range(_) => "List".to_string(),
        Type::Map(_, _) => "Map".to_string(),
        Type::Set(_) => "Set".to_string(),
        Type::Channel(_) => "Channel".to_string(),
        Type::Tuple(_) => "Tuple".to_string(),
        Type::Fun(_, _) => "Fn".to_string(),

        // ── User-declared nominal types: identity is the name ──────
        Type::Record(name, _) => crate::intern::resolve(*name),
        Type::Generic(name, _) => crate::intern::resolve(*name),

        // ── Diagnostic / inference-internal shapes ─────────────────
        // These should never reach a dispatch-name consumer in
        // production code (Var has been substituted, Error has been
        // suppressed, Never is bottom). Return descriptive placeholder
        // strings so an accidental phase-C wiring failure is debug-
        // visible rather than silently producing "" (which collides
        // with the empty-name case in lookup tables).
        Type::Var(_) => "_".to_string(),
        Type::Error => "_".to_string(),
        Type::Never => "Never".to_string(),
        // An unreduced AssocProj has no dispatch head — it is an
        // abstract type pending receiver resolution. Same placeholder
        // as Var to keep dispatch tables from accidentally keying on a
        // pending projection.
        Type::AssocProj { .. } => "_".to_string(),
        // Anonymous structural records have no nominal name; use a
        // synthetic dispatch key. v1 of row polymorphism does not
        // auto-derive any trait on anon records, so dispatch via this
        // key is intentionally never registered.
        Type::AnonRecord { .. } => "<anon>".to_string(),
    }
}


/// Canonicalise a type-name [`Symbol`] for dispatch-table keys.
///
/// Mirror of [`canonical_name`] for the case where only the head
/// constructor's surface name is in hand (as a `Symbol`) — typically
/// because a parser/AST node carries the user-supplied identifier
/// rather than a fully reconstructed [`Type`]. Today the only collapse
/// is `Range -> List`, matching [`canonical_name`]'s reduction rule.
/// Other names round-trip unchanged so the function is safe to apply
/// unconditionally to any target-type symbol.
///
/// Phase B added a sibling helper of the same name in
/// `src/typechecker/mod.rs` that the typechecker's
/// `register_trait_impl` and trait-method body-check sites call;
/// phase C adds this canonical-module copy so the compiler
/// (`src/compiler/mod.rs`) can route `trait_impl.target_type` through
/// the same reduction without depending on the typechecker module.
/// Both copies share the same single-rule (`Range -> List`)
/// implementation, so they stay in lock-step by construction; if the
/// canonicalisation rules ever expand, both must be updated together
/// (see also: the architectural lock test in
/// `tests/canonical_type_arch_lock_tests.rs`).
pub fn canonicalize_type_name(name: Symbol) -> Symbol {
    if resolve(name).as_str() == "Range" {
        return intern("List");
    }
    // Phase D: alias names route to the canonical head of their
    // target. `type Bytes = List(Int)` registers / dispatches under
    // `"List"`; `type Pair(a) = (a, a)` under `"Tuple"`. Chained
    // aliases collapse fully because `canonicalize` already follows
    // the alias chain inside the stored target — `head_symbol_of`
    // returns the final non-alias head. The recursive call protects
    // against future canonicalize changes that might leave a partial
    // chain in place.
    if let Some(info) = lookup_alias(name) {
        let canon_target = canonicalize(&info.target);
        if let Some(head) = head_symbol_of_canon(&canon_target) {
            return canonicalize_type_name(head);
        }
    }
    name
}

/// Local helper: head symbol for canonical-name resolution. Mirror of
/// the typechecker's `head_symbol_of` (kept here so this module owns
/// the alias-routing logic without crossing crate-internal boundaries).
fn head_symbol_of_canon(ty: &Type) -> Option<Symbol> {
    match ty {
        Type::Int => Some(intern("Int")),
        Type::Float => Some(intern("Float")),
        Type::ExtFloat => Some(intern("ExtFloat")),
        Type::Bool => Some(intern("Bool")),
        Type::String => Some(intern("String")),
        Type::Unit => Some(intern("Unit")),
        Type::List(_) | Type::Range(_) => Some(intern("List")),
        Type::Map(_, _) => Some(intern("Map")),
        Type::Set(_) => Some(intern("Set")),
        Type::Channel(_) => Some(intern("Channel")),
        Type::Tuple(_) => Some(intern("Tuple")),
        Type::Fun(_, _) => Some(intern("Fn")),
        Type::Record(name, _) | Type::Generic(name, _) => Some(*name),
        Type::Var(_) | Type::Error | Type::Never | Type::AssocProj { .. } | Type::AnonRecord { .. } => None,
    }
}

/// Canonical dispatch name for a runtime [`Value`], where the answer
/// can be derived from the value's shape alone.
///
/// Returns `Some(name)` for every variant whose dispatch identity is a
/// fixed function of the variant tag plus any carried name string
/// (records and type descriptors). Returns `None` for
/// [`Value::Variant`]: enum-variant-tag → parent-type lookup needs the
/// VM's `__type_of__<tag>` global table, which lives outside this
/// module. Callers (currently `Vm::value_type_name_for_dispatch` in
/// `src/vm/mod.rs`) handle the `Variant` case themselves and delegate
/// every other variant here.
///
/// The mapping mirrors [`canonical_name`] applied to each `Value`
/// variant's corresponding [`Type`] — in particular `Value::Range(..)`
/// returns `"List"` because the type system collapses `Range(t)` to
/// `List(t)` and the compiler emits trait-impl globals under the
/// canonical key. Returning `"Range"` here would route a
/// `Value::Range` receiver to a never-registered `"Range.<m>"` global
/// and surface `no method '<m>' for type 'Range'` to the user (round
/// 61 REGRESSION).
pub fn dispatch_name_for_value(val: &Value) -> Option<String> {
    match val {
        // Variant requires globals lookup for `__type_of__<tag>`; the
        // VM handles this branch directly.
        Value::Variant(_, _) => None,

        // User-declared nominal types carry their own dispatch identity.
        Value::Record(name, _) => Some(name.clone()),
        // Type descriptors dispatch on the carried type name, so
        // `Int.default()` and `Todo.decode(...)` route to impls of
        // `Int` / `Todo` even though the descriptor value itself is
        // neither an Int nor a Todo.
        Value::TypeDescriptor(name) | Value::PrimitiveDescriptor(name) => Some(name.clone()),

        // Built-ins: route every shape through `canonical_name` of the
        // corresponding `Type` so the dispatch oracle has exactly one
        // source of truth. Range collapses to "List" via canonical_name.
        Value::Int(_) => Some(canonical_name(&Type::Int)),
        Value::Float(_) => Some(canonical_name(&Type::Float)),
        Value::ExtFloat(_) => Some(canonical_name(&Type::ExtFloat)),
        Value::Bool(_) => Some(canonical_name(&Type::Bool)),
        Value::String(_) => Some(canonical_name(&Type::String)),
        Value::List(_) => Some(canonical_name(&Type::List(Box::new(Type::Unit)))),
        Value::Range(..) => Some(canonical_name(&Type::Range(Box::new(Type::Unit)))),
        Value::Map(_) => Some(canonical_name(&Type::Map(
            Box::new(Type::Unit),
            Box::new(Type::Unit),
        ))),
        Value::Set(_) => Some(canonical_name(&Type::Set(Box::new(Type::Unit)))),
        Value::Tuple(_) => Some(canonical_name(&Type::Tuple(vec![]))),
        Value::Channel(_) => Some(canonical_name(&Type::Channel(Box::new(Type::Unit)))),
        Value::VmClosure(_) => Some("Function".to_string()),
        Value::BuiltinFn(_) => Some("BuiltinFn".to_string()),
        Value::VariantConstructor(..) => Some("VariantConstructor".to_string()),
        Value::Unit => Some(canonical_name(&Type::Unit)),

        // Resource types with no Type variant (yet): keep their
        // historical dispatch names so any registered impls
        // (`trait Foo for Bytes { ... }`) still resolve.
        Value::Bytes(_) => Some("Bytes".to_string()),
        Value::Handle(_) => Some("Handle".to_string()),
        Value::TcpListener(_) => Some("TcpListener".to_string()),
        Value::TcpStream(_) => Some("TcpStream".to_string()),
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern;
    use crate::types::builtins::{BuiltinKind, BUILTIN_TYPES};

    // Helper: build the smallest Type instance whose head constructor
    // matches a given builtin surface name. Used to parity-lock
    // canonical_name against BUILTIN_TYPES.
    fn type_for_builtin(name: &str) -> Option<Type> {
        match name {
            "Int" => Some(Type::Int),
            "Float" => Some(Type::Float),
            "ExtFloat" => Some(Type::ExtFloat),
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
        assert_eq!(canonicalize(&r), Type::List(Box::new(Type::Int)));
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
        assert_eq!(canonicalize(&f), expected);
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
        assert_eq!(canonicalize(&t), expected);
    }

    #[test]
    fn canonicalize_range_in_list() {
        // List of Range collapses to List of List.
        let t = Type::List(Box::new(Type::Range(Box::new(Type::Int))));
        let expected = Type::List(Box::new(Type::List(Box::new(Type::Int))));
        assert_eq!(canonicalize(&t), expected);
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
        assert_eq!(canonicalize(&t), expected);
    }

    #[test]
    fn canonicalize_range_in_set_and_channel() {
        let s = Type::Set(Box::new(Type::Range(Box::new(Type::Int))));
        assert_eq!(
            canonicalize(&s),
            Type::Set(Box::new(Type::List(Box::new(Type::Int))))
        );
        let c = Type::Channel(Box::new(Type::Range(Box::new(Type::Int))));
        assert_eq!(
            canonicalize(&c),
            Type::Channel(Box::new(Type::List(Box::new(Type::Int))))
        );
    }

    #[test]
    fn canonicalize_range_in_record_field() {
        let name = intern::intern("Holder");
        let field = intern::intern("xs");
        let r = Type::Record(name, vec![(field, Type::Range(Box::new(Type::Int)))]);
        let expected = Type::Record(name, vec![(field, Type::List(Box::new(Type::Int)))]);
        assert_eq!(canonicalize(&r), expected);
    }

    #[test]
    fn canonicalize_range_in_generic_args() {
        let name = intern::intern("Result");
        let g = Type::Generic(name, vec![Type::Range(Box::new(Type::Int)), Type::String]);
        let expected =
            Type::Generic(name, vec![Type::List(Box::new(Type::Int)), Type::String]);
        assert_eq!(canonicalize(&g), expected);
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
        assert_eq!(canonicalize(&t), expected);
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
        for t in &cases {
            let once = canonicalize(t);
            let twice = canonicalize(&once);
            assert_eq!(
                once, twice,
                "canonicalize is not idempotent for {t:?}: once={once:?} twice={twice:?}"
            );
        }
    }

    #[test]
    fn canonicalize_leaves_primitives_unchanged() {
        for t in [
            Type::Int,
            Type::Float,
            Type::ExtFloat,
            Type::Bool,
            Type::String,
            Type::Unit,
        ] {
            assert_eq!(canonicalize(&t), t);
        }
    }

    #[test]
    fn canonicalize_leaves_special_shapes_unchanged() {
        assert_eq!(canonicalize(&Type::Var(0)), Type::Var(0));
        assert_eq!(canonicalize(&Type::Error), Type::Error);
        assert_eq!(canonicalize(&Type::Never), Type::Never);
    }

    // ── types_equal ────────────────────────────────────────────────

    #[test]
    fn types_equal_range_eq_list() {
        assert!(types_equal(
            &Type::Range(Box::new(Type::Int)),
            &Type::List(Box::new(Type::Int))
        ));
        // And symmetrically.
        assert!(types_equal(
            &Type::List(Box::new(Type::Int)),
            &Type::Range(Box::new(Type::Int))
        ));
    }

    #[test]
    fn types_equal_range_in_compound_position_eq_list() {
        // Tuple(Range(Int), Bool) == Tuple(List(Int), Bool)
        let a = Type::Tuple(vec![Type::Range(Box::new(Type::Int)), Type::Bool]);
        let b = Type::Tuple(vec![Type::List(Box::new(Type::Int)), Type::Bool]);
        assert!(types_equal(&a, &b));
    }

    #[test]
    fn types_equal_distinct_primitives_not_equal() {
        assert!(!types_equal(&Type::Int, &Type::Float));
        assert!(!types_equal(&Type::Int, &Type::Bool));
        assert!(!types_equal(&Type::String, &Type::Bool));
        assert!(!types_equal(&Type::Float, &Type::ExtFloat));
        assert!(!types_equal(&Type::Unit, &Type::Int));
    }

    #[test]
    fn types_equal_distinct_inner_types_not_equal() {
        assert!(!types_equal(
            &Type::List(Box::new(Type::Int)),
            &Type::List(Box::new(Type::String))
        ));
        assert!(!types_equal(
            &Type::Range(Box::new(Type::Int)),
            &Type::List(Box::new(Type::Bool))
        ));
    }

    #[test]
    fn types_equal_reflexive() {
        for t in [
            Type::Int,
            Type::Range(Box::new(Type::Int)),
            Type::Fun(vec![Type::Int], Box::new(Type::Bool)),
            Type::Tuple(vec![Type::Int, Type::String]),
            Type::Var(3),
        ] {
            assert!(types_equal(&t, &t), "types_equal not reflexive for {t:?}");
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
        assert!(types_equal(&Type::Var(0), &Type::Var(0)));
        assert!(!types_equal(&Type::Var(0), &Type::Var(1)));
    }

    // ── canonical_name ─────────────────────────────────────────────

    #[test]
    fn canonical_name_primitives() {
        assert_eq!(canonical_name(&Type::Int), "Int");
        assert_eq!(canonical_name(&Type::Float), "Float");
        assert_eq!(canonical_name(&Type::ExtFloat), "ExtFloat");
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
        let sym = intern::intern("Point");
        let r = Type::Record(
            sym,
            vec![
                (intern::intern("x"), Type::Int),
                (intern::intern("y"), Type::Int),
            ],
        );
        assert_eq!(canonical_name(&r), "Point");
    }

    #[test]
    fn canonical_name_user_generic_uses_name() {
        let sym = intern::intern("Result");
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
        for b in BUILTIN_TYPES.iter().filter(|b| b.kind == BuiltinKind::Primitive) {
            let Some(t) = type_for_builtin(b.name) else {
                continue;
            };
            let got = canonical_name(&t);
            let expected = if b.name == "()" { "Unit" } else { b.name };
            assert_eq!(got, expected, "primitive parity failed for {}", b.name);
        }
    }

    // ── canonicalize_type_name ─────────────────────────────────────

    #[test]
    fn canonicalize_type_name_collapses_range_to_list() {
        assert_eq!(
            resolve(canonicalize_type_name(intern::intern("Range"))),
            "List"
        );
    }

    #[test]
    fn canonicalize_type_name_round_trips_unrelated_names() {
        for n in ["Int", "List", "Map", "Set", "Tuple", "Foo", "Bar"] {
            let s = intern::intern(n);
            assert_eq!(canonicalize_type_name(s), s, "expected round-trip for {n}");
        }
    }

    // ── dispatch_name_for_value ────────────────────────────────────

    #[test]
    fn dispatch_name_for_value_range_returns_list() {
        // The whole-stack invariant: a Range receiver dispatches under
        // the same key the compiler emits for `for List(a)` impls.
        let v = Value::Range(1, 5);
        assert_eq!(dispatch_name_for_value(&v), Some("List".to_string()));
    }

    #[test]
    fn dispatch_name_for_value_list_returns_list() {
        let v = Value::List(std::sync::Arc::new(vec![]));
        assert_eq!(dispatch_name_for_value(&v), Some("List".to_string()));
    }

    #[test]
    fn dispatch_name_for_value_primitives() {
        assert_eq!(
            dispatch_name_for_value(&Value::Int(0)),
            Some("Int".to_string())
        );
        assert_eq!(
            dispatch_name_for_value(&Value::Float(0.0)),
            Some("Float".to_string())
        );
        assert_eq!(
            dispatch_name_for_value(&Value::Bool(false)),
            Some("Bool".to_string())
        );
        assert_eq!(
            dispatch_name_for_value(&Value::String(String::new())),
            Some("String".to_string())
        );
        assert_eq!(
            dispatch_name_for_value(&Value::Unit),
            Some("Unit".to_string())
        );
    }

    #[test]
    fn dispatch_name_for_value_record_uses_carried_name() {
        let v = Value::Record(
            "Point".to_string(),
            std::sync::Arc::new(std::collections::BTreeMap::new()),
        );
        assert_eq!(dispatch_name_for_value(&v), Some("Point".to_string()));
    }

    #[test]
    fn dispatch_name_for_value_descriptors_use_carried_name() {
        assert_eq!(
            dispatch_name_for_value(&Value::TypeDescriptor("Todo".to_string())),
            Some("Todo".to_string())
        );
        assert_eq!(
            dispatch_name_for_value(&Value::PrimitiveDescriptor("Int".to_string())),
            Some("Int".to_string())
        );
    }

    #[test]
    fn dispatch_name_for_value_variant_returns_none() {
        // Variant needs the VM's __type_of__<tag> globals lookup;
        // dispatch_name_for_value is shape-only, so it returns None
        // and the VM handles this branch itself.
        let v = Value::Variant("Some".to_string(), vec![Value::Int(7)]);
        assert!(dispatch_name_for_value(&v).is_none());
    }

    // ── Phase D: alias registry + expansion in canonicalize ──────────

    /// `canonicalize` expands a registered non-parametric alias to its
    /// stored target. Test isolation: every alias in this module uses
    /// the `CanonTest_*` prefix so parallel test threads don't collide
    /// with the integration tests in `tests/type_alias_tests.rs` which
    /// use `PhD*` prefixes.
    #[test]
    fn alias_expansion_simple() {
        let name = intern::intern("CanonTest_Bytes");
        register_alias(
            name,
            AliasInfo {
                params: vec![],
                param_var_ids: vec![],
                target: Type::List(Box::new(Type::Int)),
            },
        );
        let ty = Type::Generic(name, vec![]);
        assert_eq!(canonicalize(&ty), Type::List(Box::new(Type::Int)));
    }

    /// Parametric alias: the target's TyVar is substituted with the
    /// call-site argument before canonicalisation.
    #[test]
    fn alias_expansion_parametric() {
        let name = intern::intern("CanonTest_PairOf");
        // `type CanonTest_PairOf(a) = (a, a)` with a hand-rolled
        // TyVar id of 999.
        let var_id: TyVar = 999;
        register_alias(
            name,
            AliasInfo {
                params: vec![intern::intern("a")],
                param_var_ids: vec![var_id],
                target: Type::Tuple(vec![Type::Var(var_id), Type::Var(var_id)]),
            },
        );
        let ty = Type::Generic(name, vec![Type::Int]);
        assert_eq!(canonicalize(&ty), Type::Tuple(vec![Type::Int, Type::Int]));
    }

    /// Chained alias: `B = A; A = List(Int)` — `B` canonicalises to
    /// `List(Int)` because the recursive walk inside `canonicalize`
    /// re-enters expansion on the substituted target.
    #[test]
    fn alias_expansion_chained() {
        let a = intern::intern("CanonTest_ChainA");
        let b = intern::intern("CanonTest_ChainB");
        register_alias(
            a,
            AliasInfo {
                params: vec![],
                param_var_ids: vec![],
                target: Type::List(Box::new(Type::Int)),
            },
        );
        register_alias(
            b,
            AliasInfo {
                params: vec![],
                param_var_ids: vec![],
                target: Type::Generic(a, vec![]),
            },
        );
        let ty = Type::Generic(b, vec![]);
        assert_eq!(canonicalize(&ty), Type::List(Box::new(Type::Int)));
    }


    /// `canonicalize_type_name` follows alias chains to the head
    /// constructor — `Bytes -> List(Int) -> "List"` registers and
    /// dispatches under the same key as a direct `List` impl.
    #[test]
    fn canonicalize_type_name_follows_alias_to_head() {
        let name = intern::intern("CanonTest_Bytes2");
        register_alias(
            name,
            AliasInfo {
                params: vec![],
                param_var_ids: vec![],
                target: Type::List(Box::new(Type::Int)),
            },
        );
        assert_eq!(
            resolve(canonicalize_type_name(name)),
            "List",
            "alias name should route to its target's canonical head"
        );
    }

    // Note: `clear_aliases()` is intentionally not unit-tested here.
    // It's a cross-cutting hook that empties the global registry; a
    // unit test calling it would race against any concurrently-
    // running test that has registered aliases. The integration
    // suite (`tests/type_alias_tests.rs`) exercises it implicitly by
    // never calling it, relying on per-test name uniqueness for
    // isolation; the function is kept as part of the public API for
    // out-of-process tests that need a hard reset.
}
