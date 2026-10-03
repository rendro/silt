//! The verdict mode: run a case through `silt check`, `silt run`,
//! `silt test` and `silt lsp` and compare the static diagnostics each
//! door reports. The rules are in `tests/golden/README.md` ("Verdicts").

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::lsp;

/// How long `run` and `test` may execute the program before they are
/// stopped. Their static diagnostics are printed before anything runs,
/// so what they printed by then is complete.
pub const EXECUTION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Door {
    Check,
    Run,
    Test,
    Lsp,
}

/// Every door, `check` first: the others are compared with it.
pub const DOORS: [Door; 4] = [Door::Check, Door::Run, Door::Test, Door::Lsp];

impl Door {
    pub fn name(self) -> &'static str {
        match self {
            Door::Check => "check",
            Door::Run => "run",
            Door::Test => "test",
            Door::Lsp => "lsp",
        }
    }

    fn parse(name: &str) -> Option<Door> {
        DOORS.into_iter().find(|d| d.name() == name)
    }
}

/// What a case's `-- verdict:` directive says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Every door reports the same static diagnostics.
    Same,
    /// These doors report something other than `check` does; the rest
    /// agree with it.
    KnownDivergent(Vec<Door>),
}

impl Mark {
    pub fn parse(value: &str) -> Result<Mark, String> {
        let mut words = value.split_whitespace();
        match words.next() {
            Some("same") if words.next().is_none() => Ok(Mark::Same),
            Some("known-divergent") => {
                let mut doors = Vec::new();
                for w in words {
                    match Door::parse(w) {
                        Some(Door::Check) | None => {
                            return Err(format!(
                                "bad door {w:?} in `-- verdict:` (expected run, test or lsp)"
                            ));
                        }
                        Some(d) => doors.push(d),
                    }
                }
                doors.sort();
                doors.dedup();
                if doors.is_empty() {
                    return Err("`-- verdict: known-divergent` names no door".to_string());
                }
                Ok(Mark::KnownDivergent(doors))
            }
            _ => Err(format!(
                "bad `-- verdict:` value {value:?} (expected `same` or `known-divergent <doors>`)"
            )),
        }
    }

    fn from_divergent(doors: Vec<Door>) -> Mark {
        if doors.is_empty() {
            Mark::Same
        } else {
            Mark::KnownDivergent(doors)
        }
    }
}

impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mark::Same => write!(f, "same"),
            Mark::KnownDivergent(doors) => {
                write!(f, "known-divergent")?;
                for d in doors {
                    write!(f, " {}", d.name())?;
                }
                Ok(())
            }
        }
    }
}

/// One static error diagnostic, as the verdict compares it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    /// The file, relative to the case directory, `/`-separated; empty
    /// for a package error, which names no file.
    file: String,
    line: u64,
    col: u64,
    /// The message's first line, with the case's temporary directory
    /// written as `<case>`.
    message: String,
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.file.is_empty() {
            write!(f, "(package) {}", self.message)
        } else {
            write!(
                f,
                "{}:{}:{} {}",
                self.file, self.line, self.col, self.message
            )
        }
    }
}

/// A door's verdict: its static error diagnostics, or why it has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Diagnostics(BTreeSet<Key>),
    /// The door did not finish its static phase (`check` or the LSP
    /// timed out, crashed, or the LSP published nothing).
    Failed(String),
}

impl Verdict {
    fn render(&self) -> String {
        match self {
            Verdict::Failed(why) => format!("    (failed: {why})\n"),
            Verdict::Diagnostics(keys) if keys.is_empty() => "    (none)\n".to_string(),
            Verdict::Diagnostics(keys) => keys.iter().map(|k| format!("    {k}\n")).collect(),
        }
    }
}

/// Whether `message` is one of the entry-point diagnostics, which only
/// `run` and `check` give: they are about starting the program at
/// `main`, which `test` and the LSP never do.
fn is_entry_point_message(message: &str) -> bool {
    message.starts_with("program has no main() function")
        || message.starts_with("the entry point 'main' must take no parameters")
}

/// Whether `door`'s verdict agrees with `check`'s.
fn agrees(door: Door, check: &Verdict, other: &Verdict) -> bool {
    match (check, other) {
        (Verdict::Diagnostics(a), Verdict::Diagnostics(b)) => {
            if matches!(door, Door::Test | Door::Lsp) {
                let strip = |s: &BTreeSet<Key>| -> BTreeSet<Key> {
                    s.iter()
                        .filter(|k| !is_entry_point_message(&k.message))
                        .cloned()
                        .collect()
                };
                strip(a) == strip(b)
            } else {
                a == b
            }
        }
        _ => false,
    }
}

