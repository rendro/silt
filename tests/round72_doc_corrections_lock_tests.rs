//! Round-72 doc-correction locks.
//!
//! These pin the round-72 audit's BROKEN/GAP findings against
//! `docs/language/*.md` and `docs/language-guide.md` so the
//! corrections can't silently regress.
//!
//! - G3: `docs/language-guide.md` claimed Generics covered
//!   "higher-kinded types", but `docs/language/generics.md`
//!   explicitly lists "No higher-kinded types" as a non-feature.
//!   The HKT mention was dropped from the index entry.
//!
//! - G4: `docs/language/generics.md` previously claimed that
//!   `impl Convertible(Int) for String` and `impl Convertible(Float)
//!   for String` were distinct impls. The typechecker rejects this
//!   because the impl key ignores trait params and treats the two
//!   as duplicates — the sibling
//!   `docs/proposals/error-from-trait.md` documents the same
//!   limitation. The prose was rewritten to call this out as a
//!   deferred limitation.
//!
//! - L10: A targeted-pattern walker that scans every `silt`-fenced
//!   block in `docs/**/*.md` (plus `README.md`) for the specific
//!   identifier-shape bugs round-72 caught in `docs/language/`:
//!   `string.upper`, `string.from_utf8`, `where ... : Ord`-as-trait,
//!   `Result(_, Error)` (Error is a trait, not a type), and
//!   `where ... : Into(...)` / `where ... : TryInto(...)` (neither
//!   is a registered trait). These are the structural cause of why
//!   B5-B9 survived the existing `all_doc_fn_main_blocks_compile`
//!   walker — that walker only inspects fences containing
//!   `fn main`, so snippet-style fences slipped through. This
//!   walker covers fences regardless of `fn main` presence and
//!   reports `path:fence_open_line` on any hit.

use std::path::{Path, PathBuf};

const LANG_GUIDE_MD: &str = include_str!("../docs/language-guide.md");
const GENERICS_MD: &str = include_str!("../docs/language/generics.md");

// ────────────────────────────────────────────────────────────────────
// G3: language-guide.md must not advertise higher-kinded types
// ────────────────────────────────────────────────────────────────────

#[test]
fn language_guide_does_not_advertise_higher_kinded_types() {
    // Case-insensitive sentinel — guards against capitalisation
    // variants ("Higher-Kinded", "higher-kinded", "HKT") sneaking
    // back in via a future edit.
    let lower = LANG_GUIDE_MD.to_ascii_lowercase();
    assert!(
        !lower.contains("higher-kinded"),
        "docs/language-guide.md must not advertise `higher-kinded` \
         coverage in the Generics index entry — \
         `docs/language/generics.md` explicitly lists 'No \
         higher-kinded types' as a non-feature. Strike the phrase \
         from the language-guide.md bullet for [Generics]."
    );
}

// ────────────────────────────────────────────────────────────────────
// G4: generics.md must call the multi-arg `Convertible` impl shape a
// deferred limitation, not a working pattern
// ────────────────────────────────────────────────────────────────────

#[test]
fn generics_md_calls_multi_arg_convertible_a_deferred_limitation() {
    assert!(
        GENERICS_MD.contains("deferred limitation"),
        "docs/language/generics.md must describe the
         `impl Convertible(Int) for String` / `impl
         Convertible(Float) for String` collision as a 'deferred
         limitation' — the impl key ignores trait params today, so
         the second impl overwrites the first. Reference
         `docs/proposals/error-from-trait.md` for the analysis."
    );
    // Anchor on the proposals link too so a future edit that
    // removes the proposals reference is also caught.
    assert!(
        GENERICS_MD.contains("proposals/error-from-trait.md"),
        "docs/language/generics.md must point readers at \
         `docs/proposals/error-from-trait.md` for the full \
         write-up of the trait-arg-keyed impl deferral."
    );
}

// ────────────────────────────────────────────────────────────────────
// L10: targeted-pattern walker — scan every `silt` fence in
// docs/**/*.md and README.md for the round-72 identifier-shape
// regressions.
// ────────────────────────────────────────────────────────────────────

/// One forbidden pattern. The walker uses raw substring matches and
/// reports `name` plus a short `why` so the failure pinpoints the
/// drift class.
struct ForbiddenPattern {
    name: &'static str,
    needles: &'static [&'static str],
    why: &'static str,
}

