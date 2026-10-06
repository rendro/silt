//! Hardening tests that need Rust: `Value` float ordering. The builtin edge
//! cases, concurrent panic recovery, IO error paths and overflow locks
//! that used to live here are golden cases named `hardening__*` under
//! `tests/golden/lang/`.

use silt::value::Value;

// ── Value ordering: Float Eq/Ord consistency ───────────────────────

#[test]
fn test_float_ord_consistency() {
    // Verify that Value::Float ordering is consistent with equality.
    // Two equal floats must compare as Equal.
    let a = Value::Float(1.5);
    let b = Value::Float(1.5);
    assert_eq!(a, b);
    assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);

    // Different floats should order correctly.
    let c = Value::Float(2.0);
    assert_eq!(a.cmp(&c), std::cmp::Ordering::Less);
    assert_eq!(c.cmp(&a), std::cmp::Ordering::Greater);
}
