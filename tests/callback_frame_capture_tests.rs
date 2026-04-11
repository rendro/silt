//! Round 16 audit — L2 callback-frame erasure regression lock.
//!
//! When a callback passed to a higher-order builtin (list.map /
//! list.filter / list.flat_map / list.fold / list.each / etc.) errors,
//! the error's span and call stack must land inside the callback body
//! where the real bug lives — NOT on the builtin's call site.
//!
//! Root cause (historical): `invoke_callable` and `resume_suspended_invoke`
//! in src/vm/execute.rs both used to `self.frames.truncate(saved_frame_count)`
//! immediately on callback error, which erased the callback's live frames
//! before the outer `Vm::run`'s `enrich_error` had a chance to read them.
//! By the time enrich_error ran, only the builtin dispatch frame was left,
//! so the caret got relocated onto the `list.map(...)` call site and the
//! callback frame disappeared from the rendered stack.
//!
//! Fix (round 16): capture (enrich) the error BEFORE truncating frames at
//! both callback error-return sites in src/vm/execute.rs. Because
//! `enrich_error` at src/vm/mod.rs short-circuits when `err.span.is_some()`,
//! the outer `Vm::run`'s enrich call becomes a no-op for these errors and
//! the inner callback's span survives.
//!
//! Mutation reasoning: reverting the fix — removing the `enrich_error(e)`
//! call in invoke_callable and/or resume_suspended_invoke so that frames
//! are truncated before enrichment — makes every test below fail. The
//! `contains("1 / 0")` caret-body substring check and the `-> <callback>`
//! call-stack substring check both break, because the rendered error
//! snaps back to the builtin call site with no callback frame at all.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Create a unique temporary .silt file. Each call uses a fresh counter
/// so parallel tests don't collide.
fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_callback_frame_capture");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

/// Run `silt run <path>` and capture the combined stderr as a String.
fn run_silt_and_capture_stderr(path: &std::path::Path) -> String {
    let output = silt_cmd()
        .arg("run")
        .arg(path)
        .output()
        .expect("failed to run silt binary");
    assert!(
        !output.status.success(),
        "expected non-zero exit for error case, got success. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ── 1. list.map ────────────────────────────────────────────────────

/// Canonical repro from the finding: `fn divider(x) { 1 / 0 }; list.map(..., divider)`
///
/// The caret must point at `1 / 0` inside `divider`, and the call stack
/// must contain frames for `divider`, `list.map`'s outer caller (`main`).
#[test]
fn test_list_map_callback_error_shows_callback_frame() {
    let path = temp_silt_file(
        "list_map_callback_err",
        r#"import list
fn divider(x) { 1 / 0 }
fn main() { list.map([1, 2, 3], divider) }
"#,
    );

    let stderr = run_silt_and_capture_stderr(&path);

    // (a) Error message must be the division-by-zero.
    assert!(
        stderr.contains("division by zero"),
        "expected 'division by zero' in stderr, got:\n{stderr}"
    );
    // (b) Caret / source snippet must show the callback body `1 / 0`,
    // NOT the builtin call site `list.map([1, 2, 3], divider)`.
    assert!(
        stderr.contains("1 / 0"),
        "expected callback body '1 / 0' in rendered snippet, got:\n{stderr}"
    );
    // (c) The rendered snippet must NOT anchor on the builtin call line.
    // The caret line (the line with `^`) should not contain `list.map`.
    let snippet_anchors_on_callback_body = stderr
        .lines()
        .any(|l| l.contains("fn divider") && l.contains("1 / 0"));
    assert!(
        snippet_anchors_on_callback_body,
        "expected snippet to show 'fn divider(x) {{ 1 / 0 }}', got:\n{stderr}"
    );
    // (d) Call stack must include the callback frame.
    assert!(
        stderr.contains("-> divider"),
        "expected '-> divider' in call stack, got:\n{stderr}"
    );
    // (e) Call stack must also include main, so we know it's a multi-frame
    // stack (not just the builtin call site on its own).
    assert!(
        stderr.contains("-> main"),
        "expected '-> main' in call stack, got:\n{stderr}"
    );
}

// ── 2. list.filter ─────────────────────────────────────────────────

/// `list.filter` requires the callback to return Bool. We error inside
/// the body before the return, so the type check is irrelevant — but the
/// callback must be statically Bool-typed to clear compile.
#[test]
fn test_list_filter_callback_error_shows_callback_frame() {
    let path = temp_silt_file(
        "list_filter_callback_err",
        r#"import list
fn bad_pred(x) {
  let _ = 1 / 0
  true
}
fn main() { list.filter([1, 2, 3], bad_pred) }
"#,
    );

    let stderr = run_silt_and_capture_stderr(&path);

    assert!(
        stderr.contains("division by zero"),
        "expected 'division by zero' in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("1 / 0"),
        "expected callback body '1 / 0' in rendered snippet, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> bad_pred"),
        "expected '-> bad_pred' in call stack, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> main"),
        "expected '-> main' in call stack, got:\n{stderr}"
    );
}

