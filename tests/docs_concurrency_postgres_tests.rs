//! Regression locks for two doc typos fixed in round 59:
//!
//! 1. `docs/concurrency.md` briefly named a nonexistent builtin
//!    `task.sleep` when enumerating "parked" edges for `task.cancel`.
//!    The correct name is `time.sleep` (every other occurrence in the
//!    file already spelled it correctly).
//!
//! 2. `docs/why-silt.md` claimed that `postgres` "ships in-box" in the
//!    same sentence as `http`, `json`, `channel`, `stream`, `time`. But
//!    `Cargo.toml` only lists `http` / `tcp` / `repl` / `lsp` / `watch`
//!    / `local-clock` in `default = [...]` — `postgres` is opt-in per
//!    `Cargo.toml:25`, and `docs/stdlib/postgres.md` already labels it
//!    "postgres (opt-in feature)" and tells the reader to
//!    `--features postgres`.
//!
//! Both tests read the source files directly so the documentation can't
//! silently regress without the test suite noticing.

use std::path::Path;

fn read_doc(relative: &str) -> String {
    let path = Path::new(relative);
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{relative} must be readable: {e}"))
}

/// `docs/concurrency.md` must not name a nonexistent `task.sleep`
/// builtin. Every parking edge that used to say `task.sleep` should now
/// say `time.sleep`.
#[test]
fn docs_concurrency_uses_time_sleep_not_task_sleep() {
    let doc = read_doc("docs/concurrency.md");

    // Scan for a `task.sleep` token bounded on both sides so we don't
    // catch `task.sleep_for` (not a thing) or accidentally trip on
    // other `task.*` builtins like `task.cancel` / `task.join`.
    //
    // The boundary is: the char right after "task.sleep" must not be
    // ASCII alphanumeric or `_`, i.e. `task.sleep` must not be a
    // prefix of a longer identifier.
    let needle = "task.sleep";
    for (idx, _) in doc.match_indices(needle) {
        let after = doc[idx + needle.len()..].chars().next();
        let is_ident_continuation =
            matches!(after, Some(c) if c.is_ascii_alphanumeric() || c == '_');
        assert!(
            is_ident_continuation,
            "docs/concurrency.md still contains a bare `task.sleep` reference at byte offset \
             {idx}. There is no `task.sleep` builtin in silt — the correct name is \
             `time.sleep`. See every other occurrence in the same file."
        );
    }

    // Sanity: the correct name must still appear (we didn't accidentally
    // delete the passage).
    assert!(
        doc.contains("time.sleep"),
        "docs/concurrency.md must still reference `time.sleep` — that's the real builtin \
         that parks a task on a timer."
    );
}

/// `docs/why-silt.md` must not claim `postgres` ships in-box alongside
/// the default stdlib modules. `postgres` is a Cargo feature that must
/// be explicitly enabled.
#[test]
fn docs_why_silt_does_not_claim_postgres_ships_in_box() {
    let doc = read_doc("docs/why-silt.md");

    // Pin the old wrong sentence verbatim. If anyone reintroduces it,
    // this assertion fires with a pointer to Cargo.toml for proof.
    let stale_fragment = "`postgres`, `channel`, `stream`, `time` all ship in-box";
    assert!(
        !doc.contains(stale_fragment),
        "docs/why-silt.md still lists `postgres` in the \"all ship in-box\" sentence. That \
         is inconsistent with Cargo.toml (default features do not include `postgres`) and \
         with the postgres module's inlined doc (round 62 phase-2 moved that prose into \
         `super::docs::POSTGRES_MD`; it labels the module \"opt-in feature\" and tells \
         readers to build with `--features postgres`)."
    );

    // A gentler check: the substring "`postgres`" must not appear on
    // the same line as "ship in-box" — that catches rephrasings of
    // the same lie.
    for line in doc.lines() {
        let mentions_postgres = line.contains("`postgres`");
        let claims_in_box = line.contains("ship in-box") || line.contains("ships in-box");
        assert!(
            !(mentions_postgres && claims_in_box),
            "docs/why-silt.md has a line that both names `postgres` and claims it \"ships \
             in-box\": {line:?}. `postgres` is opt-in per Cargo.toml."
        );
    }

    // Positive check: postgres must still be documented somewhere in
    // the file as an opt-in feature, so we don't lose the pointer by
    // over-correcting.
    assert!(
        doc.contains("opt-in") && doc.contains("postgres"),
        "docs/why-silt.md must still mention that `postgres` is available as an opt-in \
         feature, so the existence of the module isn't lost entirely."
    );
}
