//! The formatter's property runner (stage 8 step T).
//!
//! For each input that parses, format it; then the formatter
//!
//!   1. did not refuse it (a refusal is how the formatter's own check of
//!      its result, the oracle, reports that the result would not parse,
//!      would be another program or would not carry the same comments),
//!   2. did not panic,
//!   3. produced text that lexes and parses and holds every comment of
//!      the input as often as the input holds it (checked here with the
//!      lexer, apart from the formatter's own check),
//!   4. left the comments that start the file as its first bytes, and
//!   5. is idempotent: formatting the result changes nothing.
//!
//! Inputs are the examples, the `silt` snippets of the docs, the golden
//! cases, the formatter's fuzz corpus, a directory named by
//! `SILT_FMT_CORPUS`, and comment mutants: a file with one `--` or
//! `{- -}` comment put into one gap between two of its tokens.
//!
//! The `frontend` suite runs the examples and a sample of their mutants
//! (`fmt_property_sample_tests.rs`). The `heavy` suite, which includes
//! this file by path, runs every input and a sample of the other mutants,
//! and every mutant with `SILT_FMT_FULL=1`
//! (`tests/heavy/fmt_property_sweep_tests.rs`).
//!
//! `SILT_FMT_REPORT=<file>` writes every failure to the file; the test
//! output holds the counts and the first failures.

// Two suites include this file and each uses a part of it.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use silt::diagnostic::Code;
use silt::lexer::{Lexer, Token};
use silt::parser::Parser;
use silt::source::FileId;

/// What the formatter made of one text.
pub enum Formatted {
    Ok(String),
    /// The text does not lex or parse: not an input.
    Unparseable,
    /// The formatter refused its own result, with this message.
    Refused(String),
}

/// `silt fmt` on one text.
fn format(source: &str) -> Formatted {
    match silt::format::format(FileId::default(), source) {
        Ok(text) => Formatted::Ok(text),
        Err(e) if e.code == Code::FormatRefused => Formatted::Refused(e.message),
        Err(_) => Formatted::Unparseable,
    }
}

/// A text to format, and where it came from.
pub struct Input {
    /// What the report calls it: a path relative to the repository, with
    /// a line for a doc snippet.
    pub name: String,
    /// `examples`, `docs`, `golden`, `fuzz` or `corpus`.
    pub class: &'static str,
    pub text: String,
}

#[derive(Clone, Copy)]
pub enum CommentKind {
    /// ` -- c` and a line break.
    Line,
    /// ` {- c -} `.
    Block,
    /// A line break, ` -- c` and a line break.
    OwnLine,
    /// ` {- c`, a line break and ` more -} `.
    BlockOverLines,
}

/// One run of the formatter: an input as it is, or with a comment put
/// in front of the token at each of some sites.
pub struct Job<'a> {
    pub input: &'a Input,
    /// The sites, in falling order, each with the comment it gets.
    pub mutation: Vec<(usize, CommentKind)>,
    /// What tells this job from the others of its input, when the
    /// mutation is too long to name.
    pub label: String,
}

impl Job<'_> {
    fn name(&self) -> String {
        match self.mutation.as_slice() {
            [] => self.input.name.clone(),
            [(site, CommentKind::Line)] => format!("{} + `--` at byte {site}", self.input.name),
            [(site, CommentKind::Block)] => {
                format!("{} + `{{- -}}` at byte {site}", self.input.name)
            }
            _ => format!("{} + {}", self.input.name, self.label),
        }
    }

    fn class(&self) -> String {
        match self.mutation.len() {
            0 => self.input.class.to_string(),
            1 => format!("{} mutants", self.input.class),
            _ => format!("{} with several comments", self.input.class),
        }
    }

    /// The text to format. A line comment takes the rest of its line, so
    /// the token it stands before moves to a line of its own.
    fn text(&self) -> String {
        let mut text = self.input.text.clone();
        for (site, kind) in &self.mutation {
            let comment = match kind {
                CommentKind::Line => format!(" -- C{site}Z\n "),
                CommentKind::Block => format!(" {{- C{site}Z -}} "),
                CommentKind::OwnLine => format!("\n -- C{site}Z\n "),
                CommentKind::BlockOverLines => format!(" {{- C{site}Z\n more -}} "),
            };
            text.insert_str(*site, &comment);
        }
        text
    }
}

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every file under `dir` that `keep` accepts, sorted.
fn files_under(dir: &Path, keep: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files_under(&path, keep, out);
        } else if keep(&path) {
            out.push(path);
        }
    }
}

