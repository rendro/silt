//! Regression tests for `VmError::Display` and the user-facing phrasing
//! of the frame-underflow runtime invariant.
//!
//! Audit GAP #6 (frame underflow): the error message at
//! `src/vm/execute.rs:745-747` previously read
//! `"frame underflow in invoke_callable"` — `invoke_callable` is a
//! Rust function name, not user-meaningful. Sibling round-35 F11 fixed
//! the unreachable `"cannot call value of type {}"` arm at
//! line 831-834; this site was missed. Fix uses user-facing language
//! (`"internal VM error: frame stack underflow during call"`) that still
//! flags the invariant for developers without exposing a Rust identifier.
//!
//! Audit LATENT L3 (VmError::Display attractive nuisance): the Display
//! impl emitted the raw `"VM error: <msg>"` prefix. Production paths
//! already route around it via `SourceError::runtime_at` (round 36),
//! but any fallback `eprintln!("{e}")` on a bare VmError reintroduced
//! the leak. Fix canonicalizes Display to the same
//! `error[runtime]: <msg>` shape produced by `SourceError::Display`.

use silt::VmError;

/// Lock: `format!("{err}")` on a bare `VmError::new(...)` produces the
/// canonical `error[runtime]:` header, NOT the old raw `"VM error:"`
/// prefix. Any fallback path that `eprintln!("{e}")`s a VmError will
/// now yield correctly-formed diagnostic output.
#[test]
fn test_vm_error_display_uses_canonical_runtime_header() {
    let err = VmError::new("something went wrong".into());
    let rendered = format!("{err}");
    assert!(
        rendered.starts_with("error[runtime]:"),
        "expected `error[runtime]:` prefix, got: {rendered:?}"
    );
    assert!(
        !rendered.contains("VM error:"),
        "Display must not re-emit the raw `VM error:` prefix; got: {rendered:?}"
    );
    assert!(
        rendered.contains("something went wrong"),
        "expected original message preserved; got: {rendered:?}"
    );
}

/// Lock: the frame-underflow invariant message in `src/vm/execute.rs`
/// uses user-facing language (`"frame stack underflow"`) and does NOT
/// leak the Rust identifier `invoke_callable`.
///
/// The surface-level trigger is not reachable from normal silt
/// programs: hitting this branch requires the VM's callback-invocation
/// code path to pop more frames than it pushed during a single
/// `invoke_callable` call, which is a pure internal-invariant
/// violation (nothing in user-space can drive the frame count below
/// `saved_frame_count`). We therefore lock the *source-level* string by
/// grepping `src/vm/execute.rs` directly — the previous shape of this
/// test constructed a `VmError` from the fixed string, which is
/// tautological: reverting the fix in `execute.rs` back to the leaky
/// message would still pass, because the test literal was the one
/// being asserted. The source-level grep closes that loop.
#[test]
fn test_frame_underflow_message_is_user_facing() {
    let src = include_str!("../src/vm/execute.rs");
    assert!(
        !src.contains("frame underflow in invoke_callable"),
        "execute.rs must not leak the `invoke_callable` Rust identifier \
         via the legacy `frame underflow in invoke_callable` literal"
    );
    assert!(
        src.contains("\"internal VM error: frame stack underflow during call\""),
        "execute.rs must contain the canonical round-58 user-facing \
         frame-underflow message `internal VM error: frame stack underflow during call`"
    );
}
