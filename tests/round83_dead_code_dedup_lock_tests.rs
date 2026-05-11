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
//! Two locks below pin both fixes against reintroduction:
//!
//!   * Lock A proves `common::ok(v)` produces an output value byte-for-
//!     byte equal to the inline `Value::Variant("Ok".into(), vec![v])`
//!     literal that the three deleted clones built — i.e. the deletion
//!     was a semantic no-op.
//!   * Lock A also source-greps the three files to ensure `fn ok(v: Value)
//!     -> Value` does NOT come back as a local definition (clone-rot
//!     guard) and that the canonical `use super::common::ok` is present.
//!   * Lock B source-greps `src/scheduler.rs` to assert `WakeSelectEdge`
//!     stays gone while `MainTarget` stays exported.

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

/// Clone-rot guard: assert the three sibling files do NOT each
/// re-introduce a local `fn ok(v: Value) -> Value` definition. The
/// signature is specific enough that no unrelated helper in any of these
/// files would collide. Each file must instead import the canonical
/// helper via `use super::common::ok` (potentially as part of a
/// brace-grouped list like `use super::common::{ok, value_kind};`).
#[test]
fn round83_no_local_ok_clone_in_tcp_stream_postgres() {
    let tcp = include_str!("../src/builtins/tcp.rs");
    let stream = include_str!("../src/builtins/stream.rs");
    let postgres = include_str!("../src/builtins/postgres.rs");

    let forbidden = "fn ok(v: Value) -> Value";

    for (name, src) in [("tcp.rs", tcp), ("stream.rs", stream), ("postgres.rs", postgres)] {
        assert!(
            !src.contains(forbidden),
            "src/builtins/{name} must not redefine `{forbidden}` — the \
             round-83 dedup collapsed all three clones into \
             `super::common::ok`. Re-importing or re-cloning the helper \
             would defeat the dedup."
        );
        // Positive side: the canonical import must be present. We allow
        // either a bare `use super::common::ok;` line OR a
        // brace-grouped import that names `ok`.
        let has_bare = src.contains("use super::common::ok;");
        let has_grouped =
            src.contains("use super::common::{") && src.lines().any(|l| {
                l.contains("use super::common::{")
                    && (l.contains(" ok,") || l.contains(" ok}") || l.contains("{ok,") || l.contains("{ok}"))
            });
        assert!(
            has_bare || has_grouped,
            "src/builtins/{name} must import `super::common::ok` (the \
             canonical helper). Found neither a bare \
             `use super::common::ok;` nor a brace-grouped `use \
             super::common::{{…, ok, …}};` import — did the dedup get \
             reverted?"
        );
    }
}

// ── Lock B: WakeSelectEdge removal ──────────────────────────────────────

/// Round 83 dropped the `SelectEdge as WakeSelectEdge` alias from
/// `src/scheduler.rs`'s top-level re-export tuple (it had zero external
/// callers). Pin the deletion: `WakeSelectEdge` must not appear anywhere
/// in `src/scheduler.rs`, while `MainTarget` (which DOES have external
/// callers, e.g. `src/builtins/concurrency.rs`) must still be
/// re-exported.
#[test]
fn round83_wake_select_edge_alias_stays_gone_but_main_target_stays() {
    let scheduler = include_str!("../src/scheduler.rs");

    assert!(
        !scheduler.contains("WakeSelectEdge"),
        "src/scheduler.rs must not re-introduce the `WakeSelectEdge` \
         alias — round 83 dropped it because it had zero external \
         callers. Internal `SelectEdge` users import it directly via \
         the private `use wake_graph::SelectEdge;` line; that private \
         import is the supported entry point."
    );

    // `MainTarget` IS used by `src/builtins/concurrency.rs`, so its
    // public re-export at `silt::scheduler::MainTarget` must survive.
    // We assert a `pub use` line that names `MainTarget` exists.
    let main_target_re_exported = scheduler.lines().any(|l| {
        let trimmed = l.trim_start();
        trimmed.starts_with("pub use") && trimmed.contains("MainTarget")
    });
    assert!(
        main_target_re_exported,
        "src/scheduler.rs must still `pub use` `MainTarget` — round 83 \
         only dropped the `WakeSelectEdge` half of the original re-export \
         tuple. `MainTarget` has external callers (e.g. \
         `src/builtins/concurrency.rs`) and must stay exported."
    );
}
