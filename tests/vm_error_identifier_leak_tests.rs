//! Source-level grep locks to prevent Rust identifiers and raw opcode
//! names from leaking into user-facing `VmError` messages.
//!
//! Background: round-58 fixed one site where the VM emitted
//! `"frame underflow in invoke_callable"` — the bare `invoke_callable`
//! identifier is a Rust method name, not anything a silt user could
//! meaningfully interpret. A structurally identical sibling in
//! `resume_suspended_invoke` was missed, and several other
//! internal-invariant sites in `src/vm/execute.rs` and
//! `src/builtins/data.rs` leaked similar Rust identifiers and raw
//! opcode names (`SetLocal`, `MakeClosure`, `MakeTuple`,
//! `MakeList`, `MakeMap`, `MakeSet`, `iterate_builtin`).
//!
//! This file greps the current source for those leaks and asserts
//! that the canonical user-facing replacement phrasing is in place.
//! It follows the same `include_str!` pattern used by
//! `tests/vm_destruct_error_messages_tests.rs`
//! (`test_destruct_error_strings_do_not_mention_opcode_names_in_source`).
//!
//! These invariant paths are not reachable from valid typed silt,
//! so we can't exercise them end-to-end — but we can guarantee that
//! if a human ever does reach them, the rendered message will not
//! expose internal Rust/opcode names.

// ── Source snapshots ─────────────────────────────────────────────────

const EXECUTE_RS: &str = include_str!("../src/vm/execute.rs");
const BUILTINS_DATA_RS: &str = include_str!("../src/builtins/data.rs");

// ── Finding 1 — resume_suspended_invoke identifier leaks ────────────

#[test]
fn test_resume_suspended_invoke_identifier_not_in_user_facing_errors() {
    assert!(
        !EXECUTE_RS.contains("frame underflow in resume_suspended_invoke"),
        "execute.rs must not leak the `resume_suspended_invoke` Rust \
         identifier via a `frame underflow in resume_suspended_invoke` \
         literal"
    );
    assert!(
        !EXECUTE_RS.contains("resume_suspended_invoke called with no"),
        "execute.rs must not mention `resume_suspended_invoke` in a \
         user-facing VmError message"
    );
    assert!(
        EXECUTE_RS.contains("\"internal VM error: frame stack underflow during resume\""),
        "execute.rs must contain the canonical user-facing \
         resume frame-underflow message"
    );
    assert!(
        EXECUTE_RS.contains("\"internal VM error: missing suspended state during resume\""),
        "execute.rs must contain the canonical user-facing \
         missing-suspended-state message"
    );
}

// ── Finding 1 sibling — canonical round-58 invoke_callable message ─

#[test]
fn test_invoke_callable_identifier_not_in_user_facing_errors() {
    assert!(
        !EXECUTE_RS.contains("frame underflow in invoke_callable"),
        "execute.rs must not leak the `invoke_callable` Rust identifier"
    );
    assert!(
        EXECUTE_RS.contains("\"internal VM error: frame stack underflow during call\""),
        "canonical round-58 call-path frame-underflow message must stay \
         in place"
    );
}

// ── Finding 2 — iterate_builtin Rust identifier leaks ───────────────

#[test]
fn test_iterate_builtin_identifier_not_in_user_facing_errors() {
    // The Rust method `iterate_builtin` is *called* throughout the VM
    // as a symbol reference; those call sites and comments are fine.
    // What we forbid is the identifier appearing inside a *string
    // literal* passed to `VmError::new(...)`. We approximate this by
    // searching for the identifier inside any `"..."` that also begins
    // with the `internal` or `regex` error-prefix markers used at the
    // known leak sites.
    assert!(
        !EXECUTE_RS.contains("\"internal: iterate_builtin"),
        "execute.rs must not leak the `iterate_builtin` Rust identifier \
         inside a user-facing VmError literal"
    );
    assert!(
        !BUILTINS_DATA_RS.contains("iterate_builtin returned non-list"),
        "builtins/data.rs must not leak the `iterate_builtin` Rust \
         identifier inside a user-facing VmError literal"
    );
    assert!(
        EXECUTE_RS.contains("\"internal VM error: builtin iteration resumed with stale index\""),
        "execute.rs must contain the canonical user-facing stale-index message"
    );
    assert!(
        BUILTINS_DATA_RS.contains(
            "\"internal VM error: regex.replace_all_with builtin iteration returned non-list\""
        ),
        "builtins/data.rs must contain the canonical user-facing \
         regex.replace_all_with non-list message"
    );
}

// ── Finding 2 — raw opcode-name leaks (SetLocal, MakeClosure, …) ────

/// Opcodes whose name (as a capital-camel-case identifier) must NEVER
/// appear inside a `VmError` string literal in `execute.rs`. The list
/// intentionally mirrors the opcodes that had leaks in round 58's
/// audit pass. The bare-identifier check (`"OpName"`, `"OpName:"`,
/// `"OpName "`, `OpName: count`, etc.) is a deliberately narrow
/// pattern: we only flag string-literal leaks, not legitimate
/// `Op::OpName` match arms or comments.
const LEAKING_OPCODE_NAMES: &[&str] = &[
    "SetLocal",
    "MakeClosure",
    "MakeTuple",
    "MakeList",
    "MakeMap",
    "MakeSet",
];

#[test]
fn test_opcode_names_not_in_user_facing_error_literals() {
    for name in LEAKING_OPCODE_NAMES {
        // Direct opener: `"OpName` inside a string literal means the
        // opcode name is being emitted verbatim as the first word.
        let needle_quoted = format!("\"{name}");
        assert!(
            !EXECUTE_RS.contains(&needle_quoted),
            "execute.rs contains an error-string literal starting with \
             opcode name `{name}` — rewrite the error to use a \
             user-facing phrase (e.g. `local binding`, `closure \
             construction`, `tuple construction`)"
        );
        // Mid-string leak form: `"internal: SetLocal slot …"`.
        let needle_internal = format!("internal: {name}");
        assert!(
            !EXECUTE_RS.contains(&needle_internal),
            "execute.rs contains `internal: {name}` — rewrite the error \
             to use a user-facing phrase without the raw opcode name"
        );
    }
}

// ── Finding 3 — canonical user-facing replacements are present ─────

/// Once the leaks are gone, the canonical user-facing phrasings must
/// be the ones actually compiled into the VM. This guards against a
/// future refactor that *removes* the leaky text but forgets to
/// introduce the replacement.
#[test]
fn test_canonical_user_facing_replacements_present() {
    let expected = [
        "\"internal VM error: local binding slot out of range",
        "\"internal VM error: closure construction constant is not a closure\"",
        "\"internal VM error: tuple construction count ",
        "\"internal VM error: list construction count ",
        "\"internal VM error: map construction needs ",
        "\"internal VM error: set construction count ",
    ];
    for needle in expected {
        assert!(
            EXECUTE_RS.contains(needle),
            "execute.rs missing canonical user-facing replacement: {needle}"
        );
    }
}
