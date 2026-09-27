//! Regression locks — six sibling surfaces the collection/operator Fn
//! gates missed: `map.from_entries`, `map.update`, `list.group_by`,
//! `list.min_by`, `list.max_by`, `stream.dedup`.
//!
//! FINDING (BROKEN, type-system soundness): the `ensure_no_fn` /
//! `value_contains_fn` runtime gates (collection-Fn round, plus the
//! round-97 / round-1-nightly operator and dispatch gates) covered
//! `list.sort` / `unique` / `contains` / `index_of` and the `set.*`
//! ordering ops, but missed six more surfaces with the same unbounded
//! signatures:
//!
//!   - `map.from_entries` / `map.update` have `constraints: vec![]`
//!     (src/typechecker/builtins/map.rs) — unlike `map.get`/`set`,
//!     which carry `k: Hash` — so Fn KEYS typecheck and get
//!     BTreeMap-ordered by Arc pointer address: ASLR-nondeterministic
//!     entry order across runs.
//!   - `list.group_by` inserts the callback-returned key into a
//!     `BTreeMap` (`m.entry(result)`, src/vm/execute.rs) — same
//!     pointer-ordered-Fn-key hole.
//!   - `list.min_by` / `list.max_by` run `partial_cmp` on the
//!     callback-returned keys — Fn keys pick an ASLR-nondeterministic
//!     winner (`Some(1)` or `Some(2)` across runs).
//!   - `stream.dedup` compares consecutive values with `p != &v` on a
//!     detached pump thread — closures silently deduped by Arc
//!     identity, the exact behavior `list.unique` rejects.
//!
//! FIX: mirror the established `ensure_no_fn` policy (deliberately a
//! runtime gate, NOT a static bound — see
//! tests/collection_builtin_fn_gate_tests.rs). The map arms gate the
//! KEY with the trait name from the static map contract (Hash); the
//! group_by/min_by/max_by iterator steps gate the callback-returned key
//! via `Vm::value_contains_fn` (Hash / Compare); `stream.dedup` has no
//! `VmError` path on its pump thread, so it pushes an in-band
//! `__StreamTypeError__` marker (the `__MapMapTypeError__` pattern)
//! that `spawn_pump` transforms forward and the synchronous sinks
//! translate into the canonical error.
//!
//! Every rejection test below FAILS on the pre-fix code (the program
//! ran to completion on laundered pointer ordering / identity equality
//! — every one of them typechecks clean) and PASSES post-fix. The
//! positive controls guard against over-rejection, including Fn values
//! stored as map VALUES (only keys are compared).

use std::process::Command;

/// Drive the full lex → parse → typecheck → compile → VM pipeline via
/// the `silt run` CLI and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_fn_gate_sibling_{label}.silt"));
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
/// wording (`needle` is "does not implement Hash" / "Compare" /
/// "Equal").
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

// ── REJECTIONS: Fn values reaching the six missed surfaces ─────────────

#[test]
fn map_from_entries_fn_keys_are_gated() {
    // The exact repro from the finding: pre-fix this printed the two
    // entries in ASLR-nondeterministic order across runs (BTreeMap
    // ordered the VmClosure keys by Arc pointer address).
    let src = r#"
import map
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z * 2 }
  let m = map.from_entries([(f, 1), (g, 2)])
  println(list.length(map.entries(m)))
}
"#;
    assert_gated("map_from_entries", src, "does not implement Hash");
}

#[test]
fn map_update_fn_key_is_gated() {
    let src = r#"
import map
import list
fn main() {
  let f = fn(y){ y }
  let m = map.update(map.from_entries([]), f, 1, fn(v){ v + 1 })
  println(list.length(map.entries(m)))
}
"#;
    assert_gated("map_update", src, "does not implement Hash");
}

#[test]
fn list_group_by_fn_keys_are_gated() {
    // Pre-fix: `m.entry(result)` pointer-ordered the two closure keys,
    // so entry order flipped across runs.
    let src = r#"
import list
import map
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  let groups = list.group_by([1, 2], fn(x){ match x { 1 -> f, _ -> g } })
  println(list.length(map.entries(groups)))
}
"#;
    assert_gated("list_group_by", src, "does not implement Hash");
}

