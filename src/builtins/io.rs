//! IO, filesystem and environment builtin functions (`io.*`, `fs.*`,
//! `env.*`).

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use super::typed::builtins;
use crate::builtins::time::make_datetime;
use crate::typeinfo::{BuiltinVariant, bv, ty};
use crate::value::Value;
use crate::vm::{Step, VmError};

/// Program arguments forwarded by the CLI for `io.args()`.
///
/// The CLI subcommands (`silt run`, `silt check`, `silt disasm`, plus the
/// bare-file shim) recognize `--` as a separator; everything after `--` is
/// forwarded here verbatim and surfaced to silt programs via `io.args()`.
///
/// Round-74 audit fix: prior to this round `io.args()` returned the raw
/// `std::env::args()` of the silt binary itself (`["silt", "run",
/// "file.silt", ...]`), forcing programs to know they had to drop the
/// first 3 positions to recover their own user-supplied args. That coupling
/// to the binary's argv layout was both fragile and undocumented; it also
/// broke entirely when round-72 started rejecting bare extras after the
/// script file (the only previous way to pass user args). The `--` form
/// makes the contract explicit and preserves the forwarding path.
fn program_args_lock() -> &'static RwLock<Vec<String>> {
    static ARGS: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    ARGS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Set the program arguments returned by `io.args()`. Called by the CLI
/// after parsing positionals out of `argv` and locating the `--` separator.
pub fn set_program_args(args: Vec<String>) {
    if let Ok(mut guard) = program_args_lock().write() {
        *guard = args;
    }
}

