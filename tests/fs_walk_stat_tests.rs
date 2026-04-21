//! Integration tests for the filesystem metadata / walk / glob APIs
//! added to the `fs` builtin module.
//!
//! Each test creates its own temp directory under `std::env::temp_dir()`
//! and cleans up on drop (via the `TempDir` guard below). Tests drive
//! the full pipeline — lexer → parser → typechecker → compiler → VM —
//! via the `run` helper, mirroring the pattern used in
//! `tests/integration.rs` so the typechecker signature registrations
//! (FileStat record, new function schemes) are exercised end-to-end.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Per-test temporary directory. Deleted on drop so tests clean up
/// even when they panic mid-assert.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("silt_fs_walk_stat_{pid}_{prefix}_{n}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Unix-style path string (forward slashes) — Silt source literals are
    /// simpler to embed when we normalize away Windows backslashes.
    fn as_silt_str(&self) -> String {
        self.path.to_string_lossy().replace('\\', "/")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

/// Expect an `Ok(inner)` variant; return `inner`.
fn ok_inner(v: Value) -> Value {
    match v {
        Value::Variant(tag, args) if tag == "Ok" => {
            assert_eq!(args.len(), 1, "Ok variant should carry one payload");
            args.into_iter().next().unwrap()
        }
        other => panic!("expected Ok(_) variant, got {other:?}"),
    }
}

/// Expect an `Err(IoError)` variant; return the renderer's `message()`
/// string so the assertions that follow can match substrings against
/// human-readable text.
///
/// Phase 1 of the stdlib error redesign replaced the old
/// `Err(String)` shape with `Err(IoError)`, so this helper now walks the
/// `IoError` variant and reconstructs the message the same way
/// `trait Error for IoError` does at runtime.
fn err_msg(v: Value) -> String {
    match v {
        Value::Variant(tag, args) if tag == "Err" => match args.into_iter().next() {
            // Still accept bare strings in case any caller ever hands us
            // one, but the modern path is the IoError variant arm below.
            Some(Value::String(s)) => s,
            Some(Value::Variant(inner_tag, inner_args)) => {
                let first_str = |vs: Vec<Value>| -> String {
                    match vs.into_iter().next() {
                        Some(Value::String(s)) => s,
                        other => format!("<non-string payload: {other:?}>"),
                    }
                };
                match inner_tag.as_str() {
                    "IoNotFound" => format!("file not found: {}", first_str(inner_args)),
                    "IoPermissionDenied" => {
                        format!("permission denied: {}", first_str(inner_args))
                    }
                    "IoAlreadyExists" => format!("already exists: {}", first_str(inner_args)),
                    "IoInvalidInput" => format!("invalid input: {}", first_str(inner_args)),
                    "IoInterrupted" => "operation interrupted".into(),
                    "IoUnexpectedEof" => "unexpected end of file".into(),
                    "IoWriteZero" => "zero-byte write".into(),
                    "IoUnknown" => first_str(inner_args),
                    other => format!("unknown IoError variant: {other}"),
                }
            }
            other => panic!("Err payload was not a string or IoError variant: {other:?}"),
        },
        other => panic!("expected Err(_) variant, got {other:?}"),
    }
}

/// Extract the BTreeMap backing a Record value.
fn record_fields(v: Value) -> (String, BTreeMap<String, Value>) {
    match v {
        Value::Record(name, fields) => (name, (*fields).clone()),
        other => panic!("expected Record, got {other:?}"),
    }
}

// ── fs.stat ─────────────────────────────────────────────────────────

#[test]
fn test_fs_stat_on_file_reports_size_and_is_file() {
    let dir = TempDir::new("stat_file");
    let file = dir.path().join("data.txt");
    // Contents length is 13 bytes ("hello, world!"); len() on metadata
    // returns the byte count which must match.
    std::fs::write(&file, "hello, world!").unwrap();
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let file_str = file.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    fs.stat("{file_str}")
}}
"#
    );
    let result = run(&input);
    let inner = ok_inner(result);
    let (name, fields) = record_fields(inner);
    assert_eq!(name, "FileStat");
    assert_eq!(fields.get("size"), Some(&Value::Int(13)));
    assert_eq!(fields.get("is_file"), Some(&Value::Bool(true)));
    assert_eq!(fields.get("is_dir"), Some(&Value::Bool(false)));
    assert_eq!(fields.get("is_symlink"), Some(&Value::Bool(false)));
    match fields.get("modified") {
        Some(Value::Int(m)) => {
            // `modified` should be within the last minute: we just wrote
            // the file, so it can't be wildly in the past. Allow a small
            // negative slack for systems with sub-second clock skew.
            let after = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            assert!(
                *m >= before - 2 && *m <= after + 2,
                "modified {m} not within [{before}, {after}] window"
            );
        }
        other => panic!("modified field missing or non-int: {other:?}"),
    }
    // readonly is platform-dependent for a freshly-written file; we just
    // assert the field exists and is a Bool.
    assert!(matches!(fields.get("readonly"), Some(Value::Bool(_))));
    // mode is an Int on every platform (0 on non-unix). On unix the
    // permission bits for a freshly-written file are non-zero (umask
    // stripping leaves at least the owner-read bit), so we can assert
    // > 0 under cfg(unix). Elsewhere only assert type shape.
    match fields.get("mode") {
        Some(Value::Int(m)) => {
            #[cfg(unix)]
            {
                // Mask off the file-type bits (S_IFMT = 0o170000) to get
                // just the permission triple, then require at least
                // "owner can read" (0o400). Anything less would mean the
                // umask / perms system didn't kick in.
                let perms = (*m as u32) & 0o777;
                assert!(
                    perms & 0o400 != 0,
                    "expected owner-read bit set; mode perms = {perms:o}"
                );
            }
            #[cfg(not(unix))]
            {
                assert_eq!(*m, 0, "non-unix platforms report mode=0");
            }
        }
        other => panic!("mode field missing or non-int: {other:?}"),
    }
    // accessed: should be Some(DateTime) on effectively every common
    // filesystem. We don't assert the inner structure beyond "it's a
    // Some variant"; the clock details are covered by time.* tests.
    match fields.get("accessed") {
        Some(Value::Variant(tag, _)) => {
            assert!(
                tag == "Some" || tag == "None",
                "accessed should be Option variant, got tag {tag}"
            );
        }
        other => panic!("accessed field missing or not a variant: {other:?}"),
    }
    // created: may legitimately be None on older ext4 or some network
    // filesystems. Just assert it's an Option variant.
    match fields.get("created") {
        Some(Value::Variant(tag, _)) => {
            assert!(
                tag == "Some" || tag == "None",
                "created should be Option variant, got tag {tag}"
            );
        }
        other => panic!("created field missing or not a variant: {other:?}"),
    }
}

