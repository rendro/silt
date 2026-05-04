//! Round 71 LATENT lock tests.
//!
//! ERR-2: `test.assert*` failure messages must render values via the
//!   silt-canonical `Value::format_silt` (no `ExtFloat(...)` Rust
//!   variant leak). Driven through a real `silt test` subprocess so the
//!   runtime path actually fires (a typecheck-only / format-only lock
//!   would not touch `call_test`).
//!
//! DX-3: the duplicated user-import-resolvable predicate must live in
//!   the shared `src/diagnostic_filters.rs` module. The CLI and LSP
//!   call sites must no longer carry their own copy of the message
//!   substring set — they should delegate to the shared helper. Same
//!   shape applied to `is_unknown_module_warning`.
//!
//! DX-5: `src/lsp/folding.rs` must not claim "silt has no block
//!   comments" — the lexer supports nested `{- -}` block comments.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── Test-file helpers (subprocess pattern) ──────────────────────────

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_round71_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

// ── ERR-2: assert formatting via real `silt test` subprocess ────────

/// Repro from the audit: dividing two `Float` produces an `ExtFloat`,
/// so `test.assert_eq(1.0/3.0, 0.3333)` previously rendered as
/// `assertion failed: ExtFloat(0.333…) != 0.3333`. The fix routes the
/// values through `Value::format_silt`, which writes ExtFloat as a
/// bare numeric literal.
#[test]
fn err2_silt_test_assert_eq_failure_does_not_leak_extfloat_variant_name() {
    let dir = temp_dir("err2");
    let test_file = dir.join("extfloat_test.silt");
    fs::write(
        &test_file,
        // `Float / Float` widens to `ExtFloat` (per
        // `src/vm/arithmetic.rs:53-55`), so both operands here are
        // ExtFloat at runtime — and the typechecker accepts the call
        // because both sides have the same static type. The values
        // are obviously unequal (1/3 vs 1/4), so the assertion fails
        // and the formatter is exercised on the runtime path.
        "import test\n\nfn test_extfloat_render() {\n  test.assert_eq(1.0/3.0, 1.0/4.0)\n}\n",
    )
    .unwrap();

    let output = silt_cmd()
        .arg("test")
        .arg(&test_file)
        .output()
        .expect("failed to spawn `silt test`");

    // The test must FAIL (by design — the values aren't equal).
    // Combine stdout+stderr because the test runner writes summary
    // lines to one stream and assertion bodies to another depending
    // on the path.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // Sanity: this should be the assertion failure path.
    assert!(
        combined.contains("assertion failed"),
        "expected an assertion-failed message in `silt test` output. \
         stdout={stdout:?} stderr={stderr:?}"
    );

    // The lock: no Rust variant name leak. If this assertion ever
    // re-fails, the assert builtins have regressed to using `{:?}`
    // (Debug) on `Value`.
    assert!(
        !combined.contains("ExtFloat("),
        "`silt test` failure rendered the Rust variant name `ExtFloat(...)` — \
         the assert builtins must format values with `Value::format_silt` so \
         the user-visible failure shows a bare numeric literal. \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

/// `test.assert(false)` (single-arg form, falsy) likewise uses
/// `format_silt` post-fix. We don't expect ExtFloat here — but we lock
/// that the message still says "assertion failed: false" (i.e. didn't
/// regress to "assertion failed: Bool(false)" if Debug ever changed).
#[test]
fn err2_silt_test_assert_single_arg_false_renders_canonically() {
    let dir = temp_dir("err2_single");
    let test_file = dir.join("assert_test.silt");
    fs::write(
        &test_file,
        "import test\n\nfn test_false_renders() {\n  test.assert(false)\n}\n",
    )
    .unwrap();

    let output = silt_cmd()
        .arg("test")
        .arg(&test_file)
        .output()
        .expect("failed to spawn `silt test`");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(
        combined.contains("assertion failed: false"),
        "expected `assertion failed: false`, got: {combined}"
    );
}

// ── DX-3: shared predicate dedupe ───────────────────────────────────

#[test]
fn dx3_lsp_diagnostics_no_longer_inlines_predicate_body() {
    // The duplicated message-substring set must live in the shared
    // module now. If someone re-inlines the predicate, this fails.
    let src = include_str!("../src/lsp/diagnostics.rs");

    // The whole point of the shared module: there must NOT be an
    // inlined cluster of substring matches in this file anymore.
    // Belt-and-braces — assert the most distinctive substrings of
    // the original predicate are absent in the LSP file.
    assert!(
        !src.contains("\"undefined variable\""),
        "src/lsp/diagnostics.rs still contains an inlined `\"undefined variable\"` \
         match — the predicate should have been lifted into \
         src/diagnostic_filters.rs"
    );
    assert!(
        !src.contains("\"undefined constructor\""),
        "src/lsp/diagnostics.rs still contains an inlined `\"undefined constructor\"` \
         match — the predicate should have been lifted into \
         src/diagnostic_filters.rs"
    );
    assert!(
        !src.contains("\"unknown field\""),
        "src/lsp/diagnostics.rs still contains an inlined `\"unknown field\"` \
         match — the predicate should have been lifted into \
         src/diagnostic_filters.rs"
    );
    // The LSP wrapper must call the shared helpers.
    assert!(
        src.contains("diagnostic_filters::is_user_import_resolvable_error_message"),
        "src/lsp/diagnostics.rs must delegate to \
         `diagnostic_filters::is_user_import_resolvable_error_message`"
    );
    assert!(
        src.contains("diagnostic_filters::is_unknown_module_warning_message"),
        "src/lsp/diagnostics.rs must delegate to \
         `diagnostic_filters::is_unknown_module_warning_message`"
    );
}

#[test]
fn dx3_cli_pipeline_no_longer_inlines_predicate_body() {
    let src = include_str!("../src/cli/pipeline.rs");

    // Same prefix-set, this time on the SourceError-flavoured side.
    // We allow the doc comment near `is_user_import_resolvable_error`
    // to keep referencing the shapes by name as prose, but the actual
    // *code* must not carry the substring-set anymore. Disambiguate
    // by looking for the multi-pattern `||` chain that defined the
    // old body.
    assert!(
        !src.contains("err.message.starts_with(\"undefined variable\")"),
        "src/cli/pipeline.rs still inlines the `undefined variable` prefix \
         check — the predicate should have been lifted into \
         src/diagnostic_filters.rs"
    );
    assert!(
        !src.contains("err.message.starts_with(\"undefined constructor\")"),
        "src/cli/pipeline.rs still inlines the `undefined constructor` prefix \
         check — the predicate should have been lifted into \
         src/diagnostic_filters.rs"
    );
    // Must call the shared helpers.
    assert!(
        src.contains("diagnostic_filters::is_user_import_resolvable_error_message"),
        "src/cli/pipeline.rs must delegate to \
         `diagnostic_filters::is_user_import_resolvable_error_message`"
    );
    assert!(
        src.contains("diagnostic_filters::is_unknown_module_warning_message"),
        "src/cli/pipeline.rs must delegate to \
         `diagnostic_filters::is_unknown_module_warning_message`"
    );
}

#[test]
fn dx3_shared_module_exposes_both_predicates() {
    // Belt: assert the shared module file actually exists and exports
    // the names the call sites depend on. (`include_str!` panics at
    // compile time if the file is missing, so reaching this body
    // already proves existence — but verify the names too.)
    let src = include_str!("../src/diagnostic_filters.rs");
    assert!(
        src.contains("pub fn is_unknown_module_warning_message"),
        "src/diagnostic_filters.rs must export \
         `is_unknown_module_warning_message`"
    );
    assert!(
        src.contains("pub fn is_user_import_resolvable_error_message"),
        "src/diagnostic_filters.rs must export \
         `is_user_import_resolvable_error_message`"
    );
}

// ── DX-5: stale "no block comments" prose ───────────────────────────

#[test]
fn dx5_folding_doc_does_not_claim_no_block_comments() {
    let src = include_str!("../src/lsp/folding.rs");
    assert!(
        !src.contains("has no block comments"),
        "src/lsp/folding.rs still claims silt `has no block comments` — \
         the lexer supports nested `{{- -}}` block comments \
         (see lexer::Lexer::skip_block_comment). Update the comment."
    );
}
