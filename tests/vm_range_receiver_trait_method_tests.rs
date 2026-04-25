//! Lock tests for REGRESSION introduced in round 55 by the
//! `Type::Range` zero-cost alias. The typechecker routes all Range
//! dispatch through List (see `trait_head_of` at
//! `src/typechecker/mod.rs:988`), and the compiler registers
//! `trait X for List(a) { ... }` methods under a global keyed
//! `"List.<method>"` (see `src/compiler/mod.rs:887`). But `Value::Range`
//! is a distinct VM value, and `value_type_name_for_dispatch`
//! (`src/vm/mod.rs`) used to return `"Range"` — so `Op::CallMethod`
//! looked up `"Range.<method>"` at runtime, missed, and surfaced
//! `error[runtime]: no method '<m>' for type 'Range'` to the user.
//!
//! The fix unifies Range dispatch with List dispatch at runtime by
//! returning `"List"` from `value_type_name_for_dispatch` for
//! `Value::Range(..)`. These tests lock that behaviour end-to-end
//! via the `silt` CLI so future refactors cannot silently re-break it.
//!
//! See also: tests/range_type_tests.rs (typechecker-level lock for
//! `Range(T)` as a nominal alias of `List(T)`).

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_range_receiver_trait_{label}.silt"));
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

/// Run and assert success; return stdout.
fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout:?}, stderr={stderr:?}"
    );
    stdout
}

// ── 1. Canonical REGRESSION repro ───────────────────────────────────

/// A user-defined trait method implemented for `List(a)`, invoked on a
/// Range receiver. Pre-fix this produced
/// `error[runtime]: no method 'bar' for type 'Range'`.
#[test]
fn user_trait_method_on_list_dispatches_for_range_receiver() {
    let out = run_silt_ok(
        "bar_const",
        r#"
trait Foo { fn bar(self) -> Int }
trait Foo for List(a) { fn bar(self) -> Int = 42 }
fn main() {
    let r = 1..5
    println(r.bar())
}
"#,
    );
    assert_eq!(out.trim(), "42");
}

// ── 2. Range receiver with a body that uses `self` ──────────────────

/// A trait method that actually consumes `self` (via the builtin
/// `list.sum`) to ensure Range→List dispatch is not just a stub path:
/// the receiver must flow through the method body and yield the
/// expected numeric result. Silt ranges are inclusive on both ends
/// (see `Value::Range` at src/value.rs:131), so `1..5` sums to
/// 1+2+3+4+5 = 15.
#[test]
fn user_trait_method_using_self_on_range_receiver() {
    let out = run_silt_ok(
        "sum_range",
        r#"
import list
trait Sum { fn sum_of(self) -> Int }
trait Sum for List(a) { fn sum_of(self) -> Int = list.sum(self) }
fn main() {
    let r = 1..5
    println(r.sum_of())
}
"#,
    );
    assert_eq!(out.trim(), "15");
}

// ── 3. Range receiver used inline (no intermediate let binding) ────

/// Dispatch the trait method on a range literal directly (no
/// intermediate `let`). This pins down that the dispatch path works
/// even when the receiver is the expression result of a range
/// operator, not just a variable typed as Range.
#[test]
fn user_trait_method_on_range_literal_receiver() {
    let out = run_silt_ok(
        "sum_literal",
        r#"
import list
trait Sum { fn sum_of(self) -> Int }
trait Sum for List(a) { fn sum_of(self) -> Int = list.sum(self) }
fn main() {
    println((2..4).sum_of())
}
"#,
    );
    // Inclusive: 2 + 3 + 4 = 9
    assert_eq!(out.trim(), "9");
}

// ── 4. Phase C runtime companion: `trait X for Range(a)` reachable ─

/// Phase B added the typechecker-side `trait_impl_for_range_dispatches_on_list_receiver`
/// test, which locked the typechecker's symmetric canonicalisation
/// (registering a `for Range(a)` impl under the `"List"` key so a
/// `List` receiver finds it). Phase C completes the symmetry by
/// canonicalising the compiler's trait-impl global emission too:
/// `trait Foo for Range(a) { fn bar(self) = ... }` now emits the
/// global under `"List.bar"`, matching what the typechecker registered
/// and what the VM (via `dispatch_name_for_value`) looks up at runtime
/// for both `Value::List` and `Value::Range` receivers.
///
/// This is the runtime end-to-end: a `for Range(a)` impl invoked on a
/// `Range` receiver. Pre-phase-C the typechecker accepted the call
/// but the compiler had emitted `"Range.bar"` while runtime dispatch
/// looked up `"List.bar"`, producing a runtime miss. With phase C
/// every layer agrees on `"List"` as the canonical key.
#[test]
fn trait_impl_for_range_dispatches_on_range_receiver_runtime() {
    let out = run_silt_ok(
        "for_range_on_range",
        r#"
trait Foo { fn bar(self) -> Int }
trait Foo for Range(a) { fn bar(self) -> Int = 99 }
fn main() {
    let r = 1..5
    println(r.bar())
}
"#,
    );
    assert_eq!(out.trim(), "99");
}

/// Cross-symmetry: a `for Range(a)` impl invoked on a `List`
/// receiver. Phase B's typechecker test already covered this at the
/// type level; here we verify it runs to completion.
#[test]
fn trait_impl_for_range_dispatches_on_list_receiver_runtime() {
    let out = run_silt_ok(
        "for_range_on_list",
        r#"
trait Foo { fn bar(self) -> Int }
trait Foo for Range(a) { fn bar(self) -> Int = 7 }
fn main() {
    let xs = [1, 2, 3]
    println(xs.bar())
}
"#,
    );
    assert_eq!(out.trim(), "7");
}
