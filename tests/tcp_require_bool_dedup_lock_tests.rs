//! Lock for the round-95 dead-code follow-up: `tcp.rs`'s `require_bool`
//! previously hand-rolled the `"<fn> requires Bool, got <kind>"` match
//! instead of delegating to the canonical `super::common` helper (its
//! siblings `require_int`/`require_string`/`require_bytes` all delegate).
//! The error wording was already byte-identical, so this is a pure
//! structural no-op dedup — these are source-grep locks proving the
//! duplication is gone and the canonical helper now owns the shape, so a
//! future edit can't silently reintroduce a divergent local copy.

const TCP_SRC: &str = include_str!("../src/builtins/tcp.rs");
const COMMON_SRC: &str = include_str!("../src/builtins/common.rs");

#[test]
fn common_defines_canonical_require_bool() {
    assert!(
        COMMON_SRC.contains("pub(super) fn require_bool(arg: &Value, fn_label: &str)"),
        "common.rs must define the canonical require_bool helper"
    );
    // The canonical diagnostic shape lives in common.rs.
    assert!(
        COMMON_SRC.contains("{fn_label} requires Bool, got {}"),
        "common::require_bool must carry the canonical `requires Bool, got <kind>` wording"
    );
}

#[test]
fn tcp_require_bool_delegates_and_does_not_hand_roll() {
    assert!(
        TCP_SRC.contains("super::common::require_bool(arg, fn_label)"),
        "tcp.rs require_bool must delegate to super::common::require_bool"
    );
    // The hand-rolled body (the literal Bool match arm + inline wording) must be gone.
    assert!(
        !TCP_SRC.contains("Value::Bool(b) => Ok(*b)"),
        "tcp.rs must no longer hand-roll the require_bool match arm"
    );
    assert!(
        !TCP_SRC.contains("\"{fn_label} requires Bool, got {}\""),
        "tcp.rs must no longer hand-roll the `requires Bool` diagnostic; it lives in common.rs"
    );
}
