//! Regression tests for the `channel.select` send-arm surface.
//!
//! History: round 51 documented the pre-existing send form; round 52
//! then unified the op-list shape through the `ChannelOp(a)` tagged
//! variant — `Recv(ch)` for receive arms, `Send(ch, value)` for send
//! arms. The receive-only and `(channel, value)` tuple forms are gone;
//! every list element is a `ChannelOp`. One way to do things.
//!
//! These tests pin three things so neither the docs nor the code can
//! silently regress:
//!
//!   1. `docs/stdlib/globals.md`'s `Sent` section must NOT contain the
//!      "reserved for future use" phrasing. Reverting the doc fix makes
//!      this fail.
//!
//!   2. `docs/stdlib/channel-task.md`'s `channel.select` section must
//!      document the `ChannelOp` element shape and the `Sent` result
//!      that a send arm produces.
//!
//!   3. A silt program that calls `channel.select([Send(ch, value)])`
//!      must compile and run, producing the `(ch, Sent)` arm. Dropping
//!      the `SelectOpKind::Send` branch (or the `Send` constructor
//!      registration in `src/typechecker/builtins.rs` /
//!      `src/vm/dispatch.rs`) makes this fail.
//!
//! The third check executes a direct inline silt snippet — not just a
//! doc-extracted block — so that a doc-only revert still leaves a
//! runtime-level lock.

use std::path::Path;
use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;
use silt::value::Value;

/// Round-23 B1: the `Sent` section in `docs/stdlib/globals.md` must not
/// revert to the "reserved for future use" phrasing. If a future edit
/// re-adds that wording, this test fails with a precise pointer.
#[test]
fn globals_sent_section_is_not_marked_reserved() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc_path = manifest_dir.join("docs").join("stdlib").join("globals.md");
    let body = std::fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));

    // Locate the `## `Sent`` section so the assertion narrows to just
    // that subsection rather than the whole file.
    let sent_idx = body
        .find("## `Sent`")
        .expect("docs/stdlib/globals.md must have a `## `Sent`` section");
    let rest = &body[sent_idx..];
    // Bound the section to the next `## ` heading (or end of file).
    let section_end = rest[3..].find("\n## ").map(|i| i + 3).unwrap_or(rest.len());
    let section = &rest[..section_end];

    assert!(
        !section
            .to_ascii_lowercase()
            .contains("reserved for future use"),
        "{}: the `Sent` section reintroduced the 'reserved for future use' \
         phrasing, but `channel.select` actively supports mixed send/receive \
         operations (see src/builtins/concurrency.rs `parse_select_ops`). \
         Section contents:\n{}",
        doc_path.display(),
        section
    );

    // Positive lock: the section must mention `channel.select` so it
    // links the `Sent` variant to its actual use site.
    assert!(
        section.contains("channel.select"),
        "{}: the `Sent` section must reference `channel.select` so readers \
         can find the send-arm documentation. Section contents:\n{}",
        doc_path.display(),
        section
    );
}

/// `docs/stdlib/channel-task.md`'s `channel.select` section must document
/// the `ChannelOp` element form. Reverting to the pre-round-52 signature
/// (`List(Channel(a))` with raw tuples for send) or dropping the `Sent`
/// result from the prose makes this fail.
#[test]
fn channel_select_doc_documents_send_form() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc_path = manifest_dir
        .join("docs")
        .join("stdlib")
        .join("channel-task.md");
    let body = std::fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));

    // (a) The summary-table signature must name `ChannelOp` — the
    //     tagged element shape that unified the two pre-round-52 forms.
    let select_row_idx = body
        .find("| `select` |")
        .expect("docs/stdlib/channel-task.md summary table must have a `select` row");
    let row_end = body[select_row_idx..]
        .find('\n')
        .map(|i| select_row_idx + i)
        .unwrap_or(body.len());
    let row = &body[select_row_idx..row_end];
    assert!(
        row.contains("ChannelOp"),
        "{}: the `select` summary-table row must reference `ChannelOp(a)` as \
         the element shape (one-way-to-do-things unification). Row was:\n{}",
        doc_path.display(),
        row
    );

    // (b) The descriptive section must name the `Sent` result — the
    //     unique anchor that only shows up once the doc admits send
    //     arms exist.
    let section_idx = body
        .find("## `channel.select`")
        .expect("docs/stdlib/channel-task.md must have a `## `channel.select`` section");
    let rest = &body[section_idx..];
    let section_end = rest[3..].find("\n## ").map(|i| i + 3).unwrap_or(rest.len());
    let section = &rest[..section_end];
    assert!(
        section.contains("Sent"),
        "{}: the `channel.select` section must document the `Sent` result \
         that a send arm produces. Section contents:\n{}",
        doc_path.display(),
        section
    );
    // (c) And it must show both `Recv` and `Send` constructors so
    //     readers see the only two legal element shapes.
    assert!(
        section.contains("Recv") && section.contains("Send"),
        "{}: the `channel.select` section must show both `Recv(ch)` and \
         `Send(ch, value)` as the ChannelOp constructors — these are the \
         one-way legal element shapes. Section contents:\n{}",
        doc_path.display(),
        section
    );
}

