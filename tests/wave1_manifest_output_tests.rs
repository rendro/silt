//! What silt prints when a manifest, a lockfile or git hands it text it
//! cannot trust.
//!
//! A `silt.toml` can be a dependency's, or a dependency's dependency's,
//! and a `silt.lock` can be edited by hand. A value read from either
//! must not reach the terminal as it is: a line break in it starts a
//! line that reads like one of silt's own, and an escape character
//! drives the terminal. The same goes for what git prints, because a
//! remote can send text.
//!
//! Four groups of tests:
//!
//!   1. every manifest and lockfile field that is echoed in an error is
//!      shown escaped, by one rule;
//!   2. the git URL rule, as a table of accepted and rejected URLs per
//!      form;
//!   3. every line of git's own output is marked as git's;
//!   4. the error for an unaccepted URL scheme names the replacement,
//!      and the git command is shown as it was run.
//!
//! The tests are behavioural: each one runs the compiled `silt` binary
//! on a package in a fresh temporary directory and asserts on the exit
//! status, stdout and stderr. The planted values are harmless text: a
//! line break followed by words shaped like a silt error line, an
//! escape character, and an invisible character.
//!
//! Hermeticity: every `silt` invocation gets a git checkout cache
//! inside its own workspace (`XDG_CACHE_HOME`, and `LOCALAPPDATA` for
//! Windows) and an empty git configuration, and every process has a
//! timeout. Nothing uses the network: a URL is either checked by a
//! command that never runs git (`silt update <undeclared name>`), or it
//! is a `file://` URL or a local path.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// ── The planted value ─────────────────────────────────────────────────

/// A line break followed by words shaped like a silt error line, an
/// escape character (the start of "erase line") and U+202E, which
/// reverses the direction of the text after it.
const HOSTILE: &str = "x\nerror: FORGED all checks passed\u{1b}[2K\u{202e}";

/// [`HOSTILE`] as silt must show it.
const HOSTILE_ESCAPED: &str = "x\\nerror: FORGED all checks passed\\u{1b}[2K\\u{202e}";

/// What a line of output starts with if [`HOSTILE`] forged it.
const FORGED_LINE: &str = "error: FORGED";

/// Invisible and direction-control characters that an earlier, explicit
/// list let through.
const ONCE_MISSED_INVISIBLE: [char; 11] = [
    '\u{061C}',
    '\u{180E}',
    '\u{FFF9}',
    '\u{FFFB}',
    '\u{E0001}',
    '\u{E0061}',
    '\u{1D173}',
    '\u{034F}',
    '\u{3164}',
    '\u{FE0F}',
    '\u{2065}',
];

/// Spaces other than U+0020 that were rejected in a URL, but echoed as
/// they are.
const UNUSUAL_SPACES: [char; 3] = ['\u{1680}', '\u{2003}', '\u{3000}'];

/// Non-ASCII punctuation and symbols: an en dash, a copyright sign, an
/// ellipsis and a full-width parenthesis.
const NON_ASCII_PUNCTUATION: [char; 4] = ['\u{2013}', '\u{a9}', '\u{2026}', '\u{ff08}'];

// ── Running processes ─────────────────────────────────────────────────

/// Upper bound for one process. A process that exceeds it is killed and
/// reported as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

static COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct Outcome {
    /// Exit status; `None` if the process was ended by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// True if the process exceeded [`RUN_TIMEOUT`] and was killed.
    timed_out: bool,
}

/// A fresh workspace directory, unique per call so parallel tests never
/// share state. Holds an empty `gitconfig` for [`isolate_git`].
fn fresh_workspace(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("silt_wave1_manifest_output_{tag}_{pid}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("gitconfig"), "").unwrap();
    dir
}

/// Detach `cmd` (and any `git` it spawns) from the machine's git setup:
/// no system or user configuration, no credential prompt, no repository
/// inherited from the environment of the test runner, and messages in
/// one language.
fn isolate_git(cmd: &mut Command, ws: &Path) {
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", ws.join("gitconfig"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
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

/// Run `cmd` to its end, or kill it after [`RUN_TIMEOUT`]. `None` if
/// the program could not be started.
///
/// Output goes to files in the workspace rather than to pipes, so a
/// child that is killed cannot leave the test blocked on a read. The
/// text is returned as the process wrote it, carriage returns included.
fn try_run_with_timeout(cmd: &mut Command, ws: &Path) -> Option<Outcome> {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let out_path = ws.join(format!("stdout_{n}.txt"));
    let err_path = ws.join(format!("stderr_{n}.txt"));
    let out_file = fs::File::create(&out_path).expect("create stdout file");
    let err_file = fs::File::create(&err_path).expect("create stderr file");

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .spawn()
        .ok()?;

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
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };

    Some(Outcome {
        code: status.code(),
        stdout: read_output(&out_path),
        stderr: read_output(&err_path),
        timed_out,
    })
}

/// What a process wrote to `path`, byte for byte where it is UTF-8.
fn read_output(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// [`try_run_with_timeout`] for a program that must be there.
fn run_with_timeout(cmd: &mut Command, ws: &Path) -> Outcome {
    try_run_with_timeout(cmd, ws).expect("failed to start the process")
}

/// Run `silt <args>` in `cwd`, with the git checkout cache confined to
/// the workspace `ws`.
fn silt(ws: &Path, cwd: &Path, args: &[&str]) -> Outcome {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
    cmd.args(args)
        .current_dir(cwd)
        .env("XDG_CACHE_HOME", ws.join("cache"))
        .env("LOCALAPPDATA", ws.join("cache"));
    isolate_git(&mut cmd, ws);
    run_with_timeout(&mut cmd, ws)
}

/// Is `git` installed? Prints a notice when it is not.
fn git_is_installed(ws: &Path) -> bool {
    let mut cmd = Command::new("git");
    cmd.arg("--version");
    isolate_git(&mut cmd, ws);
    let installed = match try_run_with_timeout(&mut cmd, ws) {
        Some(out) => !out.timed_out && out.code == Some(0),
        None => false,
    };
    if !installed {
        eprintln!("SKIP: `git` is not installed");
    }
    installed
}

// ── Writing packages ──────────────────────────────────────────────────

/// `s` as a TOML basic string. Everything but printable ASCII is
/// written as a `\u` or `\U` escape, so the file itself is plain ASCII
/// and the TOML parser hands silt exactly `s`. The same spelling serves
/// as a quoted key.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(c),
            c if (c as u32) <= 0xFFFF => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push_str(&format!("\\U{:08X}", c as u32)),
        }
    }
    out.push('"');
    out
}

