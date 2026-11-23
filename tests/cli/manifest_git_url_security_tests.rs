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

/// The git cache of the workspace `ws`, whose cache directory the
/// tests point `XDG_CACHE_HOME` (Unix) and `LOCALAPPDATA` (Windows) at:
/// `silt/git` under it on Unix, `silt\cache\git` on Windows (see
/// `git::cache_dir`). (Its one user runs on Unix only.)
#[cfg(unix)]
fn git_cache(ws: &Path) -> PathBuf {
    let cache = ws.join("cache").join("silt");
    if cfg!(windows) {
        cache.join("cache").join("git")
    } else {
        cache.join("git")
    }
}

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
        stderr.contains("invalid manifest"),
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
        write_package(&inner, "inner", &[evil], "lib.silt", "pub fn one() { 1 }\n");
        let path_dep = "inner = { path = \"../inner\" }".to_string();
        write_package(&app, "app", &[path_dep], "main.silt", "fn main() {}\n");

        let out = silt(&ws, &app, &[subcommand]);

        let context = format!("silt {subcommand} (transitive)");
        assert_marker_absent(&marker, &context, &out);
        assert_clean_url_rejection(&out, &context);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Manifest paths are shown relative to the working directory with
        // `/` on every platform.
        assert!(
            stderr.contains("inner/silt.toml"),
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
        stderr.contains("error: silt add: --git URL `")
            && stderr.contains("` must not start with `-`")
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
const RULE_URL: &str = "is not a valid URL: ";

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
        // A URL of `https`, `http`, `ssh` or `git` is read as a URL: a
        // port, a host or an escape that is none is refused here, not by
        // git later.
        ("https://127.0.0.1:8a/pkg.git", RULE_URL),
        ("http://127.0.0.1:99999/pkg.git", RULE_URL),
        ("ssh://git@exa%zzmple/pkg.git", RULE_URL),
        ("git://0x7f.1/pkg.git", RULE_URL),
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

/// A value git would read in another way than it reads is refused
/// before git runs: a URL of any scheme with its `//` left out (git takes
/// `ssh:`, `git:`, `file:` for a host), and the scp form with a password
/// (git takes the user for the host and sends the password to it as a
/// path), which the message hides. `git` is not on PATH, so on the
/// unfixed code these reached `git` only to find it missing: no network.
/// The same scp form with a user alone reaches git. FAIL before: each was
/// taken and reached git.
#[test]
fn values_git_reads_otherwise_are_refused_before_git_runs() {
    const RULE_SLASHES: &str = "is a URL with its `//` left out: write `://` after the scheme";
    const RULE_SCP_PASSWORD: &str = "the form `user@host:path` holds no password \
        (git would take what stands before the first `:` for the host): \
        leave the password out, and ssh asks for it or uses a key; \
        a path with an `@` in it is written with the user in front (`git@host:dir/a@b`) \
        or as an `ssh://` URL";
    const RULE_WINDOWS_PATH: &str = "is a Windows path, which git on this system would read \
        as a host and a path: write a path of this system, starting with `/`, `./` or `../`, \
        or a URL";
    const RULE_UNC_PATH: &str = "is a Windows path, which git on this system would read \
        as a file name of the current directory: write a path of this system, \
        starting with `/`, `./` or `../`, or a URL";
    const RULE_LETTER_SCHEME: &str = "is not a recognised git URL form: the scheme `c://` \
        is not accepted, use `ssh://` or `https://` instead";
    let cases: &[(&str, &str, &str)] = &[
        // With the one way to write it, when there is one, its user
        // hidden too.
        (
            "file:/srv/repo.git",
            "file:/srv/repo.git",
            "is a URL with its `//` left out: write it `file:///srv/repo.git`",
        ),
        ("FILE:srv/repo.git", "FILE:srv/repo.git", RULE_SLASHES),
        (
            "ssh:team/pkg.git",
            "ssh:team/pkg.git",
            "is a URL with its `//` left out: write it `ssh://team/pkg.git`",
        ),
        (
            "ssh:/git@example.com/team/pkg.git",
            "***@example.com/team/pkg.git",
            "is a URL with its `//` left out: write it `ssh://***@example.com/team/pkg.git`",
        ),
        (
            "git:team/pkg.git",
            "git:team/pkg.git",
            "is a URL with its `//` left out: write it `git://team/pkg.git`",
        ),
        (
            "user:S3CR3TXYZ@example.com:team/pkg.git",
            "***@example.com:team/pkg.git",
            RULE_SCP_PASSWORD,
        ),
        // The first `:` before the first `@`, whatever stands between
        // them, is a password: one with an `@` in it, one before a path with
        // an `@`, one whose path has a `:`, one with no path `:` at all (a
        // URL with its scheme forgotten), ones with a `/` in them, a path
        // with an `@` and no user in front (the same shape), and
        // one-character users with a second `:` (no drive on Windows
        // either). A refused value is hidden before its last `@`, in the
        // message and the line of silt.toml. FAIL before (the rows with a
        // `/` before the first `@`): taken, shown whole and given to git.
        (
            "user:S3CR@3TXYZ@example.com:team/pkg.git",
            "***@example.com:team/pkg.git",
            RULE_SCP_PASSWORD,
        ),
        (
            "user:S3CR3TXYZ@example.com:team/pkg@v2.git",
            "***@v2.git",
            RULE_SCP_PASSWORD,
        ),
        ("host:S3CR3TXYZ@b:c", "***@b:c", RULE_SCP_PASSWORD),
        (
            "user:S3CR3TXYZ@example.com",
            "***@example.com",
            RULE_SCP_PASSWORD,
        ),
        (
            "user:S3CR3TXYZ@example.com/team/pkg.git",
            "***@example.com/team/pkg.git",
            RULE_SCP_PASSWORD,
        ),
        ("example.com:S3CR3TXYZ@v2", "***@v2", RULE_SCP_PASSWORD),
        (
            "user:/S3CR3TXYZ@example.com:team/pkg.git",
            "***@example.com:team/pkg.git",
            RULE_SCP_PASSWORD,
        ),
        (
            "user:S3CR/3TXYZ@example.com:team/pkg.git",
            "***@example.com:team/pkg.git",
            RULE_SCP_PASSWORD,
        ),
        (
            "user:/S3CR3TXYZ@example.com/team/pkg.git",
            "***@example.com/team/pkg.git",
            RULE_SCP_PASSWORD,
        ),
        (
            "u:/S3CR3TXYZ@example.com:team/p",
            "***@example.com:team/p",
            RULE_SCP_PASSWORD,
        ),
        ("example.com:team/S3CR3TXYZ@v2", "***@v2", RULE_SCP_PASSWORD),
        ("example.com:dir/a@b", "***@b", RULE_SCP_PASSWORD),
        (
            "example.com:/srv/team@work/pkg.git",
            "***@work/pkg.git",
            RULE_SCP_PASSWORD,
        ),
        ("example.com:./pkg@v2", "***@v2", RULE_SCP_PASSWORD),
        (
            "u:S3CR3TXYZ@example.com:team/pkg.git",
            "***@example.com:team/pkg.git",
            RULE_SCP_PASSWORD,
        ),
        ("C:\\repo\\S3CR3TXYZ@x:p", "***@x:p", RULE_SCP_PASSWORD),
        // A scheme of one letter with a second `:`: no drive on Windows
        // either, a scheme silt does not take, hidden. FAIL before: shown
        // whole.
        (
            "C://user:S3CR3TXYZ@example.com/team/pkg.git",
            "***@example.com/team/pkg.git",
            "is not a recognised git URL form: the scheme `C://` is not accepted, \
             use `ssh://` or `https://` instead",
        ),
        (
            "u://us:S3CR3TXYZ@h/p",
            "***@h/p",
            "is not a recognised git URL form: the scheme `u://` is not accepted, \
             use `ssh://` or `https://` instead",
        ),
        // Refused for its empty user: hidden all the same.
        (
            "@user:S3CR3TXYZ@example.com:p",
            "***@example.com:p",
            "is not a recognised git URL form",
        ),
    ];
    let ws = fresh_workspace("read_otherwise");
    let no_git = ws.join("no-git");
    fs::create_dir_all(&no_git).unwrap();
    let run = |cwd: &Path, args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
        cmd.args(args)
            .current_dir(cwd)
            .env("XDG_CACHE_HOME", ws.join("cache"))
            .env("LOCALAPPDATA", ws.join("cache"))
            .env("PATH", &no_git);
        isolate_git(&mut cmd, &ws);
        cmd.output().expect("failed to spawn silt binary")
    };
    for &(url, shown, rule) in cases {
        let app = ws.join("app");
        let _ = fs::remove_dir_all(&app);
        write_package(
            &app,
            "app",
            &[git_dep("evil", url)],
            "main.silt",
            "fn main() {}\n",
        );
        let adder = ws.join("adder");
        let _ = fs::remove_dir_all(&adder);
        write_package(&adder, "adder", &[], "main.silt", "fn main() {}\n");
        for (cwd, args) in [
            (&app, vec!["check"]),
            (
                &adder,
                vec!["add", "evil", "--git", url, "--branch", "main"],
            ),
        ] {
            let out = run(cwd, &args);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let context = format!("{args:?} with {url:?}");
            assert_eq!(out.status.code(), Some(1), "{context}: {stderr}");
            // (A rule that writes the URL out lists no forms.)
            let said = match rule.contains("write it `") {
                true => format!(" {rule}\n"),
                false => format!(" {rule}; expected "),
            };
            assert!(
                stderr.contains(&format!("`{shown}`")) && stderr.contains(&said),
                "{context}: {stderr}"
            );
            assert!(!stderr.contains("3TX"), "{context}: {stderr}");
        }
    }
    // A drive without a second `:`: on Windows a local path, which git is
    // given; elsewhere the short form with a password, or a Windows path (a
    // letter, `:`, then `\` or `/`; or one that starts with `\\`), refused
    // and hidden before its last `@`, by `check` and by `add`. FAIL before
    // (the Windows paths): refused as a password (`C:\repo\S3CR3TXYZ@x`), or
    // taken and given to git.
    for (url, shown, rule) in [
        (
            "u:S3CR3TXYZ@example.com/p",
            "***@example.com/p",
            RULE_SCP_PASSWORD,
        ),
        ("C:\\repo\\S3CR3TXYZ@x", "***@x", RULE_WINDOWS_PATH),
        (
            "u:\\S3CR3TXYZ@example.com/team/pkg.git",
            "***@example.com/team/pkg.git",
            RULE_WINDOWS_PATH,
        ),
        (
            "C:\\Users\\me\\lib",
            "C:\\Users\\me\\lib",
            RULE_WINDOWS_PATH,
        ),
        ("C:/repo/x", "C:/repo/x", RULE_WINDOWS_PATH),
        ("C:/repo/x@h", "***@h", RULE_WINDOWS_PATH),
        ("\\\\server\\share\\x@h", "***@h", RULE_UNC_PATH),
        // A `:` with no `/` before it: git reads the short form.
        ("\\\\h:p", "\\\\h:p", RULE_WINDOWS_PATH),
        // One letter, `:` and `//`: a drive on Windows, elsewhere a URL of
        // the scheme `c` to git, refused as one and hidden. FAIL before:
        // shown whole.
        ("c://S3CR3TXYZ@h", "***@h", RULE_LETTER_SCHEME),
        (
            "C://S3CR3TXYZ@example.com/team/pkg.git",
            "***@example.com/team/pkg.git",
            "is not a recognised git URL form: the scheme `C://` is not accepted, \
             use `ssh://` or `https://` instead",
        ),
    ] {
        // (On Windows a UNC path names a host of the network: not run there.)
        if cfg!(windows) && url.starts_with(r"\\") {
            continue;
        }
        let app = ws.join("app");
        let _ = fs::remove_dir_all(&app);
        write_package(
            &app,
            "app",
            &[git_dep("evil", url)],
            "main.silt",
            "fn main() {}\n",
        );
        let adder = ws.join("adder");
        let _ = fs::remove_dir_all(&adder);
        write_package(&adder, "adder", &[], "main.silt", "fn main() {}\n");
        for (cwd, args) in [
            (&app, vec!["check"]),
            (
                &adder,
                vec!["add", "evil", "--git", url, "--branch", "main"],
            ),
        ] {
            let out = run(cwd, &args);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let context = format!("{args:?} with {url:?}");
            if cfg!(windows) {
                assert!(
                    !stderr.contains("invalid git URL") && !stderr.contains("--git URL `"),
                    "{context}: {stderr}"
                );
            } else {
                assert_eq!(out.status.code(), Some(1), "{context}: {stderr}");
                assert!(
                    stderr.contains(&format!("`{shown}`"))
                        && stderr.contains(&format!(" {rule}; expected ")),
                    "{context}: {stderr}"
                );
                assert!(!stderr.contains("3TX"), "{context}: {stderr}");
            }
        }
    }
    // A user alone, and a path with an `@` after the user, are the scp form
    // git reads as written: they are taken, shown whole (the line of
    // silt.toml too), and go to git, which is not there. FAIL before (the
    // `@` of a path): refused, or shown `***@v2.git`. The row with the
    // marker in it is shown whole ON PURPOSE: as git reads it, it holds no
    // password but a user and an `@` of the path (main hid
    // `git@example.com:S3CR3TXYZ@other:p`).
    for url in [
        "git@example.com:team/pkg.git",
        "git@example.com:team/pkg@v2.git",
        "git@example.com:dir/a@b",
        "git@example.com:S3CR3TXYZ@other:p",
    ] {
        let app = ws.join("app");
        let _ = fs::remove_dir_all(&app);
        write_package(
            &app,
            "app",
            &[git_dep("lib", url)],
            "main.silt",
            "fn main() {}\n",
        );
        let out = run(&app, &["check"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("invalid git URL")
                && stderr.contains(&format!("(`{url}`, branch = `main`)"))
                && stderr.contains(&format!("git = \"{url}\"")),
            "{url}: {stderr}"
        );
        assert!(!stderr.contains("***"), "{url}: {stderr}");
    }
    let _ = fs::remove_dir_all(&ws);
}

/// A git value written as a multi-line string has its user and password
/// hidden on every line of it that an error shows: the caret can stand
/// on a later line of the dependency (`version`, `path` and `registry`
/// are checked before the URL). `git` is not on PATH. FAIL before: the
/// second line was shown as written.
#[test]
fn a_multi_line_git_value_is_hidden_on_each_of_its_lines() {
    let ws = fresh_workspace("multi_line");
    let no_git = ws.join("no-git");
    fs::create_dir_all(&no_git).unwrap();
    for dep in [
        "lib = { git = \"\"\"\\\nuser:S3CR3TXYZ@example.com:team/pkg.git\"\"\", version = \"1\" }",
        "lib = { git = \"\"\"\nhttps://user:S3CR 3TXYZ@example.com/pkg.git\"\"\", path = \"x\" }",
        "lib = { git = '''\nuser:S3CR3TXYZ@example.com:team/pkg.git''', registry = \"r\" }",
        // A parse error on the second line.
        "lib = { git = \"\"\"\\\nuser:S3CR3TXYZ@example.com:team/pkg.git\"\"\" junk }",
    ] {
        let app = ws.join("app");
        let _ = fs::remove_dir_all(&app);
        write_package(
            &app,
            "app",
            &[dep.to_string()],
            "main.silt",
            "fn main() {}\n",
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
        cmd.arg("check")
            .current_dir(&app)
            .env("XDG_CACHE_HOME", ws.join("cache"))
            .env("LOCALAPPDATA", ws.join("cache"))
            .env("PATH", &no_git);
        isolate_git(&mut cmd, &ws);
        let out = cmd.output().expect("failed to spawn silt binary");
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.status.code(), Some(1), "{dep}: {all}");
        assert!(all.contains("@example.com"), "{dep}: {all}");
        for piece in ["S3C", "3CR", "CR3", "R3T", "3TX", "TXY", "XYZ"] {
            assert!(!all.contains(piece), "{dep}: `{piece}` is shown: {all}");
        }
    }
    let _ = fs::remove_dir_all(&ws);
}

/// A `git` key at the start of a line, as a `[dependencies.lib]` table
/// writes it, is the key `git` for the shown line of silt.toml: its
/// value's user and password are hidden there, whatever the key's
/// spelling, with `\n` and `\r\n` line ends. `git` is not on PATH.
/// FAIL before: the line was shown as written.
#[test]
fn a_git_key_at_the_start_of_a_line_is_hidden_in_the_table_form() {
    let ws = fresh_workspace("key_at_line_start");
    let no_git = ws.join("no-git");
    fs::create_dir_all(&no_git).unwrap();
    for key in ["git", "\"git\"", "'git'", "\"g\\u0069t\""] {
        for url in [
            "user:S3CR3TXYZ@example.com:team/pkg.git",
            "https://user:S3CR 3TXYZ@example.com/pkg.git",
        ] {
            for eol in ["\n", "\r\n"] {
                let app = ws.join("app");
                let _ = fs::remove_dir_all(&app);
                fs::create_dir_all(app.join("src")).unwrap();
                fs::write(app.join("src").join("main.silt"), "fn main() {}\n").unwrap();
                let manifest = [
                    "[package]",
                    "name = \"app\"",
                    "version = \"0.1.0\"",
                    "",
                    "[dependencies.lib]",
                    &format!("{key} = {}", toml_str(url)),
                    "tag = \"v1\"",
                    "",
                ]
                .join(eol);
                fs::write(app.join("silt.toml"), manifest).unwrap();
                let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
                cmd.arg("check")
                    .current_dir(&app)
                    .env("XDG_CACHE_HOME", ws.join("cache"))
                    .env("LOCALAPPDATA", ws.join("cache"))
                    .env("PATH", &no_git);
                isolate_git(&mut cmd, &ws);
                let out = cmd.output().expect("failed to spawn silt binary");
                let stderr = String::from_utf8_lossy(&out.stderr);
                let context = format!("{key} = {url:?} ({eol:?})");
                assert_eq!(out.status.code(), Some(1), "{context}: {stderr}");
                assert!(
                    stderr.contains(&format!("{key} = \"")),
                    "{context}: {stderr}"
                );
                for piece in ["S3C", "3CR", "CR3", "R3T", "3TX", "TXY", "XYZ"] {
                    assert!(!stderr.contains(piece), "{context}: `{piece}`: {stderr}");
                }
            }
        }
    }
    let _ = fs::remove_dir_all(&ws);
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
        if key == "rev" {
            // A rev that is not a commit id is rejected by the manifest.
            assert!(
                stderr.contains("error[package]: invalid manifest")
                    && stderr.contains(&format!("got `{escaped}`")),
                "{key}: expected a manifest error showing the value, escaped; stderr={stderr}"
            );
        } else {
            assert!(
                stderr.contains("error[package]: git dependency `remote`") && stderr.contains(url),
                "{key}: expected a git dependency error naming the URL; stderr={stderr}"
            );
            assert!(
                stderr.contains(&format!("{key} = `{escaped}`")),
                "{key}: the value must be shown, escaped; stderr={stderr}"
            );
        }
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
    write_package(
        repo,
        "locallib",
        &[],
        "lib.silt",
        "pub fn answer() { 42 }\n",
    );
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
/// `head` and records the URL as `locked_url`, the checkout is in the
/// workspace's own cache, and the dependency's code runs.
#[cfg(unix)]
fn assert_git_dependency_resolves(ws: &Path, url: &str, locked_url: &str, head: &str) {
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
        lock.contains(&format!("git = {}", toml_str(locked_url))),
        "the lockfile must record the dependency's URL as {locked_url:?}:\n{lock}"
    );

    // The checkout landed in the workspace's cache, under the commit.
    let cache_root = git_cache(ws);
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
    assert_git_dependency_resolves(&ws, &url, &url, &head);

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
    assert_git_dependency_resolves(&ws, &url, &url, &head);

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
    assert_git_dependency_resolves(&ws, &url, &url, &head);

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
    assert_git_dependency_resolves(&ws, &url, &url, &head);

    let _ = fs::remove_dir_all(&ws);
}

/// A repository given as a relative path resolves. There is no `file://`
/// spelling of a relative path. It is relative to the directory of the
/// manifest, here the package root `<ws>/app`, and the lockfile records
/// it made absolute.
#[cfg(unix)]
#[test]
fn relative_path_git_dependency_resolves() {
    let ws = fresh_workspace("rel_path");
    if !can_build_a_repository(&ws) {
        return;
    }
    let repo = ws.join("remote");
    let head = create_locallib_repository(&ws, &repo);

    // The lock names it as the manifest does.
    assert_git_dependency_resolves(&ws, "../remote", "../remote", &head);

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
            stderr.contains("error[package]: invalid lockfile")
                && stderr.contains("--> silt.lock:"),
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

// ── A user and password in a git URL ─────────────────────────────────

/// A git dependency's URL with a user and a password in it is shown with
/// `***` in their place by every message of `check` (the diagnostic, the
/// git command line it quotes, git's output, the JSON form), and the line
/// of silt.toml under the caret has as many `*`s as they had characters.
/// silt.toml keeps the URL as written. `silt add` with such a URL says
/// the same. FAIL on 0.43.0: the user and the password were printed
/// whole, twice. (A loopback port nothing listens on: no network.)
#[test]
fn a_git_url_s_user_and_password_are_in_no_message() {
    let ws = fresh_workspace("userinfo");
    let app = ws.join("app");
    let url = "https://MARKUSER:MARKPASS@127.0.0.1:9/remote.git";
    write_package(
        &app,
        "app",
        &[git_dep("remote", url)],
        "main.silt",
        "fn main() {}\n",
    );
    for args in [&["check"][..], &["check", "--format", "json"], &["update"]] {
        let out = silt(&ws, &app, args);
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_ne!(out.status.code(), Some(0), "{args:?}: {all}");
        assert!(
            !all.contains("MARKUSER") && !all.contains("MARKPASS"),
            "{args:?}: the user or the password is shown: {all}"
        );
        assert!(
            all.contains("***@127.0.0.1:9/remote.git"),
            "{args:?}: {all}"
        );
    }
    let out = silt(&ws, &app, &["check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let masked = format!(
        "https://{}@127.0.0.1:9",
        "*".repeat("MARKUSER:MARKPASS".len())
    );
    assert!(
        stderr.contains(&masked),
        "the excerpt keeps its columns: {stderr}"
    );
    let manifest = fs::read_to_string(app.join("silt.toml")).unwrap();
    assert!(
        manifest.contains(url),
        "silt.toml keeps the URL: {manifest}"
    );

    let out = silt(
        &ws,
        &app,
        &["add", "other", "--git", url, "--branch", "main"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        !stderr.contains("MARKUSER") && !stderr.contains("MARKPASS"),
        "silt add shows the user or the password: {stderr}"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// What `silt check`, `silt update` and `silt add` print, all of it,
/// shows nothing of the password of a git dependency's URL, on every
/// system: a password with a space or a tab (refused, the line of
/// silt.toml under the caret masked to its closing quote; or taken, in a
/// `file://` URL, and shown in the git command line and git's output),
/// the scp form with a password (`user:word@host:path`, refused on every
/// system: `u:word@host:path` is no drive on Windows; `u:word@host/path`
/// is one there, and git does not reach it by ssh), a URL with its `//`
/// left out (refused), in a double- and a single-quoted string, the key
/// `git` also written with an escape (`"g\u0069t"`), in an inline table
/// and at the start of a line of a `[dependencies.lib]` table.
///
/// The real git runs, and no network is used: an `http` or `https` URL
/// names a local port where nothing listens, an ssh form reaches a
/// stand-in for ssh (`GIT_SSH_COMMAND`, which git runs through its own
/// shell on every system) that prints what git asked it to reach, and
/// `GIT_TRACE` makes git repeat its arguments, the URL whole, on stderr.
/// With git on PATH, `silt check` of the https and the ssh:// row must
/// show git's trace and the stand-in's line, the password starred: so
/// the run proves that git's output was passed on. Without git every
/// value still fails, and nothing may show.
/// FAIL before: the shown line held the password with a space; the scp
/// form and the mistyped URL were shown whole; the command line and
/// `cannot reach` cut a `file://` URL at its space.
#[test]
fn no_output_of_a_command_shows_the_password_of_a_git_url() {
    let marker = "S3CR3TXYZ";
    /// What a value's output must show besides no piece of the marker.
    enum Expect {
        /// Nothing more.
        NoMore,
        /// silt refuses it, on every system.
        Refused,
        /// A drive on Windows, given to git as a local path, never to ssh;
        /// refused elsewhere.
        DriveOnWindows,
        /// With git on PATH, `silt check` shows these texts of git's.
        GitSays(&'static [&'static str]),
    }
    // The values, and what else their output must show (see `Expect`).
    let urls = [
        (
            "https://user:S3CR 3TXYZ@127.0.0.1:1/pkg.git".to_string(),
            Expect::NoMore,
        ),
        (
            "https://user:S3CR\t3TXYZ@127.0.0.1:1/pkg.git".to_string(),
            Expect::NoMore,
        ),
        (
            "http://S3CR 3TXYZ:pw@127.0.0.1:1/pkg.git".to_string(),
            Expect::NoMore,
        ),
        (
            format!("https://user:{marker}@127.0.0.1:1/pkg.git"),
            Expect::GitSays(&["trace: "]),
        ),
        (
            format!("ssh://git:{marker}@127.0.0.1:team/pkg.git"),
            Expect::NoMore,
        ),
        (
            format!("ssh://git:{marker}@127.0.0.1/team/pkg.git"),
            Expect::GitSays(&["ssh asked for:", "***@127.0.0.1"]),
        ),
        (
            format!("user:{marker}@127.0.0.1:team/pkg.git"),
            Expect::NoMore,
        ),
        (
            format!("user:{marker}@127.0.0.1:team/pkg@v2.git"),
            Expect::NoMore,
        ),
        (
            "user:S3CR@3TXYZ@127.0.0.1:team/pkg.git".to_string(),
            Expect::NoMore,
        ),
        (format!("host:{marker}@b:c"), Expect::NoMore),
        (format!("@user:{marker}@127.0.0.1:p"), Expect::NoMore),
        (format!("user:{marker}@127.0.0.1"), Expect::NoMore),
        (
            format!("user:{marker}@127.0.0.1/team/pkg.git"),
            Expect::NoMore,
        ),
        (format!("example.com:{marker}@v2"), Expect::NoMore),
        // A `/` between the first `:` and the first `@`, on every system.
        // FAIL before: taken, and shown whole.
        (
            format!("user:/{marker}@127.0.0.1:team/pkg.git"),
            Expect::Refused,
        ),
        (
            "user:S3CR/3TXYZ@127.0.0.1:team/pkg.git".to_string(),
            Expect::Refused,
        ),
        (
            format!("user:/{marker}@127.0.0.1/team/pkg.git"),
            Expect::Refused,
        ),
        (format!("127.0.0.1:team/{marker}@v2"), Expect::Refused),
        // A scheme of one letter with a second `:`, on every system. FAIL
        // before: shown whole.
        (
            format!("C://user:{marker}@127.0.0.1/team/pkg.git"),
            Expect::Refused,
        ),
        (
            format!("ssh:user:{marker}@127.0.0.1:team/pkg.git"),
            Expect::NoMore,
        ),
        (
            format!("git:user:{marker}@127.0.0.1:team/pkg.git"),
            Expect::NoMore,
        ),
        (
            format!("https:/user:{marker}@127.0.0.1:1/pkg.git"),
            Expect::NoMore,
        ),
        (
            format!("https:user:{marker}@127.0.0.1:1/pkg.git"),
            Expect::NoMore,
        ),
        (
            format!("HTTP:/user:{marker}@127.0.0.1:1/pkg.git"),
            Expect::NoMore,
        ),
        (
            "https://user:S3CR/3TXYZ@127.0.0.1:1/pkg.git".to_string(),
            Expect::NoMore,
        ),
        (
            "https://user:S3CR\"3TXYZ@127.0.0.1:1/pkg.git".to_string(),
            Expect::NoMore,
        ),
        // Accepted with a space in the password: they reach git.
        (
            "file://user:S3CR 3TXYZ@127.0.0.1/srv/pkg.git".to_string(),
            Expect::NoMore,
        ),
        (
            "file://S3CR 3TXYZ@127.0.0.1/srv/pkg.git".to_string(),
            Expect::NoMore,
        ),
        // A one-letter user with a second `:`: the short form on every
        // system, Windows too (no drive there), refused; without one a
        // drive on Windows, which git does not reach by ssh.
        (
            format!("u:{marker}@127.0.0.1:team/pkg.git"),
            Expect::Refused,
        ),
        (
            "u:S3CR 3TXYZ@127.0.0.1:team/pkg.git".to_string(),
            Expect::Refused,
        ),
        (
            format!("u:{marker}@127.0.0.1/team/pkg.git"),
            Expect::DriveOnWindows,
        ),
    ];
    let pieces: Vec<&str> = (0..=marker.len() - 3).map(|i| &marker[i..i + 3]).collect();
    // Whether git is there: only then can its output be asked for.
    let git_is_there = Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    let ws = fresh_workspace("password_output");
    for (url, expect) in &urls {
        // The key `git`, also written with an escape (`"g\u0069t"`), in
        // an inline table, and at the start of a line of a
        // `[dependencies.lib]` table.
        let mut entries = vec![
            ("git", toml_str(url), false),
            (r#""g\u0069t""#, toml_str(url), false),
            ("git", toml_str(url), true),
            (r#""g\u0069t""#, toml_str(url), true),
        ];
        if !url.contains(['\'', '\t']) {
            entries.push(("git", format!("'{url}'"), false));
        }
        for (key, value, table) in entries {
            let app = ws.join("app");
            let _ = fs::remove_dir_all(&app);
            if table {
                write_package(&app, "app", &[], "main.silt", "fn main() {}\n");
                let manifest = fs::read_to_string(app.join("silt.toml")).unwrap();
                fs::write(
                    app.join("silt.toml"),
                    format!("{manifest}\n[dependencies.lib]\n{key} = {value}\ntag = \"v1\"\n"),
                )
                .unwrap();
            } else {
                let dep = format!("lib = {{ {key} = {value}, tag = \"v1\" }}");
                write_package(&app, "app", &[dep], "main.silt", "fn main() {}\n");
            }
            // `silt add` writes the key itself: once, with the key `git`.
            // The other spellings and forms are read by the one rule of the
            // shown line.
            let first = key == "git" && !table && value.starts_with('"');
            let commands: &[&str] = if first {
                &["check", "update", "add"]
            } else {
                &["check"]
            };
            for &command in commands {
                let adder = ws.join("adder");
                let _ = fs::remove_dir_all(&adder);
                write_package(&adder, "adder", &[], "main.silt", "fn main() {}\n");
                let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
                match command {
                    "add" => cmd
                        .args(["add", "lib", "--git", url, "--tag", "v1"])
                        .current_dir(&adder),
                    _ => cmd.arg(command).current_dir(&app),
                };
                cmd.env("XDG_CACHE_HOME", ws.join("cache"))
                    .env("LOCALAPPDATA", ws.join("cache"))
                    .env("GIT_SSH_COMMAND", "echo >&2 ssh asked for:")
                    .env("GIT_SSH_VARIANT", "ssh")
                    .env("GIT_TRACE", "1");
                for var in [
                    "GIT_SSH",
                    "http_proxy",
                    "https_proxy",
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "ALL_PROXY",
                    "all_proxy",
                ] {
                    cmd.env_remove(var);
                }
                isolate_git(&mut cmd, &ws);
                let out = cmd.output().expect("failed to spawn silt binary");
                let all = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                let context = format!("{command} {key} = {value} (table: {table})");
                assert_ne!(out.status.code(), Some(0), "{context}: {all}");
                for piece in &pieces {
                    assert!(!all.contains(piece), "{context}: `{piece}` is shown: {all}");
                }
                // `check` and `update` say `invalid git URL`, `add`
                // `--git URL ...`.
                let refused = all.contains("invalid git URL") || all.contains("--git URL `");
                match expect {
                    Expect::NoMore => {}
                    Expect::Refused => assert!(refused, "{context}: {all}"),
                    Expect::DriveOnWindows => {
                        assert_eq!(refused, !cfg!(windows), "{context}: {all}");
                        assert!(!all.contains("ssh asked for:"), "{context}: {all}");
                    }
                    // What shows that git ran and its output was passed on
                    // and starred: on every system with git, CI's Windows too.
                    Expect::GitSays(texts) => {
                        if git_is_there && command == "check" {
                            for text in *texts {
                                assert!(all.contains(text), "{context}: no `{text}`: {all}");
                            }
                        }
                    }
                }
            }
        }
    }
    let _ = fs::remove_dir_all(&ws);
}

/// Under an error, the line of silt.toml hides the user and password of
/// the `git` value only: another value, a key or a comment that holds an
/// address is shown as written. `silt add` quotes the hidden URL whole,
/// its backticks kept. FAIL before: `note = "************@example.com"`,
/// `# ********@example.com`, and `cannot reach ***@...`` without its
/// opening backtick.
#[cfg(unix)]
#[test]
fn only_a_git_url_is_hidden_in_what_a_command_prints() {
    use std::os::unix::fs::PermissionsExt;
    let ws = fresh_workspace("hidden_text");
    let bin = ws.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let git = bin.join("git");
    fs::write(&git, "#!/bin/sh\necho \"fatal: '$*'\" >&2\nexit 128\n").unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = |cwd: &Path, args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
        cmd.args(args)
            .current_dir(cwd)
            .env("XDG_CACHE_HOME", ws.join("cache"))
            .env("PATH", &path);
        isolate_git(&mut cmd, &ws);
        let out = cmd.output().expect("failed to spawn silt binary");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };

    let app = ws.join("app");
    let dep = "lib = { git = \"user:S3CR3TXYZ@example.com:team/pkg.git\", \
               note = \"Contact: bob@example.com\", tag = \"v1\" } # Name:bob@example.com";
    write_package(
        &app,
        "app",
        &[dep.to_string()],
        "main.silt",
        "fn main() {}\n",
    );
    let stderr = run(&app, &["check"]);
    let shown = "lib = { git = \"**************@example.com:team/pkg.git\", \
                 note = \"Contact: bob@example.com\", tag = \"v1\" } # Name:bob@example.com";
    assert!(stderr.contains(shown), "{stderr}");

    let other = ws.join("other");
    write_package(&other, "other", &[], "main.silt", "fn main() {}\n");
    let stderr = run(
        &other,
        &[
            "add",
            "lib",
            "--git",
            "user:S3CR3TXYZ@example.com:team/pkg.git",
            "--tag",
            "v1",
        ],
    );
    assert!(
        stderr.contains("`***@example.com:team/pkg.git`") && !stderr.contains("S3C"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(&ws);
}
