//! Round-77 DOC parity locks for `docs/concurrency.md`.
//!
//! Two findings:
//!
//! - **DOC-3 (GAP)**: `task.deadline` and `task.spawn_until` are public
//!   builtins. Before round 77 they were only mentioned inline at
//!   `docs/concurrency.md:947-949` (one cross-reference) plus the LSP
//!   docs at `src/typechecker/builtins/docs.rs:794-820`. They now have
//!   dedicated sections in the Tasks part of the concurrency guide,
//!   mirroring the style of `task.spawn` / `task.join` / `task.cancel`.
//!   This file pins the section headers and the typed-timeout shape
//!   wording.
//!
//! - **DOC-L2 (LATENT)**: round 76 added a one-direction lock on the
//!   queued-cancel case in `docs/concurrency.md` and round 75 added a
//!   sibling lock on the builtin doc. Neither cross-checked alignment.
//!   This file adds the parity assertion: both surfaces must contain
//!   the same key phrases for the queued-cancel third case
//!   (`first-writer-wins` and `queued but not yet running`). If either
//!   side drifts, the test fires.

const CONCURRENCY_DOC_SRC: &str = include_str!("../docs/concurrency.md");
const BUILTIN_DOCS_SRC: &str = include_str!("../src/typechecker/builtins/docs.rs");

// ────────────────────────────────────────────────────────────────────
// DOC-3: dedicated sections for task.deadline / task.spawn_until
// ────────────────────────────────────────────────────────────────────

#[test]
fn doc3_concurrency_doc_has_task_deadline_section() {
    // Section header lives in the Tasks part of the guide. The header
    // is `### Scoped deadlines: \`task.deadline(...)\`` so a regression
    // that drops the section (or downgrades it back to an inline
    // mention) is caught.
    assert!(
        CONCURRENCY_DOC_SRC.contains("### Scoped deadlines: `task.deadline(dur, fn)`"),
        "concurrency.md must have a dedicated `### Scoped deadlines: \
         `task.deadline(...)`` section, mirroring task.spawn/join/cancel."
    );
    // The section must have its own runnable snippet (```silt fence
    // with the canonical io.read_file form).
    assert!(
        CONCURRENCY_DOC_SRC.contains("task.deadline(time.ms(200), fn() {"),
        "task.deadline section should include the canonical \
         `task.deadline(time.ms(200), fn() {{ io.read_file(...) }})` snippet."
    );
}

#[test]
fn doc3_concurrency_doc_has_task_spawn_until_section() {
    assert!(
        CONCURRENCY_DOC_SRC.contains("### Bounded spawn: `task.spawn_until(dur, fn)`"),
        "concurrency.md must have a dedicated `### Bounded spawn: \
         `task.spawn_until(...)`` section, mirroring task.spawn/join/cancel."
    );
    assert!(
        CONCURRENCY_DOC_SRC.contains("task.spawn_until(time.seconds(2), fn() {"),
        "task.spawn_until section should include the canonical \
         `task.spawn_until(time.seconds(2), fn() {{ io.read_file(...) }})` snippet."
    );
}

#[test]
fn doc3_task_deadline_section_names_typed_timeout_shapes() {
    // Locate the start of the task.deadline section and the start of
    // the next section (task.spawn_until). Restrict the typed-timeout
    // assertions to that range so a regression that strips the shapes
    // from the deadline section (but leaves them elsewhere) is caught.
    let start = CONCURRENCY_DOC_SRC
        .find("### Scoped deadlines: `task.deadline(dur, fn)`")
        .expect("task.deadline section must exist");
    let end = CONCURRENCY_DOC_SRC[start..]
        .find("### Bounded spawn:")
        .expect("task.spawn_until section must follow task.deadline");
    let section = &CONCURRENCY_DOC_SRC[start..start + end];

    assert!(
        section.contains("IoUnknown"),
        "task.deadline section should name `IoUnknown` as the typed timeout variant."
    );
    assert!(
        section.contains("I/O timeout (task.deadline exceeded)"),
        "task.deadline section should include the canonical \
         `I/O timeout (task.deadline exceeded)` message."
    );
    assert!(
        section.contains("TcpTimeout"),
        "task.deadline section should name `TcpTimeout` as the tcp timeout variant."
    );
    assert!(
        section.contains("HttpTimeout"),
        "task.deadline section should name `HttpTimeout` as the http timeout variant."
    );
}

