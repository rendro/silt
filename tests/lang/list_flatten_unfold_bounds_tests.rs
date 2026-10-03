//! Regression tests for `list.flatten` and `list.unfold` accumulation caps.
//!
//! Round 19 audit -- LATENT: both `list.flatten` and `list.unfold` could
//! accumulate unbounded results without ever checking `MAX_RANGE_MATERIALIZE`.
//! Every other collection-building builtin caps output at 10,000,000 elements.
//! The `list.unfold` cap test stays in Rust because driving ten million
//! closure calls through the CLI exceeds the golden harness's 20 s case
//! timeout; the `list.flatten` cap and both small-input controls are golden
//! cases `tests/golden/lang/limits/list_flatten_unfold_bounds__*`.

fn run_err(input: &str) -> String {
    match silt::session::testing::run_str(input) {
        Ok(v) => panic!("expected an error, got {v:?}"),
        Err(e) => e,
    }
}

// ── list.unfold ────────────────────────────────────────────────────────

/// Unfold that would generate more than MAX_RANGE_MATERIALIZE elements.
/// The callback never returns None, so without a cap it would loop forever.
/// With the cap it should error after 10,000,001 elements.
#[test]
fn test_list_unfold_over_cap_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  list.unfold(0) { n -> Some((n, n + 1)) }
}
        "#,
    );
    assert!(
        err.contains("list.unfold"),
        "error should mention list.unfold by name, got: {err}"
    );
    assert!(
        err.contains("exceeds maximum list length"),
        "error should mention exceeds maximum list length, got: {err}"
    );
}
