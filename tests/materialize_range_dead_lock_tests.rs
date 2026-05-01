//! Round 67 DEAD lock — `Value::materialize_range` had zero callers and
//! is removed. Source-grep lock prevents reintroduction.
//!
//! The canonical helper for range-length checks is
//! `value::checked_range_len`. Inline range-materialization sites
//! (`ListConcat`, `materialize_iter`, `list.flatten`, `list.flat_map`,
//! `value_to_json`, …) call that helper directly and walk the
//! `lo..=hi` themselves. The vestigial `materialize_range` wrapper had
//! no callers — only a doc-comment mention in
//! `tests/list_flat_map_range_bounds_tests.rs`.

const VALUE_RS: &str = include_str!("../src/value.rs");

/// `materialize_range` must not appear in `src/value.rs` — neither as
/// a method definition nor as any other identifier (preventing a
/// caller-introduction follow-up that re-creates the wrapper).
#[test]
fn value_rs_does_not_define_or_reference_materialize_range() {
    assert!(
        !VALUE_RS.contains("fn materialize_range"),
        "src/value.rs defines `fn materialize_range` — this method had \
         zero callers and was removed in round 67. If a caller now needs \
         it, prefer `checked_range_len` + an inline `(lo..=hi).map(...)` \
         materialization (matching every other range-materialization site)."
    );
    assert!(
        !VALUE_RS.contains("materialize_range"),
        "src/value.rs mentions `materialize_range` — the wrapper was \
         removed in round 67 and must not be reintroduced. Use \
         `checked_range_len` directly."
    );
}
