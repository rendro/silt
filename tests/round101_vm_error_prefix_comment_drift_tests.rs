//! Round-101 doc-drift lock: five comments in the CLI/REPL layers claimed
//! (in PRESENT tense) that `VmError::Display` leaks a bare legacy
//! "VM error:" prefix to users. That leak was fixed when the Display was
//! canonicalized to the `error[runtime]:` header (src/vm/error.rs, whose
//! own comment correctly says "previously"). The funneling through
//! `SourceError::runtime_at` / `render_runtime_error_without_source` is
//! still wanted — it adds the file-path locator and ANSI color gating a
//! bare `VmError` Display deliberately does not provide — but the
//! prefix-leak rationale is historical.
//!
//! Lock: no mention of the legacy prefix string may reappear in these
//! three files at all. The current comments describe the live rationale
//! (file path + color gating) and reference the old prefix only
//! generically ("legacy internal Display prefix"), so any reappearance of
//! the literal string means either the stale present-tense claim came
//! back or a new "VM error:"-prefixed message is being emitted from the
//! UI layers — both are regressions. (`internal VM error:` invariant
//! messages live in src/vm/ and src/builtins/, never in these files.)

const CLI_RUN: &str = include_str!("../src/cli/run.rs");
const CLI_TEST: &str = include_str!("../src/cli/test.rs");
const REPL: &str = include_str!("../src/repl.rs");

#[test]
fn no_stale_vm_error_prefix_claims_outside_vm_error_rs() {
    for (name, src) in [
        ("src/cli/run.rs", CLI_RUN),
        ("src/cli/test.rs", CLI_TEST),
        ("src/repl.rs", REPL),
    ] {
        assert!(
            !src.contains("VM error:"),
            "{name} mentions the legacy \"VM error:\" Display prefix. \
             `VmError::Display` was canonicalized to `error[runtime]:` \
             (src/vm/error.rs), so a comment describing the old leak in \
             present tense is drift — describe the current rationale \
             (file-path locator + ANSI color gating) instead, and refer \
             to the old prefix only generically/in past tense. If this \
             is a new runtime string, do not emit \"VM error:\" from the \
             CLI/REPL layers."
        );
    }
}