/// Read a snapshot of the currently-set program arguments.
pub fn program_args() -> Vec<String> {
    program_args_lock()
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Convert a `SystemTime` into a Silt `Option(DateTime)`. A missing /
/// unsupported timestamp (the OS returned `Err`, or the value predates
/// UNIX_EPOCH by more than chrono can represent) collapses to `None`
/// rather than failing the whole `fs.stat` call — some filesystems do
/// not expose creation time (`btime`) at all, and ext4 inodes created
/// before Linux 4.11 lack it even where the kernel supports it.
fn system_time_to_option_datetime(t: Result<SystemTime, std::io::Error>) -> Value {
    let Ok(t) = t else {
        return Value::variant(bv::NONE, vec![]);
    };
    let Ok(d) = t.duration_since(UNIX_EPOCH) else {
        return Value::variant(bv::NONE, vec![]);
    };
    // i64 seconds range ≈ ±292 billion years — the cast is never a
    // truncation in practice but we guard against negative→overflow
    // below. `chrono::DateTime::from_timestamp` returns None if the
    // seconds/nanoseconds compose to a value outside chrono's range.
    let secs = d.as_secs() as i64;
    let nanos = d.subsec_nanos();
    let Some(dt) = chrono::DateTime::from_timestamp(secs, nanos) else {
        return Value::variant(bv::NONE, vec![]);
    };
    Value::variant(bv::SOME, vec![make_datetime(dt.naive_utc())])
}

/// Maximum number of entries that may be materialized into a single
/// `fs.walk` / `fs.glob` result list. Mirrors the philosophy of
/// `MAX_RANGE_MATERIALIZE` in `src/value/mod.rs`: keep recursive traversal
/// bounded so a sprawling filesystem (or an accidental symlink cycle that
/// the `glob` crate follows) cannot silently OOM the VM. Hitting the cap
/// surfaces as `Err("fs.walk: exceeded N entries (cap)")` so users can
/// paginate or narrow their root instead of getting a crash.
const MAX_FS_WALK_ENTRIES: usize = 1_000_000;

/// Make an `Ok(inner)` variant value.
fn fs_ok(inner: Value) -> Value {
    Value::variant(bv::OK, vec![inner])
}

/// What an operation on `path` gave, as a `Result(a, IoError)`.
fn result<T>(done: std::io::Result<T>, path: &str, ok: impl FnOnce(T) -> Value) -> Value {
    match done {
        Ok(value) => fs_ok(ok(value)),
        Err(e) => io_result_err(&e, path),
    }
}

fn unit<T>(_: T) -> Value {
    Value::Unit
}

/// Wrap an `IoError` variant value inside an `Err(...)` outer Result.
fn io_err(inner: Value) -> Value {
    Value::variant(bv::ERR, vec![inner])
}

/// Classify a `std::io::Error` into one of the `IoError` enum variants.
/// Shared by every io/fs builtin that surfaces a filesystem / stdio
/// failure to silt code. `path` is used for variants that carry a path
/// (NotFound / PermissionDenied / AlreadyExists); pass "" when the
/// builtin has no path context (e.g. `io.read_line`).
///
/// See `module.rs::builtin_error_enum_variants_with_arity` for
/// Phase 0 background; this function lives at the Phase 1 boundary
/// (mapping native I/O failures into the typed `IoError` enum).
pub(crate) fn io_error_to_variant(err: &std::io::Error, path: &str) -> Value {
    use std::io::ErrorKind;
    let (variant, arg): (BuiltinVariant, Option<String>) = match err.kind() {
        ErrorKind::NotFound => (bv::IO_NOT_FOUND, Some(path.into())),
        ErrorKind::PermissionDenied => (bv::IO_PERMISSION_DENIED, Some(path.into())),
        ErrorKind::AlreadyExists => (bv::IO_ALREADY_EXISTS, Some(path.into())),
        ErrorKind::InvalidInput => (bv::IO_INVALID_INPUT, Some(err.to_string())),
        ErrorKind::Interrupted => (bv::IO_INTERRUPTED, None),
        ErrorKind::UnexpectedEof => (bv::IO_UNEXPECTED_EOF, None),
        ErrorKind::WriteZero => (bv::IO_WRITE_ZERO, None),
        _ => (bv::IO_UNKNOWN, Some(err.to_string())),
    };
    match arg {
        Some(a) => Value::variant(variant, vec![Value::String(a)]),
        None => Value::variant(variant, vec![]),
    }
}

/// Build a full `Err(IoError)` from a `std::io::Error`. Convenience
/// wrapper around `io_error_to_variant` for sites that always wrap in
/// `Err(...)` (i.e. every io/fs failure path).
pub(crate) fn io_result_err(err: &std::io::Error, path: &str) -> Value {
    io_err(io_error_to_variant(err, path))
}

/// Build an `Err(IoError)` with a synthetic `IoUnknown(msg)` variant
/// for cases where no underlying `std::io::Error` exists (e.g. the
/// `fs.walk` entry-cap cutoff). Keeps the result type
/// `Result(T, IoError)` uniform.
pub(crate) fn io_result_err_unknown<S: Into<String>>(msg: S) -> Value {
    io_err(Value::variant(
        bv::IO_UNKNOWN,
        vec![Value::String(msg.into())],
    ))
}

/// What `IoError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("IoNotFound", [Value::String(p)]) => format!("file not found: {p}"),
        ("IoPermissionDenied", [Value::String(p)]) => {
            format!("permission denied: {p}")
        }
        ("IoAlreadyExists", [Value::String(p)]) => format!("already exists: {p}"),
        ("IoInvalidInput", [Value::String(m)]) => format!("invalid input: {m}"),
        ("IoInterrupted", []) => "operation interrupted".to_string(),
        ("IoUnexpectedEof", []) => "unexpected end of file".to_string(),
        ("IoWriteZero", []) => "zero-byte write".to_string(),
        ("IoUnknown", [Value::String(m)]) => m.clone(),
        _ => return None,
    })
}

/// The error of an `io` function whose operation has no value of its
/// own: its wait timed out, or the operation could not run. Either
/// way `IoUnknown` with the reason.
fn io_unknown_err(failure: crate::vm::IoFailure<'_>) -> Value {
    io_err(Value::variant(
        bv::IO_UNKNOWN,
        vec![Value::String(failure.text().to_string())],
    ))
}

builtins! {
    fn inspect(x: &Value) -> String {
        x.format_silt()
    }

    fn read_file(vm, path: &str) -> Result<Step, VmError> {
        let path = path.to_string();
        vm.io("io.read_file", io_unknown_err, move || {
            result(std::fs::read_to_string(&path), &path, Value::String)
        })
    }

    fn write_file(vm, path: &str, contents: &str) -> Result<Step, VmError> {
        let (path, contents) = (path.to_string(), contents.to_string());
        vm.io("io.write_file", io_unknown_err, move || {
            result(std::fs::write(&path, &contents), &path, unit)
        })
    }

    fn read_line(vm) -> Result<Step, VmError> {
        vm.io("io.read_line", io_unknown_err, move || {
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                // Ok(0) means EOF — surface as Err(IoUnexpectedEof) so
                // match-against-Err loops terminate cleanly instead of
                // spinning on "".
                Ok(0) => io_err(Value::variant(bv::IO_UNEXPECTED_EOF, vec![])),
                Ok(_) => fs_ok(Value::String(line.trim_end().to_string())),
                Err(e) => io_result_err(&e, ""),
            }
        })
    }

    // Only the program args explicitly forwarded by the CLI past a `--`
    // separator (`silt run script.silt -- foo bar` → `["foo", "bar"]`).
    fn args() -> Vec<Value> {
        program_args().into_iter().map(Value::String).collect()
    }
}