#[test]
fn list_min_by_fn_keys_are_gated() {
    // Pre-fix: partial_cmp on the closure keys printed Some(1) or
    // Some(2) nondeterministically across runs.
    let src = r#"
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  println(list.min_by([1, 2], fn(x){ match x { 1 -> f, _ -> g } }))
}
"#;
    assert_gated("list_min_by", src, "does not implement Compare");
}

#[test]
fn list_max_by_fn_keys_are_gated() {
    let src = r#"
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  println(list.max_by([1, 2], fn(x){ match x { 1 -> f, _ -> g } }))
}
"#;
    assert_gated("list_max_by", src, "does not implement Compare");
}

#[test]
fn stream_dedup_of_fns_is_gated() {
    // Pre-fix: printed 2 — the two identical closures were silently
    // deduped by Arc identity (`p != &v` on the pump thread), the exact
    // behavior list.unique rejects.
    let src = r#"
import stream
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  let out = stream.collect(stream.dedup(stream.from_list([f, f, g])))
  println(list.length(out))
}
"#;
    assert_gated("stream_dedup", src, "does not implement Equal");
}

#[test]
fn stream_dedup_error_survives_downstream_transform() {
    // The in-band marker must pass through a `spawn_pump` transform
    // (never into its user callback) and still surface at the sink.
    let src = r#"
import stream
import list
fn main() {
  let f = fn(y){ y }
  let g = fn(z){ z + 100 }
  let out = stream.from_list([f, g])
    |> stream.dedup
    |> stream.map(fn(x){ x })
    |> stream.collect
  println(list.length(out))
}
"#;
    assert_gated("stream_dedup_mapped", src, "does not implement Equal");
}

// ── POSITIVE CONTROLS: the gates must not over-reject ──────────────────

#[test]
fn map_from_entries_int_keys_still_work() {
    let src = r#"
import map
import list
fn main() {
  println(list.length(map.entries(map.from_entries([(1, 10), (2, 20)]))))
}
"#;
    assert_runs("map_from_entries_ok", src, &["2"]);
}

#[test]
fn map_from_entries_fn_values_still_allowed() {
    // Only KEYS are compared by the map — a closure stored as a map
    // VALUE is legitimate and must not trip the gate.
    let src = r#"
import map
fn main() {
  let f = fn(y){ y + 1 }
  let m = map.from_entries([(1, f)])
  let got = map.get(m, 1)
  match got {
    Some(h) -> println(h(41))
    None -> println(0)
  }
}
"#;
    assert_runs("map_from_entries_fn_values_ok", src, &["42"]);
}

#[test]
fn map_update_int_key_still_works() {
    let src = r#"
import map
fn main() {
  let m = map.update(map.from_entries([(1, 10)]), 1, 0, fn(v){ v + 5 })
  println(map.get(m, 1))
}
"#;
    assert_runs("map_update_ok", src, &["Some(15)"]);
}

#[test]
fn list_group_by_int_keys_still_works() {
    let src = r#"
import list
import map
fn main() {
  let groups = list.group_by([1, 2, 3, 4], fn(x){ x > 2 })
  println(list.length(map.entries(groups)))
}
"#;
    assert_runs("list_group_by_ok", src, &["2"]);
}

#[test]
fn list_min_by_and_max_by_int_keys_still_work() {
    let src = r#"
import list
fn main() {
  println(list.min_by([3, 1, 2], fn(x){ x }))
  println(list.max_by([3, 1, 2], fn(x){ x }))
}
"#;
    assert_runs("list_min_max_by_ok", src, &["Some(1)", "Some(3)"]);
}

#[test]
fn stream_dedup_ints_still_works() {
    let src = r#"
import stream
fn main() {
  println(stream.collect(stream.dedup(stream.from_list([1, 1, 2, 2, 1]))))
}
"#;
    assert_runs("stream_dedup_ok", src, &["[1, 2, 1]"]);
}
