//! Round-77 GAP fix DOC-1: `docs/language/modules.md` lists `postgres`
//! as a built-in module, but the released binary builds with default
//! features only and `postgres` is **not** in
//! `Cargo.toml`'s default feature set. Before this fix, the docs gave
//! no hint of the gating, so a reader could write
//! `import postgres; postgres.connect(...)` — `silt check` happily
//! typechecks it (BUILTIN_MODULES has `postgres` unconditionally at
//! typecheck time), only for `silt run` to fail with
//! `unknown builtin namespace: postgres`. The only previous mention of
//! the opt-in lived in `docs/why-silt.md`.
//!
//! Round 77 annotates the `postgres` row in the built-in modules table
//! with a feature-gating note. This test pins:
//!   1. The `postgres` row in `docs/language/modules.md` mentions the
//!      `postgres` Cargo feature (so the gating is visible in the
//!      table itself, not buried elsewhere).
//!   2. `Cargo.toml`'s `default = [...]` line genuinely does **not**
//!      include `postgres` — if that ever changes, the doc gating note
//!      becomes a lie and this test fires so the doc gets updated.
//!
//! It deliberately does not subsume
//! `tests/round74_modules_doc_lists_all_builtins_tests.rs`, which locks
//! the *presence* of every `BUILTIN_MODULES` entry in the listing; this
//! test locks the *gating annotation* on the `postgres` row separately.

const MODULES_DOC_SRC: &str = include_str!("../docs/language/modules.md");
const CARGO_TOML_SRC: &str = include_str!("../Cargo.toml");

#[test]
fn postgres_row_flags_the_postgres_feature_gate() {
    // Locate the `| `postgres` |` row in the built-in modules table and
    // assert its description mentions the `postgres` Cargo feature so
    // readers immediately know the default release binary won't have it.
    let row_start = MODULES_DOC_SRC
        .find("| `postgres` |")
        .expect("docs/language/modules.md should contain the `postgres` built-in module row");
    let row_end = MODULES_DOC_SRC[row_start..]
        .find('\n')
        .map(|off| row_start + off)
        .unwrap_or(MODULES_DOC_SRC.len());
    let row = &MODULES_DOC_SRC[row_start..row_end];

    assert!(
        row.contains("feature"),
        "the `postgres` row in docs/language/modules.md must mention that it is feature-gated; \
         got row: {row:?}"
    );
    assert!(
        row.contains("--features postgres"),
        "the `postgres` row in docs/language/modules.md must show the cargo invocation \
         (`--features postgres`) that enables the module; got row: {row:?}"
    );
}

#[test]
fn cargo_default_features_do_not_include_postgres() {
    // Find the `default = [...]` line in `[features]`. If a future
    // change adds `postgres` to defaults, the gating note in the docs
    // becomes incorrect — fail loudly so the doc gets updated in step.
    let default_line = CARGO_TOML_SRC
        .lines()
        .find(|line| line.trim_start().starts_with("default = ["))
        .expect("Cargo.toml should declare a `default = [...]` features list");

    assert!(
        !default_line.contains("\"postgres\""),
        "Cargo.toml `default` features must not include `postgres` — \
         the docs in `docs/language/modules.md` advertise `postgres` as opt-in. \
         If you intentionally enable it by default, update the `postgres` row in \
         `docs/language/modules.md` to remove the gating note. Got: {default_line:?}"
    );
}
