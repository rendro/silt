//! Integration tests for the filesystem metadata / walk / symlink APIs
//! of the `fs` builtin module that need a temp-dir tree built from Rust
//! (symlinks, fresh mtimes). Cases that need no setup live in
//! `tests/golden/lang/fs/fs_walk_stat__*`.
//!
//! Each test creates its own temp directory under `std::env::temp_dir()`
//! and cleans up on drop (via the `TempDir` guard below). Tests drive
//! the full pipeline — lexer → parser → typechecker → compiler → VM —
//! via the `run` helper, mirroring the pattern used in
//! `tests/heavy/integration.rs` so the typechecker signature registrations
//! (FileStat record, new function schemes) are exercised end-to-end.

use silt::typeinfo::bv;
use silt::value::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    silt::session::testing::run_str(input).unwrap_or_else(|e| panic!("{e}"))
}

/// Expect an `Ok(inner)` variant; return `inner`.
fn ok_inner(v: Value) -> Value {
    match v {
        Value::Variant(ok) if ok.is(bv::OK) => {
            assert_eq!(ok.fields().len(), 1, "Ok variant should carry one payload");
            ok.fields()[0].clone()
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
/// `IoError` variant and reconstructs the message the same way an
/// `IoError` is shown at runtime.
fn err_msg(v: Value) -> String {
    match v {
        Value::Variant(err) if err.is(bv::ERR) => match err.fields().first() {
            // Still accept bare strings in case any caller ever hands us
            // one, but the modern path is the IoError variant arm below.
            Some(Value::String(s, _)) => s.to_string(),
            Some(Value::Variant(error)) => {
                let first_str = |vs: &[Value]| -> String {
                    match vs.first() {
                        Some(Value::String(s, _)) => s.to_string(),
                        other => format!("<non-string payload: {other:?}>"),
                    }
                };
                match error.name() {
                    "NotFound" => format!("file not found: {}", first_str(error.fields())),
                    "PermissionDenied" => {
                        format!("permission denied: {}", first_str(error.fields()))
                    }
                    "AlreadyExists" => format!("already exists: {}", first_str(error.fields())),
                    "InvalidInput" => format!("invalid input: {}", first_str(error.fields())),
                    "Interrupted" => "operation interrupted".into(),
                    "UnexpectedEof" => "unexpected end of file".into(),
                    "WriteZero" => "zero-byte write".into(),
                    "Other" => first_str(error.fields()),
                    other => format!("unknown IoError variant: {other}"),
                }
            }
            other => panic!("Err payload was not a string or IoError variant: {other:?}"),
        },
        other => panic!("expected Err(_) variant, got {other:?}"),
    }
}

/// The type name of a Record value, and its fields by name.
fn record_fields(v: Value) -> (String, BTreeMap<String, Value>) {
    match v {
        Value::Record(record) => (
            record.ty().name.clone(),
            record
                .named()
                .map(|(name, value)| (name.to_string(), value.clone()))
                .collect(),
        ),
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

    let file_str = file.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    host.give(fs.stat("{file_str}"))
}}
import host
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
    // modified: an Option(DateTime), Some on every common filesystem for
    // a file just written; the clock details are covered by time.* tests.
    match fields.get("modified") {
        Some(Value::Variant(option)) => {
            assert!(
                option.is(bv::SOME),
                "modified should be Some(DateTime), got {}",
                option.name()
            );
        }
        other => panic!("modified field missing or not a variant: {other:?}"),
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
        Some(Value::Variant(option)) => {
            assert!(
                option.is(bv::SOME) || option.is(bv::NONE),
                "accessed should be Option variant, got option {}",
                option.name()
            );
        }
        other => panic!("accessed field missing or not a variant: {other:?}"),
    }
    // created: may legitimately be None on older ext4 or some network
    // filesystems. Just assert it's an Option variant.
    match fields.get("created") {
        Some(Value::Variant(option)) => {
            assert!(
                option.is(bv::SOME) || option.is(bv::NONE),
                "created should be Option variant, got option {}",
                option.name()
            );
        }
        other => panic!("created field missing or not a variant: {other:?}"),
    }
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
    host.give(match fs.walk("{root_str}") {{
        Ok(paths) -> list.length(paths)
        Err(_) -> -1
    }})
}}
import host
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

/// The paths `call` (`fs.walk(...)` or `fs.glob(...)`) gives, sorted,
/// one per line.
fn sorted_paths(call: &str) -> String {
    let input = format!(
        r#"
import fs
import list
import string
fn main() {{
    host.give(match {call} {{
        Ok(paths) -> string.join(list.sort(paths), "\n")
        Err(e) -> "Err: {{e}}"
    }})
}}
import host
"#
    );
    match run(&input) {
        Value::String(s, _) => s.to_string(),
        other => panic!("expected a String, got {other:?}"),
    }
}

/// A tree with a link to a file outside it, a link to a directory
/// outside it and a link to itself:
///   <dir>/tree/own.txt
///   <dir>/tree/file -> ../outside/keep.txt
///   <dir>/tree/dir -> ../outside
///   <dir>/tree/self -> .
///   <dir>/outside/keep.txt
#[cfg(unix)]
fn tree_with_links(dir: &TempDir) -> PathBuf {
    let tree = dir.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::create_dir(dir.path().join("outside")).unwrap();
    std::fs::write(tree.join("own.txt"), "own").unwrap();
    std::fs::write(dir.path().join("outside/keep.txt"), "keep").unwrap();
    std::os::unix::fs::symlink("../outside/keep.txt", tree.join("file")).unwrap();
    std::os::unix::fs::symlink("../outside", tree.join("dir")).unwrap();
    std::os::unix::fs::symlink(".", tree.join("self")).unwrap();
    tree
}

/// fs.walk lists a link inside the tree as itself, never as the path
/// it leads to (which lies outside the tree), and goes into no linked
/// directory.
#[cfg(unix)]
#[test]
fn test_fs_walk_lists_a_link_as_itself() {
    let dir = TempDir::new("walk_links");
    let tree = tree_with_links(&dir);
    let root = std::fs::canonicalize(&tree).unwrap();
    let root = root.to_string_lossy();
    let walked = sorted_paths(&format!("fs.walk(\"{}\")", tree.to_string_lossy()));
    assert_eq!(
        walked,
        format!("{root}\n{root}/dir\n{root}/file\n{root}/own.txt\n{root}/self"),
    );
}

/// fs.walk shows every path as the root, its own links resolved, joined
/// with the path below it: a root given through a link and relative
/// parts (`tree/../tree`) give the same absolute paths.
#[cfg(unix)]
#[test]
fn test_fs_walk_resolves_only_the_root() {
    let dir = TempDir::new("walk_root");
    let tree = tree_with_links(&dir);
    std::os::unix::fs::symlink("tree", dir.path().join("alias")).unwrap();
    let root = std::fs::canonicalize(&tree).unwrap();
    let root = root.to_string_lossy();
    let expected = format!("{root}\n{root}/dir\n{root}/file\n{root}/own.txt\n{root}/self");
    let base = dir.as_silt_str();
    for given in [format!("{base}/alias"), format!("{base}/tree/../tree")] {
        assert_eq!(
            sorted_paths(&format!("fs.walk(\"{given}\")")),
            expected,
            "{given}"
        );
    }
}

/// fs.glob's `**` and `*` go into no linked directory: a link to the
/// tree itself does not make `**` loop, and one to a directory outside
/// does not reach into it. A link is matched by its own name.
#[cfg(unix)]
#[test]
fn test_fs_glob_goes_into_no_linked_directory() {
    let dir = TempDir::new("glob_links");
    let tree = tree_with_links(&dir);
    let tree = tree.to_string_lossy();
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/**/*.txt\")")),
        format!("{tree}/own.txt"),
    );
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/*/*.txt\")")),
        String::new(),
    );
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/*\")")),
        format!("{tree}/dir\n{tree}/file\n{tree}/own.txt\n{tree}/self"),
    );
    // A pattern ending in `**` matches the directories below, and a
    // link is none.
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/**\")")),
        String::new()
    );
    // A link written in the pattern with no wildcard before it is
    // followed, as the system does; the root among them.
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/dir/*.txt\")")),
        format!("{tree}/dir/keep.txt"),
    );
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/dir/keep.txt\")")),
        format!("{tree}/dir/keep.txt"),
    );
    std::os::unix::fs::symlink("tree", dir.path().join("alias")).unwrap();
    let alias = format!("{}/alias", dir.as_silt_str());
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{alias}/*.txt\")")),
        format!("{alias}/own.txt"),
    );
    // A link a wildcard matched is not gone into, also when what
    // follows is no wildcard.
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/d*/keep.txt\")")),
        String::new(),
    );
    // Nor is a link named after a wildcard: `*` matches the real
    // directory `real`, and its `inner` leads outside.
    std::fs::create_dir(format!("{tree}/real")).unwrap();
    std::os::unix::fs::symlink("../../outside", format!("{tree}/real/inner")).unwrap();
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/*/inner/keep.txt\")")),
        String::new(),
    );
    // `*/` and `**/` list real directories only.
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/*/\")")),
        format!("{tree}/real"),
    );
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{tree}/**/\")")),
        format!("{tree}/real"),
    );
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
    host.give(fs.read_link("{link_str}"))
}}
import host
"#
    );
    let inner = ok_inner(run(&input));
    match inner {
        Value::String(s, _) => {
            // read_link returns the raw target as stored, not the
            // canonicalized resolution. Since we symlinked with an
            // absolute path, that's what we expect back.
            assert_eq!(s, target.to_string_lossy().into());
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
    host.give((fs.is_symlink("{plain_str}"), fs.is_symlink("{link_str}")))
}}
import host
"#
    );
    let result = run(&input);
    assert_eq!(
        result,
        Value::tuple(vec![Value::Bool(false), Value::Bool(true)])
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
    host.give(fs.read_link("{file_str}"))
}}
import host
"#
    );
    let msg = err_msg(run(&input));
    // OS wording varies ("Invalid argument" on Linux, a different
    // ENOTSUP-style phrasing on macOS, different again on Windows), so
    // we do not lock the inner `err.to_string()` text. On unix the
    // kernel returns EINVAL, which std classifies as `InvalidInput`,
    // and silt shows that `IoError` with the `"invalid
    // input:"` prefix — lock that prefix under cfg(unix). Everywhere
    // else we still pin non-empty, so a regression that swallows the
    // error to "" or panics with a bare Rust message would fail.
    assert!(!msg.is_empty());
    #[cfg(unix)]
    assert!(
        msg.contains("invalid input"),
        "expected silt's InvalidInput framing 'invalid input' in: {msg}"
    );
}

