//! Round 101: polymorphic `.display()` on a no-Display value must be
//! rejected at the execution site, exactly like string interpolation is.
//!
//! ## Background
//!
//! Round 95 closed the polymorphic string-interpolation hole: `fn
//! show(x: a) -> String { "{x}" }` errors at runtime ("type 'Fn' does
//! not implement Display (required for string interpolation)") when
//! called with a function / channel / handle / Tcp resource /
//! descriptor, via the `value_implements_display` gate in
//! `Op::DisplayValue` (src/vm/execute.rs).
//!
//! But the METHOD spelling of the same operation was left ungated: the
//! "display" arm of `Vm::dispatch_trait_method` (src/vm/dispatch.rs)
//! unconditionally returned `display_value(receiver)`. So
//!
//!     fn show(x: a) -> String { x.display() }
//!     fn main() { println(show(fn(y) { y })) }
//!
//! silently printed `<fn:<lambda>>` and exited 0 — while (1) the
//! concrete `f.display()` is a compile error ("unknown method 'display'
//! on type Fn"), (2) the interpolation twin `"{x}"` errors at runtime,
//! and (3) the sibling `.equal()` / `.compare()` arms in the SAME
//! function carry their own runtime gates. silt enforces inferred trait
//! bounds at the execution site, so the VM is the layer responsible for
//! rejecting this.
//!
//! The fix mirrors the Op::DisplayValue gate into the "display" arm,
//! sourced from the single oracle `Vm::value_implements_display`, with
//! the message "type '{name}' does not implement Display" (no
//! interpolation suffix — this is not an interpolation site).
//!
//! Trace of the buggy path: `Op::CallMethod` on a Var-typed receiver ->
//! qualified global `Fn.display` / `Channel.display` absent (Display is
//! never auto-derived for these types) -> `dispatch_trait_method`
//! "display" arm -> pre-fix fell through to `Display for Value`
//! (`<fn:name>`, `<channel:id>`, `<handle:id>`).
//!
//! Container-of-fn shapes need NO gate: `"{[f]}"` and `[f].display()`
//! are language-accepted (both print `[<fn:<lambda>>]`), so the
//! top-level-shape oracle is exactly right and the positive controls
//! below guard against over-rejection.
//!
//! These tests exercise the EXECUTION path end-to-end via the `silt
//! run` CLI (task.spawn needs the real scheduler runtime), asserting on
//! exit status + stderr, not typecheck results.

use std::process::Command;

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_r101_display_gate_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// Run a program that must FAIL at runtime with a Display error naming
/// `type_name`, and must NOT have silently rendered `silent_render`
/// to stdout first (the pre-fix symptom).
fn assert_display_gate_fires(label: &str, src: &str, type_name: &str, silent_render: &str) {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        !ok,
        "{label}: expected non-zero exit (runtime Display error), got \
         success; stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stderr.contains("does not implement Display") && stderr.contains(type_name),
        "{label}: expected a Display runtime error naming {type_name}, \
         got stderr: {stderr}"
    );
    // The method spelling is NOT an interpolation site — the round-95
    // suffix must not leak into this diagnostic.
    assert!(
        !stderr.contains("required for string interpolation"),
        "{label}: .display() gate must not carry the interpolation \
         suffix, got stderr: {stderr}"
    );
    assert!(
        !stdout.contains(silent_render),
        "{label}: the pre-fix silent rendering {silent_render:?} \
         reached stdout: {stdout}"
    );
}

// ── The gate fires: lambda, channel, task handle ────────────────────

#[test]
fn polymorphic_display_of_lambda_errors_at_runtime() {
    // Pre-fix: printed `<fn:<lambda>>`, exit 0.
    let src = r#"
fn show(x: a) -> String { x.display() }
fn main() { println(show(fn(y) { y })) }
"#;
    assert_display_gate_fires("lambda", src, "'Fn'", "<fn:");
}

#[test]
fn polymorphic_display_of_channel_errors_at_runtime() {
    // Pre-fix: printed `<channel:0>`, exit 0.
    let src = r#"
import channel
fn show(x: a) -> String { x.display() }
fn main() {
  let c = channel.new(1)
  println(show(c))
}
"#;
    assert_display_gate_fires("channel", src, "'Channel'", "<channel:");
}

#[test]
fn polymorphic_display_of_task_handle_errors_at_runtime() {
    // Pre-fix: printed `<handle:0>`, exit 0. `task.spawn` needs the
    // real scheduler runtime, which `silt run` provides.
    let src = r#"
import task
fn show(x: a) -> String { x.display() }
fn main() {
  let h = task.spawn(fn() { 42 })
  println(show(h))
}
"#;
    assert_display_gate_fires("handle", src, "'Handle'", "<handle:");
}

// ── Positive controls: the gate must not over-reject ────────────────

#[test]
fn polymorphic_display_of_display_values_still_works() {
    // Int and String route through the same dispatch_trait_method
    // "display" arm; the stdlib error enum (ParseError) and the user
    // record exercise the variant/record display path. All four must
    // keep succeeding.
    let src = r#"
import int
type P { name: String, age: Int }
fn show(x: a) -> String { x.display() }
fn main() {
    println(show(42))
    println(show("hi"))
    println(show(ParseEmpty))
    println(show(P { name: "x", age: 3 }))
}
"#;
    let (stdout, stderr, ok) = run_silt_raw("positive_controls", src);
    assert!(
        ok,
        "Display-able values must still render through .display(); \
         stdout={stdout}, stderr={stderr}"
    );
    let lines: Vec<&str> = stdout.trim().split('\n').collect();
    assert_eq!(lines.len(), 4, "expected 4 output lines, got: {stdout}");
    assert_eq!(lines[0], "42", "Int .display(): {stdout}");
    assert_eq!(lines[1], "hi", "String .display(): {stdout}");
    assert!(
        !lines[2].is_empty(),
        "stdlib error enum .display() must render: {stdout}"
    );
    assert!(
        lines[3].contains("P"),
        "user record .display() must render the record: {stdout}"
    );
}

#[test]
fn concrete_list_of_fn_display_is_still_accepted() {
    // Container-of-fn is deliberately NOT gated: the top-level shape
    // is List, which implements Display; elements render via `Display
    // for Value`. This is language-accepted concretely, so the
    // top-level-shape oracle must keep accepting it.
    let src = r#"
fn main() {
  let f = fn(y) { y }
  println([f].display())
}
"#;
    let (stdout, stderr, ok) = run_silt_raw("list_of_fn", src);
    assert!(
        ok,
        "[f].display() is language-accepted and must keep working; \
         stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("[<fn:"),
        "expected the list-of-fn rendering, got: {stdout}"
    );
}