/// The text of a manifest: a `[package]` table with the name `name` and
/// the version `0.1.0`, followed by `rest`.
fn manifest(name: &str, rest: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n{rest}")
}

/// A `[dependencies]` table with the one line `line`.
fn dependencies(line: &str) -> String {
    format!("\n[dependencies]\n{line}\n")
}

/// Write a package at `dir`: the manifest, and one source file.
fn write_package(dir: &Path, manifest_text: &str, file: &str, body: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("silt.toml"), manifest_text).unwrap();
    fs::write(dir.join("src").join(file), body).unwrap();
}

/// Write an application package at `dir`.
fn write_app(dir: &Path, manifest_text: &str) {
    write_package(dir, manifest_text, "main.silt", "fn main() {}\n");
}

/// Write a valid library package named `name` at `dir`.
fn write_lib(dir: &Path, name: &str) {
    write_package(dir, &manifest(name, ""), "lib.silt", "pub fn one() = 1\n");
}

// ── Assertions ────────────────────────────────────────────────────────

/// Must silt show `c` as an escape? The display rule, written down
/// independently of the implementation: a control character, a
/// character that is neither ASCII nor a letter or digit, or one of the
/// invisible characters that count as letters (the Hangul fillers).
fn must_be_escaped(c: char) -> bool {
    c.is_control()
        || (!c.is_ascii() && !c.is_alphanumeric())
        || matches!(c, '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}')
        || ONCE_MISSED_INVISIBLE.contains(&c)
}

/// The one scan every test uses: the characters of `output` that are
/// there as they are although silt must show them as escapes. The line
/// feed that ends each line is the only control character allowed.
fn raw_characters(output: &str) -> Vec<String> {
    output
        .chars()
        .filter(|&c| c != '\n' && must_be_escaped(c))
        .map(|c| format!("U+{:04X}", c as u32))
        .collect()
}

/// Assert that the run ended in time, with exit code `code`, without a
/// panic, and without a raw character on stdout or stderr.
fn assert_printable_outcome(out: &Outcome, code: i32, context: &str) {
    assert!(!out.timed_out, "{context}: silt did not finish; {out:?}");
    assert_eq!(
        out.code,
        Some(code),
        "{context}: unexpected exit code; {out:?}"
    );
    assert!(
        !out.stderr.contains("panicked at"),
        "{context}: silt panicked; {out:?}"
    );
    let raw = raw_characters(&out.stderr);
    assert!(
        raw.is_empty(),
        "{context}: stderr holds raw characters {raw:?}; {out:?}"
    );
    let raw = raw_characters(&out.stdout);
    assert!(
        raw.is_empty(),
        "{context}: stdout holds raw characters {raw:?}; {out:?}"
    );
}

/// Assert that the run is a clean failure that shows [`HOSTILE`]
/// escaped, `times` times, within `expected`, and that the value forged
/// no line: the first line is silt's error, and no line starts with the
/// forged words.
fn assert_hostile_value_is_escaped(out: &Outcome, expected: &str, times: usize, context: &str) {
    assert_printable_outcome(out, 1, context);
    assert!(
        expected.contains(HOSTILE_ESCAPED),
        "{context}: the test expects a message without the escaped value: {expected}"
    );
    assert!(
        out.stderr.contains(expected),
        "{context}: expected `{expected}` on stderr; {out:?}"
    );
    assert_eq!(
        out.stderr.matches(HOSTILE_ESCAPED).count(),
        times,
        "{context}: the escaped value must be shown {times} time(s); {out:?}"
    );
    assert_no_forged_line(out, context);
}

/// Assert that stderr starts with silt's own error line and that no
/// line of it starts with the forged words, indented or not.
fn assert_no_forged_line(out: &Outcome, context: &str) {
    assert!(
        out.stderr.starts_with("error: ") && !out.stderr.starts_with(FORGED_LINE),
        "{context}: stderr must start with silt's error line; {out:?}"
    );
    for line in out.stderr.lines() {
        assert!(
            !line.trim_start().starts_with(FORGED_LINE),
            "{context}: the value forged the line {line:?}; {out:?}"
        );
    }
}

