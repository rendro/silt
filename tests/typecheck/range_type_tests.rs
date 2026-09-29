//! Lock test for Round 52 deferred item 6 — `Range(T)` nominal type.
//!
//! The behavioural locks (annotation, bidirectional unify with `List`,
//! inference, element-type rejection, `Range(Int)` in diagnostics) are
//! golden cases under `tests/golden/typecheck/ranges/range_type__*`.
//! This file keeps the internal `Display for Type` check.

use silt::types::Type;

#[test]
fn display_range_type_prints_range_prefix() {
    let ty = Type::Range(Box::new(Type::Int));
    assert_eq!(format!("{ty}"), "Range(Int)");
}
