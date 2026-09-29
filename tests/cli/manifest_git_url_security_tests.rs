//! Security regression tests for git dependency URLs.
//!
//! A `git = "..."` value in `silt.toml` is untrusted: the manifest may
//! be a transitive dependency's. Before the fix the value reached `git`
//! unvalidated and without a `--` separator, so
//!
//! ```toml
//! evil = { git = "--upload-pack=touch /some/path/MARKER", branch = "main" }
//! ```
//!
//! made a plain `silt check` run `touch /some/path/MARKER`.
//!
//! These tests are behavioural: each one runs the compiled `silt`
//! binary on a package in a fresh temporary directory and asserts on
//! the exit status, stderr and the filesystem.
//!
//! Hermeticity: every `silt` invocation gets a git checkout cache
//! inside its own workspace (`XDG_CACHE_HOME`, and `LOCALAPPDATA` for
//! Windows), so the user's real cache is never written, and an empty
//! git configuration. Nothing here uses the network, on the fixed code
//! or on the unfixed code: every URL is option-shaped, a `file://` URL,
//! a local path, or a loopback address.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

// ── Helpers ───────────────────────────────────────────────────────────

/// A fresh workspace directory, unique per call so parallel tests never
/// share state. Holds an empty `gitconfig` for [`isolate_git`].
fn fresh_workspace(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("silt_git_url_security_{tag}_{pid}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("gitconfig"), "").unwrap();
    dir
}

/// Detach `cmd` (and any `git` it spawns) from the machine's git setup:
/// no system or user configuration, no credential prompt, and no
/// repository inherited from the environment of the test runner.
fn isolate_git(cmd: &mut Command, ws: &Path) {
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", ws.join("gitconfig"))
        .env("GIT_TERMINAL_PROMPT", "0");
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
    ] {
        cmd.env_remove(var);
    }
}

/// Run `silt <args>` in `cwd`, with the git checkout cache confined to
/// the workspace `ws`.
fn silt(ws: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
    cmd.args(args)
        .current_dir(cwd)
        .env("XDG_CACHE_HOME", ws.join("cache"))
        .env("LOCALAPPDATA", ws.join("cache"));
    isolate_git(&mut cmd, ws);
    cmd.output().expect("failed to spawn silt binary")
}

