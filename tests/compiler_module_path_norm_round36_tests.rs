//! Round-36 GAP regression: compile-time module-error snippet must use a
//! CWD-relative path so the inner `--> helper.silt:...` locator matches
//! the outer `--> main.silt:...` locator in a single diagnostic. Before
//! the fix, `src/compiler/mod.rs::load_and_compile_module` built
//! `file_display = file_path.display().to_string()` on the canonicalized
//! (absolute) module path, so when the user ran `silt check main.silt`
//! from a package root they got:
//!
//!   error[compile]: ...
//!     --> main.silt:1:1              <-- relative (from SourceError)
//!   ... module 'helper': parse error at /tmp/.../helper.silt:2:1
//!     --> /tmp/.../helper.silt:2:1   <-- absolute (inner snippet)
//!
//! Mixed styles in one diagnostic. Fix: normalize `file_display` through
//! a new `normalize_module_path` helper that strips the CWD prefix when
//! the module lives inside the caller's working directory.
//!
//! Mutation reasoning: reverting the fix (`file_display =
//! file_path.display().to_string()`) makes the `!contains("/home/")` /
//! `!contains("C:\\")` negative assertions fail, because the module
//! path flows through unchanged.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn fresh_tempdir() -> PathBuf {
    // Combine PID + monotonic counter so two test processes (e.g. under
    // nextest, where every test runs in its own process and each starts
    // COUNTER at 0) cannot collide on the same tempdir name.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("silt_r36_compiler_path_norm_{pid}_{n}"));
    // Clean up any stale directory from a previous run.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("failed to create tempdir");
    dir
}

/// `silt check main.silt` from a tempdir where `helper.silt` has a
/// parse error must render the inner module-error snippet with a
/// CWD-relative path (e.g. `helper.silt:2:1`) — never with the absolute
/// tempdir prefix.
///
/// This locks `normalize_module_path` in `src/compiler/mod.rs` against
/// a regression that drops the fix and falls back to the raw
/// `file_path.display()`.
#[test]
fn test_module_error_snippet_uses_relative_path_when_cwd_is_package_root() {
    let dir = fresh_tempdir();

    // Deliberately broken helper: unclosed `(` in a function declaration.
    // The parse error surfaces at the first token after the unclosed
    // paren — somewhere inside helper.silt, definitely not at
    // the outer `import helper` site.
    let helper_src = "pub fn oops(x,\n  y,\n  z\n}\n";
    fs::write(dir.join("helper.silt"), helper_src).expect("failed to write helper.silt");

    let main_src = "import helper\n\nfn main() {\n  helper.oops(1, 2, 3)\n}\n";
    fs::write(dir.join("main.silt"), main_src).expect("failed to write main.silt");

    let output = silt_cmd()
        .arg("check")
        .arg("main.silt")
        .current_dir(&dir)
        .output()
        .expect("failed to run silt check");

    assert!(
        !output.status.success(),
        "expected silt check to fail on broken module, got success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The inner module-error snippet must reference helper.silt.
    assert!(
        stderr.contains("helper.silt:"),
        "expected inner module-error snippet to reference 'helper.silt:', got:\n{stderr}"
    );

    // Core assertion: AND-chain from the finding spec. No absolute
    // prefixes — neither unix-style `/home/` nor windows-style `C:\`
    // — anywhere in stderr. If the compiler still renders the raw
    // canonicalized path, the tempdir prefix (e.g. `/tmp/...` on linux
    // which contains no `/home/`) might slip through — so we also
    // assert that no `-->` locator contains an absolute-looking
    // tempdir path.
    assert!(
        !stderr.contains("/home/"),
        "stderr must not leak any `/home/...` absolute path, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("C:\\"),
        "stderr must not leak any `C:\\...` absolute path, got:\n{stderr}"
    );

    // Defense in depth: the CWD we ran `silt check` from is the
    // tempdir. The absolute form of that tempdir must not appear in
    // any `-->` locator line inside stderr.
    let abs_dir = dir.to_string_lossy().into_owned();
    for line in stderr.lines().filter(|l| l.contains("-->")) {
        assert!(
            !line.contains(&abs_dir),
            "`-->` locator leaked absolute tempdir path {abs_dir:?}:\n  line: {line}\nfull stderr:\n{stderr}"
        );
    }
}

/// Companion test: lex error (vs parse error above) flows through the
/// same `format_module_source_error` call site, so the path
/// normalization must work for both kinds. Locking both kinds prevents
/// a partial regression that only hits one branch.
#[test]
fn test_module_lex_error_snippet_uses_relative_path() {
    let dir = fresh_tempdir();

    // `@@@` is an illegal token — the lexer rejects it, so the parser
    // never runs on this module.
    let helper_src = "pub fn ok() = 1\n@@@\n";
    fs::write(dir.join("helper.silt"), helper_src).expect("failed to write helper.silt");

    let main_src = "import helper\n\nfn main() {\n  helper.ok()\n}\n";
    fs::write(dir.join("main.silt"), main_src).expect("failed to write main.silt");

    let output = silt_cmd()
        .arg("check")
        .arg("main.silt")
        .current_dir(&dir)
        .output()
        .expect("failed to run silt check");

    assert!(
        !output.status.success(),
        "expected silt check to fail on broken module, got success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("helper.silt:"),
        "expected inner module-error snippet to reference 'helper.silt:', got:\n{stderr}"
    );
    assert!(
        !stderr.contains("/home/"),
        "stderr must not leak any `/home/...` absolute path, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("C:\\"),
        "stderr must not leak any `C:\\...` absolute path, got:\n{stderr}"
    );

    let abs_dir = dir.to_string_lossy().into_owned();
    for line in stderr.lines().filter(|l| l.contains("-->")) {
        assert!(
            !line.contains(&abs_dir),
            "`-->` locator leaked absolute tempdir path {abs_dir:?}:\n  line: {line}\nfull stderr:\n{stderr}"
        );
    }
}
