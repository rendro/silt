//! Round 83 lock tests.
//!
//! Round 83 closed two small dead-code / bloat findings:
//!
//!   1. **`fn ok` clone collapse.** Three sibling builtin modules
//!      (`src/builtins/tcp.rs`, `src/builtins/stream.rs`,
//!      `src/builtins/postgres.rs`) each defined a private `fn ok(v: Value)
//!      -> Value` whose body was byte-identical to
//!      `src/builtins/common::ok`. Every other builtin module already
//!      called `super::common::ok` — these three were inconsistent
//!      duplicates. The fix: delete the three local clones, add
//!      `use super::common::ok;` to each file. The local `fn err` shims
//!      stay (they wrap module-specific error variants with different
//!      semantics).
//!
//!   2. **Unused `WakeSelectEdge` re-export.** `src/scheduler.rs:30`
//!      re-exported `wake_graph::SelectEdge as WakeSelectEdge`. Zero
//!      external callers consumed the alias — internal users imported
//!      `SelectEdge` directly via a separate private `use` line. Sibling
//!      `MainTarget` IS used externally (by
//!      `src/builtins/concurrency.rs`), so the line shrinks to
//!      `pub use wake_graph::MainTarget;` instead of disappearing.
//!
//! Per the audit prompt:
//!
//! > Dead-code fixes need a lock test proving the deletion was
//! > semantically a no-op — e.g. a test that asserts the HashMap size /
//! > method_table count is unchanged, or that running the shorter code
//! > produces the same output as the longer code. Idempotency is easy to
//! > claim and hard to verify; the lock closes that.
//!
//! The lock below proves `common::ok(v)` produces an output value
//! byte-for-byte equal to the inline `Value::Variant("Ok".into(), vec![v])`
//! literal that the three deleted clones built.

use silt::builtins;
use silt::value::Value;

// ── Lock A: fn ok dedup ─────────────────────────────────────────────────

/// Output-equivalence: `common::ok(v)` builds the same `Value` the three
/// deleted local clones built. This is the actual "deletion was a no-op"
/// proof — anything that breaks this assertion would also have broken
/// every Ok-returning tcp/stream/postgres builtin call site.
#[test]
fn round83_ok_helper_matches_inline_variant_for_each_dedup_site() {
    // Three representative values, one per dedup site. The shape of the
    // inner value does not matter — the helper just wraps it in the Ok
    // variant — so we vary it to exercise the wrapper, not the payload.
    let cases = [
        // tcp.rs typically wraps Bytes / Unit / Int payloads.
        Value::Int(42),
        // stream.rs wraps String payloads (line iteration).
        Value::String("line".into()),
        // postgres.rs wraps Variant / Record payloads — Unit stands in
        // as a cheap, equality-friendly sentinel.
        Value::Unit,
    ];

    for payload in cases {
        let inline = Value::Variant("Ok".into(), vec![payload.clone()]);
        let via_helper = builtins::ok(payload);
        assert_eq!(
            inline, via_helper,
            "common::ok must produce the same Value as the inline literal \
             that the deleted clones in tcp/stream/postgres built"
        );
    }
}