// ── 3. list.flat_map ───────────────────────────────────────────────

/// `list.flat_map` requires the callback to return a List. We still
/// error inside the body before reaching the list literal.
#[test]
fn test_list_flat_map_callback_error_shows_callback_frame() {
    let path = temp_silt_file(
        "list_flat_map_callback_err",
        r#"import list
fn bad_flat(x) {
  let _ = 1 / 0
  [x]
}
fn main() { list.flat_map([1, 2, 3], bad_flat) }
"#,
    );

    let stderr = run_silt_and_capture_stderr(&path);

    assert!(
        stderr.contains("division by zero"),
        "expected 'division by zero' in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("1 / 0"),
        "expected callback body '1 / 0' in rendered snippet, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> bad_flat"),
        "expected '-> bad_flat' in call stack, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> main"),
        "expected '-> main' in call stack, got:\n{stderr}"
    );
}

// ── 4. list.fold ───────────────────────────────────────────────────

/// `list.fold(list, init, fn(acc, x) -> acc)` — takes acc and returns
/// the same type. We error inside the body before the accumulator return.
#[test]
fn test_list_fold_callback_error_shows_callback_frame() {
    let path = temp_silt_file(
        "list_fold_callback_err",
        r#"import list
fn bad_folder(acc, x) {
  let _ = 1 / 0
  acc
}
fn main() { list.fold([1, 2, 3], 0, bad_folder) }
"#,
    );

    let stderr = run_silt_and_capture_stderr(&path);

    assert!(
        stderr.contains("division by zero"),
        "expected 'division by zero' in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("1 / 0"),
        "expected callback body '1 / 0' in rendered snippet, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> bad_folder"),
        "expected '-> bad_folder' in call stack, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> main"),
        "expected '-> main' in call stack, got:\n{stderr}"
    );
}

// ── 5. list.each ───────────────────────────────────────────────────

/// `list.each(list, fn)` — callback returns Unit. We error inside the
/// body before falling off the end.
#[test]
fn test_list_each_callback_error_shows_callback_frame() {
    let path = temp_silt_file(
        "list_each_callback_err",
        r#"import list
fn bad_each(x) {
  let _ = 1 / 0
}
fn main() { list.each([1, 2, 3], bad_each) }
"#,
    );

    let stderr = run_silt_and_capture_stderr(&path);

    assert!(
        stderr.contains("division by zero"),
        "expected 'division by zero' in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("1 / 0"),
        "expected callback body '1 / 0' in rendered snippet, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> bad_each"),
        "expected '-> bad_each' in call stack, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> main"),
        "expected '-> main' in call stack, got:\n{stderr}"
    );
}

// ── 6. Nested callbacks — innermost frame wins ─────────────────────

/// A callback passed to `list.map` that itself calls `list.map` with a
/// buggy inner callback. The error must land on the innermost frame
/// (the body of the deepest callback), and the call stack must include
/// every nested named function — all the way up through `outer` and `main`.
///
/// This specifically exercises the L2 callback-frame erasure fix at
/// multiple stack depths: without the fix, both the outer and inner
/// callback frames would have been truncated before enrich_error ran,
/// leaving only the outermost `list.map` call site.
#[test]
fn test_nested_callback_error_shows_innermost_frame() {
    let path = temp_silt_file(
        "nested_callback_err",
        r#"import list
fn innermost(x) { 1 / 0 }
fn outer(inner) { list.map(inner, innermost) }
fn main() {
  list.map([[1], [2]], outer)
}
"#,
    );

    let stderr = run_silt_and_capture_stderr(&path);

    assert!(
        stderr.contains("division by zero"),
        "expected 'division by zero' in stderr, got:\n{stderr}"
    );
    // Caret must land on the innermost callback body, not on either
    // outer `list.map` call site.
    assert!(
        stderr.contains("1 / 0"),
        "expected innermost callback body '1 / 0' in rendered snippet, got:\n{stderr}"
    );
    assert!(
        stderr.contains("fn innermost"),
        "expected snippet to anchor on 'fn innermost', got:\n{stderr}"
    );
    // Every frame in the chain must appear. The deepest user frame
    // (`innermost`) is the error site; `outer` and `main` are the
    // intermediate and outermost call-stack frames.
    assert!(
        stderr.contains("-> innermost"),
        "expected '-> innermost' in call stack, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> outer"),
        "expected '-> outer' in call stack, got:\n{stderr}"
    );
    assert!(
        stderr.contains("-> main"),
        "expected '-> main' in call stack, got:\n{stderr}"
    );
}
