//! Regression test for REGRESSION(f613098): docs/ffi.md must not reference
//! the removed `Vm::register_fn3` API. Round 52 deleted `register_fn3` as
//! dead code (source lock at tests/dead_code_lock_tests.rs), but the doc
//! drifted and still showed `register_fn3` examples, which would cause
//! cargo errors for any first-time embedder copying from the guide.
//!
//! This test locks the doc to reference only the surviving typed
//! registration helpers (`register_fn0`, `register_fn1`, `register_fn2`).

use std::fs;

#[test]
fn docs_ffi_does_not_reference_deleted_register_fn3() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/ffi.md");
    let contents = fs::read_to_string(path)
        .expect("docs/ffi.md must exist and be readable");

    assert!(
        !contents.contains("register_fn3"),
        "docs/ffi.md must NOT reference the deleted `register_fn3` API \
         (removed in round 52, commit f613098). Found a stale mention; \
         update the doc to use `register_fn0` / `register_fn1` / \
         `register_fn2` instead."
    );

    // Surviving APIs must still be documented so readers learn the typed
    // registration helpers that do exist.
    for expected in ["register_fn0", "register_fn1", "register_fn2"] {
        assert!(
            contents.contains(expected),
            "docs/ffi.md should still mention `{expected}` so readers \
             learn the typed registration helpers that still exist."
        );
    }
}
