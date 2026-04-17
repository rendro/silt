//! Light-weight textual scans over silt source that drive diagnostic
//! routing decisions in the CLI: "does this file define main()?",
//! "does it look like a test file?", etc. These are intentionally
//! line-based and conservative — a false negative here just means a
//! slightly less helpful diagnostic, whereas a false positive could
//! suppress a needed error.

/// Return true if `e` is the "program has no main function" runtime error.
///
/// AUDIT-NOTE: this hint is keyed on a stringly-typed error; a proper fix
/// would introduce a typed error variant. Tests pinning this live in
/// tests/cli.rs. The matcher is intentionally more permissive than a single
/// exact-string compare so a future cosmetic tweak to the producing
/// `format!` in src/vm/execute.rs doesn't silently break the "silt test"
/// nudge.
pub(crate) fn is_missing_main_error(e: &silt::vm::VmError) -> bool {
    let msg = &e.message;
    msg.starts_with("undefined global: ") && msg.contains("main")
}

/// Heuristic: does this source look like a test-only file?
///
/// Returns true if the source defines any `fn test_...` function OR contains
/// a top-level `test.` call (e.g. `test.assert_eq(...)`). Used by `silt run`
/// to suggest `silt test` when there's no `main()`.
///
/// Conservative: we scan whole lines that start (after trimming whitespace)
/// with `fn test_`, `fn skip_test_`, or `test.` so commented-out code and
/// string literals containing those substrings don't trigger a false positive.
pub(crate) fn looks_like_test_file(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim_start();
        if t.starts_with("fn test_")
            || t.starts_with("fn skip_test_")
            || t.starts_with("pub fn test_")
            || t.starts_with("pub fn skip_test_")
            || t.starts_with("test.")
        {
            return true;
        }
    }
    false
}

/// Conservative text scan: does `source` look like a library module
/// (has at least one `pub fn ...` definition)?  Used by `silt check` to
/// suppress the missing-main diagnostic on files that are intended to
/// be imported rather than run directly.
pub(crate) fn looks_like_library_module(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim_start();
        if t.starts_with("pub fn ") {
            return true;
        }
    }
    false
}

/// Conservative text scan for whether `source` defines a top-level `main`
/// function. We match lines whose trimmed prefix is `fn main(` / `fn main `
/// / `fn main{` or the `pub fn` variants. Must be conservative — a false
/// positive here would suppress the missing-main diagnostic for a program
/// that actually needs it.
pub(crate) fn program_has_main(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim_start();
        let rest = if let Some(r) = t.strip_prefix("pub fn ") {
            r
        } else if let Some(r) = t.strip_prefix("fn ") {
            r
        } else {
            continue;
        };
        // Match `main` followed by a non-identifier character.
        if let Some(after) = rest.strip_prefix("main") {
            match after.chars().next() {
                Some(c) if !(c.is_alphanumeric() || c == '_') => return true,
                None => return true,
                _ => {}
            }
        }
    }
    false
}
