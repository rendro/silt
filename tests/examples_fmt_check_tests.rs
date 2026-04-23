//! Regression lock: every bundled `examples/**/*.silt` file must be in
//! canonical `silt fmt` form.
//!
//! Round-52 audit GAP: `silt fmt --check examples/*.silt` rejected two
//! bundled examples (`concurrent.silt` and `cross_module_errors.silt`)
//! because the formatter rewrites `when let Pat(x) = expr else { return }`
//! one-liners onto multi-line blocks and splits `a(x) |> b(y)?` chains
//! across lines. Neither file had a test locking the invariant "every
//! example is already canonical", so a first-time user running
//! `silt fmt` against the shipped examples would see a surprise diff.
//!
//! The convergent silt design preference is "one way to do things":
//! every shipped example should already look exactly the way the
//! formatter would print it. This walker enforces that by iterating
//! every `.silt` file under `examples/` and asserting that formatting
//! the file's current contents in-process returns exactly the same
//! bytes. It uses `silt::format` (the library entry point — see
//! `src/formatter.rs`) rather than shelling to the CLI, so the test is
//! fast and fails clearly naming the offending file.
//!
//! Fixing a failure is straightforward: run
//! `silt fmt examples/<offender>.silt` (or
//! `silt fmt --check examples/*.silt` to discover all drifting files)
//! to bring the file back into canonical form.

use std::path::{Path, PathBuf};

/// Recursively collect every `.silt` file under `dir`. Mirrors the
/// walker in `tests/examples_check.rs` so both lock-in tests cover the
/// exact same universe of files.
fn collect_silt_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_silt_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("silt") {
            out.push(path);
        }
    }
}

/// For every shipped `examples/**/*.silt`, assert that running the
/// formatter over its current contents is a no-op (byte-for-byte
/// identical). A failure here means someone edited an example by hand
/// without running `silt fmt` on it — fix by re-running the formatter
/// on the offending file(s).
#[test]
fn every_example_is_canonical_formatted() {
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    assert!(
        examples_dir.is_dir(),
        "expected examples directory at {}",
        examples_dir.display()
    );

    let mut files = Vec::new();
    collect_silt_files(&examples_dir, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "expected at least one example .silt file under {}",
        examples_dir.display()
    );

    let mut failures: Vec<String> = Vec::new();

    for file in &files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: failed to read: {}", file.display(), e));
                continue;
            }
        };

        // Reset the interner between files so one example's interned
        // strings can't leak into another's formatter state.
        silt::intern::reset();

        let formatted = match silt::formatter::format(&source) {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!(
                    "{}: formatter returned an error — the example must \
                     be parseable for the canonical-formatting invariant \
                     to apply. Error: {}",
                    file.display(),
                    e
                ));
                continue;
            }
        };

        if formatted != source {
            // Produce a minimal hint at the first differing line so the
            // failure message points somewhere actionable without
            // dumping the whole file.
            let first_diff_line = source
                .lines()
                .zip(formatted.lines())
                .enumerate()
                .find(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| {
                    format!(
                        "first diff at line {}:\n  on-disk:   {:?}\n  formatted: {:?}",
                        i + 1,
                        a,
                        b
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "length differs: on-disk {} bytes, formatted {} bytes",
                        source.len(),
                        formatted.len()
                    )
                });
            failures.push(format!(
                "{}: file is not in canonical `silt fmt` form. Run \
                 `silt fmt {}` to fix.\n{}",
                file.display(),
                file.display(),
                first_diff_line
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} example file(s) are not in canonical `silt fmt` form. The \
         silt design preference is \"one way to do things\" — every \
         shipped example must already match what the formatter prints, \
         so a first-time user running `silt fmt` on an example never \
         sees a surprise diff. Fix by running `silt fmt <file>` on each \
         offender:\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}
