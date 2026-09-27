//! Regression locks — builtin collection functions must not launder
//! `Fn` values into `Value`'s bitwise `Ord`/`PartialEq`.
//!
//! FINDING (BROKEN, type-system soundness): the ordering/equality-
//! consuming collection builtins (`set.from_list` / `insert` /
//! `contains` / `remove` and the set algebra ops; `list.sort` /
//! `unique` / `contains` / `index_of`) have UNBOUNDED type-variable
//! signatures (e.g. `set.from_list: (List(a)) -> Set(a)`,
//! src/typechecker/builtins/set.rs — contrast `map.get`, which carries
//! `k: Hash` and statically rejects Fn keys). Fn values therefore
//! flowed straight into `Value`'s bitwise comparisons:
//!   - `set.from_list([g, f])` ordered closures by Arc pointer address,
//!     so `set.to_list` iteration order was ASLR-nondeterministic
//!     across runs;
//!   - `list.sort` pointer-sorted closures; `list.unique` deduped by
//!     identity; `list.contains([f], g)` silently answered `false` and
//!     `list.index_of([f, g], g)` answered `Some(1)` via Arc identity —
//!     while `[f] == [g]` correctly errors
//!     "type 'Fn' does not implement Equal" at runtime.
//!
//! Same bug class as the round-97 operator gate and the round-1-nightly
//! `Op::Eq`/`Op::Lt` + dispatch `equal`/`compare` gates — those covered
//! operators and trait-method dispatch but missed the builtin-function
//! surface entirely.
//!
//! FIX: `ensure_no_fn` in src/builtins/collections.rs — a runtime
//! backstop mirroring the operator gates that errors
//! "type 'Fn' does not implement Compare" / "Equal". Deliberately NOT a
//! static `where a: Compare` bound on the builtin signatures: that
//! would reject currently-working programs (sorting tuples, NaN-bearing
//! floats via `Value::cmp`).
//!
//! Every rejection test below FAILS on the pre-fix code (the program
//! ran to completion on laundered pointer ordering / identity equality)
//! and PASSES post-fix. The positive-control tests guard against
//! over-rejection (the gate's failure mode).

use std::process::Command;

/// Drive the full lex → parse → typecheck → compile → VM pipeline via
/// the `silt run` CLI and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_collection_fn_gate_{label}.silt"));
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

/// Assert `src` fails at runtime with the canonical operator-gate
/// wording (`needle` is "does not implement Compare" or "... Equal").
fn assert_gated(label: &str, src: &str, needle: &str) {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        !ok,
        "{label}: expected non-zero exit (runtime Fn gate), got success \
         with stdout: {stdout:?}"
    );
    assert!(
        stderr.contains(needle),
        "{label}: expected stderr containing {needle:?}, got: {stderr:?}"
    );
}

/// Assert `src` runs cleanly and stdout contains every needle.
fn assert_runs(label: &str, src: &str, needles: &[&str]) {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "{label}: expected clean run, got failure with stderr: {stderr:?}"
    );
    for needle in needles {
        assert!(
            stdout.contains(needle),
            "{label}: expected stdout containing {needle:?}, got: {stdout:?}"
        );
    }
}

// ── REJECTIONS: Fn values reaching ordering/equality builtins ──────────

#[test]
fn set_from_list_of_fns_is_gated() {
    // The exact repro from the finding: pre-fix, 5 consecutive runs
    // printed [101, 1] / [1, 101] nondeterministically under ASLR.
    let src = r#"
import set
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  let s = set.from_list([g, f])
  let applied = list.map(set.to_list(s), fn(h){ h(1) })
  println(applied)
}
"#;
    assert_gated("set_from_list", src, "does not implement Compare");
}

#[test]
fn set_insert_fn_is_gated() {
    let src = r#"
import set
fn main() {
  let s = set.insert(set.new(), fn(y){ y })
  println(set.length(s))
}
"#;
    assert_gated("set_insert", src, "does not implement Compare");
}

#[test]
fn set_contains_fn_probe_is_gated() {
    let src = r#"
import set
fn main() {
  println(set.contains(set.new(), fn(y){ y }))
}
"#;
    assert_gated("set_contains", src, "does not implement Compare");
}

