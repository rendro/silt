//! Regression test for `VmError::Display`.
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