/// Write an application whose manifest is `manifest_text`, next to a
/// valid library package `dep`, run `silt check` in it, and assert that
/// [`HOSTILE`] is shown escaped within `expected`.
fn assert_check_escapes(tag: &str, manifest_text: &str, expected: &str, times: usize) {
    let ws = fresh_workspace(tag);
    write_lib(&ws.join("dep"), "dep");
    let app = ws.join("app");
    write_app(&app, manifest_text);

    let out = silt(&ws, &app, &["check"]);

    assert_hostile_value_is_escaped(&out, expected, times, tag);
    assert!(
        !app.join("silt.lock").exists(),
        "{tag}: silt must not write a lockfile when it rejects the manifest"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// Write an application without dependencies and the lockfile
/// `lockfile_text`, run `silt check` in it, and assert that [`HOSTILE`]
/// is shown escaped within `expected` and the lockfile is left alone.
fn assert_check_escapes_lockfile(tag: &str, lockfile_text: &str, expected: &str, times: usize) {
    let ws = fresh_workspace(tag);
    let app = ws.join("app");
    write_app(&app, &manifest("app", ""));
    fs::write(app.join("silt.lock"), lockfile_text).unwrap();

    let out = silt(&ws, &app, &["check"]);

    assert_hostile_value_is_escaped(&out, expected, times, tag);
    assert!(
        out.stderr.starts_with("error: invalid lockfile "),
        "{tag}: expected a lockfile error; {out:?}"
    );
    assert_eq!(
        fs::read_to_string(app.join("silt.lock")).unwrap(),
        lockfile_text,
        "{tag}: a rejected lockfile must not be rewritten"
    );
    let _ = fs::remove_dir_all(&ws);
}

// ── 1. Manifest fields ────────────────────────────────────────────────

/// FAILS on the base commit: the key is printed as it is.
#[test]
fn dependency_key_is_escaped() {
    let line = format!("{} = {{ path = \"../dep\" }}", toml_str(HOSTILE));
    assert_check_escapes(
        "dependency_key",
        &manifest("app", &dependencies(&line)),
        &format!("invalid dependency name `{HOSTILE_ESCAPED}`"),
        1,
    );
}

/// FAILS on the base commit: the name is printed as it is.
#[test]
fn package_name_is_escaped() {
    let text = format!(
        "[package]\nname = {}\nversion = \"0.1.0\"\n",
        toml_str(HOSTILE)
    );
    assert_check_escapes(
        "package_name",
        &text,
        &format!("invalid package name `{HOSTILE_ESCAPED}`"),
        1,
    );
}

/// FAILS on the base commit: the version is printed as it is.
#[test]
fn package_version_is_escaped() {
    let text = format!(
        "[package]\nname = \"app\"\nversion = {}\n",
        toml_str(HOSTILE)
    );
    assert_check_escapes(
        "package_version",
        &text,
        &format!("invalid package version `{HOSTILE_ESCAPED}`"),
        1,
    );
}

/// FAILS on the base commit: the unknown key is printed as it is.
#[test]
fn unknown_key_in_a_path_dependency_is_escaped() {
    let line = format!("dep = {{ path = \"../dep\", {} = 1 }}", toml_str(HOSTILE));
    assert_check_escapes(
        "unknown_key_path",
        &manifest("app", &dependencies(&line)),
        &format!("dependency `dep`: unknown key `{HOSTILE_ESCAPED}`"),
        1,
    );
}

/// FAILS on the base commit: the unknown key is printed as it is. The
/// manifest is rejected before git is run.
#[test]
fn unknown_key_in_a_git_dependency_is_escaped() {
    let line = format!(
        "dep = {{ git = \"file:///silt-test-nonexistent/r.git\", branch = \"main\", {} = 1 }}",
        toml_str(HOSTILE)
    );
    assert_check_escapes(
        "unknown_key_git",
        &manifest("app", &dependencies(&line)),
        &format!("dependency `dep`: unknown key `{HOSTILE_ESCAPED}`"),
        1,
    );
}

/// An unknown table at the top level, and an unknown field in
/// `[package]` and in `[lints]`. The message is the TOML parser's.
/// FAILS on the base commit: the field is printed as it is.
#[test]
fn unknown_manifest_fields_are_escaped() {
    let key = toml_str(HOSTILE);
    let cases = [
        (
            "unknown_top_level",
            manifest("app", &format!("\n[{key}]\na = 1\n")),
        ),
        (
            "unknown_package_field",
            manifest("app", &format!("{key} = 1\n")),
        ),
        (
            "unknown_lints_field",
            manifest("app", &format!("\n[lints]\n{key} = true\n")),
        ),
    ];
    for (tag, text) in cases {
        assert_check_escapes(tag, &text, &format!("unknown field `{HOSTILE_ESCAPED}`"), 1);
    }
}

/// A key given twice is a TOML error, and the parser's message quotes
/// the key. FAILS on the base commit: the key is printed as it is.
#[test]
fn duplicate_manifest_key_is_escaped() {
    let key = toml_str(HOSTILE);
    assert_check_escapes(
        "duplicate_manifest_key",
        &manifest("app", &format!("{key} = 1\n{key} = 2\n")),
        &format!("duplicate key `{HOSTILE_ESCAPED}`"),
        1,
    );
}

/// The value of a `path` dependency that does not exist is shown twice:
/// as the name of the dependency and as the path. FAILS on the base
/// commit: both are printed as they are.
#[test]
fn path_dependency_value_is_escaped() {
    let line = format!("dep = {{ path = {} }}", toml_str(&format!("../{HOSTILE}")));
    assert_check_escapes(
        "path_value",
        &manifest("app", &dependencies(&line)),
        &format!("dependency `{HOSTILE_ESCAPED}` path does not exist: "),
        2,
    );
}

/// A `path` dependency that exists but holds no manifest. Unix only:
/// the directory is named by the planted value, and other systems do
/// not allow a line break in a name. FAILS on the base commit.
#[cfg(unix)]
#[test]
fn path_dependency_without_a_manifest_is_escaped() {
    let ws = fresh_workspace("path_no_manifest");
    fs::create_dir_all(ws.join(HOSTILE)).unwrap();
    let line = format!("dep = {{ path = {} }}", toml_str(&format!("../{HOSTILE}")));
    let app = ws.join("app");
    write_app(&app, &manifest("app", &dependencies(&line)));

    let out = silt(&ws, &app, &["check"]);

    assert_hostile_value_is_escaped(
        &out,
        &format!("error: dependency `{HOSTILE_ESCAPED}` at "),
        2,
        "path_no_manifest",
    );
    assert!(
        out.stderr.contains("is not a silt package"),
        "expected the missing manifest to be reported; {out:?}"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// The manifest of a `path` dependency is named by its path, which is
/// made from the `path` value. Unix only, as above. FAILS on the base
/// commit: the path is printed as it is.
#[cfg(unix)]
#[test]
fn path_of_a_dependency_manifest_is_escaped() {
    let ws = fresh_workspace("dep_manifest_path");
    let text = "[package]\nname = \"dep\"\nversion = \"not-a-version\"\n";
    write_package(&ws.join(HOSTILE), text, "lib.silt", "pub fn one() = 1\n");
    let line = format!("dep = {{ path = {} }}", toml_str(&format!("../{HOSTILE}")));
    let app = ws.join("app");
    write_app(&app, &manifest("app", &dependencies(&line)));

    let out = silt(&ws, &app, &["check"]);

    assert_hostile_value_is_escaped(
        &out,
        &format!("{HOSTILE_ESCAPED}/silt.toml: invalid package version `not-a-version`"),
        1,
        "dep_manifest_path",
    );
    let _ = fs::remove_dir_all(&ws);
}

/// Only the manifest of a dependency's dependency carries the value;
/// the package being worked on and its direct dependency are clean.
/// Every subcommand that resolves dependencies shows it escaped. FAILS
/// on the base commit.
#[test]
fn value_in_a_transitive_manifest_is_escaped() {
    for subcommand in ["check", "run", "disasm", "update"] {
        let ws = fresh_workspace("transitive");
        let inner_text = format!(
            "[package]\nname = \"inner\"\nversion = {}\n",
            toml_str(HOSTILE)
        );
        write_package(
            &ws.join("inner"),
            &inner_text,
            "lib.silt",
            "pub fn one() = 1\n",
        );
        let outer_deps = dependencies("inner = { path = \"../inner\" }");
        write_package(
            &ws.join("outer"),
            &manifest("outer", &outer_deps),
            "lib.silt",
            "pub fn two() = 2\n",
        );
        let app = ws.join("app");
        let app_deps = dependencies("outer = { path = \"../outer\" }");
        write_app(&app, &manifest("app", &app_deps));

        let out = silt(&ws, &app, &[subcommand]);

        let context = format!("silt {subcommand} (transitive)");
        assert_hostile_value_is_escaped(
            &out,
            &format!("invalid package version `{HOSTILE_ESCAPED}`"),
            1,
            &context,
        );
        assert!(
            out.stderr.starts_with("error: invalid manifest ")
                && out.stderr.contains("inner")
                && out.stderr.contains("silt.toml"),
            "{context}: the error must point at the manifest of `inner`; {out:?}"
        );
        let _ = fs::remove_dir_all(&ws);
    }
}

/// A `branch`, `tag` or `rev` that holds characters the earlier list
/// let through. The repository is a local path that does not exist, so
/// resolution fails at once, with or without git installed. FAILS on
/// the base commit: the characters are printed as they are.
#[test]
fn ref_value_with_once_missed_characters_is_escaped() {
    let url = "file:///silt-test-nonexistent/remote.git";
    let keys = ["branch", "tag", "rev"];
    let chars = ONCE_MISSED_INVISIBLE.into_iter().chain(UNUSUAL_SPACES);
    // Every character once; the three keys take turns. The display
    // rule's unit tests cover every character in every message.
    for (i, c) in chars.enumerate() {
        let key = keys[i % keys.len()];
        let ws = fresh_workspace("ref_value");
        let value = format!("main{c}x");
        let line = format!(
            "remote = {{ git = \"{url}\", {key} = {} }}",
            toml_str(&value)
        );
        let app = ws.join("app");
        write_app(&app, &manifest("app", &dependencies(&line)));

        let out = silt(&ws, &app, &["check"]);

        let context = format!("{key} with U+{:04X}", c as u32);
        assert_printable_outcome(&out, 1, &context);
        let shown = format!("{key} = `main\\u{{{:x}}}x`", c as u32);
        assert!(
            out.stderr.starts_with("error: git dependency ") && out.stderr.contains(&shown),
            "{context}: expected `{shown}` on stderr; {out:?}"
        );
        let _ = fs::remove_dir_all(&ws);
    }
}

// ── 1b. Lockfile fields ───────────────────────────────────────────────

/// The name of a package is quoted in every message about its entry.
/// FAILS on the base commit: the name is printed as it is.
#[test]
fn lockfile_package_name_is_escaped() {
    let name = toml_str(HOSTILE);
    let url = "git = \"file:///silt-test-nonexistent/r.git\"";
    let cases = [
        ("lock_missing_version", String::new(), "missing `version`"),
        (
            "lock_source_not_a_table",
            "version = \"0.1.0\"\nsource = 1\n".to_string(),
            "`source` must be a table",
        ),
        (
            "lock_path_without_checksum",
            "version = \"0.1.0\"\nsource = { path = \"/silt-test-nonexistent\" }\n".to_string(),
            "has source but no checksum",
        ),
        (
            "lock_git_without_rev",
            format!("version = \"0.1.0\"\nsource = {{ {url} }}\n"),
            "git source missing `rev`",
        ),
        (
            "lock_git_without_checksum",
            format!("version = \"0.1.0\"\nsource = {{ {url}, rev = \"abc1234\" }}\n"),
            "has source but no checksum",
        ),
        (
            "lock_branch_and_tag",
            format!(
                "version = \"0.1.0\"\n\
                 source = {{ {url}, rev = \"abc1234\", branch = \"a\", tag = \"b\" }}\n"
            ),
            "git source has both `branch` and `tag`",
        ),
        (
            "lock_unrecognized_source",
            "version = \"0.1.0\"\nsource = { other = 1 }\n".to_string(),
            "source is unrecognized",
        ),
    ];
    for (tag, rest, problem) in cases {
        let lockfile = format!("version = 1\n\n[[package]]\nname = {name}\n{rest}");
        assert_check_escapes_lockfile(
            tag,
            &lockfile,
            &format!("[[package]] `{HOSTILE_ESCAPED}` {problem}"),
            1,
        );
    }
}

/// A `rev` that is not a commit id is quoted next to the name of its
/// package. FAILS on the base commit: the name is printed as it is (the
/// `rev` itself was escaped already).
#[test]
fn lockfile_rev_is_escaped() {
    let lockfile = format!(
        "version = 1\n\n[[package]]\nname = {name}\nversion = \"0.1.0\"\n\
         source = {{ git = \"file:///silt-test-nonexistent/r.git\", rev = {rev} }}\n\
         checksum = \"sha256:0\"\n",
        name = toml_str(HOSTILE),
        rev = toml_str(HOSTILE),
    );
    assert_check_escapes_lockfile(
        "lock_rev",
        &lockfile,
        &format!(
            "[[package]] `{HOSTILE_ESCAPED}` git source has invalid `rev` `{HOSTILE_ESCAPED}`"
        ),
        2,
    );
}

/// A key given twice is a TOML error, and the parser's message quotes
/// the key. FAILS on the base commit: the key is printed as it is, in
/// the message and in the quoted line of the file.
#[test]
fn duplicate_lockfile_key_is_escaped() {
    let key = toml_str(HOSTILE);
    let lockfile = format!("version = 1\n{key} = 1\n{key} = 2\n");
    assert_check_escapes_lockfile(
        "lock_duplicate_key",
        &lockfile,
        &format!("line 3, column 1: duplicate key `{HOSTILE_ESCAPED}`"),
        1,
    );
}

/// A lockfile that is not TOML: the line the parser stops at holds an
/// escape character and U+202E as they are. The error gives the
/// position and the parser's message on one line and does not quote the
/// line. FAILS on the base commit: the line is quoted as it is.
#[test]
fn lockfile_syntax_error_does_not_quote_the_file() {
    let ws = fresh_workspace("lock_syntax");
    let app = ws.join("app");
    write_app(&app, &manifest("app", ""));
    let lockfile = "version = 1\n\n[[package]]\nname = oops\u{1b}[2K\u{202e} error: FORGED\n";
    fs::write(app.join("silt.lock"), lockfile).unwrap();

    let out = silt(&ws, &app, &["check"]);

    assert_printable_outcome(&out, 1, "lock_syntax");
    assert_no_forged_line(&out, "lock_syntax");
    assert!(
        out.stderr.starts_with("error: invalid lockfile ")
            && out.stderr.contains("silt.lock: line 4, column 8: "),
        "expected a lockfile error with the position; {out:?}"
    );
    assert!(
        !out.stderr.contains("FORGED"),
        "the error must not quote the line of the file; {out:?}"
    );
    assert_eq!(
        out.stderr.lines().count(),
        1,
        "the error must be one line; {out:?}"
    );
    let _ = fs::remove_dir_all(&ws);
}

// ── 1c. `silt add` ────────────────────────────────────────────────────

/// `silt add` quotes its arguments in its errors. FAILS on the base
/// commit: each is printed as it is.
#[test]
fn silt_add_escapes_its_arguments() {
    let hostile_path = format!("../{HOSTILE}");
    let hostile_flag = format!("--{HOSTILE}");
    let cases: [(&str, Vec<&str>, String); 4] = [
        (
            "add_name",
            vec!["add", HOSTILE, "--path", "../dep"],
            format!("silt add: invalid dependency name `{HOSTILE_ESCAPED}`"),
        ),
        (
            "add_path",
            vec!["add", "dep", "--path", hostile_path.as_str()],
            "silt add: path does not exist: ".to_string(),
        ),
        (
            "add_extra_argument",
            vec!["add", "dep", HOSTILE, "--path", "../dep"],
            format!("silt add: unexpected extra argument '{HOSTILE_ESCAPED}'"),
        ),
        (
            "add_unknown_flag",
            vec!["add", "dep", hostile_flag.as_str()],
            format!("silt add: unknown flag '--{HOSTILE_ESCAPED}'"),
        ),
    ];
    for (tag, args, expected) in cases {
        let ws = fresh_workspace(tag);
        write_lib(&ws.join("dep"), "dep");
        let app = ws.join("app");
        write_app(&app, &manifest("app", ""));
        let manifest_before = fs::read_to_string(app.join("silt.toml")).unwrap();

        let out = silt(&ws, &app, &args);

        assert_printable_outcome(&out, 1, tag);
        assert_no_forged_line(&out, tag);
        assert!(
            out.stderr.contains(&expected) && out.stderr.contains(HOSTILE_ESCAPED),
            "{tag}: expected `{expected}` and the escaped value on stderr; {out:?}"
        );
        let lines: Vec<&str> = out.stderr.lines().collect();
        if tag == "add_unknown_flag" {
            assert_eq!(
                lines.len(),
                2,
                "{tag}: expected the error and the usage line; {out:?}"
            );
            assert_eq!(lines[1], "Run 'silt add --help' for usage.", "{out:?}");
        } else {
            assert_eq!(lines.len(), 1, "{tag}: the error must be one line; {out:?}");
        }
        assert_eq!(
            fs::read_to_string(app.join("silt.toml")).unwrap(),
            manifest_before,
            "{tag}: the manifest must not be touched"
        );
        let _ = fs::remove_dir_all(&ws);
    }
}

/// The line `silt add` prints on success quotes the path as it was
/// given. FAILS on the base commit: the en dash is printed as it is.
#[test]
fn silt_add_escapes_its_summary() {
    let ws = fresh_workspace("add_summary");
    let dep_dir = "caf\u{e9} \u{2013} dep";
    write_lib(&ws.join(dep_dir), "dep");
    let app = ws.join("app");
    write_app(&app, &manifest("app", ""));

    let path = format!("../{dep_dir}");
    let out = silt(&ws, &app, &["add", "dep", "--path", path.as_str()]);

    assert_printable_outcome(&out, 0, "add_summary");
    assert!(
        out.stdout.starts_with("Added dependency 'dep' (path = \"")
            && out.stdout.contains("caf\u{e9} \\u{2013} dep\")"),
        "expected the summary with the letter as it is and the dash escaped; {out:?}"
    );
    // The manifest and the lockfile hold the path itself.
    let written = fs::read_to_string(app.join("silt.toml")).unwrap();
    assert!(
        written.contains(dep_dir),
        "the manifest must hold the path as it was given:\n{written}"
    );
    let lock = fs::read_to_string(app.join("silt.lock")).expect("silt.lock was written");
    assert!(
        lock.contains("name = \"dep\"") && lock.contains(dep_dir),
        "the lockfile must pin the dependency:\n{lock}"
    );
    let _ = fs::remove_dir_all(&ws);
}

// ── 2. The git URL rule ───────────────────────────────────────────────

const RULE_SPACE: &str = "must not contain whitespace or control characters";
const RULE_INVISIBLE: &str = "must not contain invisible or bidirectional formatting characters";
const RULE_NON_ASCII: &str = "must not contain non-ASCII characters other than letters and digits";

/// The network forms, as prefixes of a repository name.
const NETWORK_FORMS: [&str; 6] = [
    "https://example.invalid/team/",
    "http://127.0.0.1:1/team/",
    "ssh://git@example.invalid/team/",
    "git://example.invalid/team/",
    "git@example.invalid:team/",
    "example.invalid:team/",
];

/// The local forms, as prefixes of a repository name.
const LOCAL_FORMS: [&str; 4] = [
    "file:///silt-test-nonexistent/",
    "/silt-test-nonexistent/",
    "./silt-test-nonexistent/",
    "../silt-test-nonexistent/",
];

/// Ask silt whether it accepts `url` as the `git` value of a
/// dependency, without git being run: `silt update <name>` loads the
/// manifest, and stops when `<name>` is not a declared dependency.
fn validate_url(url: &str) -> Outcome {
    let ws = fresh_workspace("url");
    let app = ws.join("app");
    let line = format!("remote = {{ git = {}, branch = \"main\" }}", toml_str(url));
    write_app(&app, &manifest("app", &dependencies(&line)));
    let out = silt(&ws, &app, &["update", "zzz_not_declared"]);
    assert!(
        !app.join("silt.lock").exists() && !ws.join("cache").exists(),
        "the URL check must not resolve anything; url={url:?} {out:?}"
    );
    let _ = fs::remove_dir_all(&ws);
    out
}

fn assert_url_accepted(url: &str) {
    let out = validate_url(url);
    let context = format!("git URL {url:?}");
    assert!(!out.timed_out, "{context}: silt did not finish; {out:?}");
    assert_eq!(out.code, Some(1), "{context}: {out:?}");
    assert!(
        out.stderr
            .contains("dependency `zzz_not_declared` is not declared")
            && !out.stderr.contains("invalid"),
        "{context}: the URL must be accepted; {out:?}"
    );
}

/// Assert that `url` is rejected by `rule` and that the message shows
/// `c` as an escape.
fn assert_url_rejected(url: &str, rule: &str, c: char) {
    let out = validate_url(url);
    let context = format!("git URL {url:?}");
    assert_printable_outcome(&out, 1, &context);
    assert!(
        out.stderr.starts_with("error: invalid manifest ")
            && out
                .stderr
                .contains("dependency `remote`: invalid git URL `"),
        "{context}: the URL must be rejected; {out:?}"
    );
    assert!(
        out.stderr.contains(rule),
        "{context}: the error must state the rule `{rule}`; {out:?}"
    );
    let escape = format!("\\u{{{:x}}}", c as u32);
    assert!(
        out.stderr.contains(&escape),
        "{context}: the error must show the character as {escape}; {out:?}"
    );
    assert_eq!(
        out.stderr.lines().count(),
        1,
        "{context}: the error must be one line; {out:?}"
    );
}

// The tables below run one process per URL, so they do not cross every
// character with every form: each form is met, and each character is
// met in a network form and in a local form. The unit tests of the rule
// in `src/git.rs` cross all of them.

/// GUARD: passes on the base commit and with the fix. An ordinary URL
/// of every form, and letters and digits of several scripts in every
/// form: a Latin letter with a diacritic, Cyrillic, Han, and an
/// Arabic-Indic digit.
#[test]
fn git_url_table_accepted_in_every_form() {
    let names = ["pkg", "caf\u{e9}_\u{43f}\u{440}_\u{65e5}\u{672c}_v\u{661}"];
    for form in NETWORK_FORMS.into_iter().chain(LOCAL_FORMS) {
        for name in names {
            assert_url_accepted(&format!("{form}{name}.git"));
        }
    }
}

/// GUARD: passes on the base commit and with the fix. Directory names
/// hold letters of any script, spaces and punctuation, so the local
/// forms accept all three.
#[test]
fn git_url_table_accepted_in_local_forms() {
    let punctuation: String = NON_ASCII_PUNCTUATION.into_iter().collect();
    for form in LOCAL_FORMS {
        assert_url_accepted(&format!("{form}caf\u{e9}/pkg.git"));
        assert_url_accepted(&format!("{form}my repos/pkg.git"));
        assert_url_accepted(&format!("{form}a{punctuation}b/pkg.git"));
    }
}

/// An invisible or direction-control character is rejected in the
/// network forms and in the local forms. FAILS on the base commit:
/// these characters were accepted.
#[test]
fn git_url_table_rejects_invisible_characters() {
    // Every character, in a network form and in a local form.
    for c in ONCE_MISSED_INVISIBLE {
        assert_url_rejected(
            &format!("https://example.invalid/a{c}b.git"),
            RULE_INVISIBLE,
            c,
        );
        assert_url_rejected(
            &format!("file:///silt-test-nonexistent/a{c}b.git"),
            RULE_INVISIBLE,
            c,
        );
    }
    // Every form; the characters take turns.
    let forms = NETWORK_FORMS.into_iter().chain(LOCAL_FORMS);
    for (i, form) in forms.enumerate() {
        let c = ONCE_MISSED_INVISIBLE[i % ONCE_MISSED_INVISIBLE.len()];
        assert_url_rejected(&format!("{form}a{c}b.git"), RULE_INVISIBLE, c);
    }
    // In the host.
    let c = ONCE_MISSED_INVISIBLE[0];
    assert_url_rejected(
        &format!("https://exam{c}ple.invalid/pkg.git"),
        RULE_INVISIBLE,
        c,
    );
}

/// A space other than U+0020 is rejected in every form. These URLs
/// were rejected before, but FAIL on the base commit all the same: the
/// space was echoed as it is.
#[test]
fn git_url_table_rejects_unusual_spaces_in_every_form() {
    let forms = NETWORK_FORMS.into_iter().chain(LOCAL_FORMS);
    for (i, form) in forms.enumerate() {
        let c = UNUSUAL_SPACES[i % UNUSUAL_SPACES.len()];
        assert_url_rejected(&format!("{form}a{c}b.git"), RULE_SPACE, c);
    }
}

/// Punctuation and symbols outside ASCII are rejected in the network
/// forms. FAILS on the base commit: they were accepted.
#[test]
fn git_url_table_rejects_non_ascii_punctuation_in_network_forms() {
    for (i, form) in NETWORK_FORMS.into_iter().enumerate() {
        let c = NON_ASCII_PUNCTUATION[i % NON_ASCII_PUNCTUATION.len()];
        assert_url_rejected(&format!("{form}a{c}b.git"), RULE_NON_ASCII, c);
    }
}

/// An ordinary space stays rejected in the network forms. GUARD: passes
/// on the base commit and with the fix.
#[test]
fn git_url_table_rejects_a_space_in_network_forms() {
    for form in NETWORK_FORMS {
        let out = validate_url(&format!("{form}my repos/pkg.git"));
        let context = format!("{form}my repos/pkg.git");
        assert_printable_outcome(&out, 1, &context);
        assert!(
            out.stderr.contains("invalid git URL") && out.stderr.contains(RULE_SPACE),
            "{context}: a space must be rejected; {out:?}"
        );
    }
}

/// The error for a URL with a scheme silt does not accept names the
/// schemes to use instead. FAILS on the base commit: the error only
/// listed the accepted forms.
#[test]
fn unaccepted_scheme_names_the_replacement() {
    for (url, scheme) in [
        ("git+ssh://example.invalid/pkg.git", "`git+ssh://`"),
        ("ftp://example.invalid/pkg.git", "`ftp://`"),
    ] {
        let out = validate_url(url);
        assert_printable_outcome(&out, 1, url);
        assert!(
            out.stderr.contains("is not a recognised git URL form")
                && out
                    .stderr
                    .contains(&format!("the scheme {scheme} is not accepted"))
                && out.stderr.contains("use `ssh://` or `https://` instead"),
            "{url}: the error must name the replacement; {out:?}"
        );
    }
}

// ── 3. git's own output ───────────────────────────────────────────────

/// What every line of git's output starts with.
const GIT_LINE: &str = "  git: ";

/// Assert that `stderr` is one line of silt's, starting with `first`
/// and showing the git command as it was run, followed by at least two
/// lines of git's, each one marked.
fn assert_git_output_is_marked(out: &Outcome, first: &str, command: &str, context: &str) {
    assert_printable_outcome(out, 1, context);
    let lines: Vec<&str> = out.stderr.lines().collect();
    assert!(
        lines.len() >= 3,
        "{context}: expected silt's line and at least two lines of git's; {out:?}"
    );
    assert!(
        lines[0].starts_with(first),
        "{context}: expected the first line to start with `{first}`; {out:?}"
    );
    assert!(
        lines[0].contains(command),
        "{context}: expected the command `{command}` on the first line; {out:?}"
    );
    for line in &lines[1..] {
        assert!(
            line.starts_with(GIT_LINE),
            "{context}: the line {line:?} of git's output is not marked; {out:?}"
        );
    }
}

/// git fails on a repository that does not exist, and says so on
/// several lines. FAILS on the base commit: only the first of git's
/// lines was marked, and the command was shown without
/// `-c protocol.ext.allow=never`.
#[test]
fn every_line_of_git_output_is_marked() {
    let url = "file:///silt-test-nonexistent/remote.git";
    for subcommand in ["check", "run", "update"] {
        let ws = fresh_workspace("git_output");
        if !git_is_installed(&ws) {
            let _ = fs::remove_dir_all(&ws);
            return;
        }
        let app = ws.join("app");
        let line = format!("remote = {{ git = \"{url}\", branch = \"main\" }}");
        write_app(&app, &manifest("app", &dependencies(&line)));

        let out = silt(&ws, &app, &[subcommand]);

        assert_git_output_is_marked(
            &out,
            &format!("error: git dependency `{url}` (branch = `main`): git command failed"),
            &format!("`git -c protocol.ext.allow=never ls-remote -- {url} refs/heads/main`"),
            &format!("silt {subcommand}"),
        );
        let _ = fs::remove_dir_all(&ws);
    }
}

/// The same through `silt add`. FAILS on the base commit.
#[test]
fn every_line_of_git_output_is_marked_by_silt_add() {
    let url = "file:///silt-test-nonexistent/remote.git";
    let ws = fresh_workspace("git_output_add");
    if !git_is_installed(&ws) {
        let _ = fs::remove_dir_all(&ws);
        return;
    }
    let app = ws.join("app");
    write_app(&app, &manifest("app", ""));
    let manifest_before = fs::read_to_string(app.join("silt.toml")).unwrap();

    let args = ["add", "remote", "--git", url, "--branch", "main"];
    let out = silt(&ws, &app, &args);

    assert_git_output_is_marked(
        &out,
        &format!("error: silt add: cannot reach `{url}`: git command failed"),
        &format!("`git -c protocol.ext.allow=never ls-remote -- {url} HEAD`"),
        "silt add",
    );
    assert_eq!(
        fs::read_to_string(app.join("silt.toml")).unwrap(),
        manifest_before,
        "silt add: the manifest must not be touched"
    );
    let _ = fs::remove_dir_all(&ws);
}

// ── 4. Real use still works ───────────────────────────────────────────

/// Run `git <args>` in `cwd` for fixture setup, with a fixed identity
/// so the commit does not depend on the machine's git configuration.
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
    let out = run_with_timeout(&mut cmd, ws);
    assert!(
        !out.timed_out && out.code == Some(0),
        "git {args:?} failed; {out:?}"
    );
    out.stdout.trim().to_string()
}

/// GUARD: passes on the base commit and with the fix. A repository in a
/// directory whose name holds a letter outside ASCII, a space and an en
/// dash resolves, locks and runs, addressed as a `file://` URL, as an
/// absolute path and as a relative path.
///
/// Unix only: the fixture spells the URL from an absolute Unix path.
#[cfg(unix)]
#[test]
fn repository_in_a_directory_with_punctuation_resolves() {
    let dir_name = "caf\u{e9} \u{2013} repo";
    for form in ["file", "absolute", "relative"] {
        let ws = fresh_workspace("real_repo");
        if !git_is_installed(&ws) {
            let _ = fs::remove_dir_all(&ws);
            return;
        }
        // The workspace's own path is part of the URL, so it has to be
        // one the rule accepts.
        let ws_text = ws.display().to_string();
        if ws_text
            .chars()
            .any(|c| must_be_escaped(c) || c.is_whitespace())
        {
            eprintln!("SKIP: the temporary directory's path is not a valid git URL: {ws_text:?}");
            let _ = fs::remove_dir_all(&ws);
            return;
        }

        let repo = ws.join(dir_name);
        write_lib(&repo, "locallib");
        fs::write(repo.join("src").join("lib.silt"), "pub fn answer() = 42\n").unwrap();
        git(&ws, &repo, &["init", "--quiet"]);
        git(&ws, &repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(&ws, &repo, &["add", "."]);
        git(&ws, &repo, &["commit", "--quiet", "-m", "initial"]);
        let head = git(&ws, &repo, &["rev-parse", "HEAD"]);

        let url = match form {
            "file" => format!("file://{}", repo.display()),
            "absolute" => repo.display().to_string(),
            _ => format!("../{dir_name}"),
        };
        let app = ws.join("app");
        let line = format!(
            "locallib = {{ git = {}, branch = \"main\" }}",
            toml_str(&url)
        );
        write_package(
            &app,
            &manifest("app", &dependencies(&line)),
            "main.silt",
            "import locallib\nfn main() { println(locallib.answer()) }\n",
        );

        let check = silt(&ws, &app, &["check"]);
        assert_printable_outcome(&check, 0, &format!("silt check, git = {url:?}"));
        let lock = fs::read_to_string(app.join("silt.lock")).expect("silt.lock was written");
        assert!(
            lock.contains(&format!("rev = \"{head}\"")) && lock.contains(dir_name),
            "the lockfile must pin `{head}` and record the URL {url:?}:\n{lock}"
        );

        let run = silt(&ws, &app, &["run"]);
        assert_printable_outcome(&run, 0, &format!("silt run, git = {url:?}"));
        assert_eq!(run.stdout.trim(), "42", "{run:?}");

        let _ = fs::remove_dir_all(&ws);
    }
}
