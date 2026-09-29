//! Round-67 audit regressions: subcommands that previously accepted
//! and silently ignored positional arguments now reject them with
//! a clear diagnostic and exit 1.
//!
//! Covers F13, F14, F15:
//!
//! - F13: `silt init <name>` (init.rs)
//! - F14: `silt repl <pos>` (repl.rs) and `silt lsp <pos>` (lsp.rs)
//! - F15: `silt fmt <dir>` errors with a raw "Is a directory" OS
//!         error instead of recursing into the directory
//!
//! All tests shell out to the real `silt` binary so the full argv
//! parsing path is exercised.

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
    let dir = std::env::temp_dir().join(format!("silt_positional_gate_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

// ── F13: silt init <pos> ─────────────────────────────────────────────

#[test]
fn silt_init_rejects_positional_argument() {
    // Pre-fix: `silt init my_project` silently ignored `my_project`
    // and created a package named after the cwd. Users coming from
    // cargo/npm will assume the positional names the new package.
    let dir = fresh_dir("init_pos");
    let out = silt_cmd()
        .args(["init", "my_project"])
        .current_dir(&dir)
        .output()
        .expect("failed to run silt init");
    assert!(
        !out.status.success(),
        "silt init my_project must fail; instead exited successfully.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt init") && stderr.contains("positional"),
        "stderr must mention the rejection; got: {stderr}"
    );
    // The error must point users at the discoverable workaround.
    assert!(
        stderr.contains("mkdir") || stderr.contains("current directory"),
        "stderr must explain the workaround (mkdir/cd or 'current directory'); got: {stderr}"
    );
    // No partial init detritus was created.
    assert!(
        !dir.join("silt.toml").exists(),
        "silt.toml should NOT have been written"
    );
    assert!(
        !dir.join("src").exists(),
        "src/ should NOT have been written"
    );
}

#[test]
fn silt_init_with_no_args_still_works() {
    // Regression guard: rejecting positionals must not break the
    // zero-arg path that does the actual init.
    let dir = fresh_dir("init_noarg");
    // Need a deterministic dirname — temp_dir_named is in cli_init_tests
    // but we just need ANY valid dir for this guard.
    let inner = dir.join("ok_pkg");
    fs::create_dir_all(&inner).unwrap();
    let out = silt_cmd()
        .arg("init")
        .current_dir(&inner)
        .output()
        .expect("failed to run silt init");
    assert!(
        out.status.success(),
        "silt init (no positionals) must still succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(inner.join("silt.toml").is_file());
}

// ── F14: silt repl <pos> / silt lsp <pos> ─────────────────────────────

#[test]
fn silt_repl_rejects_positional_argument() {
    let out = silt_cmd()
        .args(["repl", "ignored_arg"])
        .output()
        .expect("failed to run silt repl ignored_arg");
    assert!(
        !out.status.success(),
        "silt repl ignored_arg must fail; got success.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt repl") && stderr.contains("ignored_arg"),
        "stderr must name the subcommand and the rejected arg; got: {stderr}"
    );
}

#[test]
fn silt_lsp_rejects_positional_argument() {
    let out = silt_cmd()
        .args(["lsp", "foo"])
        .output()
        .expect("failed to run silt lsp foo");
    assert!(
        !out.status.success(),
        "silt lsp foo must fail; got success.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt lsp") && stderr.contains("foo"),
        "stderr must name the subcommand and the rejected arg; got: {stderr}"
    );
}

#[test]
fn silt_lsp_rejects_multiple_positionals() {
    let out = silt_cmd()
        .args(["lsp", "foo", "bar"])
        .output()
        .expect("failed to run silt lsp foo bar");
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Only the first is named, but we must reject before reaching the LSP.
    assert!(
        stderr.contains("silt lsp"),
        "stderr must name the subcommand; got: {stderr}"
    );
}

// ── F15: silt fmt <dir> recurses instead of "Is a directory" ──────────

#[test]
fn silt_fmt_recurses_into_directory_argument() {
    // Pre-fix: `silt fmt src/` emitted "error reading src/: Is a
    // directory (os error 21)" because format_file straight-up
    // read_to_string'd the path. After fix, fmt must recurse the
    // same way `silt test <dir>` does.
    let ws = fresh_dir("fmt_dir_arg");
    let src = ws.join("src");
    fs::create_dir_all(&src).unwrap();
    // A file that's already formatted — no rewriting needed, but we
    // verify fmt finds it.
    let formatted = "fn main() {\n  println(\"hello\")\n}\n";
    fs::write(src.join("main.silt"), formatted).unwrap();
    // A reformattable file in a nested subdir, to ensure recursion.
    let nested = src.join("sub");
    fs::create_dir_all(&nested).unwrap();
    let unformatted = "fn  foo( ) {\nprintln(\"nested\")\n}\n";
    fs::write(nested.join("foo.silt"), unformatted).unwrap();

    // First sanity-check with `--check` (read-only, exit-coded).
    let out = silt_cmd()
        .args(["fmt", "--check", "src"])
        .current_dir(&ws)
        .output()
        .expect("failed to run silt fmt --check src");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Crucially, must NOT emit the OS-21 raw error.
    assert!(
        !stderr.contains("Is a directory"),
        "fmt must not surface the raw OS-21 error; got stderr:\n{stderr}\nstdout:\n{stdout}"
    );
    // Exit 1 because the nested file is unformatted (recursion worked).
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1 (drift detected via recursion); got code={:?}\nstderr={stderr}\nstdout={stdout}",
        out.status.code()
    );
    // The drift line names the nested file, proving we recursed.
    assert!(
        stderr.contains("foo.silt"),
        "stderr must name the nested file we recursed to; got: {stderr}"
    );

    // Now let's actually format and verify the nested file changes.
    let out = silt_cmd()
        .args(["fmt", "src"])
        .current_dir(&ws)
        .output()
        .expect("failed to run silt fmt src");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("Is a directory"),
        "fmt must not surface the raw OS-21 error; got stderr:\n{stderr}"
    );
    assert!(
        out.status.success(),
        "silt fmt src must succeed when no infra error; stderr:\n{stderr}"
    );
    // The reformattable file was rewritten.
    let after = fs::read_to_string(nested.join("foo.silt")).unwrap();
    assert_ne!(
        after, unformatted,
        "nested file should have been reformatted in place"
    );
}

#[test]
fn silt_fmt_recurses_into_dot_argument() {
    // Belt-and-suspenders: `silt fmt .` (explicit current dir) was
    // already special-cased in the existing dispatcher. Lock that
    // behavior together with the new directory recursion.
    let ws = fresh_dir("fmt_dot");
    fs::write(ws.join("a.silt"), "fn  a( ) {\n}\n").unwrap();
    let out = silt_cmd()
        .args(["fmt", "--check", "."])
        .current_dir(&ws)
        .output()
        .expect("failed to run silt fmt --check .");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("Is a directory"),
        "got OS-21 raw error: {stderr}"
    );
    // Drift detected in a.silt.
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr.contains("a.silt"),
        "stderr should reference a.silt; got: {stderr}"
    );
}
