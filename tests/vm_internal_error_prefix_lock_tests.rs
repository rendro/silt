//! Source-grep locks for two sibling invariants on the VM error
//! prefix and Rust-identifier leaks in `VmError::new(...)` string
//! literals.
//!
//! Round 58 established `"internal VM error:"` as the canonical
//! prefix for user-facing internal-invariant failures in the VM;
//! rounds 58, 60 and follow-ups canonicalised `src/vm/execute.rs`
//! and `src/builtins/data.rs` accordingly. `src/vm/mod.rs` was
//! never audited for either drift, and two sibling paths (the
//! `read_byte` helper and several `ok_or_else` sites around frame
//! access) still emitted `"internal:"` bare prefixes — one of them
//! leaking the Rust identifier `read_byte` verbatim into the error
//! text.
//!
//! These tests source-grep the current `src/vm/mod.rs` and
//! `src/vm/execute.rs` for bare `"internal:"` string-literal
//! prefixes and for `read_byte` inside a string literal. Both must
//! fail pre-fix (from the live source) and pass post-fix.
//!
//! Complements tests/vm_error_identifier_leak_tests.rs (execute.rs
//! + builtins/data.rs audit) — this file extends the same invariant
//! to `src/vm/mod.rs`.

// ── Source snapshots ─────────────────────────────────────────────────

const VM_MOD_RS: &str = include_str!("../src/vm/mod.rs");
const VM_EXECUTE_RS: &str = include_str!("../src/vm/execute.rs");

// ── Canonical-prefix lock: `"internal VM error:"` not `"internal:"` ─

/// Every `VmError::new("internal: ...")` literal in `src/vm/mod.rs`
/// must be replaced with `"internal VM error: ..."`. We grep for the
/// bare literal prefix `"internal:` (opening double quote + prefix)
/// so legitimate non-literal occurrences (doc comments, etc.) don't
/// trigger a false positive.
#[test]
fn vm_mod_uses_canonical_internal_prefix() {
    assert!(
        !VM_MOD_RS.contains("\"internal:"),
        "src/vm/mod.rs contains a bare '\"internal:' string-literal \
         prefix; use '\"internal VM error:' instead to match the \
         canonical round-58 prefix"
    );
}

/// `src/vm/execute.rs` had one stray `"internal:"` prefix at
/// `StringConcat` that escaped the round-58 sweep; the rest of the
/// file is already canonical. Lock the file as a whole now.
#[test]
fn vm_execute_uses_canonical_internal_prefix() {
    assert!(
        !VM_EXECUTE_RS.contains("\"internal:"),
        "src/vm/execute.rs contains a bare '\"internal:' \
         string-literal prefix; use '\"internal VM error:' instead"
    );
}

// ── Rust-identifier leak lock: `read_byte` not in user-facing text ─

/// The `read_byte` method on `Vm` exists as a Rust identifier — that
/// is expected and unavoidable. What must NOT happen is the
/// identifier appearing inside a `VmError::new(...)` string literal
/// the end user can see. We approximate "inside a string literal" by
/// searching for `read_byte` flanked on the left by a double-quote
/// or by text indicating it is embedded in prose.
#[test]
fn vm_mod_does_not_leak_read_byte_rust_identifier() {
    // Form 1: verbatim `"... read_byte ..."` string-literal fragment.
    // The round-58-style leak text was
    // `"internal: no call frame in read_byte"`; the canonical
    // replacement is
    // `"internal VM error: no call frame while reading bytecode"`.
    assert!(
        !VM_MOD_RS.contains(" in read_byte"),
        "src/vm/mod.rs leaks the Rust identifier 'read_byte' inside a \
         user-facing VmError message (' in read_byte' substring found); \
         rephrase using neutral wording like 'while reading bytecode'"
    );
    assert!(
        !VM_MOD_RS.contains("read_byte\""),
        "src/vm/mod.rs contains 'read_byte\"' which indicates the Rust \
         identifier is being emitted as the tail of a user-facing \
         string literal; rephrase using neutral wording"
    );
}

// ── Post-fix canonical presence: guard against bare-string removal ──

/// Once the leaks are gone, the canonical replacement phrasing must
/// be the one actually compiled into the VM. This guards against a
/// future refactor that *removes* the leaky text but forgets to
/// introduce the replacement (mirrors
/// `test_canonical_user_facing_replacements_present` in
/// tests/vm_error_identifier_leak_tests.rs).
#[test]
fn vm_mod_canonical_replacements_present() {
    let expected = [
        "\"internal VM error: no call frame while reading bytecode\"",
        "\"internal VM error: no call frame\"",
    ];
    for needle in expected {
        assert!(
            VM_MOD_RS.contains(needle),
            "src/vm/mod.rs missing canonical user-facing replacement: {needle}"
        );
    }
}