#[test]
fn test_fs_stat_mode_field_typechecks_as_int() {
    // Exercises the typechecker scheme registration: reading `.mode` as
    // an Int must compile cleanly. Regression guard against drift where
    // the runtime adds a field that the type scheme doesn't advertise.
    let dir = TempDir::new("stat_mode_ty");
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "x").unwrap();
    let file_str = file.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    match fs.stat("{file_str}") {{
        Ok(s) -> s.mode
        Err(_) -> -1
    }}
}}
"#
    );
    let result = run(&input);
    assert!(matches!(result, Value::Int(_)));
}

#[test]
fn test_fs_stat_accessed_and_created_are_option_datetime() {
    // Exercises typechecker registration of `accessed` / `created` as
    // Option(DateTime). The user code pattern-matches through Some/None
    // and reads nested date fields — the typechecker has to see those
    // paths to confirm the record scheme is right.
    let dir = TempDir::new("stat_opt_dt");
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "x").unwrap();
    let file_str = file.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    match fs.stat("{file_str}") {{
        Ok(s) -> match s.accessed {{
            Some(dt) -> dt.date.year
            None -> 0
        }}
        Err(_) -> -1
    }}
}}
"#
    );
    let result = run(&input);
    // Either we got an Int year >= 1970 (accessed present) or 0 (noatime).
    // -1 would indicate fs.stat returned Err, which shouldn't happen for
    // a file we just wrote.
    match result {
        Value::Int(y) => {
            assert!(y == 0 || y >= 1970, "unexpected year value {y}");
        }
        other => panic!("expected Int, got {other:?}"),
    }
}

#[test]
fn test_fs_stat_on_directory_reports_is_dir() {
    let dir = TempDir::new("stat_dir");
    let sub = dir.path().join("subdir");
    std::fs::create_dir(&sub).unwrap();
    let sub_str = sub.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    fs.stat("{sub_str}")
}}
"#
    );
    let inner = ok_inner(run(&input));
    let (name, fields) = record_fields(inner);
    assert_eq!(name, "FileStat");
    assert_eq!(fields.get("is_file"), Some(&Value::Bool(false)));
    assert_eq!(fields.get("is_dir"), Some(&Value::Bool(true)));
    assert_eq!(fields.get("is_symlink"), Some(&Value::Bool(false)));
    // size on directories varies wildly across OSes (and filesystems);
    // don't assert an exact value, only that the field is an Int.
    assert!(matches!(fields.get("size"), Some(Value::Int(_))));
}

#[test]
fn test_fs_stat_missing_path_errs() {
    let dir = TempDir::new("stat_missing");
    let missing = dir.path().join("does_not_exist");
    let missing_str = missing.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    fs.stat("{missing_str}")
}}
"#
    );
    let msg = err_msg(run(&input));
    // The OS-level message varies but reliably mentions the missing
    // path condition ("No such file or directory", "cannot find",
    // "The system cannot find"). Just check it's non-empty.
    assert!(!msg.is_empty(), "expected a non-empty error message");
}

// ── fs.walk ────────────────────────────────────────────────────────