/// A larger file is not an input: the golden cases hold programs of up
/// to a megabyte that test the limits of the lexer and the parser, and
/// are no test of layout.
const MAX_INPUT_BYTES: usize = 64 * 1024;

/// A larger input gets no mutants: it has thousands of gaps, and each
/// mutant is the whole file again. Every example is smaller.
const MAX_MUTATED_BYTES: usize = 16 * 1024;

fn is_silt(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "silt")
}

/// The files under `dir` that `keep` accepts and that are UTF-8, as
/// inputs of `class`, named relative to `base` with `/` separators.
fn inputs_under(
    dir: &Path,
    base: &Path,
    class: &'static str,
    keep: &dyn Fn(&Path) -> bool,
) -> Vec<Input> {
    let mut files = Vec::new();
    files_under(dir, keep, &mut files);
    files
        .into_iter()
        .filter_map(|path| {
            let text = String::from_utf8(std::fs::read(&path).ok()?).ok()?;
            if text.len() > MAX_INPUT_BYTES {
                return None;
            }
            let name = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            Some(Input { name, class, text })
        })
        .collect()
}

/// `examples/**/*.silt`.
pub fn examples() -> Vec<Input> {
    inputs_under(&repo().join("examples"), repo(), "examples", &is_silt)
}

/// `tests/golden/**/*.silt`.
pub fn golden_files() -> Vec<Input> {
    inputs_under(&repo().join("tests/golden"), repo(), "golden", &is_silt)
}

/// `fuzz/corpus/fuzz_formatter/*`: every file, whatever its name.
pub fn fuzz_corpus() -> Vec<Input> {
    inputs_under(
        &repo().join("fuzz/corpus/fuzz_formatter"),
        repo(),
        "fuzz",
        &|_| true,
    )
}

/// The `.silt` files under the directory `SILT_FMT_CORPUS` names, if it
/// is set; with `SILT_FMT_CORPUS_ALL=1`, every file under it, whatever
/// its name (a fuzz corpus names its files by their hash).
pub fn extra_corpus() -> Vec<Input> {
    match std::env::var_os("SILT_FMT_CORPUS") {
        Some(dir) if !dir.is_empty() => {
            let dir = PathBuf::from(dir);
            assert!(
                dir.is_dir(),
                "SILT_FMT_CORPUS is not a directory: {}",
                dir.display()
            );
            let all = std::env::var_os("SILT_FMT_CORPUS_ALL").is_some_and(|v| v != "0");
            let keep: &dyn Fn(&Path) -> bool = if all { &|_| true } else { &is_silt };
            inputs_under(&dir, &dir, "corpus", keep)
        }
        _ => Vec::new(),
    }
}

/// The ```` ```silt ```` blocks of `README.md` and `docs/**/*.md`, each
/// named by its file and the line of its first line of code.
pub fn doc_snippets() -> Vec<Input> {
    let mut files = vec![repo().join("README.md")];
    files_under(
        &repo().join("docs"),
        &|p| p.extension().is_some_and(|e| e == "md"),
        &mut files,
    );
    let mut out = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = path
            .strip_prefix(repo())
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let mut block: Option<(usize, String)> = None;
        for (i, line) in text.lines().enumerate() {
            match &mut block {
                None if line.trim() == "```silt" => block = Some((i + 2, String::new())),
                None => {}
                Some((first_line, code)) if line.trim() == "```" => {
                    out.push(Input {
                        name: format!("{file}:{first_line}"),
                        class: "docs",
                        text: std::mem::take(code),
                    });
                    block = None;
                }
                Some((_, code)) => {
                    code.push_str(line);
                    code.push('\n');
                }
            }
        }
    }
    out
}

/// The gaps of `text`: the byte offset of each of its tokens, the end of
/// the file included. A comment put there stands between that token and
/// the one before it. Empty when `text` does not lex. A token that
/// continues a string after an interpolation hole has no gap in front of
/// it: what is put there is string text.
pub fn gaps(text: &str) -> Vec<usize> {
    let Ok(lexed) = Lexer::new(FileId::default(), text).tokenize() else {
        return Vec::new();
    };
    lexed
        .tokens
        .iter()
        .filter(|tok| {
            !matches!(
                tok.kind,
                Token::Newline | Token::StringMiddle(_) | Token::StringEnd(_)
            )
        })
        .map(|tok| tok.span.start as usize)
        .collect()
}

