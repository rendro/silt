//! Regression tests for round-92 audit fix: `Hash for Value::Range` walked
//! the whole range unbounded — `(0..4_000_000_000).hash()` spun ~4e9
//! iterations inside a single opcode (uninterruptible by the time-slice
//! scheduler) and `0..i64::MAX` hung the program forever. Every other range
//! materialization site is guarded by `checked_range_len`; Hash was the
//! single unguarded walk.
//!
//! Fix (src/value.rs, `Value::Range` arm of `impl Hash`): cap the
//! element-by-element walk at `MAX_RANGE_MATERIALIZE` (10_000_000) and hash
//! over-cap ranges in closed form (`tag, len, lo, hi`). Contract reasoning:
//!   - within the cap the byte stream is identical to the equal `List`'s,
//!     so the round-74 List ↔ Range hash contract is preserved;
//!   - over the cap, no `Value::List` can exist with > cap elements (every
//!     list-producing site enforces the cap), so the only values equal to
//!     an over-cap Range are Ranges with identical endpoints — hashing the
//!     endpoints is contract-safe;
//!   - doing the cap inside `impl Hash` (not the dispatch arm) bounds the
//!     nested auto-derive paths (record/variant/tuple/list containing a
//!     range field) for free, since the impl recurses into fields.
//!
//! Pre-fix, the silt-level tests in this file hang; a watchdog around the
//! child process turns that hang into a clean failure.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use silt::value::Value;

/// Mirror of `silt::value::MAX_RANGE_MATERIALIZE` (pub(crate), not
/// re-exported). If the constant changes, the boundary tests below should
/// be updated to match.
const CAP: i64 = 10_000_000;

fn hash_of(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

// ── (a) Rust-level: huge range hashes complete promptly ────────────

#[test]
fn huge_range_hash_completes_promptly() {
    let start = Instant::now();
    // ~4e9 elements — the audit repro. Pre-fix this alone takes minutes.
    let _ = hash_of(&Value::Range(0, 4_000_000_000));
    // ~2^63 elements — pre-fix this never terminates.
    let _ = hash_of(&Value::Range(0, i64::MAX));
    let _ = hash_of(&Value::Range(i64::MIN, i64::MAX));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "over-cap range hashing must be closed-form (O(1)), took {:?}",
        start.elapsed()
    );
}

#[test]
fn range_hash_at_cap_boundary_completes() {
    let start = Instant::now();
    // Exactly at the cap: still element-by-element (matches list hashing).
    let _ = hash_of(&Value::Range(1, CAP));
    // One past the cap: closed-form.
    let _ = hash_of(&Value::Range(1, CAP + 1));
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "cap-boundary range hashing took {:?}",
        start.elapsed()
    );
}

// ── (b) Rust-level: List ↔ Range contract preserved within the cap ──

#[test]
fn small_list_range_equal_pairs_still_hash_equal() {
    let list = Value::List(Arc::new((1..=5).map(Value::Int).collect()));
    let range = Value::Range(1, 5);
    assert_eq!(list, range, "PartialEq: List([1..5]) == Range(1,5)");
    assert_eq!(
        hash_of(&list),
        hash_of(&range),
        "round-74 contract: equal List/Range pairs must hash equal"
    );
}

#[test]
fn empty_ranges_hash_equal_to_each_other_and_empty_list() {
    // All empty ranges compare equal regardless of endpoints, and equal
    // the empty list; the over-cap closed form must not disturb this
    // (empty ranges take the len == 0 path).
    let a = Value::Range(5, 4);
    let b = Value::Range(100, 2);
    let empty = Value::List(Arc::new(vec![]));
    assert_eq!(a, b);
    assert_eq!(a, empty);
    assert_eq!(hash_of(&a), hash_of(&b));
    assert_eq!(hash_of(&a), hash_of(&empty));
}

// ── (d) Rust-level: equal over-cap ranges hash equal ───────────────

#[test]
fn equal_over_cap_ranges_hash_equal() {
    let a = Value::Range(0, 4_000_000_000);
    let b = Value::Range(0, 4_000_000_000);
    assert_eq!(a, b, "non-empty ranges are equal iff endpoints match");
    assert_eq!(
        hash_of(&a),
        hash_of(&b),
        "Hash/Eq contract: equal over-cap ranges must hash equal"
    );
    // Distinct over-cap ranges are unequal (no hash requirement, but the
    // closed form should keep them distinguishable in practice).
    let c = Value::Range(1, 4_000_000_001);
    assert_ne!(a, c);
}

// ── (c) Rust-level: nested huge ranges (auto-derive recursion path) ─