/// `fs.*`
pub(crate) mod fs {
    use std::path::Path;

    use super::*;

    builtins! {
        fn exists(path: &str) -> bool {
            Path::new(path).exists()
        }

        fn is_file(path: &str) -> bool {
            Path::new(path).is_file()
        }

        fn is_dir(path: &str) -> bool {
            Path::new(path).is_dir()
        }

        fn list_dir(path: &str) -> Value {
            let names = std::fs::read_dir(path).and_then(|entries| {
                let names = entries.map(|entry| {
                    let name = entry?.file_name();
                    Ok(Value::String(name.to_string_lossy().into_owned()))
                });
                names.collect::<std::io::Result<Vec<Value>>>()
            });
            result(names, path, |names| Value::List(Arc::new(names)))
        }

        fn mkdir(path: &str) -> Value {
            result(std::fs::create_dir_all(path), path, unit)
        }

        fn remove(path: &str) -> Value {
            let at = Path::new(path);
            let removed = match at.is_dir() {
                true => std::fs::remove_dir(at),
                false => std::fs::remove_file(at),
            };
            result(removed, path, unit)
        }

        fn rename(from: &str, to: &str) -> Value {
            result(std::fs::rename(from, to), from, unit)
        }

        fn copy(from: &str, to: &str) -> Value {
            result(std::fs::copy(from, to), from, unit)
        }

        fn stat(path: &str) -> Value {
            // Use symlink_metadata so the returned stat describes the path
            // itself (and `is_symlink` reflects that), rather than the
            // target's metadata. Users who want the target's metadata can
            // call `fs.read_link` then `fs.stat` on the result.
            result(std::fs::symlink_metadata(path), path, |md| {
                // When the entry is a symlink, symlink_metadata reports
                // is_file=false / is_dir=false. Surface that directly so
                // callers can see "this is a symlink, neither file nor
                // dir" without a follow step.
                //
                // modified() can fail on platforms that don't track mtime
                // (rare, but the API requires us to handle it). Fall back
                // to 0 in that case rather than fail the whole stat call.
                let modified = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                // Unix permission bits (e.g. 0o755). On Windows no
                // equivalent exists — std exposes `FILE_ATTRIBUTE_*`
                // bits via `MetadataExt::file_attributes()` but those
                // aren't permission bits, so we report 0 to signal
                // "not applicable". User code that actually needs
                // Unix perms should only read `mode` under `cfg(unix)`.
                #[cfg(unix)]
                let mode: i64 = {
                    use std::os::unix::fs::MetadataExt;
                    md.mode() as i64
                };
                #[cfg(not(unix))]
                let mode: i64 = 0;
                // accessed() may fail on filesystems mounted with
                // `noatime`, and created() (`btime`) is notoriously
                // flaky: it's absent on older ext4, only surfaced via
                // statx(2) on Linux, and not exposed at all on some
                // Unixes. Both map to Option(DateTime) so callers can
                // pattern-match rather than probe for sentinels.
                let fields = [
                    ("size", Value::Int(md.len() as i64)),
                    ("is_file", Value::Bool(md.is_file())),
                    ("is_dir", Value::Bool(md.is_dir())),
                    ("is_symlink", Value::Bool(md.file_type().is_symlink())),
                    ("modified", Value::Int(modified)),
                    ("readonly", Value::Bool(md.permissions().readonly())),
                    ("mode", Value::Int(mode)),
                    ("accessed", system_time_to_option_datetime(md.accessed())),
                    ("created", system_time_to_option_datetime(md.created())),
                ];
                let fields: BTreeMap<String, Value> =
                    fields.into_iter().map(|(name, value)| (name.into(), value)).collect();
                Value::builtin_record(ty::FILE_STAT, fields)
            })
        }

        // (The link itself is asked, not what it leads to.)
        fn is_symlink(path: &str) -> bool {
            std::fs::symlink_metadata(path).is_ok_and(|md| md.file_type().is_symlink())
        }

        fn read_link(path: &str) -> Value {
            result(std::fs::read_link(path), path, |target| {
                Value::String(target.to_string_lossy().into_owned())
            })
        }

        fn walk(root: &str) -> Value {
            // Default: do NOT follow symlinks. This avoids infinite loops
            // on cyclic trees and matches the principle of least surprise
            // for build tooling (a symlink loop in node_modules should not
            // hang a build).
            let walker = walkdir::WalkDir::new(root).follow_links(false);
            let mut out: Vec<Value> = Vec::new();
            for entry in walker {
                let entry = match entry {
                    Ok(entry) => entry,
                    // walkdir::Error -> reconstruct an io::Error when
                    // possible so variant classification is accurate;
                    // fall back to IoUnknown when walkdir wraps a
                    // non-io cause (cycle detection etc.).
                    Err(err) => {
                        return match err.io_error() {
                            Some(io_error) => io_result_err(io_error, root),
                            None => io_result_err_unknown(err.to_string()),
                        };
                    }
                };
                if out.len() >= MAX_FS_WALK_ENTRIES {
                    return io_result_err_unknown(format!(
                        "fs.walk: exceeded {MAX_FS_WALK_ENTRIES} entries (cap)"
                    ));
                }
                // Use absolute path where possible so callers can
                // pass the result straight into other fs.* calls
                // without worrying about cwd drift. Fall back to
                // the raw path if canonicalize fails (e.g. the
                // entry was already removed between the walk and
                // this call — a classic TOCTOU race — or it lives
                // in a directory we don't have read access to).
                let path = entry.path();
                let absolute = std::fs::canonicalize(path);
                let shown = absolute.as_deref().unwrap_or(path).to_string_lossy();
                out.push(Value::String(shown.into_owned()));
            }
            fs_ok(Value::List(Arc::new(out)))
        }

        fn glob(pattern: &str) -> Value {
            let paths = match glob::glob(pattern) {
                Ok(paths) => paths,
                // PatternError (bad glob pattern) is a user-input problem;
                // route to IoInvalidInput so callers can distinguish
                // "your pattern was malformed" from fs failures.
                Err(e) => {
                    return io_err(Value::variant(
                        bv::IO_INVALID_INPUT,
                        vec![Value::String(e.to_string())],
                    ));
                }
            };
            let mut out: Vec<Value> = Vec::new();
            for entry in paths {
                if out.len() >= MAX_FS_WALK_ENTRIES {
                    return io_result_err_unknown(format!(
                        "fs.glob: exceeded {MAX_FS_WALK_ENTRIES} entries (cap)"
                    ));
                }
                match entry {
                    Ok(path) => out.push(Value::String(path.to_string_lossy().into_owned())),
                    // glob's per-entry error wraps std::io::Error.
                    Err(e) => return io_result_err(e.error(), pattern),
                }
            }
            fs_ok(Value::List(Arc::new(out)))
        }
    }
}