/// `s` as a TOML basic string, with everything TOML requires escaped.
/// Lets a test put any value (newlines, tabs, Windows paths) in a
/// manifest and have the TOML parser hand silt exactly that value.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Write a package: `silt.toml` with the given `[dependencies]` lines,
/// plus one source file under `src/`.
fn write_package(dir: &Path, name: &str, deps: &[String], file: &str, body: &str) {
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

/// A `[dependencies]` line declaring `name` as a git dependency on
/// `url`, tracking the branch `main`.
fn git_dep(name: &str, url: &str) -> String {
    format!("{name} = {{ git = {}, branch = \"main\" }}", toml_str(url))
}

/// The option-shaped URL from the audit: if it reaches `git ls-remote`
/// as an option, git runs `touch <marker>`.
fn injection_url(marker: &Path) -> String {
    format!("--upload-pack=touch {}", marker.display())
}

/// True if a terminal would act on `c` or hide it: a control character
/// (other than the line feed that ends a line of output), an invisible
/// or bidirectional formatting character, or a space look-alike.
fn is_unprintable(c: char) -> bool {
    if c == '\n' {
        return false;
    }
    c.is_control()
        || matches!(
            c,
            '\u{00A0}'
                | '\u{00AD}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// Assert the payload did not run.
fn assert_marker_absent(marker: &Path, context: &str, out: &Output) {
    assert!(
        !marker.exists(),
        "{context}: the git URL was executed as a command, {} exists; stderr={}",
        marker.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Assert `out` is a clean manifest error about dependency `evil`'s git
/// URL: exit code 1, the dependency named, the accepted forms listed,
/// no panic, and no control or invisible character echoed back raw.
fn assert_clean_url_rejection(out: &Output, context: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{context}: expected a clean error exit with code 1; stderr={stderr}"
    );
    assert!(
        stderr.contains("error: invalid manifest"),
        "{context}: expected a manifest error; stderr={stderr}"
    );
    assert!(
        stderr.contains("dependency `evil`"),
        "{context}: the error must name the dependency; stderr={stderr}"
    );
    assert!(
        stderr.contains("invalid git URL"),
        "{context}: the error must say the git URL is invalid; stderr={stderr}"
    );
    assert!(
        stderr.contains("`https://`") && stderr.contains("`user@host:path`"),
        "{context}: the error must say what is accepted; stderr={stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "{context}: silt panicked instead of reporting an error; stderr={stderr}"
    );
    assert!(
        !stderr.chars().any(is_unprintable),
        "{context}: stderr echoes a raw control or invisible character: {stderr:?}"
    );
}

// ── 1. Direct injection ───────────────────────────────────────────────

/// The audit's reproduction: the package's own manifest carries the
/// option-shaped URL. FAILS on the unfixed code (the marker exists).
#[test]
fn direct_option_shaped_git_url_runs_nothing() {
    let ws = fresh_workspace("direct");
    let marker = ws.join("MARKER");
    let app = ws.join("app");
    let dep = git_dep("evil", &injection_url(&marker));
    write_package(&app, "app", &[dep], "main.silt", "fn main() {}\n");

    let out = silt(&ws, &app, &["check"]);

    assert_marker_absent(&marker, "silt check", &out);
    assert_clean_url_rejection(&out, "silt check");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--upload-pack=touch") && stderr.contains("must not start with `-`"),
        "the error must show the offending value and the rule; stderr={stderr}"
    );
    assert!(
        !app.join("silt.lock").exists(),
        "silt must not write a lockfile when it rejects the manifest"
    );

    let _ = fs::remove_dir_all(&ws);
}

// ── 2. Transitive injection ───────────────────────────────────────────

/// Only a path dependency's manifest carries the option-shaped URL; the
/// package being checked is clean. FAILS on the unfixed code.
#[test]
fn transitive_option_shaped_git_url_runs_nothing() {
    for subcommand in ["check", "run", "disasm", "update"] {
        let ws = fresh_workspace("transitive");
        let marker = ws.join("MARKER");
        let app = ws.join("app");
        let inner = ws.join("inner");
        let evil = git_dep("evil", &injection_url(&marker));
        write_package(&inner, "inner", &[evil], "lib.silt", "pub fn one() = 1\n");
        let path_dep = "inner = { path = \"../inner\" }".to_string();
        write_package(&app, "app", &[path_dep], "main.silt", "fn main() {}\n");

        let out = silt(&ws, &app, &[subcommand]);

        let context = format!("silt {subcommand} (transitive)");
        assert_marker_absent(&marker, &context, &out);
        assert_clean_url_rejection(&out, &context);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let inner_manifest = Path::new("inner").join("silt.toml");
        assert!(
            stderr.contains(&inner_manifest.display().to_string()),
            "{context}: the error must point at the dependency's manifest; stderr={stderr}"
        );

        let _ = fs::remove_dir_all(&ws);
    }
}

// ── 3. Every subcommand that resolves dependencies ────────────────────

/// `run`, `disasm`, `update` and `test` resolve the dependency tree the
/// same way `check` does (`disasm` in memory, when no lockfile exists).
/// FAILS on the unfixed code.
#[test]
fn option_shaped_git_url_runs_nothing_under_any_subcommand() {
    let invocations: [&[&str]; 5] = [
        &["run"],
        &["disasm"],
        &["update"],
        &["test", "src/main.silt"],
        &["check", "src/main.silt"],
    ];
    for args in invocations {
        let ws = fresh_workspace("subcommand");
        let marker = ws.join("MARKER");
        let app = ws.join("app");
        let dep = git_dep("evil", &injection_url(&marker));
        write_package(&app, "app", &[dep], "main.silt", "fn main() {}\n");

        let out = silt(&ws, &app, args);

        let context = format!("silt {}", args.join(" "));
        assert_marker_absent(&marker, &context, &out);
        assert_clean_url_rejection(&out, &context);

        let _ = fs::remove_dir_all(&ws);
    }
}

/// `silt add` shares the manifest's rule. The old, separate check let
/// an option-shaped value through its scp-style arm whenever the value
/// held an `@` followed by a `:`. FAILS on the unfixed code.
#[test]
fn silt_add_rejects_option_shaped_git_url() {
    let ws = fresh_workspace("add");
    let marker = ws.join("MARKER");
    let app = ws.join("app");
    write_package(&app, "app", &[], "main.silt", "fn main() {}\n");
    let manifest_before = fs::read_to_string(app.join("silt.toml")).unwrap();
    // No whitespace, so the old check could not reject it on that
    // ground; the shell git hands the command to expands `${IFS}`.
    let payload = format!("touch${{IFS}}{}${{IFS}}@h:p", marker.display());
    let url = format!("--upload-pack={payload}");

    let args = ["add", "evil", "--git", url.as_str(), "--branch", "main"];
    let out = silt(&ws, &app, &args);

    assert_marker_absent(&marker, "silt add", &out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "silt add: expected a clean error exit with code 1; stderr={stderr}"
    );
    assert!(
        stderr.contains("doesn't look like a git URL")
            && stderr.contains("must not start with `-`")
            && stderr.contains("`user@host:path`"),
        "silt add: expected the shared URL rule in the diagnostic; stderr={stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "silt add: silt panicked instead of reporting an error; stderr={stderr}"
    );
    assert_eq!(
        fs::read_to_string(app.join("silt.toml")).unwrap(),
        manifest_before,
        "silt add: the manifest must not be touched when the URL is rejected"
    );

    let _ = fs::remove_dir_all(&ws);
}

// ── 4. Rejected URL table ─────────────────────────────────────────────

const RULE_EMPTY: &str = "must not be empty";
const RULE_DASH: &str = "must not start with `-`";
const RULE_SPACE: &str = "must not contain whitespace or control characters";
const RULE_INVISIBLE: &str = "must not contain invisible or bidirectional formatting characters";
const RULE_FORM: &str = "is not a recognised git URL form";

/// Every malformed URL is a clean manifest error that names the
/// dependency and the rule, with no panic, and with the offending
/// character escaped. None of these values reaches the network, even on
/// the unfixed code.
#[test]
fn malformed_git_urls_are_clean_manifest_errors() {
    let cases: &[(&str, &str)] = &[
        ("", RULE_EMPTY),
        ("-oProxyCommand=x", RULE_DASH),
        ("--upload-pack=echo", RULE_DASH),
        // A space is allowed in the two local forms only.
        ("http://127.0.0.1:1/silt test/pkg.git", RULE_SPACE),
        ("silt-test-nonexistent/my repos/pkg.git", RULE_SPACE),
        (
            "ext::sh -c touch% /silt-test-nonexistent/MARKER",
            RULE_SPACE,
        ),
        // Any other whitespace, and any control character, is rejected
        // everywhere, the local forms included.
        ("file:///silt-test-nonexistent/a\nb.git", RULE_SPACE),
        ("file:///silt-test-nonexistent/a\rb.git", RULE_SPACE),
        ("file:///silt-test-nonexistent/a\tb.git", RULE_SPACE),
        ("file:///silt-test-nonexistent/a\u{1b}[31mb.git", RULE_SPACE),
        ("file:///silt-test-nonexistent/a\u{a0}b.git", RULE_SPACE),
        ("file:///silt-test-nonexistent/a\u{2028}b.git", RULE_SPACE),
        ("/silt-test-nonexistent/a\nb.git", RULE_SPACE),
        ("../silt-test-nonexistent/a\u{1b}[31mb.git", RULE_SPACE),
        // Invisible and bidirectional formatting characters.
        ("file:///silt-test-nonexistent/a\u{ad}b.git", RULE_INVISIBLE),
        (
            "file:///silt-test-nonexistent/a\u{200b}b.git",
            RULE_INVISIBLE,
        ),
        (
            "file:///silt-test-nonexistent/\u{202e}tig.b",
            RULE_INVISIBLE,
        ),
        (
            "file:///silt-test-nonexistent/a\u{2060}b.git",
            RULE_INVISIBLE,
        ),
        (
            "file:///silt-test-nonexistent/a\u{2066}b.git",
            RULE_INVISIBLE,
        ),
        (
            "file:///silt-test-nonexistent/a\u{feff}b.git",
            RULE_INVISIBLE,
        ),
        ("/silt-test-nonexistent/a\u{200f}b.git", RULE_INVISIBLE),
        ("http://127.0.0.1:1/a\u{200d}b.git", RULE_INVISIBLE),
        // Forms that would run a remote-helper program, and a relative
        // path that does not start with `./` or `../`.
        ("ext::sh", RULE_FORM),
        ("foo://127.0.0.1:1/pkg.git", RULE_FORM),
        ("silt-test-nonexistent/pkg.git", RULE_FORM),
    ];
    for &(url, rule) in cases {
        let ws = fresh_workspace("table");
        let app = ws.join("app");
        let dep = git_dep("evil", url);
        write_package(&app, "app", &[dep], "main.silt", "fn main() {}\n");

        let out = silt(&ws, &app, &["check"]);

        let context = format!("git URL {url:?}");
        assert_clean_url_rejection(&out, &context);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(rule),
            "{context}: the error must state the rule `{rule}`; stderr={stderr}"
        );
        assert!(
            !app.join("silt.lock").exists(),
            "{context}: silt must not write a lockfile when it rejects the manifest"
        );

        let _ = fs::remove_dir_all(&ws);
    }
}

// ── 5. Untrusted text in error output ─────────────────────────────────

/// A `branch`, `tag` or `rev` value is echoed in the error when the
/// dependency cannot be resolved. A newline or an escape character in
/// it must not reach the terminal: it would forge a line of output.
/// The repository is a local path that does not exist, so resolution
/// fails at once, with or without `git` installed.
#[test]
fn hostile_ref_value_cannot_forge_a_line_of_output() {
    let url = "file:///silt-test-nonexistent/remote.git";
    let hostile = "main\nFORGED-LINE: all checks passed\u{1b}[2K\u{202e}";
    let escaped = "main\\nFORGED-LINE: all checks passed\\u{1b}[2K\\u{202e}";
    for key in ["branch", "tag", "rev"] {
        let ws = fresh_workspace("hostile_ref");
        let app = ws.join("app");
        let value = toml_str(hostile);
        let dep = format!("remote = {{ git = {}, {key} = {value} }}", toml_str(url));
        write_package(&app, "app", &[dep], "main.silt", "fn main() {}\n");

        let out = silt(&ws, &app, &["check"]);

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{key}: expected a clean error exit with code 1; stderr={stderr}"
        );
        assert!(
            stderr.contains("error: git dependency") && stderr.contains(url),
            "{key}: expected a git dependency error naming the URL; stderr={stderr}"
        );
        assert!(
            stderr.contains(&format!("{key} = `{escaped}`")),
            "{key}: the value must be shown, escaped; stderr={stderr}"
        );
        assert!(
            !stderr.chars().any(is_unprintable),
            "{key}: stderr echoes a raw control or invisible character: {stderr:?}"
        );
        assert!(
            !stderr.lines().any(|line| line.starts_with("FORGED-LINE")),
            "{key}: the value forged a line of output; stderr={stderr}"
        );
        assert!(
            !stderr.contains("panicked at"),
            "{key}: silt panicked instead of reporting an error; stderr={stderr}"
        );

        let _ = fs::remove_dir_all(&ws);
    }
}

// ── 6. Real use still works ───────────────────────────────────────────

/// Run `git <args>` in `cwd` for fixture setup, with a fixed identity so
/// the commit does not depend on the machine's git configuration.
/// Returns trimmed stdout.
#[cfg(unix)]
fn git(ws: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "user.name=silt tests"])
        .args(["-c", "user.email=tests@silt.local"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(cwd);
    isolate_git(&mut cmd, ws);
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} spawn failed: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Can the tests that build a real repository run here? They need `git`,
/// and a temporary directory whose own path the URL rule accepts (a
/// space in it is fine; any other whitespace is rejected by design).
/// Prints a notice when the answer is no.
#[cfg(unix)]
fn can_build_a_repository(ws: &Path) -> bool {
    let git_installed = Command::new("git")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    if !git_installed {
        eprintln!("SKIP: `git` is not installed");
        return false;
    }
    let path = ws.display().to_string();
    let rejected = |c: char| c == '\n' || is_unprintable(c) || (c.is_whitespace() && c != ' ');
    if path.chars().any(rejected) {
        eprintln!("SKIP: the temporary directory's path is not a valid git URL: {path:?}");
        return false;
    }
    true
}

/// Create at `repo` a git repository holding the silt package
/// `locallib`: one commit, on the branch `main`. Returns the commit id.
#[cfg(unix)]
fn create_locallib_repository(ws: &Path, repo: &Path) -> String {
    write_package(repo, "locallib", &[], "lib.silt", "pub fn answer() = 42\n");
    git(ws, repo, &["init", "--quiet"]);
    // Name the branch explicitly rather than relying on the default
    // branch name or on `git init -b` (git 2.28+).
    git(ws, repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(ws, repo, &["add", "."]);
    git(ws, repo, &["commit", "--quiet", "-m", "initial"]);
    git(ws, repo, &["rev-parse", "HEAD"])
}

/// Write the package `<ws>/app` with `locallib` declared as a git
/// dependency on `url`, run `silt check` and `silt run` in it, and
/// assert the dependency resolved: both succeed, the lockfile pins
/// `head`, the checkout is in the workspace's own cache, and the
/// dependency's code runs.
#[cfg(unix)]
fn assert_git_dependency_resolves(ws: &Path, url: &str, head: &str) {
    let app = ws.join("app");
    let dep = git_dep("locallib", url);
    let main_body = "import locallib\nfn main() { println(locallib.answer()) }\n";
    write_package(&app, "app", &[dep], "main.silt", main_body);

    let check = silt(ws, &app, &["check"]);
    assert!(
        check.status.success(),
        "silt check failed for git = {url:?}: stdout={} stderr={}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );

    // The lockfile pins the commit the branch resolved to.
    let lock = fs::read_to_string(app.join("silt.lock")).expect("silt.lock was written");
    assert!(
        lock.contains(&format!("rev = \"{head}\"")),
        "the lockfile must pin the resolved commit `{head}`:\n{lock}"
    );
    assert!(
        lock.contains(&format!("git = {}", toml_str(url))),
        "the lockfile must record the dependency's URL {url:?}:\n{lock}"
    );

    // The checkout landed in the workspace's cache, under the commit.
    let cache_root = ws.join("cache").join("silt").join("git");
    let checkouts: Vec<PathBuf> = fs::read_dir(&cache_root)
        .expect("cache root exists")
        .map(|entry| entry.unwrap().path().join(head))
        .collect();
    assert_eq!(
        checkouts.len(),
        1,
        "expected exactly one cached repository under {}",
        cache_root.display()
    );
    assert!(
        checkouts[0].join("silt.toml").is_file(),
        "expected a checkout at {}",
        checkouts[0].display()
    );

    // And the dependency's code actually runs from it.
    let run = silt(ws, &app, &["run"]);
    assert!(
        run.status.success(),
        "silt run failed for git = {url:?}: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "42");
}

/// A dependency on a local git repository, addressed by a `file://` URL
/// and a branch, resolves, locks, and runs. Proves that the `--`
/// separator, `-c protocol.ext.allow=never` and `GIT_TERMINAL_PROMPT=0`
/// did not break `git ls-remote`, `git clone` or `git checkout`.
///
/// Unix only, like the other tests in this section: the fixtures spell
/// the URL from an absolute Unix path.
#[cfg(unix)]
#[test]
fn file_url_git_dependency_still_resolves() {
    let ws = fresh_workspace("positive");
    if !can_build_a_repository(&ws) {
        return;
    }
    let repo = ws.join("locallib_repo");
    let head = create_locallib_repository(&ws, &repo);

    let url = format!("file://{}", repo.display());
    assert_git_dependency_resolves(&ws, &url, &head);

    let _ = fs::remove_dir_all(&ws);
}

/// A `file://` URL whose directory name contains a space resolves: an
/// ordinary space is allowed in the local forms.
#[cfg(unix)]
#[test]
fn file_url_with_a_space_git_dependency_resolves() {
    let ws = fresh_workspace("file_space");
    if !can_build_a_repository(&ws) {
        return;
    }
    let repo = ws.join("local lib repo");
    let head = create_locallib_repository(&ws, &repo);

    let url = format!("file://{}", repo.display());
    assert!(url.contains("/local lib repo"), "fixture lost its space");
    assert_git_dependency_resolves(&ws, &url, &head);

    let _ = fs::remove_dir_all(&ws);
}

/// A repository given as an absolute local path resolves.
#[cfg(unix)]
#[test]
fn absolute_path_git_dependency_resolves() {
    let ws = fresh_workspace("abs_path");
    if !can_build_a_repository(&ws) {
        return;
    }
    let repo = ws.join("remote");
    let head = create_locallib_repository(&ws, &repo);

    let url = repo.display().to_string();
    assert!(url.starts_with('/'), "fixture path is not absolute: {url}");
    assert_git_dependency_resolves(&ws, &url, &head);

    let _ = fs::remove_dir_all(&ws);
}

/// An absolute local path whose directory name contains a space
/// resolves.
#[cfg(unix)]
#[test]
fn absolute_path_with_a_space_git_dependency_resolves() {
    let ws = fresh_workspace("abs_path_space");
    if !can_build_a_repository(&ws) {
        return;
    }
    let repo = ws.join("remote repo");
    let head = create_locallib_repository(&ws, &repo);

    let url = repo.display().to_string();
    assert!(url.ends_with("/remote repo"), "fixture lost its space");
    assert_git_dependency_resolves(&ws, &url, &head);

    let _ = fs::remove_dir_all(&ws);
}

/// A repository given as a relative path resolves. There is no `file://`
/// spelling of a relative path. git resolves it against the directory
/// `silt` runs in, here the package root `<ws>/app`.
#[cfg(unix)]
#[test]
fn relative_path_git_dependency_resolves() {
    let ws = fresh_workspace("rel_path");
    if !can_build_a_repository(&ws) {
        return;
    }
    let head = create_locallib_repository(&ws, &ws.join("remote"));

    assert_git_dependency_resolves(&ws, "../remote", &head);

    let _ = fs::remove_dir_all(&ws);
}

// ── 7. Lockfile `rev` ─────────────────────────────────────────────────

/// A hand-edited `silt.lock` whose `rev` is a relative path must be a
/// clean lockfile error. Before the fix `rev` was joined onto the cache
/// path unchecked, so `../../../../x` pointed the package root at an
/// arbitrary directory. FAILS on the unfixed code: `disasm` accepted
/// the lockfile and exited 0; `check` and `run` fell through to a git
/// error for the (nonexistent, local) repository.
#[test]
fn lockfile_rev_with_path_traversal_is_a_clean_error() {
    let url = "file:///silt-test-nonexistent/remote.git";
    let lockfile = format!(
        "# This file is generated by silt. Do not edit by hand.\n\
         version = 1\n\n\
         [[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [[package]]\nname = \"remote\"\nversion = \"0.1.0\"\n\
         source = {{ git = {}, branch = \"main\", rev = \"../../../../x\" }}\n\
         checksum = \"sha256:deadbeef\"\n",
        toml_str(url)
    );
    for subcommand in ["check", "run", "disasm"] {
        let ws = fresh_workspace("lock_rev");
        let app = ws.join("app");
        let dep = git_dep("remote", url);
        write_package(&app, "app", &[dep], "main.silt", "fn main() {}\n");
        fs::write(app.join("silt.lock"), &lockfile).unwrap();

        let out = silt(&ws, &app, &[subcommand]);

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(1),
            "silt {subcommand}: expected a clean error exit with code 1; stderr={stderr}"
        );
        assert!(
            stderr.contains("error: invalid lockfile") && stderr.contains("silt.lock"),
            "silt {subcommand}: expected a lockfile error; stderr={stderr}"
        );
        assert!(
            stderr.contains("`remote`")
                && stderr.contains("invalid `rev`")
                && stderr.contains("../../../../x"),
            "silt {subcommand}: the error must name the package and the value; stderr={stderr}"
        );
        assert!(
            !stderr.contains("panicked at"),
            "silt {subcommand}: silt panicked instead of reporting an error; stderr={stderr}"
        );
        assert_eq!(
            fs::read_to_string(app.join("silt.lock")).unwrap(),
            lockfile,
            "silt {subcommand}: a rejected lockfile must not be rewritten"
        );

        let _ = fs::remove_dir_all(&ws);
    }
}
