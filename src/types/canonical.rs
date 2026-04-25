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

use crate::intern::{Symbol, intern, resolve};
use crate::types::Type;
use crate::value::Value;

/// Reduce a type to its canonical form.
///
/// Recursive structural walk. The current reduction set is:
///
/// - `Type::Range(t)` -> `Type::List(canonicalize(t))`
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
    match resolve(name).as_str() {
        "Range" => intern("List"),
        _ => name,
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
}