/// The doors that disagree with `check`, given every door's verdict in
/// the order of [`DOORS`].
pub fn divergent(verdicts: &[Verdict; 4]) -> Vec<Door> {
    DOORS[1..]
        .iter()
        .zip(&verdicts[1..])
        .filter(|(door, v)| !agrees(**door, &verdicts[0], v))
        .map(|(door, _)| *door)
        .collect()
}

/// The problems with the verdicts of a case marked `mark`, empty when
/// the doors agree and disagree exactly as the mark says.
pub fn judge(mark: &Mark, verdicts: &[Verdict; 4]) -> Vec<String> {
    let actual = Mark::from_divergent(divergent(verdicts));
    if &actual == mark {
        return Vec::new();
    }
    let mut report = format!("marked `-- verdict: {mark}`, but the doors say `{actual}`:\n");
    for (door, v) in DOORS.iter().zip(verdicts) {
        report.push_str(&format!("  {}:\n{}", door.name(), v.render()));
    }
    if actual == Mark::Same {
        report.push_str(
            "  the doors agree now: change the mark to `same` (SILT_BLESS=1 rewrites it)",
        );
    }
    vec![report]
}

/// The mark the verdicts call for, for bless mode.
pub fn mark_for(verdicts: &[Verdict; 4]) -> Mark {
    Mark::from_divergent(divergent(verdicts))
}

/// Every door's verdict for the case copied into a fresh directory by
/// `fresh_copy` (one copy per door, so nothing one door or the program
/// writes reaches the next), whose entry file is `entry`; and the error
/// diagnostics `check` printed without a location (see
/// `unlocated_errors`).
pub fn verdicts(
    fresh_copy: &dyn Fn() -> std::path::PathBuf,
    entry: &str,
    timeout: Duration,
) -> ([Verdict; 4], Vec<String>) {
    let mut unlocated = Vec::new();
    let verdicts = DOORS.map(|door| {
        let dir = fresh_copy();
        let (v, problems) = door_verdict(door, &dir, entry, timeout);
        unlocated.extend(problems);
        let _ = std::fs::remove_dir_all(&dir);
        v
    });
    (verdicts, unlocated)
}

fn door_verdict(door: Door, dir: &Path, entry: &str, timeout: Duration) -> (Verdict, Vec<String>) {
    let (v, stderr) = door_verdict_and_stderr(door, dir, entry, timeout);
    let unlocated = match (door, stderr) {
        (Door::Check, Some(stderr)) => crate::unlocated_errors(&stderr),
        _ => Vec::new(),
    };
    (v, unlocated)
}

