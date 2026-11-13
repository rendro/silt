//! Round 80 typechecker-side audit fixes.
//!
//! B1 (Generic-vs-Generic mismatch dropped type args) and B3 (builtin
//! type-name shadow guard) are locked by golden cases under
//! `tests/golden/typecheck/diagnostics/round80_typechecker_fixes__*`.
//!
//! B2: `unify`'s `(Type::Generic(n1, a1), Type::Generic(n2, a2))` arm
//! used to format the bare `Symbol` heads, leaking a `TypeOf(..)` head
//! verbatim instead of rendering it as `type X`. The fix formats via the
//! parent `Type` values, so `Type::Display`'s TypeOf special case
//! applies; this file locks that special case.

/// B2 (Display lock): `Type::Display` renders `Generic(TypeOf, [Person])`
/// as `type Person`, the property the `(Generic, Generic)` mismatch arm
/// relies on now that it formats via the parent `Type` values.
#[test]
fn typeof_head_renders_as_surface_type_form() {
    use silt::intern::intern;
    use silt::types::Type;

    // Build `Type::Generic("TypeOf", [Generic("Person", [])])` —
    // exactly what flows through the `(Generic, Generic)` arm when a
    // user-declared record `Person` is used as a type descriptor.
    let person = Type::Generic(
        silt::types::TypeRef {
            id: silt::defs::TypeId(silt::defs::DefId(u32::MAX - 1)),
            name: intern("Person"),
        },
        vec![],
    );
    let typeof_person = Type::type_of(person);
    let rendered = format!("{typeof_person}");
    assert_eq!(
        rendered, "type Person",
        "round 80 B2 regression: TypeOf head must render as `type X`; \
         got `{rendered}`"
    );
    assert!(
        !rendered.contains("TypeOf"),
        "round 80 B2 regression: internal `TypeOf` head leaked into \
         Display output; got `{rendered}`"
    );
}
