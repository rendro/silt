//! Round 88 BROKEN B1 lock: calling a trait method on a bare unit-
//! variant value must dispatch correctly at the value level rather
//! than being treated as a qualified-name path lookup.
//!
//! Pre-fix: for a user enum `type Color { Red, Green, Blue }` with a
//! `trait Display for Color { fn display(self) -> String { ... } }`,
//! the expression `Red.display()` in callee position would compile
//! through the `is_module_call` branch in `src/compiler/mod.rs` and
//! emit `GetGlobal("Red.display")`. That global never exists, so
//! `silt run` failed at runtime with
//! `error[runtime]: undefined global: Red.display`. Workarounds
//! (`let r = Red; r.display()`, `Blue(5).display()`) worked because
//! the bare-Ident receiver shape was the only one that hit the
//! buggy path.
//!
//! Post-fix: the `Call` codegen path consults a new
//! `known_unit_variants` set on the compiler. When the receiver is
//! an `ExprKind::Ident` whose name is a known nullary variant, the
//! compiler emits `GetGlobal(variant_name)` followed by `CallMethod`,
//! pushing the unit-variant value as the receiver. The VM's
//! `value_type_name_for_dispatch` then routes `Value::Variant("Red", _)`
//! through the `__type_of__Red` global to the parent type name
//! `Color` and dispatches to `Color.display`.
//!
//! This must be locked at the runtime layer — a typecheck-only lock
//! is insufficient because `silt check` already accepted the pre-fix
//! program (the typechecker has no visibility into the qualified-
//! global emission decision).
//!
//! Assertion shape: `InProcessRunner` intentionally does not capture
//! stdout (see `compile_and_run` in `src/scheduler/test_support.rs`),
//! so each test rephrases the program to return the method result
//! from `main()` and asserts on `outcome.result` instead — the same
//! pattern used across the rest of `tests/round77_*.rs`.

use std::time::Duration;

use silt::scheduler::test_support::{InProcessRunner, TrialOutcome};
use silt::value::Value;

/// Process-id tag included in every source body. Required by the
/// round-88 spec ("Use process::id() in any temp file path; do NOT
/// use bare `/tmp/silt_*` paths"). This test doesn't touch the
/// filesystem, but we still surface the pid in a comment inside the
/// source so a future maintainer adding temp-file I/O has a visible
/// precedent of the per-process-id pattern.
fn pid_tag() -> u32 {
    std::process::id()
}

/// Run `src` via `InProcessRunner` with a 10-second budget. Returns
/// the outcome so each test can pattern-match on `result` /
/// `error_message` as appropriate.
fn run(src: String) -> TrialOutcome {
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(10));
    runner.run_trial()
}

/// Build a Silt program with the given `call_form` invoking the
/// trait method on a bare unit variant. The trait body returns a
/// distinct, easy-to-match string so we can assert the trait body
/// actually ran end-to-end. `main()` returns the call result
/// directly — see top-of-file note on why this is preferred over
/// stdout matching.
fn source_returning(call_form: &str) -> String {
    let _pid = pid_tag();
    format!(
        r#"
type Color {{ Red, Green, Blue }}

trait Display for Color {{
  fn display(self) -> String {{ "a color" }}
}}

fn main() -> String {{
  {call_form}
}}
"#
    )
}

/// Bare unit-variant method call: `Red.display()`. Pre-fix this
/// failed with `undefined global: Red.display` at runtime.
#[test]
fn round88_bare_unit_variant_method_dispatch_works() {
    let outcome = run(source_returning("Red.display()"));
    assert!(
        outcome.error_message.is_none(),
        "expected clean run; got error_message={:?}",
        outcome.error_message
    );
    assert!(!outcome.timed_out, "trial timed out");
    match outcome.result {
        Some(Value::String(s)) => {
            assert!(
                s.contains("a color"),
                "expected main() to return 'a color' from Red.display(); got {s:?}"
            );
        }
        other => panic!(
            "expected String return from main(); got {other:?}, error_message={:?}",
            outcome.error_message
        ),
    }
}