#[test]
fn set_remove_fn_is_gated() {
    let src = r#"
import set
fn main() {
  let s = set.remove(set.new(), fn(y){ y })
  println(set.length(s))
}
"#;
    assert_gated("set_remove", src, "does not implement Compare");
}

#[test]
fn set_union_of_fn_sets_is_gated() {
    // A Fn-bearing set built by an UNGATED producer (`set.map` callback
    // returning closures) must still be caught by the algebra-op gate,
    // which walks whole operand sets rather than just a probe value.
    let src = r#"
import set
fn main() {
  let s = set.map(set.from_list([1, 2]), fn(x){ fn(y){ y + x } })
  let u = set.union(s, s)
  println(set.length(u))
}
"#;
    assert_gated("set_union", src, "does not implement Compare");
}

#[test]
fn list_sort_of_fns_is_gated() {
    // Pre-fix: v.sort() ordered the closures by Arc pointer address.
    let src = r#"
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  println(list.length(list.sort([g, f])))
}
"#;
    assert_gated("list_sort", src, "does not implement Compare");
}

#[test]
fn list_sort_of_tuples_holding_fns_is_gated() {
    // The gate must recurse into container elements: a tuple wrapping a
    // closure launders the same pointer ordering one level down.
    let src = r#"
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  println(list.length(list.sort([(2, g), (1, f)])))
}
"#;
    assert_gated("list_sort_nested", src, "does not implement Compare");
}

#[test]
fn list_unique_of_fns_is_gated() {
    // Pre-fix: BTreeSet::insert deduped closures by identity.
    let src = r#"
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  println(list.length(list.unique([f, g, f])))
}
"#;
    assert_gated("list_unique", src, "does not implement Equal");
}

#[test]
fn list_contains_fn_is_gated() {
    // Pre-fix: answered false via Arc identity — while `f == g` errors.
    let src = r#"
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  println(list.contains([f], g))
}
"#;
    assert_gated("list_contains", src, "does not implement Equal");
}

#[test]
fn list_index_of_fn_is_gated() {
    // Pre-fix: answered Some(1) via Arc identity.
    let src = r#"
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  println(list.index_of([f, g], g))
}
"#;
    assert_gated("list_index_of", src, "does not implement Equal");
}

// ── POSITIVE CONTROLS: the gate must not over-reject ───────────────────

#[test]
fn set_from_list_ints_still_dedups() {
    let src = r#"
import set
fn main() {
  println(set.length(set.from_list([1, 2, 2])))
}
"#;
    assert_runs("set_ints_ok", src, &["2"]);
}

#[test]
fn set_ops_ints_still_work() {
    let src = r#"
import set
fn main() {
  let a = set.from_list([1, 2])
  let b = set.insert(set.from_list([2, 3]), 4)
  println(set.length(set.union(a, b)))
  println(set.length(set.intersection(a, b)))
  println(set.contains(set.remove(a, 1), 2))
}
"#;
    assert_runs("set_ops_ok", src, &["4", "1", "true"]);
}

#[test]
fn list_sort_and_unique_ints_still_work() {
    let src = r#"
import list
fn main() {
  println(list.sort([3, 1, 2]))
  println(list.unique([1, 1, 2]))
}
"#;
    assert_runs("list_sort_ok", src, &["[1, 2, 3]", "[1, 2]"]);
}

#[test]
fn list_sort_of_fn_free_tuples_still_works() {
    // The finding's fix sketch calls this out explicitly: sorting
    // tuples via `Value::cmp` must keep working — a static
    // `where a: Compare` bound would have rejected it.
    let src = r#"
import list
fn main() {
  println(list.length(list.sort([(2, "b"), (1, "a")])))
}
"#;
    assert_runs("list_sort_tuples_ok", src, &["2"]);
}

#[test]
fn list_contains_and_index_of_ints_still_work() {
    let src = r#"
import list
fn main() {
  println(list.contains([1, 2], 2))
  println(list.index_of([1, 2], 2))
}
"#;
    assert_runs("list_contains_ok", src, &["true", "Some(1)"]);
}