#[test]
fn doc3_task_spawn_until_section_names_typed_timeout_shapes() {
    let start = CONCURRENCY_DOC_SRC
        .find("### Bounded spawn: `task.spawn_until(dur, fn)`")
        .expect("task.spawn_until section must exist");
    // The next section is `## 4. Select` (top-level h2 starts after).
    let end = CONCURRENCY_DOC_SRC[start..]
        .find("## 4. Select")
        .expect("task.spawn_until section must precede the Select chapter");
    let section = &CONCURRENCY_DOC_SRC[start..start + end];

    assert!(
        section.contains("IoUnknown"),
        "task.spawn_until section should name `IoUnknown` as the typed timeout variant."
    );
    assert!(
        section.contains("I/O timeout (task.deadline exceeded)"),
        "task.spawn_until section should include the canonical \
         `I/O timeout (task.deadline exceeded)` message — \
         spawn_until inherits the same shape as task.deadline."
    );
    assert!(
        section.contains("TcpTimeout"),
        "task.spawn_until section should name `TcpTimeout`."
    );
    assert!(
        section.contains("HttpTimeout"),
        "task.spawn_until section should name `HttpTimeout`."
    );
}

// ────────────────────────────────────────────────────────────────────
// DOC-L2: queued-cancel parity between concurrency.md and the builtin
// docs at src/typechecker/builtins/docs.rs.
// ────────────────────────────────────────────────────────────────────

#[test]
fn docl2_queued_cancel_first_writer_wins_parity() {
    // Both surfaces describe the queued-but-not-yet-running case for
    // task.cancel using "first-writer-wins" semantics. If either side
    // drifts away from this phrase, the lock fires.
    assert!(
        CONCURRENCY_DOC_SRC.contains("first-writer-wins"),
        "docs/concurrency.md must mention `first-writer-wins` for \
         task.cancel — round 76 enumerated the third (queued) case using \
         this phrase."
    );
    assert!(
        BUILTIN_DOCS_SRC.contains("first-writer-wins"),
        "src/typechecker/builtins/docs.rs must mention `first-writer-wins` \
         for task.cancel — round 75 added this lock on the builtin side."
    );
}

#[test]
fn docl2_queued_cancel_third_case_phrase_parity() {
    // Both surfaces describe the third bullet (task spawned but not
    // yet picked up by the scheduler) using the phrase
    // `queued but not yet running`. If either side drifts (e.g. the
    // builtin docs reword to "queued but not yet scheduled" without
    // updating the guide, or vice versa), the lock fires.
    assert!(
        CONCURRENCY_DOC_SRC.contains("queued but not yet running"),
        "docs/concurrency.md must use the canonical phrase \
         `queued but not yet running` for the third task.cancel case."
    );
    assert!(
        BUILTIN_DOCS_SRC.contains("queued but not yet running"),
        "src/typechecker/builtins/docs.rs must use the canonical phrase \
         `queued but not yet running` for the third task.cancel case — \
         this is the parity lock between the two surfaces (DOC-L2)."
    );
}

#[test]
fn docl2_queued_cancel_run_a_slice_parity() {
    // Both surfaces use the phrase "run a slice" (the guide says
    // "may still run a slice", the builtin docs say "may run a slice").
    // The shared substring "run a slice" pins both: a regression that
    // reworded either side to e.g. "execute briefly" would fire.
    assert!(
        CONCURRENCY_DOC_SRC.contains("run a slice"),
        "docs/concurrency.md must describe the queued-cancel case as \
         `run a slice` (shared phrasing with the builtin docs)."
    );
    assert!(
        BUILTIN_DOCS_SRC.contains("run a slice"),
        "src/typechecker/builtins/docs.rs must describe the queued-cancel \
         case as `run a slice` (shared phrasing with the concurrency guide)."
    );
}
