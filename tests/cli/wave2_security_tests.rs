//! Values from the command line, from manifests and from the file
//! system are shown by one display rule (`silt::git::escape_for_display`):
//! a control or invisible character is printed as an escape such as
//! `\t`, so it cannot split a line of output or drive the terminal.
//!
//! These tests cover the places that printed such a value raw:
//! `silt update`'s argument errors, the path in the ` --> ` locator of
//! a diagnostic inside a dependency, an empty line of git's output, and
//! the whitespace hint of a rejected git URL.
//!
//! Every test runs the built `silt` binary in a fresh temporary
//! directory, with a 20 s kill timeout. The only unusual character any
//! test uses is a tab.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and reported as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
struct Outcome {
    /// Exit status; `None` if the process was ended by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// True if the run exceeded `RUN_TIMEOUT` and was killed.
    timed_out: bool,
}

/// A fresh workspace directory, unique per call. Holds an empty
/// `gitconfig` and the output files of each run.
fn fresh_workspace(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("silt_wave2_security_{tag}_{pid}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("gitconfig"), "").unwrap();
    dir
}

/// Run `silt <args>` in `cwd`, with the git cache confined to the
/// workspace `ws` and git detached from the machine's configuration.
/// Output goes to files, so a killed child cannot block a pipe read.
fn silt(ws: &Path, cwd: &Path, args: &[&str]) -> Outcome {
    let out_path = ws.join("stdout.txt");
    let err_path = ws.join("stderr.txt");
    let out_file = fs::File::create(&out_path).unwrap();
    let err_file = fs::File::create(&err_path).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
    cmd.args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .env("XDG_CACHE_HOME", ws.join("cache"))
        .env("LOCALAPPDATA", ws.join("cache"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", ws.join("gitconfig"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
    ] {
        cmd.env_remove(var);
    }
    let mut child = cmd.spawn().expect("failed to spawn silt binary");
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if started.elapsed() >= RUN_TIMEOUT => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().expect("wait after kill");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    Outcome {
        code: status.code(),
        stdout: fs::read_to_string(&out_path)
            .unwrap_or_default()
            .replace("\r\n", "\n"),
        stderr: fs::read_to_string(&err_path)
            .unwrap_or_default()
            .replace("\r\n", "\n"),
        timed_out,
    }
}

/// Write a package: `silt.toml` with the given `[dependencies]` lines,
/// plus one source file under `src/`.
fn write_package(dir: &Path, name: &str, deps: &[&str], file: &str, body: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    let mut manifest = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n");
    if !deps.is_empty() {
        manifest.push_str("\n[dependencies]\n");
        for dep in deps {
            manifest.push_str(dep);
            manifest.push('\n');
        }
    }
    fs::write(dir.join("silt.toml"), manifest).unwrap();
    fs::write(dir.join("src").join(file), body).unwrap();
}

const MAIN: &str = "fn main() {\n  println(\"hi\")\n}\n";

/// Assert the run failed with exit 1, did not hang, and that stderr
/// holds no control character other than the line feeds that end its
/// lines.
fn assert_failed_printably(out: &Outcome, context: &str) {
    assert!(!out.timed_out, "{context}: silt hung; {out:?}");
    assert_eq!(out.code, Some(1), "{context}: expected exit 1; {out:?}");
    assert!(
        out.stdout.is_empty(),
        "{context}: expected nothing on stdout; {out:?}"
    );
    assert!(
        !out.stderr.chars().any(|c| c.is_control() && c != '\n'),
        "{context}: stderr holds a raw control character; {out:?}"
    );
}

// ── 1. `silt update` arguments ────────────────────────────────────────

/// FAILS on the base commit: the flag was echoed with a raw tab.
#[test]
fn update_escapes_an_unknown_flag() {
    let ws = fresh_workspace("update_flag");
    let out = silt(&ws, &ws, &["update", "--bad\tflag"]);
    assert_failed_printably(&out, "unknown flag");
    assert!(
        out.stderr
            .contains("silt update: unknown flag '--bad\\tflag'"),
        "{out:?}"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// FAILS on the base commit: the argument was echoed with a raw tab.
#[test]
fn update_escapes_an_extra_argument() {
    let ws = fresh_workspace("update_extra");
    let out = silt(&ws, &ws, &["update", "first", "sec\tond"]);
    assert_failed_printably(&out, "extra argument");
    assert!(
        out.stderr
            .contains("silt update: unexpected extra argument 'sec\\tond'"),
        "{out:?}"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// FAILS on the base commit: the dependency name was printed with a
/// raw tab.
#[test]
fn update_escapes_an_undeclared_dependency_name() {
    let ws = fresh_workspace("update_name");
    let app = ws.join("app");
    write_package(&app, "app", &[], "main.silt", MAIN);
    let out = silt(&ws, &app, &["update", "no\tsuch"]);
    assert_failed_printably(&out, "undeclared dependency");
    assert!(
        out.stderr
            .contains("silt update: dependency `no\\tsuch` is not declared in "),
        "{out:?}"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// FAILS on the base commit: the manifest path was printed with a raw
/// tab. Unix only: a tab cannot be part of a Windows file name.
#[cfg(unix)]
#[test]
fn update_escapes_the_manifest_path() {
    let ws = fresh_workspace("update_path");
    let app = ws.join("pkg\tdir");
    write_package(&app, "app", &[], "main.silt", MAIN);
    let out = silt(&ws, &app, &["update", "missing"]);
    assert_failed_printably(&out, "manifest path");
    assert!(
        out.stderr.contains("pkg\\tdir/silt.toml"),
        "the path must be shown with `\\t`; {out:?}"
    );
    let _ = fs::remove_dir_all(&ws);
}

// ── 2. the locator of a diagnostic inside a dependency ────────────────

/// A package whose dependency lives in a directory named with a tab,
/// and whose library holds `lib_body`.
#[cfg(unix)]
fn package_with_tab_dependency(ws: &Path, lib_body: &str) -> PathBuf {
    let app = ws.join("app");
    write_package(
        &app,
        "app",
        &["libx = { path = \"../lib\\tx\" }"],
        "main.silt",
        "import libx\nfn main() {\n  println(libx.f(0))\n}\n",
    );
    write_package(&ws.join("lib\tx"), "libx", &[], "lib.silt", lib_body);
    app
}

/// The ` --> ` lines of `stderr`.
fn locator_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.trim_start().starts_with("-->"))
        .collect()
}

/// FAILS on the base commit: the ` --> ` line showed the dependency's
/// path with a raw tab.
#[cfg(unix)]
#[test]
fn a_type_error_in_a_dependency_escapes_the_locator_path() {
    for subcommand in ["check", "run"] {
        let ws = fresh_workspace("locator_type");
        let app = package_with_tab_dependency(&ws, "pub fn f(n: Int) -> Int {\n  n + 1.5\n}\n");
        let out = silt(&ws, &app, &[subcommand]);
        let context = format!("silt {subcommand}");
        assert_failed_printably(&out, &context);
        let locators = locator_lines(&out.stderr);
        assert!(
            locators
                .iter()
                .any(|line| line.contains("lib\\tx/src/lib.silt:2:")),
            "{context}: expected a locator naming `lib\\tx/src/lib.silt`; {out:?}"
        );
        let _ = fs::remove_dir_all(&ws);
    }
}

/// FAILS on the base commit: the ` --> ` line of a run-time error in a
/// dependency showed the path with a raw tab. Only the locator is
/// asserted; the call stack below it is rendered elsewhere.
#[cfg(unix)]
#[test]
fn a_runtime_error_in_a_dependency_escapes_the_locator_path() {
    let ws = fresh_workspace("locator_runtime");
    let app = package_with_tab_dependency(&ws, "pub fn f(n: Int) -> Int {\n  10 / n\n}\n");
    let out = silt(&ws, &app, &["run"]);
    assert!(!out.timed_out, "silt hung; {out:?}");
    assert_eq!(out.code, Some(1), "{out:?}");
    let locators = locator_lines(&out.stderr);
    assert!(!locators.is_empty(), "expected a locator; {out:?}");
    for line in &locators {
        assert!(
            !line.chars().any(|c| c.is_control()),
            "the locator {line:?} holds a raw control character; {out:?}"
        );
    }
    assert!(
        locators
            .iter()
            .any(|line| line.contains("lib\\tx/src/lib.silt:2:")),
        "expected a locator naming `lib\\tx/src/lib.silt`; {out:?}"
    );
    // The call stack names the dependency's path too.
    let frames: Vec<&str> = out
        .stderr
        .lines()
        .filter(|line| line.trim_start().starts_with("-> "))
        .collect();
    assert!(
        frames
            .iter()
            .any(|line| line.contains("lib\\tx/src/lib.silt:2:")),
        "expected a call-stack frame naming `lib\\tx/src/lib.silt`; {out:?}"
    );
    for line in &frames {
        assert!(
            !line.chars().any(|c| c.is_control()),
            "the frame {line:?} holds a raw control character; {out:?}"
        );
    }
    let _ = fs::remove_dir_all(&ws);
}

/// Guard, passes on the base commit too: a path with nothing to escape
/// is shown as it is.
#[test]
fn a_plain_locator_path_is_unchanged() {
    let ws = fresh_workspace("locator_plain");
    let app = ws.join("app");
    write_package(
        &app,
        "app",
        &[],
        "main.silt",
        "fn main() {\n  let s: String = 1\n  println(s)\n}\n",
    );
    let out = silt(&ws, &app, &["check"]);
    assert_failed_printably(&out, "plain path");
    let locators = locator_lines(&out.stderr);
    assert!(
        locators.iter().any(|line| {
            let line = line.replace('\\', "/");
            line.contains("src/main.silt:2:")
        }),
        "{out:?}"
    );
    let _ = fs::remove_dir_all(&ws);
}

// ── 3. git's output ───────────────────────────────────────────────────

fn git_is_installed(ws: &Path) -> bool {
    Command::new("git")
        .arg("--version")
        .current_dir(ws)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// git's message for a repository that does not exist has an empty
/// line in it. FAILS on the base commit where git writes that empty
/// line: it was shown as `  git: `, with a trailing space. Skipped
/// when git is not installed.
#[test]
fn an_empty_line_of_git_output_has_no_trailing_space() {
    let ws = fresh_workspace("git_blank");
    if !git_is_installed(&ws) {
        let _ = fs::remove_dir_all(&ws);
        return;
    }
    let app = ws.join("app");
    write_package(
        &app,
        "app",
        &["remote = { git = \"file:///silt-test-nonexistent/remote.git\", branch = \"main\" }"],
        "main.silt",
        MAIN,
    );
    let out = silt(&ws, &app, &["update"]);
    assert_failed_printably(&out, "git failure");
    let lines: Vec<&str> = out.stderr.lines().collect();
    assert!(
        lines.len() >= 2 && lines[0].contains("git command failed"),
        "expected silt's line and git's output; {out:?}"
    );
    // git's lines are the notes of the diagnostic.
    let notes: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("  = note: "))
        .collect();
    assert!(notes.len() >= 2, "expected git's output; {out:?}");
    for line in notes {
        assert!(
            line.starts_with("git: ") || line == "git:",
            "the line {line:?} of git's output is not marked; {out:?}"
        );
    }
    for line in &lines {
        assert!(
            !line.ends_with(' '),
            "the line {line:?} ends with a space; {out:?}"
        );
    }
    let _ = fs::remove_dir_all(&ws);
}

// ── 4. the whitespace hint of a rejected git URL ──────────────────────

/// The hint for a URL in a network form.
const NETWORK_HINT: &str = "(a space is allowed only in a `file://` URL or a local path)";
/// The hint for a `file://` URL or a local path.
const LOCAL_HINT: &str =
    "(the ordinary space is the only one a `file://` URL or a local path may contain)";

fn add_git_url(tag: &str, url: &str) -> Outcome {
    let ws = fresh_workspace(tag);
    let app = ws.join("app");
    write_package(&app, "app", &[], "main.silt", MAIN);
    let out = silt(&ws, &app, &["add", "x", "--git", url, "--branch", "main"]);
    let _ = fs::remove_dir_all(&ws);
    out
}

/// FAILS on the base commit: a local value was told that a space is
/// allowed only in a local value.
#[test]
fn a_local_git_url_gets_the_local_whitespace_hint() {
    for url in ["./a\tb", "../a\tb", "/a\tb", "file:///a\tb"] {
        let out = add_git_url("hint_local", url);
        assert_failed_printably(&out, url);
        assert!(
            out.stderr
                .contains("must not contain whitespace or control characters"),
            "{url:?}: {out:?}"
        );
        assert!(out.stderr.contains(LOCAL_HINT), "{url:?}: {out:?}");
        assert!(!out.stderr.contains(NETWORK_HINT), "{url:?}: {out:?}");
    }
}

/// Guard, passes on the base commit too: a network form keeps its hint.
#[test]
fn a_network_git_url_keeps_the_network_whitespace_hint() {
    for url in ["https://example.com/a b.git", "host:a\tb.git"] {
        let out = add_git_url("hint_network", url);
        assert_failed_printably(&out, url);
        assert!(out.stderr.contains(NETWORK_HINT), "{url:?}: {out:?}");
        assert!(!out.stderr.contains(LOCAL_HINT), "{url:?}: {out:?}");
    }
}

/// `silt check --format json` puts the real path of a dependency's file
/// in `file`, JSON-escaped only: a display-escaped path (`lib\\tx`) would
/// decode to a file that does not exist.
#[cfg(unix)]
#[test]
fn json_output_names_the_real_path_of_a_dependency_file() {
    let ws = fresh_workspace("json_real_path");
    let app = package_with_tab_dependency(
        &ws,
        "pub fn f(n: Int) -> Int {\n  let s: String = n\n  1\n}\n",
    );
    let out = silt(&ws, &app, &["check", "--format", "json"]);
    assert!(!out.timed_out, "silt hung; {out:?}");
    let reported: serde_json::Value =
        serde_json::from_str(out.stdout.trim()).unwrap_or_else(|e| panic!("{e}; {out:?}"));
    let files: Vec<&str> = reported
        .as_array()
        .expect("a list of diagnostics")
        .iter()
        .filter_map(|d| d["file"].as_str())
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with("lib\tx/src/lib.silt")),
        "expected the real path with its tab; files: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.contains("lib\\tx")),
        "a display-escaped path in `file`; files: {files:?}"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// The "did you mean" hint of a failed import shows a file name from a
/// dependency's directory through the display rule, like the path above
/// it.
#[cfg(unix)]
#[test]
fn a_did_you_mean_hint_escapes_the_file_name() {
    let ws = fresh_workspace("did_you_mean");
    let app =
        package_with_tab_dependency(&ws, "import helper\npub fn f(n: Int) -> Int {\n  n\n}\n");
    fs::write(ws.join("lib\tx/src/helpe\tr.silt"), "pub fn g() { 1 }\n").unwrap();
    let out = silt(&ws, &app, &["check"]);
    assert!(!out.timed_out, "silt hung; {out:?}");
    let hint = out
        .stderr
        .lines()
        .find(|line| line.contains("did you mean"))
        .unwrap_or_else(|| panic!("expected a did-you-mean hint; {out:?}"));
    assert!(hint.contains("helpe\\tr"), "{hint:?}");
    assert!(!hint.chars().any(|c| c.is_control()), "{hint:?}");
    let _ = fs::remove_dir_all(&ws);
}