/// Round-23 B1: executable lock. A silt program that uses
/// `channel.select` with a send arm MUST compile and run successfully and
/// produce the send-side result. If `SelectOpKind::Send` is removed from
/// `src/builtins/concurrency.rs` (or `parse_select_ops` is narrowed back
/// to bare channels only), this test fails.
///
/// Scenario: a buffered channel of capacity 1 starts empty, so the send
/// arm is immediately ready. The select picks it, the match arm that
/// destructures `(^out, Sent)` fires, and `main` returns 1. If the send
/// form is rejected at parse time, the compile step in `InProcessRunner`
/// surfaces a hard error and the assertion below fires.
#[test]
fn channel_select_send_arm_runs_and_returns_sent() {
    let src = r#"
import channel
fn main() {
    let out = channel.new(1)
    match channel.select([Send(out, 42)]) {
        (^out, Sent) -> 1
        _ -> 0
    }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(
        outcome.ok(),
        "channel.select with a send arm should compile and run cleanly: {outcome:?}"
    );
    assert_eq!(
        outcome.result,
        Some(Value::Int(1)),
        "expected the `(^out, Sent)` arm to fire (returning 1), got {:?}",
        outcome.result
    );
}

/// Round-52 audit G2/G3: lock the `Available After Import` summary table
/// in `docs/stdlib/globals.md`. This is a separate regression surface
/// from the per-variant `## `Sent`` prose section (already locked by
/// `globals_sent_section_is_not_marked_reserved`). Prior to round-52 the
/// table row for `Sent` still read "Reserved for future select-send
/// support", and the `Recv` / `Send` `ChannelOp` constructors — registered
/// globally by `src/typechecker/builtins.rs` and dispatched by
/// `src/vm/dispatch.rs` — were missing from the table entirely.
///
/// This test pins only the summary table (between the `## Available After
/// Import` heading and the next `## ` heading) so that a reader skimming
/// top-down still discovers all `import channel` globals, and so that the
/// stale "reserved for future" wording cannot sneak back into the row.
#[test]
fn globals_available_after_import_table_covers_select_ops() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc_path = manifest_dir.join("docs").join("stdlib").join("globals.md");
    let body = std::fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", doc_path.display(), e));

    // Narrow to just the `## Available After Import` section — this is
    // the summary table, not the per-variant prose further down.
    let heading_idx = body
        .find("## Available After Import")
        .expect("docs/stdlib/globals.md must have a `## Available After Import` section");
    let rest = &body[heading_idx..];
    // Stop at the next `## ` heading (which begins the per-variant
    // prose, e.g. `## `print``).
    let section_end = rest[3..].find("\n## ").map(|i| i + 3).unwrap_or(rest.len());
    let table = &rest[..section_end];

    // (a) The `Sent` row must not still be marked as reserved — the
    //     per-variant `## `Sent`` section was updated in round 51/52 but
    //     the table row lagged behind until round-52 audit G2.
    let lower = table.to_ascii_lowercase();
    assert!(
        !lower.contains("reserved for future"),
        "{}: the `Available After Import` table still contains \
         'Reserved for future' phrasing. The `Sent` row must describe the \
         current behavior (result variant produced by a completed \
         `channel.select` send arm). Table contents:\n{}",
        doc_path.display(),
        table
    );

    // (b) The table must have a row for each of `Sent`, `Recv`, and
    //     `Send`. The `| `Name` |` prefix is how every existing row in
    //     this table starts, so matching that form keeps the assertion
    //     tight against the table grammar rather than matching stray
    //     references elsewhere in the file.
    for name in &["Sent", "Recv", "Send"] {
        let needle = format!("| `{name}` |");
        assert!(
            table.contains(&needle),
            "{}: the `Available After Import` table is missing a row for \
             `{name}`. These constructors are globally callable after \
             `import channel` (registered in `src/typechecker/builtins.rs` \
             and dispatched by `src/vm/dispatch.rs`). Table contents:\n{}",
            doc_path.display(),
            table
        );
    }

    // (c) The `Recv` and `Send` rows must describe their role in
    //     `channel.select` — the one and only legal use site. Matching
    //     the row substring (from the row-start marker up to the next
    //     newline) keeps the assertion scoped to the row.
    for name in &["Recv", "Send"] {
        let row_start = table
            .find(&format!("| `{name}` |"))
            .expect("row presence already asserted above");
        let row_end = table[row_start..]
            .find('\n')
            .map(|i| row_start + i)
            .unwrap_or(table.len());
        let row = &table[row_start..row_end];
        assert!(
            row.contains("ChannelOp"),
            "{}: the `{name}` row must name the `ChannelOp(a)` result \
             type — that is the unified `channel.select` element shape. \
             Row was:\n{}",
            doc_path.display(),
            row
        );
        assert!(
            row.contains("channel.select") || row.contains("select"),
            "{}: the `{name}` row must mention `channel.select` so \
             readers can find the use site. Row was:\n{}",
            doc_path.display(),
            row
        );
    }
}

/// Round-23 B1: mixed send/receive arms must also work. This exercises
/// the same `SelectOpKind` dispatch but makes the receive arm the one
/// that wins — guarding against a regression that special-cases only
/// all-send or all-receive lists.
#[test]
fn channel_select_mixed_send_and_receive_arms_run() {
    let src = r#"
import channel
import task
fn main() {
    let out = channel.new(0)  -- rendezvous: send would park
    let inp = channel.new(1)  -- buffered: we pre-load it so receive is ready
    channel.send(inp, 7)
    match channel.select([Send(out, 99), Recv(inp)]) {
        (^out, Sent) -> 1
        (^inp, Message(v)) -> v
        _ -> -1
    }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(
        outcome.ok(),
        "mixed select (send + receive) should run cleanly: {outcome:?}"
    );
    assert_eq!(
        outcome.result,
        Some(Value::Int(7)),
        "expected the receive arm to win (inp preloaded with 7) and \
         return 7, got {:?}",
        outcome.result
    );
}