/// `env.*`
pub(crate) mod env {
    use super::*;

    /// Refuses a change of the environment from a task: it is the
    /// process's, a task that reads it would race with the change, and
    /// libc's setenv/unsetenv are not synchronized. Only the program's
    /// own thread changes it.
    fn own_thread_only(vm: &crate::vm::Vm, name: &str) -> Result<(), VmError> {
        match vm.spawned {
            true => Err(VmError::new(format!(
                "{name} cannot be called from a spawned task"
            ))),
            false => Ok(()),
        }
    }

    builtins! {
        fn get(key: &str) -> Option<Value> {
            std::env::var(key).ok().map(Value::String)
        }

        fn set(vm, key: &str, value: &str) -> Result<(), VmError> {
            own_thread_only(vm, "env.set")?;
            // SAFETY: Only reachable from the main thread (guarded above).
            unsafe { std::env::set_var(key, value) };
            Ok(())
        }

        fn remove(vm, name: &str) -> Result<(), VmError> {
            own_thread_only(vm, "env.remove")?;
            // Idempotent by contract: std::env::remove_var does not
            // error when the variable was not set, so we don't need to
            // pre-check with env::var. SAFETY: main thread (guarded).
            unsafe { std::env::remove_var(name) };
            Ok(())
        }

        // std::env::vars() snapshots the environment at call time
        // into an iterator. The iteration order is unspecified (on
        // glibc it's roughly insertion order into `environ`; we
        // don't sort, to avoid lying about stability). Each entry
        // becomes a `(String, String)` tuple.
        fn vars() -> Vec<Value> {
            std::env::vars()
                .map(|(k, v)| Value::Tuple(vec![Value::String(k), Value::String(v)]))
                .collect()
        }
    }
}