// ── fs.walk, fs.glob: a directory that cannot be read ───────────────

/// One directory that cannot be read fails fs.walk and fs.glob, and the
/// error names that directory, as fs.list_dir does: not the root of the
/// walk, not the pattern. (Unix: a directory is made unreadable by its
/// mode; a test run as root reads it anyway, and then has nothing to
/// show.)
#[cfg(unix)]
#[test]
fn test_fs_walk_and_glob_name_the_directory_they_cannot_read() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("walk_denied");
    let root = dir.path().join("w");
    let locked = root.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(root.join("a.txt"), "a").unwrap();
    std::fs::write(locked.join("b.txt"), "b").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(&locked).is_ok();
    let call = |expr: String| {
        err_msg(run(&format!(
            "import fs\nimport host\nfn main() {{\n  host.give({expr})\n}}\n"
        )))
    };
    let (root_str, locked_str) = (root.to_string_lossy(), locked.to_string_lossy());
    let outcomes = (!readable).then(|| {
        (
            call(format!("fs.walk(\"{root_str}\")")),
            call(format!("fs.glob(\"{root_str}/**/*.txt\")")),
            call(format!("fs.glob(\"{root_str}/*/*\")")),
            call(format!("fs.list_dir(\"{locked_str}\")")),
        )
    });
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let Some((walked, globbed, starred, listed)) = outcomes else {
        eprintln!("skipped: the test runs as a user that reads any directory");
        return;
    };
    let denied = format!("permission denied: {locked_str}");
    assert_eq!(listed, denied);
    assert_eq!(walked, denied);
    assert_eq!(globbed, denied);
    assert_eq!(starred, denied);
}

