//! Regression tests for `list.zip` result-length bounds (L1).
//!
//! Before the fix, `list.zip` computed its result capacity via
//! `ValueIter::len()` which used `size_hint` saturating to `usize::MAX`
//! for huge ranges like `0..i64::MAX`. The subsequent
//! `Vec::with_capacity(usize::MAX)` then panicked opaquely, surfacing
//! as "builtin module 'list' panicked". The fix validates both input
//! lengths (via `checked_range_len` semantics for ranges) against
//! `MAX_RANGE_MATERIALIZE` and returns a clean `VmError` on overflow.
//!
//! The rejection and small-range cases are golden cases in
//! tests/golden/lang/stdlib/list_zip_bounds__*.silt. The at-cap case stays
//! here: materializing 10M tuples takes longer than the golden harness's
//! 20 s per-case limit in a debug build, and in-process it is quicker.

use silt::value::Value;

fn run(input: &str) -> Value {
    silt::session::testing::run_str(input).unwrap_or_else(|e| panic!("{e}"))
}

// ── Exactly at the cap: must pass ───────────────────────────────────
//
// `MAX_RANGE_MATERIALIZE` is 10_000_000. The inclusive range `0..9999999`
// yields exactly 10_000_000 elements. Zipping two such ranges must
// produce a 10_000_000-element list without hitting the cap.

#[test]
fn test_list_zip_range_range_at_cap_ok() {
    let result = run(r#"
import list
fn main() {
  list.length(list.zip(0..9999999, 0..9999999))
}
        "#);
    assert_eq!(result, Value::Int(10_000_000));
}