/// The drift patterns. Substring matches; tighten by including
/// surrounding punctuation so `String.parse` doesn't false-positive
/// on `string.parse` etc.
const FORBIDDEN: &[ForbiddenPattern] = &[
    ForbiddenPattern {
        name: "string.upper",
        // Include the `.` and trailing word boundary via the close-
        // paren / space / pipe so `string.upper_case` (hypothetical)
        // would not match. Real builtin is `string.to_upper`.
        needles: &["string.upper ", "string.upper(", "string.upper}", "string.upper\n"],
        why: "real builtin is `string.to_upper` (see \
              `src/typechecker/builtins/string.rs:103`)",
    },
    ForbiddenPattern {
        name: "string.lower",
        // Symmetric guard: `string.lower` is also not a builtin —
        // the real one is `string.to_lower`.
        needles: &["string.lower ", "string.lower(", "string.lower}", "string.lower\n"],
        why: "real builtin is `string.to_lower` (see \
              `src/typechecker/builtins/string.rs:109`)",
    },
    ForbiddenPattern {
        name: "string.from_utf8",
        needles: &["string.from_utf8"],
        why: "no such builtin; the bytes-to-string conversion is \
              `bytes.to_string` (see \
              `src/typechecker/builtins/bytes.rs:31`)",
    },
    ForbiddenPattern {
        name: "where-clause `Ord` constraint",
        needles: &[": Ord ", ": Ord\n", ": Ord,", ": Ord+", ": Ord +"],
        why: "the comparison trait is `Compare`, not `Ord` (see \
              `src/typechecker/mod.rs::BUILTIN_TRAIT_NAMES`)",
    },
    ForbiddenPattern {
        name: "where-clause `Into(...)` constraint",
        needles: &[": Into(", "+ Into(", ", a: Into(", ", b: Into(", ", k: Into(", ", v: Into("],
        why: "`Into` is not a registered trait; define a domain \
              trait (see the `Convert(b)` example in generics.md)",
    },
    ForbiddenPattern {
        name: "where-clause `TryInto(...)` constraint",
        needles: &[": TryInto(", "+ TryInto("],
        why: "`TryInto` is not a registered trait; define a domain \
              trait that returns `Result(b, e)` for whatever error \
              type the caller wants",
    },
    ForbiddenPattern {
        name: "`Result(_, Error)` (Error as a type)",
        // `Error` is a trait, not a type. Real return shapes use
        // domain-specific record/enum error types like `ParseError`,
        // `HttpError`, `IoError`.
        needles: &[", Error)", ",Error)"],
        why: "`Error` is a trait, not a type — use a concrete error \
              record/enum (`ParseError`, `HttpError`, `IoError`, …)",
    },
];

/// Some fences legitimately reference these patterns inside prose-
/// shaped commentary that's still fenced as `silt`. Allow on a
/// per-fence basis by matching a sentinel substring inside the fence
/// body. The sentinel must be specific enough not to collide with
/// real code.
fn fence_is_intentionally_shown_as_broken(body: &str) -> bool {
    body.contains("-- ERROR")
        || body.contains("-- error:")
        || body.contains("-- error")
        || body.contains("-- Incorrect")
        || body.contains("won't parse")
        || body.contains("won't compile")
        || body.contains("compile error")
        || body.contains("// noexec")
        || body.contains("-- noexec")
}

#[test]
fn doc_silt_fences_do_not_contain_round72_drift_patterns() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    let mut targets: Vec<PathBuf> = Vec::new();
    let readme = manifest_dir.join("README.md");
    if readme.is_file() {
        targets.push(readme);
    }
    fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                // Proposals discuss hypothetical trait shapes — skip.
                if p.file_name().and_then(|s| s.to_str()) == Some("proposals") {
                    continue;
                }
                collect_md(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("md") {
                out.push(p);
            }
        }
    }
    collect_md(&manifest_dir.join("docs"), &mut targets);
    targets.sort();
    assert!(!targets.is_empty(), "no doc targets discovered");

    let mut violations: Vec<String> = Vec::new();
    let mut scanned_fences = 0usize;

    for path in &targets {
        let body = std::fs::read_to_string(path).expect("read doc");
        let mut in_fence = false;
        let mut fence_open_line = 0usize;
        let mut fence_body = String::new();

        for (i, line) in body.lines().enumerate() {
            let lineno = i + 1;
            if !in_fence && line.trim_start().starts_with("```silt") {
                in_fence = true;
                fence_open_line = lineno;
                fence_body.clear();
                continue;
            }
            if in_fence && line.trim_start().starts_with("```") {
                scanned_fences += 1;
                if !fence_is_intentionally_shown_as_broken(&fence_body) {
                    for pat in FORBIDDEN {
                        for needle in pat.needles {
                            if fence_body.contains(needle) {
                                violations.push(format!(
                                    "{}:{} (```silt fence): contains \
                                     `{}` ({}). {}",
                                    path.display(),
                                    fence_open_line,
                                    needle.escape_default(),
                                    pat.name,
                                    pat.why
                                ));
                                // Report once per pattern per fence,
                                // not per needle, to keep failure
                                // output readable.
                                break;
                            }
                        }
                    }
                }
                in_fence = false;
                fence_body.clear();
                continue;
            }
            if in_fence {
                fence_body.push_str(line);
                fence_body.push('\n');
            }
        }
    }

    assert!(
        scanned_fences > 0,
        "the round-72 walker scanned zero ```silt fences — did the \
         doc layout change?"
    );

    assert!(
        violations.is_empty(),
        "{} round-72 drift pattern(s) found in doc ```silt fences. \
         These shapes correspond to identifier-form bugs the \
         existing `all_doc_fn_main_blocks_compile` walker missed \
         because it skips fences that lack `fn main`. Fix each \
         hit per the noted real builtin or trait name:\n\n{}",
        violations.len(),
        violations.join("\n")
    );
}