#[test]
fn nested_huge_range_hash_completes_promptly() {
    use std::collections::BTreeMap;
    let start = Instant::now();

    // Record with an over-cap range field.
    let mut fields = BTreeMap::new();
    fields.insert("xs".to_string(), Value::Range(0, i64::MAX));
    let _ = hash_of(&Value::Record("R".to_string(), Arc::new(fields)));

    // Tuple, list, and variant containing over-cap ranges.
    let _ = hash_of(&Value::Tuple(vec![
        Value::Int(1),
        Value::Range(0, 4_000_000_000),
    ]));
    let _ = hash_of(&Value::List(Arc::new(vec![Value::Range(0, i64::MAX)])));
    let _ = hash_of(&Value::Variant(
        "Some".to_string(),
        vec![Value::Range(i64::MIN, i64::MAX)],
    ));

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "nested over-cap range hashing must be O(1) per range, took {:?}",
        start.elapsed()
    );
}

// ── End-to-end silt binary (run path through vm/dispatch.rs) ───────

fn silt_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_silt") {
        return PathBuf::from(p);
    }
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("silt");
    p
}

/// Run a silt program with a watchdog. Pre-fix, the huge-range programs
/// below hang forever inside a single opcode; the watchdog kills the
/// child and fails the test instead of wedging the suite.
fn run_silt_with_timeout(tag: &str, src: &str, timeout: Duration) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!(
        "silt_round92_range_hash_{tag}_{}_{}.silt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&tmp, src).unwrap();
    let mut child = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn silt binary");
    let start = Instant::now();
    loop {
        match child.try_wait().expect("try_wait failed") {
            Some(_) => break,
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&tmp);
                    panic!(
                        "silt program `{tag}` did not finish within {timeout:?} — \
                         range hash walk is unbounded again (round-92 regression)"
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    let output = child.wait_with_output().expect("wait_with_output failed");
    let _ = std::fs::remove_file(&tmp);
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// (a) The audit repro: hashing `0..4000000000` must complete promptly.
#[test]
fn silt_huge_range_hash_completes() {
    let src = r#"
fn main() {
  let r = 0..4000000000
  let h = r.hash()
  println("hashed ok")
  let big = 0..9223372036854775806
  let h2 = big.hash()
  println("hashed max ok")
  println(h == h)
  println(h2 == h2)
}
"#;
    let (stdout, stderr, ok) = run_silt_with_timeout("huge_range", src, Duration::from_secs(30));
    assert!(ok, "silt run failed; stderr={stderr}; stdout={stdout}");
    assert!(
        stdout.contains("hashed ok") && stdout.contains("hashed max ok"),
        "expected both hash calls to complete; got stdout={stdout:?}"
    );
}

/// (b) The round-74 contract probe from the finding must stay `true`.
#[test]
fn silt_small_range_list_hash_contract_stays_true() {
    let src = r#"
fn main() {
  println((1..5).hash() == [1, 2, 3, 4, 5].hash())
}
"#;
    let (stdout, stderr, ok) =
        run_silt_with_timeout("small_contract", src, Duration::from_secs(30));
    assert!(ok, "silt run failed; stderr={stderr}; stdout={stdout}");
    assert_eq!(
        stdout.lines().next().unwrap_or("").trim_end(),
        "true",
        "List↔Range hash contract for small ranges must hold; got stdout={stdout:?}"
    );
}

/// (c) Nested: a tuple and a record carrying a huge range, hashed via the
/// auto-derive path, must complete promptly (the Hash impl recurses into
/// fields, so the cap must live inside the impl).
#[test]
fn silt_nested_huge_range_hash_completes() {
    let src = r#"
type R { xs: List(Int) }
fn main() {
  let t = (1, 0..4000000000)
  let th = t.hash()
  println("tuple hashed ok")
  let r = R { xs: 0..4000000000 }
  let rh = r.hash()
  println("record hashed ok")
  println(th == th)
  println(rh == rh)
}
"#;
    let (stdout, stderr, ok) = run_silt_with_timeout("nested_huge", src, Duration::from_secs(30));
    assert!(ok, "silt run failed; stderr={stderr}; stdout={stdout}");
    assert!(
        stdout.contains("tuple hashed ok") && stdout.contains("record hashed ok"),
        "expected nested hash calls to complete; got stdout={stdout:?}"
    );
}

/// (d) Equal over-cap ranges must hash equal at the language level too.
#[test]
fn silt_equal_over_cap_ranges_hash_equal() {
    let src = r#"
fn main() {
  let a = 0..4000000000
  let b = 0..4000000000
  println(a == b)
  println(a.hash() == b.hash())
}
"#;
    let (stdout, stderr, ok) = run_silt_with_timeout("over_cap_eq", src, Duration::from_secs(30));
    assert!(ok, "silt run failed; stderr={stderr}; stdout={stdout}");
    let trues = stdout.lines().filter(|l| l.trim_end() == "true").count();
    assert!(
        trues >= 2,
        "expected `a == b` and `a.hash() == b.hash()` to both print true; got stdout={stdout:?}"
    );
}