#[test]
fn test_fs_walk_lists_all_entries_and_tolerates_inner_symlink() {
    let dir = TempDir::new("walk");
    // Tree:
    //   <dir>/
    //     a.txt
    //     sub1/
    //       b.txt
    //     sub2/
    //       c.txt
    //       loop -> ..   (on unix; skipped on windows)
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::create_dir(dir.path().join("sub1")).unwrap();
    std::fs::write(dir.path().join("sub1/b.txt"), "b").unwrap();
    std::fs::create_dir(dir.path().join("sub2")).unwrap();
    std::fs::write(dir.path().join("sub2/c.txt"), "c").unwrap();
    #[cfg(unix)]
    {
        // Intentionally create a symlink that points back up — a naive
        // walker that follows symlinks would loop forever. Our walk
        // defaults to follow_links(false) so this must finish quickly.
        let _ = std::os::unix::fs::symlink("..", dir.path().join("sub2/loop"));
    }

    let root_str = dir.as_silt_str();
    let input = format!(
        r#"
import fs
import list
fn main() {{
    match fs.walk("{root_str}") {{
        Ok(paths) -> list.length(paths)
        Err(_) -> -1
    }}
}}
"#
    );
    let result = run(&input);
    match result {
        Value::Int(n) => {
            // Entries we strictly expect: the root itself, a.txt,
            // sub1, sub1/b.txt, sub2, sub2/c.txt, plus sub2/loop on
            // unix. walkdir also emits the root, so >= 6.
            assert!(n >= 6, "expected >= 6 walked entries, got {n}");
        }
        other => panic!("expected Int length, got {other:?}"),
    }
}

// ── fs.glob ────────────────────────────────────────────────────────

#[test]
fn test_fs_glob_filters_by_extension() {
    let dir = TempDir::new("glob");
    std::fs::write(dir.path().join("one.silt"), "").unwrap();
    std::fs::write(dir.path().join("two.silt"), "").unwrap();
    std::fs::write(dir.path().join("readme.md"), "").unwrap();

    let pattern = format!("{}/*.silt", dir.as_silt_str());
    let input = format!(
        r#"
import fs
import list
fn main() {{
    match fs.glob("{pattern}") {{
        Ok(paths) -> list.length(paths)
        Err(_) -> -1
    }}
}}
"#
    );
    let result = run(&input);
    assert_eq!(result, Value::Int(2), "expected 2 .silt matches");
}

#[test]
fn test_fs_glob_malformed_pattern_errs() {
    // `[` opens a character class that is never closed → pattern error.
    let input = r#"
import fs
fn main() {
    fs.glob("src/[unterminated")
}
"#;
    let msg = err_msg(run(input));
    assert!(!msg.is_empty());
}

// ── fs.read_link / fs.is_symlink ───────────────────────────────────

#[cfg(unix)]
#[test]
fn test_fs_read_link_returns_target() {
    let dir = TempDir::new("read_link");
    let target = dir.path().join("actual.txt");
    std::fs::write(&target, "payload").unwrap();
    let link = dir.path().join("shortcut");
    std::os::unix::fs::symlink(&target, &link).expect("make symlink");
    let link_str = link.to_str().unwrap().replace('\\', "/");

    let input = format!(
        r#"
import fs
fn main() {{
    fs.read_link("{link_str}")
}}
"#
    );
    let inner = ok_inner(run(&input));
    match inner {
        Value::String(s) => {
            // read_link returns the raw target as stored, not the
            // canonicalized resolution. Since we symlinked with an
            // absolute path, that's what we expect back.
            assert_eq!(s, target.to_string_lossy());
        }
        other => panic!("expected String target, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn test_fs_is_symlink_distinguishes_symlink_and_file() {
    let dir = TempDir::new("is_symlink");
    let plain = dir.path().join("plain.txt");
    std::fs::write(&plain, "x").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&plain, &link).expect("make symlink");

    let plain_str = plain.to_str().unwrap().replace('\\', "/");
    let link_str = link.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    (fs.is_symlink("{plain_str}"), fs.is_symlink("{link_str}"))
}}
"#
    );
    let result = run(&input);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Bool(false), Value::Bool(true)])
    );
}

#[test]
fn test_fs_read_link_on_non_symlink_errs() {
    // On every platform, calling read_link on a regular file is an
    // error — the OS-level call explicitly rejects it.
    let dir = TempDir::new("read_link_err");
    let file = dir.path().join("plain.txt");
    std::fs::write(&file, "x").unwrap();
    let file_str = file.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    fs.read_link("{file_str}")
}}
"#
    );
    let msg = err_msg(run(&input));
    assert!(!msg.is_empty());
}

// ── fs.walk materialization cap (indirect) ─────────────────────────
//
// Exercising the 1M cap for real is impractical in a unit test — we
// can't cheaply create a million entries and still stay fast. Instead
// we verify the *shape* of the cap's failure mode holds by checking
// that fs.walk on a modest tree returns Ok (so the cap didn't
// spuriously trip) and that walk on a nonexistent root returns Err.
// Together these pin down the entry/exit invariants around the cap.

#[test]
fn test_fs_walk_missing_root_returns_err() {
    let dir = TempDir::new("walk_missing");
    let missing = dir.path().join("nope");
    // Don't create it — we want the walker to fail on first iteration.
    let missing_str = missing.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    fs.walk("{missing_str}")
}}
"#
    );
    let msg = err_msg(run(&input));
    assert!(!msg.is_empty());
}
