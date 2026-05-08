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
//! Each test shells out to the real `silt` binary so the full argv
//! parsing path is exercised end-to-end.
//!
//! Source-grep locks are also included for:
//!   - `install.sh` URL switch (must advertise silt-lang.com, not the
//!     raw.githubusercontent.com path).
//!   - `silt fmt --help` text mentioning directory expansion + `.`
//!     sentinel (which fmt legitimately accepts and the help body
//!     previously failed to document).

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

// ── L9: run / check / disasm / bare-file shim positional rejection ──

#[test]
fn silt_run_rejects_two_file_positionals() {
    let dir = fresh_dir("run");
    let a = dir.join("a.silt");
    let b = dir.join("b.silt");
    write_trivial(&a);
    write_trivial(&b);

    let out = silt_cmd()
        .args(["run", a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("failed to run silt");

    assert!(
        !out.status.success(),
        "silt run a.silt b.silt must fail; instead exited successfully.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt run") && stderr.contains("unexpected extra argument"),
        "stderr must mention the rejection; got: {stderr}"
    );
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

#[test]
fn silt_check_rejects_two_file_positionals() {
    let dir = fresh_dir("check");
    let a = dir.join("a.silt");
    let b = dir.join("b.silt");
    write_trivial(&a);
    write_trivial(&b);

    let out = silt_cmd()
        .args(["check", a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("failed to run silt");

    assert!(
        !out.status.success(),
        "silt check a.silt b.silt must fail; instead exited successfully.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt check") && stderr.contains("unexpected extra argument"),
        "stderr must mention the rejection; got: {stderr}"
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

#[test]
fn silt_disasm_rejects_two_file_positionals() {
    let dir = fresh_dir("disasm");
    let a = dir.join("a.silt");
    let b = dir.join("b.silt");
    write_trivial(&a);
    write_trivial(&b);

    let out = silt_cmd()
        .args(["disasm", a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("failed to run silt");

    assert!(
        !out.status.success(),
        "silt disasm a.silt b.silt must fail; instead exited successfully.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt disasm") && stderr.contains("unexpected extra argument"),
        "stderr must mention the rejection; got: {stderr}"
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

#[test]
fn silt_bare_file_shim_rejects_two_file_positionals() {
    // `silt a.silt b.silt` — bare-file convenience shim. The shim
    // bakes args[1] in as the file and is supposed to forward only
    // FLAGS from args[2..]. Pre-fix, any extra `.silt` in args[2..]
    // was silently ignored.
    let dir = fresh_dir("bare");
    let a = dir.join("a.silt");
    let b = dir.join("b.silt");
    write_trivial(&a);
    write_trivial(&b);

    let out = silt_cmd()
        .args([a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("failed to run silt");

    assert!(
        !out.status.success(),
        "silt a.silt b.silt must fail; instead exited successfully.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt run") && stderr.contains("unexpected extra argument"),
        "stderr must mention the rejection; got: {stderr}"
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

// ── fmt --help text lock (source grep) ──────────────────────────────

#[test]
fn silt_fmt_help_documents_directory_expansion() {
    // Help body must mention directory expansion and the `.` sentinel
    // so users discover the implementation's actual capability.
    // Pre-fix the help only mentioned `[files...]`, hiding both.
    let src = include_str!("../src/cli/fmt.rs");
    assert!(
        src.contains("recursively"),
        "fmt help text must mention recursive directory expansion"
    );
    assert!(
        src.contains("[files-or-dirs...]"),
        "fmt help usage line must advertise dirs in addition to files"
    );
    assert!(
        src.contains("`.`"),
        "fmt help text must call out the `.` recursive sentinel"
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
}

// ── L8: install.sh public URL lock (source grep) ────────────────────

#[test]
fn install_sh_uses_public_url_not_raw_github() {
    // The README and getting-started docs advertise
    // `https://silt-lang.com/install.sh` as the install command.
    // The script's own usage comment must agree, otherwise users
    // copying from the script's header land on a non-canonical URL.
    let src = include_str!("../install.sh");
    assert!(
        !src.contains("raw.githubusercontent.com"),
        "install.sh must not advertise raw.githubusercontent.com URLs in its usage comment"
    );
    assert!(
        src.contains("silt-lang.com"),
        "install.sh usage comment must point at the public silt-lang.com URL"
    );
}
