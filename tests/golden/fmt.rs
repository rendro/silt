//! `-- cmd: fmt` cases: what `silt fmt` writes.
//!
//! The harness has run `silt fmt <file>` in the case's copy. A case
//! that expects exit status 0 has a `<case>.formatted` file (`case.formatted`
//! for a directory case): the copy must now equal it, and a second
//! `silt fmt` must leave it as it is. A case that expects another
//! status is one `silt fmt` rejects or refuses: the copy must be
//! unchanged.

use std::path::Path;

use super::{Case, Output};

/// Whether `case` is a `-- cmd: fmt` case: the command alone, without
/// `--check` or a path of its own.
pub(crate) fn is_fmt_case(case: &Case) -> bool {
    // `--next` chooses the printer of `src/format/` until it is the
    // only one (stage 8 step A3).
    matches!(
        case.directives
            .cmd
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["fmt"] | ["fmt", "--next"]
    ) && !case.file.is_empty()
}

/// What is wrong with the file `silt fmt` left in `dir`, if anything.
/// `first` is the output of the run; `again` runs `silt fmt` once more.
pub(crate) fn check(
    case: &Case,
    dir: &Path,
    first: &Output,
    again: &dyn Fn() -> Output,
) -> Option<String> {
    // The exit status and a timeout are judged with the rest of the
    // output; here only a run that ended as the case expects is looked at.
    if first.timed_out || first.code != Some(case.directives.exit) {
        return None;
    }
    let file = dir.join(&case.file);
    let read = |what: &str| {
        std::fs::read_to_string(&file).map_err(|e| format!("cannot read the {what} file: {e}"))
    };
    let original = match std::fs::read_to_string(&case.source_path) {
        Ok(text) => text,
        Err(e) => return Some(format!("cannot read the case: {e}")),
    };
    let formatted = match read("formatted") {
        Ok(text) => text,
        Err(e) => return Some(e),
    };
    if case.directives.exit != 0 {
        return (formatted != original).then(|| {
            format!("`silt fmt` failed and changed the file all the same:\n{formatted}---")
        });
    }

    let expected_path = case.expected_base.with_extension("formatted");
    if !expected_path.is_file() {
        return Some(format!(
            "a `-- cmd: fmt` case needs {}; `silt fmt` wrote:\n{formatted}---",
            expected_path.display()
        ));
    }
    if std::env::var_os("SILT_BLESS").is_some() {
        std::fs::write(&expected_path, &formatted).expect("write blessed formatted file");
    } else {
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_default();
        if expected != formatted {
            return Some(format!(
                "the formatted file differs from {}:\n--- expected\n{expected}--- actual\n{formatted}---",
                expected_path.display()
            ));
        }
    }

    let second = again();
    if second.timed_out || second.code != Some(0) {
        return Some(format!(
            "a second `silt fmt` failed (exit status {:?}):\n{}",
            second.code, second.stderr
        ));
    }
    match read("twice formatted") {
        Ok(twice) if twice == formatted => None,
        Ok(twice) => Some(format!(
            "a second `silt fmt` changed the file:\n--- first\n{formatted}--- second\n{twice}---"
        )),
        Err(e) => Some(e),
    }
}