/// The verdict of `door`, and the stderr of a CLI door.
fn door_verdict_and_stderr(
    door: Door,
    dir: &Path,
    entry: &str,
    timeout: Duration,
) -> (Verdict, Option<String>) {
    let roots = roots(dir);
    if door == Door::Lsp {
        let session = lsp::session(dir, entry, timeout);
        if let Some(why) = session.error {
            return (Verdict::Failed(why), None);
        }
        let keys = session
            .files
            .iter()
            .flat_map(|(file, diags)| {
                diags
                    .iter()
                    .filter(|d| d.severity == "error")
                    .map(|d| Key {
                        file: file.clone(),
                        line: d.line,
                        col: d.col,
                        message: normalise(&d.message, &roots),
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        return (Verdict::Diagnostics(keys), None);
    }
    let executes = matches!(door, Door::Run | Door::Test);
    let limit = if executes { EXECUTION_TIMEOUT } else { timeout };
    let out = run_cli(dir, door.name(), entry, limit);
    if !executes {
        if out.timed_out {
            return (
                Verdict::Failed(format!("did not exit within {limit:?}")),
                None,
            );
        }
        if !matches!(out.code, Some(0) | Some(1)) {
            return (
                Verdict::Failed(format!(
                    "exit status {:?}; stderr:\n{}",
                    out.code, out.stderr
                )),
                None,
            );
        }
    }
    let v = Verdict::Diagnostics(static_diagnostics(&out.stderr, entry, &roots));
    (v, Some(out.stderr))
}

/// The case directory as it may appear in paths: as given and
/// canonicalised, `/`-separated.
fn roots(dir: &Path) -> Vec<String> {
    let mut roots = vec![dir.to_string_lossy().replace('\\', "/")];
    let c = silt::source::canonical_path(dir)
        .to_string_lossy()
        .replace('\\', "/");
    if !roots.contains(&c) {
        roots.push(c);
    }
    // The longest first, so a root inside another is replaced whole.
    roots.sort_by_key(|r| std::cmp::Reverse(r.len()));
    roots
}

/// The first line of `message`, trimmed, with the case directory written
/// as `<case>`.
fn normalise(message: &str, roots: &[String]) -> String {
    let mut first = message
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .replace('\\', "/");
    for root in roots {
        first = first.replace(root.as_str(), "<case>");
    }
    first
}

/// A path printed in a `-->` line, relative to the case directory.
fn relative_file(path: &str, roots: &[String]) -> String {
    let path = path.replace('\\', "/");
    for root in roots {
        if let Some(rest) = path.strip_prefix(root.as_str())
            && let Some(rest) = rest.strip_prefix('/')
        {
            return rest.to_string();
        }
    }
    path.strip_prefix("./").unwrap_or(&path).to_string()
}

/// The static error diagnostics in a CLI door's stderr.
///
/// A diagnostic is a header line `error[<kind>]: <message>` (or
/// `warning[...]`), followed by a ` --> file:line:col` line when it has a
/// location. Reading stops at the first `error[runtime]` header: the
/// static diagnostics are printed before the program runs, and nothing
/// after that point is static. Warnings are not part of the verdict. A
/// diagnostic with no location is keyed at line 1, column 1 of the entry
/// file, where the LSP puts the same location-less diagnostic. Package
/// errors are `error[package]` diagnostics in a `silt.toml` or
/// `silt.lock` like any other.
fn static_diagnostics(stderr: &str, entry: &str, roots: &[String]) -> BTreeSet<Key> {
    let mut keys = BTreeSet::new();
    // The current diagnostic: its message, and whether it is an error.
    let mut pending: Option<(String, bool)> = None;
    let flush = |pending: &mut Option<(String, bool)>, keys: &mut BTreeSet<Key>| {
        if let Some((message, true)) = pending.take() {
            keys.insert(Key {
                file: entry.replace('\\', "/"),
                line: 1,
                col: 1,
                message,
            });
        }
    };
    for line in stderr.lines() {
        if let Some((is_error, kind, message)) = header(line) {
            flush(&mut pending, &mut keys);
            if kind == "runtime" {
                break;
            }
            pending = Some((normalise(message, roots), is_error));
            continue;
        }
        if let Some(loc) = line.trim_start().strip_prefix("--> ") {
            let Some((message, is_error)) = pending.take() else {
                continue;
            };
            let mut parts = loc.trim().rsplitn(3, ':');
            let (Some(col), Some(ln), Some(file)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if is_error && let (Ok(line), Ok(col)) = (ln.parse(), col.parse()) {
                keys.insert(Key {
                    file: relative_file(file, roots),
                    line,
                    col,
                    message,
                });
            }
            continue;
        }
    }
    flush(&mut pending, &mut keys);
    keys
}

/// `error[kind]: message` or `warning[kind]: message`, split into
/// (is it an error, kind, message).
pub fn header(line: &str) -> Option<(bool, &str, &str)> {
    let (is_error, rest) = if let Some(rest) = line.strip_prefix("error[") {
        (true, rest)
    } else {
        (false, line.strip_prefix("warning[")?)
    };
    let (kind, message) = rest.split_once("]: ")?;
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    Some((is_error, kind, message))
}

struct CliOutput {
    code: Option<i32>,
    stderr: String,
    timed_out: bool,
}

/// Run `silt <cmd> <entry>` in `dir` with empty stdin, stopping it after
/// `limit`.
fn run_cli(dir: &Path, cmd: &str, entry: &str, limit: Duration) -> CliOutput {
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(cmd)
        .arg(entry)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn silt");
    let mut out_pipe = child.stdout.take().expect("stdout");
    let mut err_pipe = child.stderr.take().expect("stderr");
    let out_reader = std::thread::spawn(move || {
        // The program's output is not part of a verdict; keep reading so
        // the program never blocks on a full pipe.
        let _ = std::io::copy(&mut out_pipe, &mut std::io::sink());
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = std::io::Read::read_to_end(&mut err_pipe, &mut s);
        s
    });
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if started.elapsed() >= limit => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().expect("wait after kill");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let _ = out_reader.join();
    CliOutput {
        code: status.code(),
        stderr: String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned(),
        timed_out,
    }
}
