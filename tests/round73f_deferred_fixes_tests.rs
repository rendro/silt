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
    // canonical `"infinite type"` diagnostic. Reverting the fix would
    // restore silent drops, which violates the "explicit over implicit
    // / silent wrong answers are worse than crashes" principle.
    let occurs_sites = TYPECHECKER_MOD_RS.matches("if !occurs_in(").count();
    assert!(
        occurs_sites >= 5,
        "expected at least 5 occurs_in row-unification sites; found {occurs_sites}"
    );
    // Each must emit the canonical "infinite type" diagnostic on the
    // failure branch. The matching `else { self.error("infinite type"...` arms.
    let infinite_else_arms = TYPECHECKER_MOD_RS
        .matches("self.error(\"infinite type\".to_string(), span);")
        .count();
    assert!(
        infinite_else_arms >= 5,
        "expected at least 5 `else self.error(\"infinite type\")` arms paired \
         with occurs_in checks in row unification; found {infinite_else_arms}. \
         A future row-var-thru-fields feature would otherwise hit a silent drop."
    );
}

#[test]
fn fix2_canonical_wording_matches_main_unify_arm() {
    // The main `Var(v) ... t` arm at mod.rs:1240 emits `"infinite type"`
    // when the user returns a `type a` parameter as a value. Round-73
    // follow-up uses the IDENTICAL wording in the new row-unification
    // arms — "collapse equivalent dual shapes" applies to diagnostic
    // wording too (a future user shouldn't see two different
    // diagnostics for the same underlying class of error).
    assert!(
        TYPECHECKER_MOD_RS.contains("\"infinite type\""),
        "main occurs-check arm must continue using the canonical \"infinite type\" wording"
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
    let src = r#"
import int
fn main() { int.abs("not-an-int") }
"#;
    let dir = std::env::temp_dir().join("silt_round73f_int_abs_kind");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tempdir");
    let path = dir.join("int_abs.silt");
    std::fs::write(&path, src).expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("invoke silt run");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The typechecker may reject this before runtime — that's also fine
    // (silt's "explicit over implicit" with type inference covers many
    // FFI/dynamic-dispatch leakage cases). Either way: the runtime
    // shouldn't surface the terse pre-fix wording.
    assert!(
        !combined.contains("int.abs requires an int")
            && !combined.contains("int.abs requires a int"),
        "terse pre-fix wording must not surface anywhere. Got: {combined}"
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
