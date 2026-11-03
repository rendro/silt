//! Round-72 audit regression: runtime subcommands that take at most
//! one file (`silt run`, `silt check`, `silt disasm`) must reject extra
//! positional arguments with a clear error rather than silently
//! dropping them (run/disasm) or last-wins overwriting them (check).
//!
//! Pre-fix:
//!   - `silt run a.silt b.silt` — silently ran only `a.silt`.
//!   - `silt check a.silt b.silt` — silently checked only `b.silt`
//!     (last-wins).
//!   - `silt disasm a.silt b.silt` — silently disassembled only
//!     `a.silt`.
//!
//! The rejection cases themselves are golden cases under
//! `tests/golden/cli/positional/`; this file keeps the `--` separator
//! cases, whose program arguments must follow the file.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn fresh_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_extra_pos_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a trivial main-bearing .silt file at `path`.
fn write_trivial(path: &std::path::Path) {
    fs::write(path, "fn main() { print(\"hi\") }\n").unwrap();
}

/// Round-74 follow-up: round-72 over-fired by rejecting EVERY non-`.silt`
/// positional after the script, which made program args unreachable.
/// Post-fix, `--` switches the parser into "forward to program" mode and
/// extras after `--` succeed (and reach `io.args()`).
#[test]
fn silt_run_accepts_extras_after_double_dash() {
    let dir = fresh_dir("run_dd");
    let a = dir.join("a.silt");
    write_trivial(&a);

    let out = silt_cmd()
        .args(["run", a.to_str().unwrap(), "--", "extra1", "extra2"])
        .output()
        .expect("failed to run silt");

    assert!(
        out.status.success(),
        "silt run a.silt -- extra1 extra2 must succeed; \
         exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// `silt check` runs no program, so `--` and what follows it are
/// arguments too many: refused, with exit status 1.
#[test]
fn silt_check_refuses_a_double_dash() {
    let dir = fresh_dir("check_dd");
    let a = dir.join("a.silt");
    write_trivial(&a);

    let out = silt_cmd()
        .args(["check", a.to_str().unwrap(), "--", "extra1", "extra2"])
        .output()
        .expect("failed to run silt");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("unexpected extra argument '--'")
            && !stderr.contains("meant for the program"),
        "stderr={stderr}"
    );
}

/// `silt disasm` runs no program, so `--` and what follows it are
/// arguments too many: refused, with exit status 1.
#[test]
fn silt_disasm_refuses_a_double_dash() {
    let dir = fresh_dir("disasm_dd");
    let a = dir.join("a.silt");
    write_trivial(&a);

    let out = silt_cmd()
        .args(["disasm", a.to_str().unwrap(), "--", "extra1", "extra2"])
        .output()
        .expect("failed to run silt");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("unexpected extra argument '--'")
            && !stderr.contains("meant for the program"),
        "stderr={stderr}"
    );
}

#[test]
fn silt_fmt_help_runtime_emits_directory_expansion_blurb() {
    // Belt-and-suspenders: the actual `silt fmt --help` invocation
    // must surface the new wording, not just the source.
    let out = silt_cmd()
        .args(["fmt", "--help"])
        .output()
        .expect("failed to run silt fmt --help");
    assert!(
        out.status.success(),
        "silt fmt --help must exit 0; got {:?}",
        out.status.code()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("recursively"),
        "fmt --help stdout must mention recursive expansion; got: {stdout}"
    );
    assert!(
        stdout.contains("[files-or-dirs...]"),
        "fmt --help stdout must advertise dirs in addition to files; got: {stdout}"
    );
    assert!(
        stdout.contains("`.`"),
        "fmt --help stdout must call out the `.` recursive sentinel; got: {stdout}"
    );
}

/// Under `--format json` a file that cannot be read is an entry of the
/// JSON array on stdout, like every other error. FAILS on the base: it
/// was a plain "error reading ..." line on stderr.
#[test]
fn silt_check_json_reports_an_unreadable_file_as_json() {
    let dir = fresh_dir("check_json_unreadable");
    let missing = dir.join("missing.silt");

    let out = silt_cmd()
        .args(["check", "--format", "json", missing.to_str().unwrap()])
        .output()
        .expect("failed to run silt");

    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("JSON on stdout");
    let entry = &json[0];
    assert_eq!(entry["severity"], "error", "{stdout}");
    assert_eq!(entry["code"], "E0401", "{stdout}");
    assert!(
        entry["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("error reading ") && m.ends_with("no such file")),
        "{stdout}"
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `silt check|disasm` and the run command given no file outside a
/// package: an error line in the prefix form, then the usage line.
/// FAILS on the base: the usage line alone.
#[test]
fn no_file_outside_a_package_is_an_error_with_the_usage() {
    let dir = fresh_dir("no_file");
    for command in ["run", "check", "disasm"] {
        let out = silt_cmd()
            .arg(command)
            .current_dir(&dir)
            .output()
            .expect("failed to run silt");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{command}: {stderr}");
        let mut lines = stderr.lines();
        assert_eq!(
            lines.next(),
            Some(
                format!(
                    "error: silt {command}: no file given, and no silt.toml found in this \
                     directory or any parent"
                )
                .as_str()
            ),
            "{stderr}"
        );
        assert!(
            lines.next().is_some_and(|l| l.starts_with(&format!("Usage: silt {command}"))),
            "{stderr}"
        );
    }
}

/// A module's name has no upper-case letter in any script: a file named
/// `Мир.silt` is refused as `Demo.silt` is. FAILS on the base: only
/// ASCII upper case counted.
#[test]
fn a_file_named_with_a_non_ascii_upper_case_letter_is_refused() {
    let dir = fresh_dir("unicode_upper");
    let file = dir.join("Мир.silt");
    write_trivial(&file);

    let out = silt_cmd()
        .args(["check", file.to_str().unwrap()])
        .output()
        .expect("failed to run silt");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("`Мир` is not a valid module name") && stderr.contains("`мир.silt`"),
        "{stderr}"
    );
}
