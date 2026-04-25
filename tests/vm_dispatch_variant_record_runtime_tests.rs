//! Runtime-dispatch tests for `Compare` / `Hash` on `Variant` and `Record`.
//!
//! Three BROKEN findings (pre-fix, all in `src/vm/dispatch.rs`):
//!
//! 1. `cmp_gen(Red, Green)` where `cmp_gen` carries a `where a: Compare`
//!    bound fails at runtime:
//!      `error[runtime]: compare() not supported between Variant and Variant`.
//!    The `"compare"` arm in `dispatch_trait_method` had no
//!    `(Value::Variant, Value::Variant)` pair, so it fell into the
//!    catch-all error. Fix: add a `(Variant, Variant) => receiver.cmp(other)`
//!    arm; `Value::cmp` (`src/value.rs:1551`) already orders variants by
//!    name with a weekday-ordinal special case.
//!
//! 2. `cmp_gen(p, q)` where `p`/`q` are user-defined records fails:
//!      `error[runtime]: compare() not supported between Record and Record`.
//!    Same arm missing `(Value::Record, Value::Record)`. Fix: defer to
//!    `Value::cmp`, which orders records by name then field-wise
//!    (`src/value.rs:1537`) with canonical Date/Time/DateTime ordering.
//!
//! 3. `h(p)` where `p` is a user record and `h` has `where a: Hash` fails:
//!      `error[runtime]: no method 'hash' for type 'Point'`.
//!    The `"hash"` arm's allowlist omitted `Value::Record`, even though
//!    `impl Hash for Value` (`src/value.rs:1814`) already hashes records
//!    structurally. Fix: add `Value::Record(..)` to the allowlist.
//!
//! These tests exercise the runtime end-to-end via the `silt` CLI
//! (mirroring `tests/vm_trait_dispatch_runtime_tests.rs`) so they fail
//! before the `dispatch.rs` fix and pass after.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_variant_record_dispatch_{label}.silt"));
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

// ── Bug 1: Variant-vs-Variant `.compare()` via `Compare` bound ──────

/// `Compare` on a user-declared enum-style variant must dispatch through
/// the runtime `"compare"` arm. `Value::cmp` orders variants by their
/// declaration-order ordinal (registered into the global variant-ordinal
/// registry by the typechecker when it processes the enum decl). For
/// `type Color { Red, Green, Blue }` we have Red=0, Green=1, Blue=2, so
/// `Red.compare(Green)` = Less = -1. (Pre-round-61 this was alphabetical
/// fallback returning 1.)
#[test]
fn compare_runs_on_user_variant() {
    let out = run_silt_ok(
        "variant_user",
        r#"
type Color { Red, Green, Blue, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen(Red, Green)) }
"#,
    );
    assert_eq!(out.trim(), "-1");
}

/// The built-in `Weekday` variant (from `import time`) is a Variant at
/// runtime. `Value::cmp` consults the global variant-ordinal registry,
/// which seeds Weekday with Monday=0..Sunday=6, so Monday < Friday and
/// `cmp_gen(Monday, Friday)` = Less = -1. (Pre-round-61 this used a
/// hand-rolled `weekday_ordinal` table; the new registry-based path
/// preserves the same semantics.)
#[test]
fn compare_runs_on_builtin_weekday() {
    let out = run_silt_ok(
        "variant_weekday",
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen(Monday, Friday)) }
"#,
    );
    assert_eq!(out.trim(), "-1");
}

// ── Bug 2: Record-vs-Record `.compare()` via `Compare` bound ────────

/// `Compare` on a user-declared record must dispatch through the
/// runtime `"compare"` arm. `Value::cmp` orders records by name then
/// field-wise (`src/value.rs:1537`); `Point { x: 1, y: 2 }` vs
/// `Point { x: 1, y: 3 }` => Less = -1.
#[test]
fn compare_runs_on_user_record() {
    let out = run_silt_ok(
        "record_user",
        r#"
type Point { x: Int, y: Int, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    let p = Point { x: 1, y: 2 }
    let q = Point { x: 1, y: 3 }
    println(cmp_gen(p, q))
}
"#,
    );
    assert_eq!(out.trim(), "-1");
}

// ── Bug 3: Record `.hash()` via `Hash` bound ────────────────────────

/// `Hash` on a user-declared record must dispatch through the runtime
/// `"hash"` arm. `impl Hash for Value` already hashes records
/// structurally (`src/value.rs:1814`); the only fix needed was adding
/// `Value::Record(..)` to the dispatch allowlist. The hash is a
/// deterministic i64 derived from `DefaultHasher`. The concrete value
/// observed for `Point { x: 1, y: 2 }` on this build is locked below —
/// if hashing semantics change the test will flag it.
#[test]
fn hash_runs_on_user_record() {
    let out = run_silt_ok(
        "record_hash",
        r#"
type Point { x: Int, y: Int, }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
    let p = Point { x: 1, y: 2 }
    println(h(p))
}
"#,
    );
    assert_eq!(out.trim(), "426486041218162106");
}
