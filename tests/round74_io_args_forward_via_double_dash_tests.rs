//! Round-74 audit regression: `silt run` (and the bare-file shim) must
//! forward positional arguments after `--` to the running program, where
//! they're surfaced via `io.args()`.
//!
//! Pre-fix: round-72's bare-extra-positional rejection over-fired by
//! making EVERY non-`.silt` positional after the script file a hard
//! error — there was no remaining channel to pass user args, so the
//! shipped `examples/text_stats.silt` example (which does
//! `io.args() |> list.drop(3)`) became dead code: the very invocation
//! its source code documents (`silt run text_stats.silt
//! examples/sample_text.txt`) was rejected.
//!
//! Post-fix: the `--` separator switches the parser into "forward to
//! program" mode. `silt run script.silt -- foo bar` now succeeds and
//! `io.args()` returns `["foo", "bar"]`.
//!
//! Each test shells out to the real `silt` binary so the full argv
//! parsing path is exercised end-to-end.

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
    let dir = std::env::temp_dir().join(format!("silt_round74_args_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a script that prints each `io.args()` entry on its own line.
/// Uses `list.each` because silt has no `for` keyword.
fn write_args_dumper(path: &std::path::Path) {
    fs::write(
        path,
        "import io\n\
         import list\n\
         fn main() {\n  \
           io.args() |> list.each { arg -> println(arg) }\n\
         }\n",
    )
    .unwrap();
}

#[test]
fn silt_run_forwards_program_args_after_double_dash() {
    let dir = fresh_dir("run_fwd");
    let script = dir.join("dump_args.silt");
    write_args_dumper(&script);

    let out = silt_cmd()
        .args([
            "run",
            script.to_str().unwrap(),
            "--",
            "foo",
            "bar",
            "baz",
        ])
        .output()
        .expect("failed to run silt");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "silt run script -- foo bar baz must succeed; \
         exit={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );

    // Each forwarded arg should appear as its own line in stdout.
    for needle in ["foo", "bar", "baz"] {
        assert!(
            stdout.lines().any(|l| l.trim() == needle),
            "stdout must contain a line equal to '{needle}' from io.args(); \
             got: {stdout}",
        );
    }
}

#[test]
fn silt_run_with_no_program_args_returns_empty_io_args() {
    // Without `--`, no extra positionals are accepted; `io.args()`
    // should return an empty list (not the silt binary's own argv).
    let dir = fresh_dir("run_empty");
    let script = dir.join("dump_args.silt");
    write_args_dumper(&script);

    let out = silt_cmd()
        .args(["run", script.to_str().unwrap()])
        .output()
        .expect("failed to run silt");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "silt run must succeed with no program args; \
         exit={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );
    // Pre-fix io.args() returned the binary's own argv ("silt", "run",
    // "<path>"); post-fix it returns nothing when no `--` is present.
    let trimmed = stdout.trim();
    assert!(
        trimmed.is_empty(),
        "io.args() must be empty when no `--` is present; got stdout: {stdout}"
    );
    // Specifically the binary path must NOT leak into io.args().
    assert!(
        !stdout.contains("silt") || !stdout.contains("run"),
        "binary argv must not leak through io.args(); got: {stdout}"
    );
}

#[test]
fn silt_run_double_dash_with_no_args_succeeds() {
    // `silt run script -- ` (trailing `--` with no args after) must be
    // accepted as a no-op; io.args() returns empty.
    let dir = fresh_dir("run_bare_dd");
    let script = dir.join("dump_args.silt");
    write_args_dumper(&script);

    let out = silt_cmd()
        .args(["run", script.to_str().unwrap(), "--"])
        .output()
        .expect("failed to run silt");

    assert!(
        out.status.success(),
        "silt run script -- (no args after) must succeed; exit={:?}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn silt_run_forwards_args_that_look_like_flags() {
    // `--foo` past the `--` separator must NOT be treated as a silt CLI
    // flag — the whole point of `--` is that everything after is
    // verbatim. Pre-fix this case never had a path to test (round-72
    // rejected all extras unconditionally).
    let dir = fresh_dir("run_flag_arg");
    let script = dir.join("dump_args.silt");
    write_args_dumper(&script);

    let out = silt_cmd()
        .args([
            "run",
            script.to_str().unwrap(),
            "--",
            "--strict-effects",
            "-v",
            "--unknown-thing",
        ])
        .output()
        .expect("failed to run silt");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "args after `--` must be forwarded verbatim, even when they look like flags; \
         exit={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );
    for needle in ["--strict-effects", "-v", "--unknown-thing"] {
        assert!(
            stdout.lines().any(|l| l.trim() == needle),
            "stdout must contain a line equal to '{needle}'; got: {stdout}"
        );
    }
}

#[test]
fn silt_bare_file_shim_forwards_program_args_after_double_dash() {
    // `silt foo.silt -- arg1 arg2` should also forward, mirroring
    // `silt run foo.silt -- arg1 arg2`. The bare-file shim shares the
    // same parser as `silt run`, just with the file pre-pinned.
    let dir = fresh_dir("bare_fwd");
    let script = dir.join("dump_args.silt");
    write_args_dumper(&script);

    let out = silt_cmd()
        .args([
            script.to_str().unwrap(),
            "--",
            "alpha",
            "beta",
        ])
        .output()
        .expect("failed to run silt");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "bare-file shim must forward args after `--`; \
         exit={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status.code()
    );
    for needle in ["alpha", "beta"] {
        assert!(
            stdout.lines().any(|l| l.trim() == needle),
            "stdout must contain a line equal to '{needle}'; got: {stdout}"
        );
    }
}

#[test]
fn silt_check_accepts_double_dash_separator() {
    // `silt check script.silt -- foo bar` must succeed (the args are
    // ignored since `check` doesn't execute, but the separator must
    // parse cleanly so CI scripts can swap `silt run` ↔ `silt check`
    // without rewriting argv).
    let dir = fresh_dir("check_dd");
    let script = dir.join("ok.silt");
    fs::write(&script, "fn main() { print(\"hi\") }\n").unwrap();

    let out = silt_cmd()
        .args(["check", script.to_str().unwrap(), "--", "foo", "bar"])
        .output()
        .expect("failed to run silt");

    assert!(
        out.status.success(),
        "silt check must accept `--` separator; \
         exit={:?}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn silt_disasm_accepts_double_dash_separator() {
    let dir = fresh_dir("disasm_dd");
    let script = dir.join("ok.silt");
    fs::write(&script, "fn main() { print(\"hi\") }\n").unwrap();

    let out = silt_cmd()
        .args(["disasm", script.to_str().unwrap(), "--", "foo", "bar"])
        .output()
        .expect("failed to run silt");

    assert!(
        out.status.success(),
        "silt disasm must accept `--` separator; \
         exit={:?}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn silt_run_extra_positional_without_double_dash_still_rejected() {
    // Round-72's intent is preserved: bare extras WITHOUT `--` still
    // hard-error. This protects against `silt run a.silt b.silt`
    // typos where the user thought both files would run.
    let dir = fresh_dir("run_no_dd");
    let a = dir.join("a.silt");
    let b = dir.join("b.silt");
    fs::write(&a, "fn main() { print(\"a\") }\n").unwrap();
    fs::write(&b, "fn main() { print(\"b\") }\n").unwrap();

    let out = silt_cmd()
        .args(["run", a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("failed to run silt");

    assert!(
        !out.status.success(),
        "silt run a.silt b.silt (no `--`) must STILL fail; \
         exit={:?}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected extra argument"),
        "rejection error must remain; got: {stderr}"
    );
    // The error must point users at `--` as the fix.
    assert!(
        stderr.contains("--"),
        "rejection error should suggest `--` separator; got: {stderr}"
    );
}
