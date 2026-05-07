//! Round-73 follow-up: locks for the three formerly-deferred audit
//! findings, now fixed in alignment with silt's stated principles
//! ("explicit over implicit / silent wrong answers are worse than
//! crashes" — `docs/language/design-decisions.md:48`; "one way" /
//! "collapse equivalent dual shapes" — `MEMORY.md`).
//!
//! Three fixes locked here:
//!
//! - **Fix #2** (LATENT, soundness barrier): `unify_anon_anon` /
//!   `unify_anon_nominal` previously dropped occurs-check failures
//!   silently. Five sites in `src/typechecker/mod.rs` now emit
//!   `"infinite type"` instead of silently dropping the binding.
//!   Not currently reachable from surface syntax (row tail vars don't
//!   transitively reach leftover record fields), but the barrier is
//!   load-bearing for any future row-var-thru-fields feature.
//!
//! - **Fix #1** (BROKEN, runtime): `format!("{e}")` and
//!   `e.message()` now produce the same text for stdlib error enums.
//!   Previously the doc claimed equivalence but the runtime rendered
//!   the variant constructor form (e.g. `IoNotFound(missing)`)
//!   instead of the typed message — exactly the silent-wrong-answer
//!   failure mode the language opposes. Routed via
//!   `vm::dispatch::render_stdlib_error_message` from
//!   `Value::Display`.
//!
//! - **Fix #3** (LATENT, dedup): `src/builtins/numeric.rs` and
//!   `src/builtins/string.rs` previously hand-rolled terse
//!   diagnostics (`"int.abs requires an int"`) that hid the offending
//!   kind. Replaced with `super::common::require_int` /
//!   `require_string` which produce the canonical
//!   `"<fn> requires <Kind>, got <kind>"` form.

use std::process::Command;
use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;

const NUMERIC_RS: &str = include_str!("../src/builtins/numeric.rs");
const STRING_RS: &str = include_str!("../src/builtins/string.rs");
const TYPECHECKER_MOD_RS: &str = include_str!("../src/typechecker/mod.rs");
const VM_DISPATCH_RS: &str = include_str!("../src/vm/dispatch.rs");
const MODULE_RS: &str = include_str!("../src/module.rs");
const VALUE_RS: &str = include_str!("../src/value.rs");
const ERROR_HANDLING_DOC: &str = include_str!("../docs/language/error-handling.md");

// ─── Fix #2: occurs-check defensive diagnostic ───────────────────────

#[test]
fn fix2_occurs_check_emits_infinite_type_not_silent_drop() {
    // Every `if !occurs_in(v, &leftover) { self.subst[v] = Some(leftover); }`
    // in row unification must have a paired `else` arm emitting the
    // canonical occurs-check diagnostic via `infinite_type_message`.
    // Reverting the fix would restore silent drops, which violates the
    // "explicit over implicit / silent wrong answers are worse than
    // crashes" principle.
    //
    // Round 74 Fix #5: the row-unif arms route through the shared
    // helper `Self::infinite_type_message(...)` so the diagnostic
    // wording matches the main `Var(v) ↔ t` arm
    // (`"infinite type: the type variable appears inside {t}"`).
    // Round 75 DEAD-5: the symmetric Open×Closed and Closed×Open
    // arms were collapsed into one shared `unify_open_closed` helper,
    // dropping the distinct-site count from 5 to 4 (the helper is
    // the single occurs_in site for both arms). The pinned shape is
    // still each-site-routes-through-helper; the count is the
    // post-dedup minimum.
    let occurs_sites = TYPECHECKER_MOD_RS.matches("if !occurs_in(").count();
    assert!(
        occurs_sites >= 4,
        "expected at least 4 occurs_in row-unification sites \
         (post-DEAD-5 dedup; pre-dedup was 5 separate sites); found {occurs_sites}"
    );
    // Each must call into the canonical helper on the failure branch.
    // Post-DEAD-5 the count is 4 row-unif sites (one helper covers
    // Open×Closed and Closed×Open) + 1 main `Var(v) ↔ t` arm = 5.
    let canonical_helper_calls = TYPECHECKER_MOD_RS
        .matches("Self::infinite_type_message(")
        .count();
    assert!(
        canonical_helper_calls >= 5,
        "expected at least 5 `Self::infinite_type_message(...)` call sites \
         (4 row-unif arms + 1 main `Var(v) ↔ t` arm; round-75 DEAD-5 \
         dedup'd the symmetric Open×Closed / Closed×Open arms into a \
         single helper); found {canonical_helper_calls}. \
         A future row-var-thru-fields feature would otherwise hit a \
         silent drop OR diverge in wording from the main arm."
    );
}

