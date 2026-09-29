//! Regression lock for BROKEN(round 59): the Rust snippet at
//! docs/ffi.md showed `e.message()` (method call) on a `VmError`, but
//! `src/vm/error.rs` declares `pub message: String` — it's a public
//! field, not a method. The snippet therefore failed to compile for
//! any first-time embedder copy-pasting from the FFI guide with
//! `no method named 'message' found ... field, not a method`.
//!
//! The fix changes `e.message()` to `e.message`. This test locks the
//! fix in place by grep-asserting that the FFI guide never pairs
//! `e.message()` with the "silt error" phrasing that identifies the
//! surrounding example, and, symmetrically, that the corrected field
//! access `e.message` IS present.

use std::fs;

fn ffi_doc() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/ffi.md");
    fs::read_to_string(path).expect("docs/ffi.md must exist and be readable")
}

/// The literal method-call form must never appear next to the
/// "silt error" log line that frames the Rust example. We anchor
/// against the exact phrase to avoid false positives (unrelated
/// `e.message()` strings in silt-language examples inside the same
/// file — silt does use a `.message()` method on its builtin Error
/// trait).
#[test]
fn ffi_doc_does_not_call_message_as_method_on_vmerror() {
    let doc = ffi_doc();
    // The original BROKEN line, verbatim. Locking on the full phrase
    // keeps false positives out (silt code inside the same doc may
    // legitimately use `.message()` on a silt-side Error value).
    assert!(
        !doc.contains("eprintln!(\"silt error: {}\", e.message())"),
        "docs/ffi.md must not call `e.message()` on a Rust-side `VmError` — \
         `message` is a `pub String` field (see src/vm/error.rs). Use \
         `e.message` (field access) instead."
    );

    // Broader guard: the "silt error:" phrasing must not be followed
    // on the same line by `.message()`. If a future editor rewrites
    // the example but keeps the method call, we still catch it.
    for line in doc.lines() {
        if line.contains("silt error:") {
            assert!(
                !line.contains(".message()"),
                "docs/ffi.md line {:?} reintroduces a `.message()` method \
                 call on a VmError. That field is not a method; use \
                 `e.message` instead.",
                line
            );
        }
    }
}

/// The corrected field access must actually be present. Without this
/// assertion, the first test would trivially pass if the example were
/// deleted entirely — which would silently remove a useful snippet.
#[test]
fn ffi_doc_uses_message_field_access_on_vmerror() {
    let doc = ffi_doc();
    assert!(
        doc.contains("eprintln!(\"silt error: {}\", e.message)"),
        "docs/ffi.md must show the corrected field access `e.message` \
         in the VmError example (the matching Rust snippet near the \
         'silt error' log line). Got no such line."
    );
}
