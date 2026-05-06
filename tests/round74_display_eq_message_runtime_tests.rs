//! Round-74 BROKEN lock: `e.display()` must equal `format!("{e}")`
//! must equal `e.message()` for every stdlib error enum variant.
//!
//! Round-73f fixed `format!("{e}")` (Variant's Display impl in
//! `src/value.rs`) by routing through
//! `vm::dispatch::render_stdlib_error_message`. But the trait method
//! `e.display()` is serviced by the auto-derived constructor-form
//! impl synthesized in `src/typechecker/auto_derive.rs::
//! synth_display_impl_for_enum`, which emitted the constructor form
//! (`IoNotFound(nope)`) instead of the message form
//! (`file not found: nope`). Same logical operation, two outputs —
//! exactly the dual-shape silent-wrong-answer the round-73f fix was
//! meant to close. The doc at
//! `docs/language/error-handling.md` claims equivalence; the trait
//! method violated it.
//!
//! Round-74 closes the gap inside `synth_display_impl_for_enum`: when
//! the type being derived is a stdlib error enum (registry consulted
//! via `crate::module::builtin_error_enum_variants_with_arity`), the
//! synthesized `display(self)` body delegates to `self.message()`
//! rather than emitting the per-variant constructor match. User
//! enums are unaffected (the registry only covers stdlib errors), so
//! `Color::Green(42).display()` still renders as `"Green(42)"`.

use std::process::Command;

const AUTO_DERIVE_RS: &str = include_str!("../src/typechecker/auto_derive.rs");

/// Behavioral lock: all three rendering shapes — `format!("{e}")`,
/// `e.message()`, and `e.display()` — must agree on the message-form
/// text for a stdlib `IoError` variant.
///
/// Subprocess test (NOT typecheck-only): the bug lives in the
/// runtime-dispatched method body, so the test must compile and run
/// the program end-to-end.
#[test]
fn display_method_equals_format_equals_message_for_io_error_runtime() {
    let src = r#"
import io
fn main() {
    match io.read_file("nope_round74_runtime_lock.txt") {
        Ok(_) -> ()
        Err(e) -> {
            let display_form = "{e}"
            let message_form = e.message()
            let display_method_form = e.display()
            match display_form == message_form {
                true -> ()
                false -> {
                    println("DRIFT_FORMAT_VS_MESSAGE")
                    println(display_form)
                    println(message_form)
                }
            }
            match display_method_form == message_form {
                true -> ()
                false -> {
                    println("DRIFT_DISPLAY_METHOD_VS_MESSAGE")
                    println(display_method_form)
                    println(message_form)
                }
            }
            match display_method_form == display_form {
                true -> println("EQUIV_ALL_THREE")
                false -> {
                    println("DRIFT_DISPLAY_METHOD_VS_FORMAT")
                    println(display_method_form)
                    println(display_form)
                }
            }
            -- And finally: assert the canonical message-form text
            -- (not the constructor form). If the message form ever
            -- drifts to the constructor form, this catches it.
            match message_form == "file not found: nope_round74_runtime_lock.txt" {
                true -> println("CANONICAL_TEXT")
                false -> {
                    println("DRIFT_CANONICAL_TEXT")
                    println(message_form)
                }
            }
        }
    }
}
"#;
    let dir = std::env::temp_dir().join("silt_round74_display_eq_message_runtime");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tempdir");
    let path = dir.join("repro.silt");
    std::fs::write(&path, src).expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("invoke silt run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.lines().any(|l| l.trim() == "EQUIV_ALL_THREE"),
        "expected `e.display() == format!(\"{{e}}\") == e.message()`. \
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.lines().any(|l| l.trim() == "CANONICAL_TEXT"),
        "expected message form to be canonical `file not found: ...` text \
         (not constructor form). stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.lines().any(|l| l.trim().starts_with("DRIFT_")),
        "no DRIFT_ markers should appear. stdout: {stdout}"
    );
}

/// Same lock but for a different stdlib error enum (`JsonError`),
/// to ensure the fix is not accidentally io-specific. JsonSyntax
/// takes 2 args; if the synth ever falls back to the constructor
/// form, the printed text would include `JsonSyntax(...)` and
/// drift from `e.message()`.
#[test]
fn display_method_equals_message_for_json_error_runtime() {
    let src = r#"
import json
fn main() {
    match json.parse("not valid json at all", String) {
        Ok(_) -> ()
        Err(e) -> {
            let display_method_form = e.display()
            let message_form = e.message()
            match display_method_form == message_form {
                true -> println("EQUIV")
                false -> {
                    println("DRIFT")
                    println(display_method_form)
                    println(message_form)
                }
            }
        }
    }
}
"#;
    let dir = std::env::temp_dir().join("silt_round74_display_eq_message_json");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tempdir");
    let path = dir.join("repro.silt");
    std::fs::write(&path, src).expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("invoke silt run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.lines().any(|l| l.trim() == "EQUIV"),
        "expected `e.display() == e.message()` for JsonError variants. \
         stdout: {stdout}\nstderr: {stderr}"
    );
}

/// User-defined enums must keep the constructor form for `display()`.
/// The fix targets stdlib error enums only; user code is unchanged.
#[test]
fn user_enum_display_keeps_constructor_form() {
    let src = r#"
type Color { Red, Green(Int), Blue }
fn main() {
    let c = Green(42)
    println(c.display())
}
"#;
    let dir = std::env::temp_dir().join("silt_round74_user_enum_display_method");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tempdir");
    let path = dir.join("user_enum.silt");
    std::fs::write(&path, src).expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("invoke silt run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Green(42)"),
        "user-defined enum `display()` must keep the constructor form. \
         Got: {stdout}"
    );
}

/// Structural lock: the auto-derive Display synthesis must consult
/// the stdlib-error-enum registry. Reverting the gate would silently
/// restore the dual-shape divergence.
#[test]
fn auto_derive_consults_stdlib_error_registry_for_display() {
    assert!(
        AUTO_DERIVE_RS.contains("is_stdlib_error_enum"),
        "auto_derive.rs must define an `is_stdlib_error_enum` gate \
         consulted by `synth_display_impl_for_enum` — otherwise the \
         constructor-form body would silently re-emerge for stdlib \
         error enums"
    );
    assert!(
        AUTO_DERIVE_RS.contains("builtin_error_enum_variants_with_arity"),
        "auto_derive.rs must consult the authoritative stdlib-error \
         registry (the same one used by \
         `vm::dispatch::render_stdlib_error_message` for \
         `format!(\"{{e}}\")`) so the two paths stay in lock-step"
    );
}