// ── fs.list_dir, fs.walk, fs.glob: a name that is no UTF-8 ──────────

/// A file name that is no UTF-8 fails fs.list_dir, fs.walk and fs.glob
/// alike, and the error names the directory it is in: no String can
/// hold the name, and the name with U+FFFD in it (which list_dir and
/// walk gave) names no file, while glob left the file out without a
/// word. A pattern that lists no such directory is not touched. (Not on
/// macOS, whose file system refuses such a name, nor on Windows.)
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn test_a_file_name_that_is_no_utf8_fails_the_listing_and_names_its_directory() {
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new("names");
    let names = dir.path().join("names");
    std::fs::create_dir_all(names.join("sub")).unwrap();
    std::fs::write(names.join("ok.txt"), "ok").unwrap();
    std::fs::write(names.join("sub/fine.txt"), "fine").unwrap();
    let bad = std::ffi::OsStr::from_bytes(b"bad\xffname.txt");
    if std::fs::write(names.join(bad), "bad").is_err() {
        eprintln!("skipped: the file system refuses a name that is no UTF-8");
        return;
    }
    let (base, at) = (dir.as_silt_str(), names.to_string_lossy());
    let call = |expr: String| {
        err_msg(run(&format!(
            "import fs\nimport host\nfn main() {{\n  host.give({expr})\n}}\n"
        )))
    };
    let refused = format!("invalid input: a name in {at} is not UTF-8: bad\u{FFFD}name.txt");
    assert_eq!(call(format!("fs.list_dir(\"{at}\")")), refused);
    assert_eq!(call(format!("fs.walk(\"{at}\")")), refused);
    assert_eq!(call(format!("fs.walk(\"{base}\")")), refused);
    assert_eq!(call(format!("fs.glob(\"{at}/*.txt\")")), refused);
    assert_eq!(call(format!("fs.glob(\"{base}/**/fine.txt\")")), refused);
    // A pattern that lists only other directories, or names its path.
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{at}/sub/*.txt\")")),
        format!("{at}/sub/fine.txt")
    );
    assert_eq!(
        sorted_paths(&format!("fs.glob(\"{at}/ok.txt\")")),
        format!("{at}/ok.txt")
    );
}

// ── stream.file_lines: a line is as long as the file makes it ───────

/// A line of a file longer than 10,000,000 bytes is read whole, as
/// io.read_file reads the file whole: what limits a read is what comes
/// from the network, not a file the person who runs the program chose.
/// (It ended the source with "line 2 of <path> is longer than 10000000
/// bytes".)
#[test]
fn test_a_line_of_a_file_longer_than_ten_million_bytes_is_read_whole() {
    let dir = TempDir::new("long_line");
    let file = dir.path().join("long.txt");
    let mut text = b"ok\n".to_vec();
    text.extend(std::iter::repeat_n(b'x', 10_000_001));
    text.extend(b"\nafter\n");
    std::fs::write(&file, text).unwrap();
    let at = file.to_string_lossy().replace('\\', "/");
    let input = format!(
        "import list\nimport stream\nimport string\nimport host\nfn main() {{\n  \
         let lengths = stream.collect(stream.file_lines(\"{at}\")) |> list.map {{ line ->\n    \
         match line {{\n      Ok(text) -> string.length(text)\n      Err(_) -> -1\n    }}\n  }}\n  \
         host.give(lengths)\n}}\n"
    );
    assert_eq!(
        run(&input),
        Value::list(vec![Value::Int(2), Value::Int(10_000_001), Value::Int(5)])
    );
}

// ── fs.remove: a link is removed itself ─────────────────────────────

/// A link to a directory, made as the system makes one: `None` where
/// the system does not let the test make it (Windows without the
/// privilege for links).
fn dir_link(target: &Path, link: &Path) -> Option<()> {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(target, link);
    made.ok()
}

/// fs.remove removes a link to a directory as it removes a link to a
/// file: the link goes, the directory and what is in it stay. (It was
/// refused with "not a directory", the link left in place.)
#[test]
fn test_fs_remove_removes_a_link_to_a_directory_not_the_directory() {
    let dir = TempDir::new("remove_dir_link");
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "keep").unwrap();
    let link = dir.path().join("link");
    if dir_link(&target, &link).is_none() {
        eprintln!("skipped: the system does not let this test make a link");
        return;
    }
    let link_str = link.to_string_lossy().replace('\\', "/");
    let input = format!(
        "import fs\nimport host\nfn main() {{\n  host.give(fs.remove(\"{link_str}\"))\n}}\n"
    );
    assert_eq!(ok_inner(run(&input)), Value::Unit);
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the link is gone"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("keep.txt")).unwrap(),
        "keep"
    );
    // A link to a file and a link that leads nowhere go as before, and
    // a directory with something in it is still refused.
    #[cfg(unix)]
    {
        let file_link = dir.path().join("file_link");
        std::os::unix::fs::symlink(target.join("keep.txt"), &file_link).unwrap();
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();
        for gone in [&file_link, &dangling] {
            let at = gone.to_string_lossy();
            let input = format!(
                "import fs\nimport host\nfn main() {{\n  host.give(fs.remove(\"{at}\"))\n}}\n"
            );
            assert_eq!(ok_inner(run(&input)), Value::Unit, "{at}");
            assert!(std::fs::symlink_metadata(gone).is_err(), "{at} is gone");
        }
        let full = target.to_string_lossy();
        let input = format!(
            "import fs\nimport host\nfn main() {{\n  host.give(fs.remove(\"{full}\"))\n}}\n"
        );
        let refused = err_msg(run(&input));
        assert!(refused.contains(&*full), "{refused}");
        assert!(target.join("keep.txt").exists());
    }
}

// ── fs.copy, fs.rename: a fault of a directory ──────────────────────

/// A fault of `fs.copy` or `fs.rename` that comes of a directory names
/// the path it is about, in the form of `file not found: <path>`. (Unix:
/// Windows answers these with other faults.)
#[cfg(unix)]
#[test]
fn copy_and_rename_name_the_path_of_a_directory_fault() {
    let dir = TempDir::new("moves");
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::create_dir_all(dir.path().join("dir/sub")).unwrap();
    std::fs::create_dir_all(dir.path().join("full/inner")).unwrap();
    let d = dir.as_silt_str();
    for (call, message) in [
        (
            format!(r#"fs.copy("{d}/a.txt", "{d}/dir")"#),
            format!("is a directory: {d}/dir"),
        ),
        (
            format!(r#"fs.copy("{d}/dir", "{d}/out.txt")"#),
            format!("is a directory: {d}/dir"),
        ),
        (
            format!(r#"fs.rename("{d}/a.txt", "{d}/dir")"#),
            format!("is a directory: {d}/dir"),
        ),
        (
            format!(r#"fs.rename("{d}/dir", "{d}/a.txt")"#),
            format!("not a directory: {d}/a.txt"),
        ),
        (
            format!(r#"fs.rename("{d}/dir", "{d}/dir/sub/inside")"#),
            format!("cannot move a directory into itself: {d}/dir/sub/inside"),
        ),
        (
            format!(r#"fs.copy("{d}/a.txt", "{d}/a.txt/b.txt")"#),
            format!("not a directory: {d}/a.txt/b.txt"),
        ),
        // Any other fault: what it is, in the words of its kind, and
        // the path.
        (
            format!(r#"fs.rename("{d}/dir", "{d}/full")"#),
            format!("directory not empty: {d}/full"),
        ),
    ] {
        let got = err_msg(run(&format!(
            "import fs\nimport host\nfn main() {{\n  host.give({call})\n}}\n"
        )));
        assert_eq!(got, message, "{call}");
    }
    // Nothing moved, nothing written.
    assert!(dir.path().join("dir/sub").is_dir() && !dir.path().join("out.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "a"
    );
}

/// A file last changed before 1970 has that date, not `None`.
#[test]
fn stat_gives_a_time_before_1970() {
    use std::time::{Duration, UNIX_EPOCH};
    let dir = TempDir::new("before_1970");
    let file = dir.path().join("old.txt");
    std::fs::write(&file, "old").unwrap();
    let old = UNIX_EPOCH - Duration::new(315_619_199, 500_000_000);
    let opened = std::fs::File::options().write(true).open(&file).unwrap();
    opened.set_modified(old).unwrap();
    drop(opened);
    let path = file.to_str().unwrap().replace('\\', "/");
    let stat = ok_inner(run(&format!(
        "import fs\nfn main() {{\n  host.give(fs.stat(\"{path}\"))\n}}\nimport host\n"
    )));
    let (_, fields) = record_fields(stat);
    assert_eq!(
        fields["modified"].to_string(),
        "Some(1960-01-01T00:00:00.500000000)"
    );
}

// ── Errors name the path they are about ────────────────────────────

/// Every fs function names the path an error is about, in the words of
/// the fault (`is a directory: d`), not the system's text without the
/// path (`Is a directory (os error 21)`). Unix only: on Windows reading
/// a directory is a refused access, which has a variant of its own.
#[cfg(unix)]
#[test]
fn test_fs_errors_name_the_path_they_are_about() {
    let dir = TempDir::new("errors");
    let sub = dir.path().join("d");
    std::fs::create_dir(&sub).unwrap();
    let file = dir.path().join("f.txt");
    std::fs::write(&file, "x").unwrap();
    let ring = dir.path().join("ring.txt");
    std::os::unix::fs::symlink(&ring, &ring).expect("make symlink");
    let (sub, file, ring) = (
        sub.to_str().unwrap().to_string(),
        file.to_str().unwrap().to_string(),
        ring.to_str().unwrap().to_string(),
    );
    let call = |expr: &str| {
        err_msg(run(&format!(
            "import fs\nimport io\nfn main() {{\n  host.give({expr})\n}}\nimport host\n"
        )))
    };
    assert_eq!(
        call(&format!("io.read_file(\"{sub}\")")),
        format!("is a directory: {sub}")
    );
    assert_eq!(
        call(&format!("io.write_file(\"{sub}\", \"x\")")),
        format!("is a directory: {sub}")
    );
    assert_eq!(
        call(&format!("fs.list_dir(\"{file}\")")),
        format!("not a directory: {file}")
    );
    assert_eq!(
        call(&format!("io.read_file(\"{ring}\")")),
        format!("symlink loop: {ring}")
    );
    assert_eq!(
        call(&format!("fs.read_link(\"{file}\")")),
        format!("invalid input: not a symlink: {file}")
    );
}

/// The file sources and sink of `stream` name the path as `io` does
/// (`is a directory: d`, `file not found: f`), not in the system's text
/// without it, and a line that is no UTF-8 by its number and the path.
/// Unix only, as above.
#[cfg(unix)]
#[test]
fn test_stream_file_errors_name_the_path_they_are_about() {
    let dir = TempDir::new("stream-errors");
    let sub = dir.path().join("d");
    std::fs::create_dir(&sub).unwrap();
    let missing = dir.path().join("missing.txt");
    let bad = dir.path().join("bad.log");
    std::fs::write(&bad, b"ok\nfine\n\xff\xfe\nafter\n").unwrap();
    let (sub, missing, bad) = (
        sub.to_str().unwrap().to_string(),
        missing.to_str().unwrap().to_string(),
        bad.to_str().unwrap().to_string(),
    );
    // The last element of what a source gives.
    let last = |source: &str| {
        err_msg(run(&format!(
            "import list\nimport stream\nimport host\nfn main() {{\n  \
             match stream.collect({source}) |> list.last {{\n    \
             Some(r) -> host.give(r)\n    None -> panic(\"nothing\")\n  }}\n}}\n"
        )))
    };
    assert_eq!(
        last(&format!("stream.file_lines(\"{sub}\")")),
        format!("is a directory: {sub}")
    );
    assert_eq!(
        last(&format!("stream.file_chunks(\"{sub}\", 4)")),
        format!("is a directory: {sub}")
    );
    assert_eq!(
        last(&format!("stream.file_lines(\"{missing}\")")),
        format!("file not found: {missing}")
    );
    assert_eq!(
        last(&format!("stream.file_lines(\"{bad}\")")),
        format!("invalid input: line 3 of {bad} is not UTF-8")
    );
    let write = err_msg(run(&format!(
        "import stream\nimport host\nfn main() {{\n  \
         host.give(stream.write_to_file(stream.from_list([]), \"{sub}\"))\n}}\n"
    )));
    assert_eq!(write, format!("is a directory: {sub}"));
}