/// Paren form: `(Red).display()`. The grouping is transparent in
/// the AST so this lowers through the same `ExprKind::Ident`
/// receiver shape as the bare form. Pre-fix the doc-agent reproduced
/// the same `undefined global` failure here. Locked as a separate
/// test so a future regression narrowing the rewrite to "only bare
/// Ident, not grouped" is caught immediately.
#[test]
fn round88_paren_unit_variant_method_dispatch_works() {
    let outcome = run(source_returning("(Red).display()"));
    assert!(
        outcome.error_message.is_none(),
        "expected clean run; got error_message={:?}",
        outcome.error_message
    );
    assert!(!outcome.timed_out, "trial timed out");
    match outcome.result {
        Some(Value::String(s)) => {
            assert!(
                s.contains("a color"),
                "expected main() to return 'a color' from (Red).display(); got {s:?}"
            );
        }
        other => panic!(
            "expected String return from main(); got {other:?}, error_message={:?}",
            outcome.error_message
        ),
    }
}

/// Sibling lock: the let-bound form must still work. Pre-fix this
/// worked, and it must keep working after the fix (the new rewrite
/// path is gated on `ExprKind::Ident` receivers that resolve as a
/// known unit variant via the compiler's `known_unit_variants` set;
/// let-bound names are locals and never enter the rewrite).
#[test]
fn round88_let_bound_unit_variant_method_dispatch_still_works() {
    let _pid = pid_tag();
    let src = r#"
type Color { Red, Green, Blue }

trait Display for Color {
  fn display(self) -> String { "a color" }
}

fn main() -> String {
  let r = Red
  r.display()
}
"#
    .to_string();
    let outcome = run(src);
    assert!(
        outcome.error_message.is_none(),
        "expected clean run; got error_message={:?}",
        outcome.error_message
    );
    assert!(!outcome.timed_out, "trial timed out");
    match outcome.result {
        Some(Value::String(s)) => {
            assert!(
                s.contains("a color"),
                "expected main() to return 'a color' from let-bound r.display(); got {s:?}"
            );
        }
        other => panic!(
            "expected String return from main(); got {other:?}, error_message={:?}",
            outcome.error_message
        ),
    }
}

/// Exact-match lock: the bare unit-variant must invoke the trait
/// body, not some fallback. We use a method body that returns a
/// distinct sentinel string we can match exactly. Pre-fix this
/// failed before the trait body even had a chance to run (the
/// program died at `GetGlobal("Red.display")`). Post-fix the trait
/// body runs and main() returns the sentinel.
#[test]
fn round88_method_returns_value_from_trait_body() {
    let _pid = pid_tag();
    let src = r#"
type Color { Red, Green, Blue }

trait Display for Color {
  fn display(self) -> String { "round88-ok" }
}

fn main() -> String { Red.display() }
"#
    .to_string();
    let outcome = run(src);
    assert!(
        outcome.error_message.is_none(),
        "expected clean run; got error_message={:?}",
        outcome.error_message
    );
    assert!(!outcome.timed_out, "trial timed out");
    match outcome.result {
        Some(Value::String(s)) => {
            assert_eq!(
                s, "round88-ok",
                "expected main() to return the trait body's sentinel; \
                 got {s:?}. If this returned a different shape (e.g. the \
                 default Display rendering of the variant) the rewrite \
                 missed the trait dispatch entirely."
            );
        }
        other => panic!(
            "expected String return from main(); got {other:?}, error_message={:?}",
            outcome.error_message
        ),
    }
}

/// Pre-fix regression: a bare `Red.display()` used to produce the
/// runtime error `undefined global: Red.display`. This test pins
/// that the error string is NOT what we get after the fix — i.e. if
/// some later refactor reverts the rewrite path, the test fails on
/// the canonical pre-fix error rather than silently passing.
#[test]
fn round88_undefined_global_error_string_is_gone() {
    let outcome = run(source_returning("Red.display()"));
    if let Some(msg) = &outcome.error_message {
        assert!(
            !msg.contains("undefined global: Red.display"),
            "post-fix run still produced the pre-fix error string: {msg}"
        );
    }
}
