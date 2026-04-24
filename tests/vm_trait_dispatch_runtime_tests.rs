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
//! These tests exercise the runtime path end-to-end via the `silt`
//! CLI so they fail before the dispatch.rs / mod.rs fix and pass
//! after.

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

// ── Finding 1: `.hash()` on primitives ──────────────────────────────

/// The canonical Finding-1 repro: `.hash()` on Int via a Hash trait
/// bound must run and produce a non-panicking `Int`.
#[test]
fn hash_runs_on_int() {
    let out = run_silt_ok(
        "hash_int",
        r#"
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() { println(h(42)) }
"#,
    );
    let line = out.lines().next().unwrap_or("");
    // The hash is a deterministic i64 — just assert it parses.
    line.trim()
        .parse::<i64>()
        .unwrap_or_else(|e| panic!("expected Int hash on stdout, got {line:?}: {e}"));
}

/// `.hash()` works on every other auto-derived primitive the
/// typechecker accepts at this bound: Float, String, Bool, List.
/// Each hash must be a parseable `Int`.
#[test]
fn hash_runs_on_float_string_bool_list() {
    let out = run_silt_ok(
        "hash_multi",
        r#"
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
  println(h(3.14))
  println(h("abc"))
  println(h(true))
  println(h([1, 2, 3]))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "expected 4 lines of output, got: {out:?}"
    );
    for (i, line) in lines.iter().enumerate() {
        line.trim().parse::<i64>().unwrap_or_else(|e| {
            panic!("line {i} {line:?} is not a parseable Int hash: {e}")
        });
    }
}

/// Determinism: hashing the same Int value twice returns the same
/// `Int`. Guards against accidental non-determinism if someone swaps
/// in a randomized hasher.
#[test]
fn hash_is_deterministic_for_same_value() {
    let out = run_silt_ok(
        "hash_det",
        r#"
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
  println(h(42))
  println(h(42))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 lines of output, got: {out:?}");
    assert_eq!(
        lines[0].trim(),
        lines[1].trim(),
        "hash of same value should be deterministic across two calls"
    );
}

// ── Finding 2: `ExtFloat.compare()` via trait bound ─────────────────

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
    assert_eq!(out.trim(), "-1", "r1 < r2 should compare to -1; got {out:?}");
}

// ── Regression guard: user-defined Hash still routes correctly ──────

// ── Round 60 B5: List.compare via trait bound ───────────────────────
//
// `src/typechecker/mod.rs:3386` auto-registers Compare for List, but
// pre-fix the `"compare"` arm in `src/vm/dispatch.rs` lacked a
// `(Value::List, Value::List)` case, so a `List(Int)` flowing through
// a `Compare` bound errored at runtime with
//   `compare() not supported between List and List`.
// Fix: defer to `Value::cmp`, which already orders Lists element-wise
// (mirrors `<`/`>` operator path at `src/vm/arithmetic.rs:138`).
#[test]
fn compare_runs_on_list() {
    let out = run_silt_ok(
        "cmp_list",
        r#"
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    let x: List(Int) = [1, 2, 3]
    let y: List(Int) = [1, 2, 4]
    println(cmp_gen(x, y))
    println(cmp_gen(y, x))
    println(cmp_gen(x, x))
}
"#,
    );
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    assert_eq!(lines.len(), 3, "expected 3 lines, got {out:?}");
    assert_eq!(lines[0], "-1", "x < y should compare to -1; got {out:?}");
    assert_eq!(lines[1], "1", "y > x should compare to 1; got {out:?}");
    assert_eq!(lines[2], "0", "x == x should compare to 0; got {out:?}");
}

// ── Round 60 B6: ().compare via trait bound ─────────────────────────
//
// `src/typechecker/mod.rs:3383` auto-registers Compare for `()`, but
// pre-fix the `"compare"` arm lacked a `(Value::Unit, Value::Unit)`
// case, so a Unit flowing through a `Compare` bound errored at
// runtime with
//   `compare() not supported between Unit and Unit`.
// Fix: all units are equal — return `Ordering::Equal`.
#[test]
fn compare_runs_on_unit() {
    let out = run_silt_ok(
        "cmp_unit",
        r#"
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen((), ())) }
"#,
    );
    assert_eq!(out.trim(), "0", "unit-vs-unit must compare to 0; got {out:?}");
}

// ── Round 60 B7: Variant.hash via trait bound ───────────────────────
//
// `src/typechecker/mod.rs:3391` auto-registers Hash for Option and
// Result (both `Value::Variant`), but pre-fix the `"hash"` arm in
// `src/vm/dispatch.rs` had an explicit allowlist that omitted
// `Value::Variant(..)`, so `h(Some(1))` errored at runtime with
//   `no method 'hash' for type 'Option(Int)'`.
// Fix: extend the allowlist to `Value::Variant(..)`. The existing
// `impl Hash for Value` (src/value.rs:1821) already hashes Variant by
// name + payload.
#[test]
fn hash_runs_on_some_int() {
    let out = run_silt_ok(
        "hash_some_int",
        r#"
fn h(v: a) -> Int where a: Hash { v.hash() }
fn main() { println(h(Some(1))) }
"#,
    );
    out.trim().parse::<i64>().unwrap_or_else(|e| {
        panic!("expected Int hash for Some(1), got {out:?}: {e}")
    });
}

#[test]
fn hash_runs_on_err_string() {
    let out = run_silt_ok(
        "hash_err_string",
        r#"
fn h(v: a) -> Int where a: Hash { v.hash() }
fn main() { println(h(Err("boom"))) }
"#,
    );
    out.trim().parse::<i64>().unwrap_or_else(|e| {
        panic!("expected Int hash for Err(\"boom\"), got {out:?}: {e}")
    });
}

// ── Round 60 L1: Range.hash via trait bound ─────────────────────────
//
// Range values share the same Silt type as `List(T)`, for which the
// typechecker auto-derives Hash, but pre-fix the dispatch allowlist
// omitted `Value::Range(..)` so `h(1..5)` errored at runtime. The
// fix mirrors the List arm: defer to the existing `impl Hash for
// Value` (src/value.rs:1791).
#[test]
fn hash_runs_on_range() {
    let out = run_silt_ok(
        "hash_range",
        r#"
fn h(v: a) -> Int where a: Hash { v.hash() }
fn main() {
    let r = 1..5
    println(h(r))
}
"#,
    );
    out.trim().parse::<i64>().unwrap_or_else(|e| {
        panic!("expected Int hash for 1..5, got {out:?}: {e}")
    });
}

/// User-defined `trait Hash for Foo` impls register as qualified
/// globals (`"Foo.hash"`), so they must take precedence over the
/// primitive-fallback arm we added. Here the user impl returns 999
/// regardless of input; any other value would mean the method
/// silently routed through the new fallback.
#[test]
fn user_defined_hash_still_routes_to_impl() {
    let out = run_silt_ok(
        "user_hash_regression",
        r#"
type Foo { v: Int }

trait Hash for Foo {
  fn hash(self) -> Int { 999 }
}

fn h(a: a) -> Int where a: Hash { a.hash() }

fn main() {
  let f = Foo { v: 7 }
  println(h(f))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "999",
        "user-defined Hash impl should still win over the primitive fallback; got {out:?}"
    );
}