/// The stack of a thread that lexes, parses or formats: the parser and
/// the printer recurse, and some inputs nest deeply.
const STACK: usize = 64 << 20;

/// For each input, whether it lexes and parses.
fn parse(inputs: &[Input]) -> Vec<bool> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(scope, || {
                inputs
                    .iter()
                    .map(|input| {
                        let text = &input.text;
                        Lexer::new(FileId::default(), text)
                            .tokenize()
                            .is_ok_and(|lexed| Parser::new(lexed, text).parse_program().is_ok())
                    })
                    .collect()
            })
            .expect("spawn parser")
            .join()
            .expect("parse inputs")
    })
}

/// `input` as it is.
pub fn plain(inputs: &[Input]) -> Vec<Job<'_>> {
    inputs
        .iter()
        .map(|input| Job {
            input,
            mutation: Vec::new(),
            label: String::new(),
        })
        .collect()
}

/// The comment mutants of `inputs`: for every `every`-th gap of each
/// input, one job with a line comment there and one with a block comment.
/// The gaps that are taken shift by one from file to file. An input that
/// does not parse has no mutants, nor has one over `MAX_MUTATED_BYTES`.
pub fn mutants(inputs: &[Input], every: usize) -> Vec<Job<'_>> {
    let mut jobs = Vec::new();
    let parses = parse(inputs);
    for (file, input) in inputs.iter().enumerate() {
        if !parses[file] || input.text.len() > MAX_MUTATED_BYTES {
            continue;
        }
        for (n, site) in gaps(&input.text).into_iter().enumerate() {
            if (n + file) % every != 0 {
                continue;
            }
            for kind in [CommentKind::Line, CommentKind::Block] {
                jobs.push(Job {
                    input,
                    mutation: vec![(site, kind)],
                    label: String::new(),
                });
            }
        }
    }
    jobs
}

/// `count` jobs, each an input of `inputs` with two to twelve comments
/// of every kind at sites drawn with `seed`: what one comment per input
/// does not find, how two comments get in each other's way.
pub fn stress(inputs: &[Input], count: usize, seed: u64) -> Vec<Job<'_>> {
    // xorshift64*: the same jobs for the same seed, on every platform.
    let mut state = seed.max(1);
    let mut next = move |below: usize| {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as usize % below
    };
    let parses = parse(inputs);
    let sites: Vec<(&Input, Vec<usize>)> = inputs
        .iter()
        .zip(parses)
        .filter(|(input, parses)| *parses && input.text.len() <= MAX_MUTATED_BYTES)
        .map(|(input, _)| (input, gaps(&input.text)))
        .filter(|(_, gaps)| !gaps.is_empty())
        .collect();
    if sites.is_empty() {
        return Vec::new();
    }
    let kinds = [
        CommentKind::Line,
        CommentKind::Block,
        CommentKind::OwnLine,
        CommentKind::BlockOverLines,
    ];
    (0..count)
        .map(|n| {
            let (input, gaps) = &sites[next(sites.len())];
            let wanted = [2, 3, 4, 6, 8, 12][next(6)].min(gaps.len());
            let mut chosen: Vec<usize> = Vec::new();
            while chosen.len() < wanted {
                let site = gaps[next(gaps.len())];
                if !chosen.contains(&site) {
                    chosen.push(site);
                }
            }
            chosen.sort_unstable_by(|a, b| b.cmp(a));
            Job {
                input,
                mutation: chosen
                    .into_iter()
                    .map(|site| (site, kinds[next(4)]))
                    .collect(),
                label: format!("{wanted} comments (seed {seed}, job {n})"),
            }
        })
        .collect()
}

/// One input the formatter got wrong.
pub struct Failure {
    pub name: String,
    pub class: String,
    /// `refused`, `panicked`, `result does not parse`, `comments changed`,
    /// `header changed`, `second pass refused` or `not idempotent`.
    pub kind: &'static str,
    pub detail: String,
}

