//! BROKEN T1 lock: a type-decl whose name shadows a builtin enum
//! variant constructor must produce ONE spanned diagnostic, not the
//! 8-error unspanned cascade `expected TypeOf, got ChannelResult`
//! that pre-fix `register_type_decl` emitted via the auto-derive +
//! TypeOf-stamping path.
//!
//! Background:
//!   `type Empty {}` parses as an empty enum (the parser's
//!   `is_record_body` heuristic falls through when there are no
//!   tokens), and `type Some { x: Int }` / `type Ok { x: Int }` /
//!   `type Closed { x: Int }` / `type Sent { x: Int }` parse as
//!   records. In all cases the type name shadows a builtin variant
//!   constructor (`Empty`/`Sent`/`Closed`/`Message` of
//!   `ChannelResult`, `Some`/`None` of `Option`, `Ok`/`Err` of
//!   `Result`). Pre-fix, the typechecker silently overwrote the
//!   variant binding via `env.define(td.name, TypeOf(...))`, then
//!   downstream auto-derive synth or stamping unified the residual
//!   variant scheme against the new TypeOf and emitted up to 8
//!   unspanned cascade errors.
//!
//! The fix lives in `src/typechecker/mod.rs::register_type_decl`,
//! which now early-returns with one spanned shadow diagnostic when
//! `td.name` already exists in `self.variant_to_enum` AND its owning
//! enum is not also `td.name` (the latter caveat keeps the existing
//! enum-vs-builtin-enum warning intact for `type Option { ... }`).
//!
//! Strategy:
//!   - Subprocess `silt check` invocations against tiny .silt files,
//!     mirroring tests/lambda_effect_enforcement_tests.rs.
//!   - Anti-cascade asserts: stderr contains AT MOST one diagnostic
//!     and does NOT contain 8 copies of the cascade message.
//!   - Anti-empty-span assert: the rendered diagnostic must include
//!     the source-line context (`type X ...`) or a caret marker —
//!     proves the diagnostic carries a real span.
//!   - Negative cover: `type MyRecord { x: Int }` must still
//!     typecheck cleanly (the guard is precise, not a sledgehammer).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_record_shadow_builtin_variant_tests");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{prefix}_{n}.silt"));
    fs::write(&path, content).unwrap();
    path
}

fn check_output(path: &PathBuf) -> (bool, String) {
    let output = silt_cmd()
        .arg("check")
        .arg(path)
        .output()
        .expect("silt check");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Count occurrences of the cascade error message so the anti-cascade
/// assertion can prove the pre-fix 8x-repeat is gone.
fn count_cascade_lines(stderr: &str) -> usize {
    stderr
        .lines()
        .filter(|l| l.contains("expected TypeOf, got ChannelResult"))
        .count()
}

/// Count the `error[...]: ...` headers — the diagnostic count.
fn count_error_headers(stderr: &str) -> usize {
    stderr.lines().filter(|l| l.starts_with("error[")).count()
}

#[test]
fn record_named_empty_emits_single_shadow_diagnostic_not_eight_cascade() {
    // `type Empty {}` is the original repro: parses as an empty
    // enum body, and `Empty` is a builtin variant of `ChannelResult`.
    // Pre-fix: 8 copies of the cascade. Post-fix: 1 spanned shadow.
    let path = temp_silt_file("empty_shadow", "type Empty {}\nfn main() = ()\n");
    let (success, stderr) = check_output(&path);
    assert!(
        !success,
        "shadowing a builtin variant must reject the program; stderr=`{stderr}`"
    );
    let cascades = count_cascade_lines(&stderr);
    assert!(
        cascades < 8,
        "pre-fix produced 8 `expected TypeOf, got ChannelResult` lines — \
         post-fix must collapse to 0 (or at most 1). Got {cascades}; stderr=`{stderr}`"
    );
    let headers = count_error_headers(&stderr);
    assert!(
        headers <= 2,
        "expected at most 2 diagnostic headers, got {headers}; stderr=`{stderr}`"
    );
    assert!(
        stderr.contains("shadows variant of builtin enum"),
        "expected shadow-diagnostic wording, got stderr=`{stderr}`"
    );
    assert!(
        stderr.contains("ChannelResult"),
        "expected the owning builtin enum name, got stderr=`{stderr}`"
    );
}

#[test]
fn record_named_some_emits_single_shadow_diagnostic() {
    let path = temp_silt_file("some_shadow", "type Some { x: Int }\nfn main() = ()\n");
    let (success, stderr) = check_output(&path);
    assert!(
        !success,
        "type Some shadowing Option::Some must be rejected; stderr=`{stderr}`"
    );
    let headers = count_error_headers(&stderr);
    assert!(
        headers <= 2,
        "expected at most 2 diagnostic headers, got {headers}; stderr=`{stderr}`"
    );
    assert!(
        stderr.contains("shadows variant of builtin enum"),
        "expected shadow-diagnostic wording, got stderr=`{stderr}`"
    );
    assert!(
        stderr.contains("Option"),
        "expected owning builtin enum 'Option', got stderr=`{stderr}`"
    );
    // Anti-cascade for the Some/Option flavor: the pre-fix tail used
    // a 'record-pattern syntax' message; ensure it's not flooding now.
    let pattern_repeats = stderr
        .lines()
        .filter(|l| l.contains("use record-pattern syntax"))
        .count();
    assert!(
        pattern_repeats < 4,
        "expected the pre-fix pattern-syntax cascade to be suppressed, \
         got {pattern_repeats} copies; stderr=`{stderr}`"
    );
}

#[test]
fn record_named_ok_emits_single_shadow_diagnostic() {
    let path = temp_silt_file("ok_shadow", "type Ok { x: Int }\nfn main() = ()\n");
    let (success, stderr) = check_output(&path);
    assert!(
        !success,
        "type Ok shadowing Result::Ok must be rejected; stderr=`{stderr}`"
    );
    let headers = count_error_headers(&stderr);
    assert!(
        headers <= 2,
        "expected at most 2 diagnostic headers, got {headers}; stderr=`{stderr}`"
    );
    assert!(
        stderr.contains("shadows variant of builtin enum"),
        "expected shadow-diagnostic wording, got stderr=`{stderr}`"
    );
    assert!(
        stderr.contains("Result"),
        "expected owning builtin enum 'Result', got stderr=`{stderr}`"
    );
}

#[test]
fn non_shadowing_record_name_typechecks_clean() {
    // Anti-overfire: the guard must NOT reject record names that
    // happen to be unused — `MyRecord` is not a builtin variant.
    let path = temp_silt_file(
        "non_shadowing",
        "type MyRecord { x: Int }\nfn main() = ()\n",
    );
    let (success, stderr) = check_output(&path);
    assert!(
        success,
        "non-shadowing record name must typecheck cleanly; stderr=`{stderr}`"
    );
}

#[test]
fn record_shadow_diagnostic_carries_a_span() {
    // Anti-empty-span: the rendered diagnostic for the shadow case
    // must include the source-line preview (`type Empty ...`) or a
    // caret marker. Pre-fix the cascade was unspanned (no source
    // location at all), so this assertion would have failed.
    let path = temp_silt_file("span_check", "type Empty {}\nfn main() = ()\n");
    let (_success, stderr) = check_output(&path);
    let has_arrow = stderr.contains("--> ");
    let has_caret = stderr.contains('^');
    let has_source_line = stderr.contains("type Empty");
    assert!(
        has_arrow && has_caret && has_source_line,
        "shadow diagnostic must be spanned (arrow={has_arrow}, caret={has_caret}, \
         source_line={has_source_line}); stderr=`{stderr}`"
    );
}
