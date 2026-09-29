//! Runtime-dispatch tests for built-in trait methods on primitives.
//!
//! Two BROKEN findings (pre-fix):
//!
//! 1. `.hash()` fails at runtime on every primitive.
//!    The typechecker auto-derives `Hash` for `Int` / `Float` /
//!    `ExtFloat` / `Bool` / `String` / `List` (see
//!    `src/typechecker/mod.rs:3214-3224`), so `x.hash()` typechecks.
//!    But at runtime, `Op::CallMethod` first tries the qualified
//!    global `"<TypeName>.hash"` (misses — only user-defined impls
//!    register there), then falls through to
//!    `Vm::dispatch_trait_method`, which only had arms for
//!    `"display" | "equal" | "compare"`. Every primitive `.hash()`
//!    call therefore produced:
//!      `error[runtime]: no method 'hash' for type 'Int'` (etc.)
//!    Fix: add a `"hash"` arm in `src/vm/dispatch.rs` that reuses
//!    the existing `Hash for Value` impl (`src/value.rs:1759`) via
//!    `DefaultHasher`, bit-casting the `u64` result to `i64` for the
//!    trait-declared return type `Int`.
//!
//! 2. `ExtFloat.compare()` via a trait bound fails at runtime.
//!    `value_type_name_for_dispatch` (`src/vm/mod.rs`) returned
//!    `"Unknown"` for `ExtFloat`, so the qualified-global lookup
//!    `"Unknown.compare"` missed, and the fallback `"compare"` arm
//!    in `dispatch_trait_method` had no `(ExtFloat, ExtFloat)` case
//!    — producing
//!      `error[runtime]: compare() not supported between ExtFloat and ExtFloat`.
//!    Fix: (a) `value_type_name_for_dispatch` now returns the
//!    canonical `"ExtFloat"` (matching `type_name` / the typechecker
//!    registration) for every `Value` variant, not just a subset;
//!    (b) the `"compare"` arm now handles `(ExtFloat, ExtFloat)` and
//!    the mixed `(Float, ExtFloat)` / `(ExtFloat, Float)` pairs,
//!    mirroring the ordering-comparison logic in
//!    `src/vm/arithmetic.rs:113`.
//!
//! The `.hash()` / List / Unit / user-impl cases live as golden cases
//! under `tests/golden/typecheck/traits/vm_trait_dispatch_runtime__*`.
//! The ExtFloat cases stay here until stage 4 removes ExtFloat; they
//! exercise the runtime path end-to-end via the `silt` CLI.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_trait_dispatch_rt_{label}.silt"));
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

// ── ExtFloat runtime dispatch via trait bounds (stage 4 removes ExtFloat) ─

/// The canonical Finding-2 repro: `Float / Float` widens to
/// `ExtFloat`; comparing the result to itself via a `Compare` bound
/// must print `0`.
#[test]
fn compare_runs_on_extfloat_via_bound() {
    let out = run_silt_ok(
        "cmp_extfloat_self",
        r#"
fn cmp(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
  let x: Float = 3.0
  let y: Float = 2.0
  let r = x / y
  println(cmp(r, r))
}
"#,
    );
    assert_eq!(out.trim(), "0", "self-compare should be 0; got {out:?}");
}

/// Ordering check on two ExtFloat values where the first is strictly
/// less than the second — must print `-1`.
#[test]
fn compare_runs_on_extfloat_ordering() {
    let out = run_silt_ok(
        "cmp_extfloat_lt",
        r#"
fn cmp(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
  let x: Float = 3.0
  let y: Float = 2.0
  let r1 = x / y        -- 1.5 as ExtFloat
  let r2 = (x + x) / y  -- 3.0 as ExtFloat
  println(cmp(r1, r2))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "-1",
        "r1 < r2 should compare to -1; got {out:?}"
    );
}

/// LATENT-1: `.equal()` on ExtFloat via an Equal trait bound must
/// run and produce `true` for two bit-equal ExtFloat values.
#[test]
fn equal_runs_on_extfloat_via_bound() {
    let out = run_silt_ok(
        "eq_extfloat_self",
        r#"
fn eq(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn main() {
  let x: Float = 3.0
  let y: Float = 2.0
  let r1 = x / y
  let r2 = x / y
  println(eq(r1, r2))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "true",
        "two ExtFloats produced by the same Float division should be \
         Equal-equal; got {out:?}"
    );
}

/// LATENT-1: `.display()` on ExtFloat via a Display trait bound
/// must run and emit a non-empty string. We don't pin the exact
/// textual form (`1.5` vs `1.5e0` etc.) — only that Display routes
/// to a successful runtime arm and the output is non-empty.
#[test]
fn display_runs_on_extfloat_via_bound() {
    let out = run_silt_ok(
        "display_extfloat",
        r#"
fn show(a: a) -> String where a: Display { a.display() }
fn main() {
  let x: Float = 3.0
  let y: Float = 2.0
  let r = x / y
  println(show(r))
}
"#,
    );
    let trimmed = out.trim();
    assert!(
        !trimmed.is_empty(),
        "Display on ExtFloat should produce a non-empty string; got {out:?}"
    );
    // Sanity: the rendered form should at least mention a digit. We
    // don't pin the exact format because Float-vs-ExtFloat printing
    // can differ across platforms (e.g. `1.5` vs `1.5e0`).
    assert!(
        trimmed.chars().any(|c| c.is_ascii_digit()),
        "Display output for ExtFloat should contain at least one digit; got {out:?}"
    );
}

/// LATENT-1: `.hash()` on ExtFloat via a Hash trait bound must run
/// and emit a deterministic Int. Bit-equal ExtFloats must hash the
/// same (mirrors `hash_is_deterministic_for_same_value` for Int).
#[test]
fn hash_runs_on_extfloat_via_bound() {
    let out = run_silt_ok(
        "hash_extfloat",
        r#"
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
  let x: Float = 3.0
  let y: Float = 2.0
  let r1 = x / y
  let r2 = x / y
  println(h(r1))
  println(h(r2))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 lines of output, got: {out:?}");
    for (i, line) in lines.iter().enumerate() {
        line.trim()
            .parse::<i64>()
            .unwrap_or_else(|e| panic!("line {i} {line:?} is not a parseable Int hash: {e}"));
    }
    assert_eq!(
        lines[0].trim(),
        lines[1].trim(),
        "hash of two bit-equal ExtFloats should be deterministic; got {out:?}"
    );
}
