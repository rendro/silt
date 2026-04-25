//! Declaration-order comparison tests for user-defined enum variants.
//!
//! Round-61 introduced declaration-order comparison: every enum decl
//! registers its variants with a global ordinal registry keyed by tag
//! name. `Value::cmp` (and via it the trait-bound `compare()` method
//! plus the direct `<` / `>` operators on Variants) consults the
//! registry first, ordering by declaration index regardless of
//! alphabetical order.
//!
//! The previous round-60 baseline used alphabetical comparison for all
//! variants except a hand-rolled `Weekday` ordinal table. That table
//! has been deleted; Weekday is now just another enum that happens to
//! be declared in chronological order.
//!
//! These tests exercise the runtime end-to-end via the `silt` CLI
//! (mirroring `tests/vm_dispatch_variant_record_runtime_tests.rs`) so
//! they observe the full typechecker → compiler → VM pipeline.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_variant_decl_order_{label}.silt"));
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
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

// ── Test 1: declaration-order ordering basic ─────────────────────────

/// `type Color { Red, Green, Blue }` — declaration order is Red < Green
/// < Blue regardless of alphabetical order. Three pairs locked.
#[test]
fn decl_order_color_basic() {
    let out = run_silt_ok(
        "color_basic",
        r#"
type Color { Red, Green, Blue, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Red, Green))
    println(cmp_gen(Green, Red))
    println(cmp_gen(Red, Red))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1", "0"]);
}

// ── Test 2: reverse alphabetical declaration ─────────────────────────

/// `type Backwards { Z, Y, A }` — alphabetical order would put A first,
/// but declaration order says Z=0, Y=1, A=2 so `Z < A`.
#[test]
fn decl_order_reverse_alphabetical() {
    let out = run_silt_ok(
        "reverse_alpha",
        r#"
type Backwards { Z, Y, A, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Z, A))
}
"#,
    );
    assert_eq!(out.trim(), "-1");
}

// ── Test 3: three variants, full ordering ────────────────────────────

/// First < Second < Third via three pairwise compares.
#[test]
fn decl_order_three_variants_full_ordering() {
    let out = run_silt_ok(
        "three_variants",
        r#"
type Three { First, Second, Third, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(First, Second))
    println(cmp_gen(Second, Third))
    println(cmp_gen(First, Third))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "-1", "-1"]);
}

// ── Test 4: constructor variants with args ───────────────────────────

/// `type Tagged { Foo(Int), Bar(Int) }` — `Foo(1) < Bar(1)` by ordinal
/// (Foo declared first), and `Foo(1) < Foo(2)` by lexicographic
/// comparison of fields when ordinals tie.
#[test]
fn decl_order_with_constructor_args() {
    let out = run_silt_ok(
        "constructor_args",
        r#"
type Tagged { Foo(Int), Bar(Int), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Foo(1), Bar(1)))
    println(cmp_gen(Foo(1), Foo(2)))
    println(cmp_gen(Foo(2), Foo(1)))
    println(cmp_gen(Foo(7), Foo(7)))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "-1", "1", "0"]);
}

// ── Test 5: built-in Weekday still works ─────────────────────────────

/// `cmp_gen(Monday, Friday)` returns -1 (Monday declared first). Locks
/// that the Weekday behaviour is unchanged after deleting the
/// hand-rolled `weekday_ordinal` function — it is now seeded into the
/// generic ordinal registry and goes through the same code path as
/// every other enum.
#[test]
fn builtin_weekday_decl_order_preserved() {
    let out = run_silt_ok(
        "weekday",
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Monday, Friday))
    println(cmp_gen(Sunday, Monday))
    println(cmp_gen(Wednesday, Wednesday))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1", "0"]);
}

// ── Test 6: another built-in enum (extra Weekday coverage) ───────────

/// Extra coverage on the built-in Weekday: locks every transition pair
/// (Mon→Tue, Tue→Wed, ...) plus boundary Sun and a non-adjacent pair
/// (Mon vs Sat). Demonstrates that the registry seeded for built-ins
/// matches their `module::builtin_enum_variants` declaration order.
#[test]
fn builtin_weekday_full_decl_order_chain() {
    let out = run_silt_ok(
        "weekday_chain",
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Monday, Tuesday))
    println(cmp_gen(Tuesday, Wednesday))
    println(cmp_gen(Wednesday, Thursday))
    println(cmp_gen(Thursday, Friday))
    println(cmp_gen(Friday, Saturday))
    println(cmp_gen(Saturday, Sunday))
    println(cmp_gen(Monday, Saturday))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "-1", "-1", "-1", "-1", "-1", "-1"]);
}

// ── Test 7: manual trait Compare opt-out ─────────────────────────────

/// User-defined `trait Compare for Color { ... }` overrides the
/// auto-derive: `cmp_gen` dispatches through the user's body, which
/// returns a sentinel value (-42), not the registry-derived ordinal
/// answer. Locks that the auto-derive does not stomp the user impl.
#[test]
fn manual_compare_impl_overrides_auto_derive() {
    let out = run_silt_ok(
        "manual_compare",
        r#"
type Color { Red, Green, Blue, }
trait Compare for Color {
    fn compare(self, other: Color) -> Int = 0 - 42
}
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen(Red, Green)) }
"#,
    );
    assert_eq!(
        out.trim(),
        "-42",
        "user-defined trait Compare for Color must win over auto-derive"
    );
}

// ── Test 8: direct `<` operator uses ordinal ────────────────────────

/// `Red < Green` should return `true` via the direct comparison
/// operator (not just trait dispatch). `vm/arithmetic.rs` defers to
/// `Value::cmp` for Variant pairs, which now uses the ordinal registry,
/// so direct operators and trait dispatch share the same semantics.
/// (Silt has no `if` keyword — branching is via `match cond { true ->
/// ..., false -> ... }`.)
#[test]
fn direct_lt_operator_uses_ordinal() {
    let out = run_silt_ok(
        "direct_lt",
        r#"
type Color { Red, Green, Blue, }
fn main() {
    let lt = Red < Green
    let gt = Blue > Red
    let lt_label = match lt {
        true -> "red_lt_green"
        false -> "green_lte_red"
    }
    let gt_label = match gt {
        true -> "blue_gt_red"
        false -> "red_gte_blue"
    }
    println(lt_label)
    println(gt_label)
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["red_lt_green", "blue_gt_red"]);
}

// ── Test 9: list.sort honours declaration order ──────────────────────

/// `list.sort([Green, Red, Blue])` returns `[Red, Green, Blue]` —
/// declaration order regardless of alphabetical name order. This locks
/// that sort, which uses the same ordering as `<`, sees the new
/// ordinal-based comparison.
#[test]
fn list_sort_uses_decl_order() {
    let out = run_silt_ok(
        "list_sort",
        r#"
import list
type Color { Red, Green, Blue, }
fn main() {
    let xs = list.sort([Green, Red, Blue])
    println(xs)
}
"#,
    );
    assert_eq!(
        out.trim(),
        "[Red, Green, Blue]",
        "list.sort must order by declaration order, not alphabetically"
    );
}
