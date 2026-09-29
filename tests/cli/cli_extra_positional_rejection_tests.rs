//! Round-72 audit regression: runtime subcommands that take at most
//! one file (`silt run`, `silt check`, `silt disasm`) and the
//! bare-file shim (`silt foo.silt ...`) must reject extra positional
//! arguments with a clear error rather than silently dropping them
//! (run/disasm) or last-wins overwriting them (check).
//!
//! Pre-fix:
//!   - `silt run a.silt b.silt` — silently ran only `a.silt`.
//!   - `silt check a.silt b.silt` — silently checked only `b.silt`
//!     (last-wins).
//!   - `silt disasm a.silt b.silt` — silently disassembled only
//!     `a.silt`.
//!   - `silt a.silt b.silt` — bare-file shim silently ran only
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

/// Round-74 follow-up: extras after `--` are accepted by `silt check`
/// (silently ignored — `check` doesn't execute, but `--` parses as the
/// program-args separator so CI scripts can swap subcommands cleanly).
#[test]
fn silt_check_accepts_extras_after_double_dash() {
    let dir = fresh_dir("check_dd");
    let a = dir.join("a.silt");
    write_trivial(&a);

    let out = silt_cmd()
        .args(["check", a.to_str().unwrap(), "--", "extra1", "extra2"])
        .output()
        .expect("failed to run silt");

    assert!(
        out.status.success(),
        "silt check a.silt -- extra1 extra2 must succeed; \
         exit={:?}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Round-74 follow-up: extras after `--` are accepted by `silt disasm`
/// (silently ignored — disasm doesn't execute, but `--` parses cleanly).
#[test]
fn silt_disasm_accepts_extras_after_double_dash() {
    let dir = fresh_dir("disasm_dd");
    let a = dir.join("a.silt");
    write_trivial(&a);

    let out = silt_cmd()
        .args(["disasm", a.to_str().unwrap(), "--", "extra1", "extra2"])
        .output()
        .expect("failed to run silt");

    assert!(
        out.status.success(),
        "silt disasm a.silt -- extra1 extra2 must succeed; \
         exit={:?}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Round-74 follow-up: bare-file shim also accepts `--` as the
/// program-args separator.
#[test]
fn silt_bare_file_shim_accepts_extras_after_double_dash() {
    let dir = fresh_dir("bare_dd");
    let a = dir.join("a.silt");
    write_trivial(&a);

    let out = silt_cmd()
        .args([a.to_str().unwrap(), "--", "extra1", "extra2"])
        .output()
        .expect("failed to run silt");

    assert!(
        out.status.success(),
        "silt a.silt -- extra1 extra2 must succeed; \
         exit={:?}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
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