#[derive(Default)]
pub struct Report {
    /// Per class: the jobs whose text parses, and so were checked.
    pub checked: BTreeMap<String, usize>,
    /// Per class: the jobs whose text does not parse, which are not
    /// inputs (a mutant whose comment broke the program, a golden case
    /// about a syntax error).
    pub unparseable: BTreeMap<String, usize>,
    /// Per class: the time its jobs took, summed over the workers.
    pub time: BTreeMap<String, std::time::Duration>,
    pub failures: Vec<Failure>,
}

/// The comments of `text`, each with every run of white space as one
/// space, sorted. `None` when `text` does not lex and parse.
fn parsed_comments(text: &str) -> Option<Vec<String>> {
    let lexed = Lexer::new(FileId::default(), text).tokenize().ok()?;
    let mut comments: Vec<String> = lexed
        .comments
        .iter()
        .map(|c| {
            c.text(text)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    comments.sort();
    Parser::new(lexed, text).parse_program().ok()?;
    Some(comments)
}

/// The header of `text` as the formatter writes it: the comments that
/// start the file, `--` and `{- -}` alike, up to the first empty line or
/// the first declaration, one to a line, without the white space behind
/// them and with line feeds for their line ends. The golden harness reads
/// a case's directives from it, and a reader the file's purpose: the
/// result starts with exactly these bytes. Empty when `text` does not
/// lex or starts with no comment.
fn header(text: &str) -> String {
    let Ok(lexed) = Lexer::new(FileId::default(), text).tokenize() else {
        return String::new();
    };
    let Some(first) = lexed.tokens.iter().find(|tok| tok.kind != Token::Newline) else {
        return String::new();
    };
    let leading = &lexed.comments[..first.comments.end as usize];
    let mut header = String::new();
    for (i, comment) in leading.iter().enumerate() {
        if i > 0 && comment.newlines_before >= 2 {
            break;
        }
        // A `{- -}` comment with the first token behind it on its line
        // belongs to that token.
        let on_token_line = first.kind != Token::Eof
            && first.newlines_before == 0
            && leading[i + 1..].iter().all(|c| c.newlines_before == 0);
        if on_token_line && !comment.text(text).starts_with("--") {
            break;
        }
        header.push_str(&comment.text(text).trim_end().replace("\r\n", "\n"));
        header.push('\n');
    }
    header
}

/// The first line of `text`, shortened.
fn excerpt(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() > 160 {
        format!("{}...", line.chars().take(160).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Check one text. `Ok(false)` when it does not parse.
fn check(text: &str) -> Result<bool, (&'static str, String)> {
    let first = match format(text) {
        Formatted::Unparseable => return Ok(false),
        Formatted::Refused(message) => return Err(("refused", excerpt(&message))),
        Formatted::Ok(first) => first,
    };
    let Some(after) = parsed_comments(&first) else {
        return Err(("result does not parse", String::new()));
    };
    let before = parsed_comments(text).unwrap_or_default();
    if before != after {
        let lost = before.iter().find(|c| !after.contains(c));
        let detail = match lost {
            Some(c) => format!("lost `{}`", excerpt(c)),
            None => "a comment is repeated or new".to_string(),
        };
        return Err(("comments changed", detail));
    }
    let result = first.strip_prefix('\u{feff}').unwrap_or(&first);
    if !result.starts_with(&header(text)) {
        return Err(("header changed", excerpt(&first)));
    }
    match format(&first) {
        Formatted::Ok(second) if second == first => Ok(true),
        Formatted::Ok(second) => {
            let line = first
                .lines()
                .zip(second.lines())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| first.lines().count().min(second.lines().count()));
            Err((
                "not idempotent",
                format!("first difference at line {}", line + 1),
            ))
        }
        Formatted::Unparseable => Err(("second pass refused", "does not parse".to_string())),
        Formatted::Refused(message) => Err(("second pass refused", excerpt(&message))),
    }
}

/// The threads of a run that is part of the suite. `.config/nextest.toml`
/// counts a `fmt_property` test as this many, so that the run does not
/// take the CPUs from the tests beside it, some of which assert on
/// timing.
pub const SUITE_WORKERS: usize = 2;

/// Check `jobs` on `workers` threads.
pub fn run(jobs: &[Job<'_>], workers: usize) -> Report {
    let report = Mutex::new(Report::default());
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let worker = std::thread::Builder::new().stack_size(STACK);
            worker
                .spawn_scoped(scope, || {
                    loop {
                        let index = next.fetch_add(1, Ordering::SeqCst);
                        let Some(job) = jobs.get(index) else { break };
                        // A long run says where it is (visible with
                        // `--no-capture`).
                        if index > 0 && index.is_multiple_of(50_000) {
                            eprintln!("fmt property: {index} of {} jobs", jobs.len());
                        }
                        // The interner is per thread; keep it small.
                        silt::intern::reset();
                        let text = job.text();
                        let started = std::time::Instant::now();
                        let outcome = std::panic::catch_unwind(|| check(&text));
                        let took = started.elapsed();
                        let mut report = report.lock().unwrap();
                        *report.time.entry(job.class()).or_default() += took;
                        let failure = match outcome {
                            Ok(Ok(true)) => {
                                *report.checked.entry(job.class()).or_default() += 1;
                                continue;
                            }
                            Ok(Ok(false)) => {
                                *report.unparseable.entry(job.class()).or_default() += 1;
                                continue;
                            }
                            Ok(Err(failure)) => failure,
                            Err(_) => ("panicked", String::new()),
                        };
                        *report.checked.entry(job.class()).or_default() += 1;
                        // `SILT_FMT_FAILED=<dir>` keeps the inputs that
                        // failed, numbered as in the report.
                        let mut name = job.name();
                        if let Some(dir) = std::env::var_os("SILT_FMT_FAILED") {
                            let file = format!("failed-{}.silt", report.failures.len());
                            let _ = std::fs::create_dir_all(&dir);
                            let _ = std::fs::write(Path::new(&dir).join(&file), &text);
                            name = format!("{name} [{file}]");
                        }
                        report.failures.push(Failure {
                            name,
                            class: job.class(),
                            kind: failure.0,
                            detail: failure.1,
                        });
                    }
                })
                .expect("spawn worker");
        }
    });
    let mut report = report.into_inner().unwrap();
    report.failures.sort_by(|a, b| a.name.cmp(&b.name));
    report
}

impl Report {
    /// The counts per class, the failures per kind, and the first
    /// failures of each class.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        let mut failed: BTreeMap<&str, usize> = BTreeMap::new();
        let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &self.failures {
            *failed.entry(&f.class).or_default() += 1;
            *kinds.entry(f.kind).or_default() += 1;
        }
        let classes: Vec<&String> = self
            .checked
            .keys()
            .chain(
                self.unparseable
                    .keys()
                    .filter(|k| !self.checked.contains_key(*k)),
            )
            .collect();
        for class in classes {
            out.push_str(&format!(
                "  {class}: {} checked, {} failed ({} more do not parse), {:.0?}\n",
                self.checked.get(class).copied().unwrap_or(0),
                failed.get(class.as_str()).copied().unwrap_or(0),
                self.unparseable.get(class).copied().unwrap_or(0),
                self.time.get(class).copied().unwrap_or_default(),
            ));
        }
        for (kind, n) in &kinds {
            out.push_str(&format!("  {n} {kind}\n"));
        }
        let mut shown: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &self.failures {
            let n = shown.entry(&f.class).or_default();
            *n += 1;
            if *n <= 10 {
                out.push_str(&format!("  {}\n", f.line()));
            }
        }
        out
    }
}

impl Failure {
    fn line(&self) -> String {
        if self.detail.is_empty() {
            format!("{}: {}", self.name, self.kind)
        } else {
            format!("{}: {}: {}", self.name, self.kind, self.detail)
        }
    }
}

/// Print the report of the run called `what`; every input must pass.
pub fn conclude(what: &str, report: &Report) {
    let checked: usize = report.checked.values().sum();
    let failures = report.failures.len();
    let summary = report.summary();
    eprintln!("fmt property, {what}: {failures} of {checked} inputs failed\n{summary}");
    if let Some(path) = std::env::var_os("SILT_FMT_REPORT").filter(|p| !p.is_empty()) {
        let mut text = format!("{what}: {failures} of {checked} inputs failed\n{summary}\n");
        for f in &report.failures {
            text.push_str(&f.line());
            text.push('\n');
        }
        // Several runs may share the file.
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open SILT_FMT_REPORT");
        file.write_all(text.as_bytes())
            .expect("write SILT_FMT_REPORT");
    }
    assert!(checked > 0, "fmt property, {what}: no input was checked");
    assert!(
        failures == 0,
        "fmt property, {what}: {failures} of {checked} inputs failed\n{summary}"
    );
}