#[test]
fn fix2_canonical_wording_matches_main_unify_arm() {
    // Round 74 Fix #5: every occurs-check site (main `Var(v) ↔ t` arm
    // plus the five row-unif arms) emits the canonical
    // `"infinite type: the type variable appears inside {t}"` form
    // through the shared helper `infinite_type_message`. The lock
    // pins the suffix since it is the discriminating part — the
    // prefix `"infinite type"` was the pre-fix wording and would
    // give a false-pass if the suffix were ever dropped.
    assert!(
        TYPECHECKER_MOD_RS.contains("the type variable appears inside"),
        "canonical occurs-check wording must include the \"the type variable \
         appears inside\" suffix — Round 74 Fix #5 routes all 6 sites through \
         `infinite_type_message` so the diagnostic shape is uniform"
    );
    // Helper definition itself must exist, otherwise the call sites
    // above would not compile and we'd get a different (and louder)
    // failure mode — but the explicit assertion makes the intent clear.
    assert!(
        TYPECHECKER_MOD_RS.contains("fn infinite_type_message("),
        "Round 74 Fix #5 requires a `fn infinite_type_message(...)` helper \
         that all six occurs-check sites route through"
    );
}

// ─── Fix #1: Display ≡ message() for stdlib error enums ──────────────

#[test]
fn fix1_display_equals_message_for_stdlib_io_error_runtime() {
    // Behavioral lock: format!("{}") on a stdlib error variant must
    // produce the same text as e.message(). This is the user-facing
    // contract the doc now claims — and what the runtime now backs.
    let src = r#"
import io
fn main() {
    match io.read_file("nope_round73f_does_not_exist.txt") {
        Ok(_) -> ()
        Err(e) -> {
            let display_form = "{e}"
            let message_form = e.message()
            match display_form == message_form {
                true -> println("EQUIV")
                false -> {
                    println("DRIFT")
                    println(display_form)
                    println(message_form)
                }
            }
        }
    }
}
"#;
    let dir = std::env::temp_dir().join("silt_round73f_display_eq_message");
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
    assert!(
        stdout.lines().any(|l| l.trim() == "EQUIV"),
        "expected `\"{{e}}\" == e.message()` for stdlib error enums. Got stdout: {stdout}"
    );
}

