//! Round-68 DOC agent locks for `docs/language/traits.md`:
//!
//! - **Count fix.** The supertrait-bounds section previously claimed
//!   "the four built-in traits — `Equal`, `Hash`, `Compare`, `Display`",
//!   but silt actually ships **five** built-in traits (the
//!   `BUILTIN_TRAIT_NAMES` table in `src/typechecker/mod.rs` includes
//!   `Error`). We reword to "four of silt's five built-in traits" so
//!   the count is honest. The Built-in Traits table grew an
//!   "Auto-derived?" column so `Error`'s opt-out status is visible
//!   inline.
//!
//! - **Orphan rule.** `register_trait_impl` rejects an
//!   `impl Trait for Type` when both the trait and the head type are
//!   foreign to the current package (built-ins count as foreign). The
//!   doc had zero coverage of this rule. We add a "Coherence — The
//!   Orphan Rule" section with a positive example (local trait + built-in
//!   type) and a negative example, plus a verbatim quote of the
//!   compiler diagnostic so users searching for the wording land on the
//!   section.
//!
//! Test plan:
//!
//! 1. Source-grep locks: the old phrasing is gone; the new orphan
//!    section header / keyword is present.
//! 2. Walker tests: every code snippet in the new prose is
//!    extracted and run end-to-end via `silt run` / `silt check`. The
//!    negative orphan example asserts the documented diagnostic
//!    appears in stderr.

use std::path::{Path, PathBuf};
use std::process::Command;

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round68_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