#[test]
fn fix1_display_method_equals_message_for_stdlib_io_error_runtime() {
    // Round-74 lock-gap closure: round-73f's `Display ≡ message` fix
    // routed `format!("{e}")` through `render_stdlib_error_message` in
    // `value.rs::Display`, but the trait-method form `e.display()` was
    // serviced by the auto-derived constructor-form impl in
    // `auto_derive::synth_display_impl_for_enum` and produced the
    // constructor form (`IoNotFound(nope)`) instead of the message
    // form. This regression test exercises the trait-method dispatch
    // path specifically — the round-73f test only covered
    // `format!("{e}")`. Without the round-74 fix this test fails
    // because the synthesized `IoError.display` body emits the
    // constructor-form match arms.
    let src = r#"
import io
fn main() {
    match io.read_file("nope_round74_lock_does_not_exist.txt") {
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
    let dir = std::env::temp_dir().join("silt_round74_display_method_eq_message");
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
    assert!(
        stdout.lines().any(|l| l.trim() == "EQUIV"),
        "expected `e.display() == e.message()` for stdlib error enums. Got stdout: {stdout}"
    );
}

#[test]
fn fix1_render_stdlib_error_message_helper_exists() {
    // Structural lock: the helper that routes Display through
    // Error::message must exist. Reverting it would silently restore
    // the dual-shape divergence.
    assert!(
        VM_DISPATCH_RS.contains("pub fn render_stdlib_error_message"),
        "vm/dispatch.rs must define `pub fn render_stdlib_error_message` \
         (the Display→message() router) — fix #1 collapse"
    );
    assert!(
        MODULE_RS.contains("pub fn variant_to_error_enum"),
        "module.rs must define `pub fn variant_to_error_enum` \
         (the variant-tag → enum-name reverse lookup) — fix #1 helper"
    );
    assert!(
        VALUE_RS.contains("render_stdlib_error_message"),
        "value.rs Display impl must call render_stdlib_error_message \
         for stdlib error variants — otherwise the dual shape returns"
    );
}

#[test]
fn fix1_doc_now_asserts_display_message_equivalence() {
    // The doc previously claimed `"{e}"` ≡ `e.message()`, then was
    // rewritten to acknowledge the divergence (round 73), then the
    // runtime was fixed (round 73 follow-up) so the doc reverts to
    // claiming equivalence.
    let lower = ERROR_HANDLING_DOC.to_lowercase();
    assert!(
        lower.contains("produce the same text"),
        "error-handling.md must claim Display and message() produce the same text now \
         that the runtime fix backs the equivalence"
    );
    // The conservative caveat from the doc-only fix must be gone.
    assert!(
        !ERROR_HANDLING_DOC.contains("variant constructor form"),
        "error-handling.md should no longer warn about the \"variant constructor form\" \
         divergence — the runtime now collapses the dual shape"
    );
}

#[test]
fn fix1_user_enum_display_unchanged_constructor_form() {
    // The fix only routes STDLIB error variants through Error::message.
    // User-defined enums must still render in constructor form so the
    // change is scoped — no surprises for user code.
    let src = r#"
type Color { Red, Green(Int), Blue }
fn main() {
    let c = Green(42)
    println("{c}")
}
"#;
    let dir = std::env::temp_dir().join("silt_round73f_user_enum_display");
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
        "user-defined enum Display must keep constructor form. Got: {stdout}"
    );
}

// ─── Fix #3: builtin-arity diagnostic dedup ──────────────────────────

#[test]
fn fix3_numeric_rs_drops_terse_requires_form() {
    // Pre-fix wording was `"int.abs requires an int"` (no offending kind
    // named). Post-fix uses common::require_int which emits
    // `"int.abs requires Int, got <kind>"`. Reverting drops the kind
    // name — a silent diagnostic regression.
    let banned = [
        "int.abs requires an int",
        "int.parse requires a string",
        "int.min requires ints",
        "int.max requires ints",
        "int.clamp requires ints",
        "int.to_float requires an int",
        "int.to_string requires an int",
        "float.parse requires a string",
        "float.round requires a float",
        "float.ceil requires a float",
        "float.floor requires a float",
        "float.abs requires a float",
        "float.to_string requires a float",
        "float.to_string requires an int for decimals",
        "float.to_int requires a float",
    ];
    for needle in banned {
        assert!(
            !NUMERIC_RS.contains(needle),
            "numeric.rs still contains terse pre-fix diagnostic {needle:?} — should use \
             require_int / require_string / canonical \"requires <Kind>, got <kind>\" form"
        );
    }
}

#[test]
fn fix3_string_rs_drops_terse_requires_form() {
    let banned = [
        "string.split requires strings",
        "string.trim requires a string",
        "string.contains requires strings",
        "string.replace requires strings",
        "string.length requires a string",
        "string.to_upper requires a string",
        "string.starts_with requires strings",
        "string.ends_with requires strings",
        "string.repeat requires a string",
        "string.repeat requires an int",
        "string.split_at requires a string",
        "string.split_at requires an int",
        "string.lines requires a string",
        "string.starts_with_at requires a string",
        "first arg must be string",
        "second arg must be int",
        "third arg must be string",
    ];
    for needle in banned {
        assert!(
            !STRING_RS.contains(needle),
            "string.rs still contains terse pre-fix diagnostic {needle:?} — should use \
             require_string / require_int helpers"
        );
    }
}

#[test]
fn fix3_runtime_emits_canonical_kind_named_form() {
    // Behavioral lock: passing the wrong kind to a converted builtin
    // produces the canonical "requires <Kind>, got <kind>" form.
    //
    // Round 75 lock tightening: the previous shape used the `silt run`
    // binary, which runs the full pipeline (typechecker → compiler →
    // VM). The typechecker rejects `int.abs("not-an-int")` with
    // "type mismatch: expected Int, got String" before the runtime
    // ever fires, so the original assertion (absence of pre-fix
    // wording) passed vacuously even if the runtime had silently
    // regressed. We restructure to drive parser → compiler → VM
    // directly while ignoring typechecker diagnostics — exactly the
    // pattern round 74's `collections_canonical_kind_wording_tests.rs`
    // uses to genuinely exercise the runtime guard. A positive
    // assertion on `int.abs requires Int, got` then locks the
    // canonical shape (and its presence catches a regression).
    let src = r#"
import int
fn main() { int.abs("not-an-int") }
"#;
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    // Discard typechecker diagnostics; the runtime is what we want to
    // exercise. The compiler still consumes the AST and produces
    // bytecode that the VM runs.
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler
        .compile_program(&program)
        .expect("expected the compiler to accept the program when typechecker errors are dropped");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm
        .run(script)
        .expect_err("expected a runtime error from int.abs(\"not-an-int\")");
    let err_str = format!("{err}");
    // Positive lock: canonical `<fn> requires <Kind>, got <kind>` form.
    assert!(
        err_str.contains("int.abs requires Int, got"),
        "expected int.abs runtime to emit the canonical \
         `int.abs requires Int, got <kind>` wording; got: {err_str}"
    );
    assert!(
        err_str.contains("String"),
        "expected the offending kind `String` in the canonical wording; got: {err_str}"
    );
    // Negative locks: pre-fix terse wording must not return.
    assert!(
        !err_str.contains("int.abs requires an int")
            && !err_str.contains("int.abs requires a int"),
        "terse pre-fix wording must not surface anywhere. Got: {err_str}"
    );
}

#[test]
fn fix3_canonical_helpers_in_common_rs_carry_kind() {
    // The shared helpers in src/builtins/common.rs are the canonical
    // wording source. They must keep the ", got <kind>" suffix —
    // dropping it would silently revert every site that calls them.
    let src = include_str!("../src/builtins/common.rs");
    assert!(
        src.contains("requires String, got"),
        "common::require_string must emit canonical kind-named diagnostic"
    );
    assert!(
        src.contains("requires Int, got"),
        "common::require_int must emit canonical kind-named diagnostic"
    );
}