/// Set up a real silt package rooted at `dir` (silt.toml + src/main.silt)
/// so `package_setup_for_file` returns the configured package name.
/// The orphan rule is gated on having a real current_package — the
/// scratch / no-package path disables it.
fn write_package(dir: &Path, pkg_name: &str, main_body: &str) -> PathBuf {
    std::fs::write(
        dir.join("silt.toml"),
        format!("[package]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let main = src_dir.join("main.silt");
    std::fs::write(&main, main_body).unwrap();
    main
}

// ────────────────────────────────────────────────────────────────────
// Source-grep locks
// ────────────────────────────────────────────────────────────────────

const TRAITS_MD: &str = include_str!("../docs/language/traits.md");

#[test]
fn count_fix_old_phrasing_removed() {
    // The pre-fix doc said "the four built-in traits". silt has FIVE
    // built-ins (Equal, Compare, Hash, Display, Error per
    // `BUILTIN_TRAIT_NAMES`); only four of them are auto-derived.
    assert!(
        !TRAITS_MD.contains("the four built-in traits"),
        "traits.md must not say `the four built-in traits` — silt has \
         FIVE built-in traits (Equal, Compare, Hash, Display, Error). \
         Reword to count auto-derived ones explicitly."
    );
}

#[test]
fn count_fix_correct_phrasing_present() {
    // The replacement language must acknowledge the five-trait count
    // while still calling out which four are auto-derived.
    assert!(
        TRAITS_MD.contains("four of silt's five built-in traits"),
        "traits.md should phrase the count as `four of silt's five \
         built-in traits` (or similar) so the auto-derive subset and \
         the total count are both honest"
    );
}

#[test]
fn builtin_traits_table_includes_error_row() {
    // The Built-in Traits table previously listed only the four
    // auto-derived traits. `Error` is built-in but not auto-derived —
    // surface that in the table itself with an `Auto-derived?` column.
    assert!(
        TRAITS_MD.contains("| `Error`"),
        "the Built-in Traits table should have an `Error` row"
    );
    assert!(
        TRAITS_MD.contains("Auto-derived?"),
        "the Built-in Traits table should have an `Auto-derived?` \
         column so `Error`'s opt-out status is visible inline"
    );
}

#[test]
fn orphan_rule_section_present() {
    // The orphan rule was undocumented. Lock the section header and
    // the diagnostic-keyword anchor — the whole point of including the
    // exact compiler wording is so users searching their stderr land
    // on this section.
    assert!(
        TRAITS_MD.contains("Orphan Rule") || TRAITS_MD.contains("orphan rule"),
        "traits.md should have an `Orphan Rule` section header"
    );
    assert!(
        TRAITS_MD.contains("orphan impl"),
        "traits.md should quote the compiler's `orphan impl` diagnostic \
         verbatim so users can find this section by searching the error"
    );
    assert!(
        TRAITS_MD.contains("__builtin__"),
        "traits.md should mention `__builtin__` — that's the package \
         stamp users will see in real diagnostics about built-in types"
    );
}

// ────────────────────────────────────────────────────────────────────
// Walker tests — every code snippet must compile + run.
// ────────────────────────────────────────────────────────────────────

/// "Local trait + built-in type" snippet from the Orphan Rule section.
/// Wrapped in a `main` so it's runnable end-to-end. Must succeed under
/// `silt run`.
#[test]
fn orphan_local_trait_for_builtin_type_snippet_runs() {
    let dir = scratch_dir("orphan_local_trait_builtin_type");
    let src = "trait Greet { fn greet(self) -> String }\n\
        \n\
        trait Greet for List(a) {\n\
        \x20\x20fn greet(self) -> String { \"a list\" }\n\
        }\n\
        \n\
        fn main() {\n\
        \x20\x20let xs = [1, 2, 3]\n\
        \x20\x20println(xs.greet())\n\
        }\n";
    let main = write_package(&dir, "myapp", src);

    let output = silt_bin()
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("silt run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "local-trait + built-in-type snippet must run cleanly; \
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("a list"),
        "Greet for List(a) should print `a list`; got stdout:\n{stdout}"
    );
}

/// "Built-in trait + local type" snippet — the type-local arm of the
/// orphan rule. Override the auto-derived `Display` for a local enum
/// and confirm the override actually dispatches.
#[test]
fn orphan_builtin_trait_for_local_type_snippet_runs() {
    let dir = scratch_dir("orphan_builtin_trait_local_type");
    let src = "type Color { Red, Green, Blue }\n\
        \n\
        trait Display for Color {\n\
        \x20\x20fn display(self) -> String { \"a color\" }\n\
        }\n\
        \n\
        fn main() {\n\
        \x20\x20let c: Color = Red\n\
        \x20\x20println(c.display())\n\
        }\n";
    let main = write_package(&dir, "myapp", src);

    let output = silt_bin()
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("silt run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "built-in-trait + local-type snippet must run cleanly; \
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("a color"),
        "user-defined Display for Color should print `a color`; got \
         stdout:\n{stdout}"
    );
}

/// "Local trait + local type" — the trivial both-local case. Must
/// also run cleanly inside a real package (the rule doesn't fire).
#[test]
fn orphan_local_trait_for_local_type_snippet_runs() {
    let dir = scratch_dir("orphan_local_trait_local_type");
    let src = "type Color { Red, Green, Blue }\n\
        trait Greet { fn greet(self) -> String }\n\
        \n\
        trait Greet for Color {\n\
        \x20\x20fn greet(self) -> String { \"color\" }\n\
        }\n\
        \n\
        fn main() {\n\
        \x20\x20let c: Color = Red\n\
        \x20\x20println(c.greet())\n\
        }\n";
    let main = write_package(&dir, "myapp", src);

    let output = silt_bin()
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("silt run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "local-trait + local-type snippet must run cleanly; \
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("color"),
        "Greet for Color should print `color`; got stdout:\n{stdout}"
    );
}

/// Negative example: built-in trait + built-in type from a user
/// package. Must be rejected with the verbatim diagnostic the doc
/// quotes.
#[test]
fn orphan_negative_snippet_rejected_with_documented_message() {
    let dir = scratch_dir("orphan_negative");
    // Mirror the doc's negative example. Wrap in a main so the rest
    // of the program is well-formed; the orphan diagnostic should
    // still fire on the impl block.
    let src = "trait Display for List(a) {\n\
        \x20\x20fn display(self) -> String { \"stolen\" }\n\
        }\n\
        \n\
        fn main() {\n\
        \x20\x20()\n\
        }\n";
    let main = write_package(&dir, "myapp", src);

    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "user-package `Display for List(a)` must be rejected as an \
         orphan impl; got success.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Lock the structural pieces of the documented diagnostic. We
    // assert each fragment individually so a benign reformatting (line
    // wrapping, quoting style) doesn't break the test, but the user-
    // visible substrings the doc quotes must all appear.
    for needle in [
        "orphan impl",
        "trait 'Display'",
        "type 'List'",
        "__builtin__",
        "current package 'myapp'",
    ] {
        assert!(
            stderr.contains(needle),
            "orphan diagnostic missing `{needle}`; doc quotes this \
             phrasing verbatim. Full stderr:\n{stderr}"
        );
    }
}

/// Stand-alone-script enforcement: the doc calls out that the CLI
/// enforces the orphan rule even on stand-alone scripts (the synthetic
/// `__local__` package). Lock that the same negative impl, run from a
/// directory with no `silt.toml`, also fires the orphan diagnostic —
/// just with `__local__` instead of a real package name.
#[test]
fn orphan_fires_for_standalone_script_under_local_package() {
    let dir = scratch_dir("orphan_standalone");
    let src = "trait Display for List(a) {\n\
        \x20\x20fn display(self) -> String { \"x\" }\n\
        }\n\
        \n\
        fn main() {\n\
        \x20\x20()\n\
        }\n";
    // No silt.toml: package_setup_for_file falls back to the
    // anonymous `__local__` package. The rule still fires because
    // current_package is `Some(__local__)`, not `None`.
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "stand-alone-script `Display for List(a)` must still be \
         rejected under the synthetic __local__ package; got success.\n\
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("orphan impl"),
        "stand-alone-script orphan must surface the same diagnostic \
         keyword; got stderr:\n{stderr}"
    );
    // The CLI's stand-alone-script path uses `__local__` as the
    // package name — lock the doc's parenthetical claim.
    assert!(
        stderr.contains("__local__"),
        "stand-alone-script orphan should mention the `__local__` \
         package name (the doc says the CLI uses this synthetic name); \
         got stderr:\n{stderr}"
    );
}
