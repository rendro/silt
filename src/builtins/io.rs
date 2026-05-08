//! IO and filesystem builtin functions (`io.*`, `fs.*`).

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use super::common::value_kind;
use crate::builtins::data::make_datetime;
use crate::value::Value;
use crate::vm::{Vm, VmError};

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
        return Value::Variant("None".into(), vec![]);
    };
    let Ok(d) = t.duration_since(UNIX_EPOCH) else {
        return Value::Variant("None".into(), vec![]);
    };
    // i64 seconds range ≈ ±292 billion years — the cast is never a
    // truncation in practice but we guard against negative→overflow
    // below. `chrono::DateTime::from_timestamp` returns None if the
    // seconds/nanoseconds compose to a value outside chrono's range.
    let secs = d.as_secs() as i64;
    let nanos = d.subsec_nanos();
    let Some(dt) = chrono::DateTime::from_timestamp(secs, nanos) else {
        return Value::Variant("None".into(), vec![]);
    };
    Value::Variant("Some".into(), vec![make_datetime(dt.naive_utc())])
}

/// Maximum number of entries that may be materialized into a single
/// `fs.walk` / `fs.glob` result list. Mirrors the philosophy of
/// `MAX_RANGE_MATERIALIZE` in `src/value.rs`: keep recursive traversal
/// bounded so a sprawling filesystem (or an accidental symlink cycle that
/// the `glob` crate follows) cannot silently OOM the VM. Hitting the cap
/// surfaces as `Err("fs.walk: exceeded N entries (cap)")` so users can
/// paginate or narrow their root instead of getting a crash.
const MAX_FS_WALK_ENTRIES: usize = 1_000_000;

/// Make an `Ok(inner)` variant value.
fn fs_ok(inner: Value) -> Value {
    Value::Variant("Ok".into(), vec![inner])
}

/// Wrap an `IoError` variant value inside an `Err(...)` outer Result.
fn io_err(inner: Value) -> Value {
    Value::Variant("Err".into(), vec![inner])
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
    let (name, arg): (&str, Option<String>) = match err.kind() {
        ErrorKind::NotFound => ("IoNotFound", Some(path.into())),
        ErrorKind::PermissionDenied => ("IoPermissionDenied", Some(path.into())),
        ErrorKind::AlreadyExists => ("IoAlreadyExists", Some(path.into())),
        ErrorKind::InvalidInput => ("IoInvalidInput", Some(err.to_string())),
        ErrorKind::Interrupted => ("IoInterrupted", None),
        ErrorKind::UnexpectedEof => ("IoUnexpectedEof", None),
        ErrorKind::WriteZero => ("IoWriteZero", None),
        _ => ("IoUnknown", Some(err.to_string())),
    };
    match arg {
        Some(a) => Value::Variant(name.into(), vec![Value::String(a)]),
        None => Value::Variant(name.into(), vec![]),
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
    io_err(Value::Variant(
        "IoUnknown".into(),
        vec![Value::String(msg.into())],
    ))
}

/// Dispatch the builtin `trait Error for IoError` method table. Today
/// only `message` is implemented; additional Error-trait methods would
/// land here too. The receiver (self) is the first argument — `CallMethod`
/// compiles the receiver as arg 0 of the call. Scaffolding lives in
/// `super::dispatch_error_trait`; this site just supplies the
/// variant → message rendering.
pub fn call_io_error_trait(name: &str, args: &[Value]) -> Result<Value, VmError> {
    super::dispatch_error_trait("IoError", name, args, |tag, fields| {
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
    })
}

/// Dispatch `io.<name>(args)`.
pub fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "inspect" => {
            if args.len() != 1 {
                return Err(VmError::new("io.inspect takes 1 argument".into()));
            }
            Ok(Value::String(args[0].format_silt()))
        }
        "read_file" => {
            if args.len() != 1 {
                return Err(VmError::new("io.read_file takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "io.read_file requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            let path = path.clone();
            vm.submit_io_or_run(
                args,
                crate::value::IoCompletion::new(),
                &crate::value::io_unknown_timeout_err,
                move || match std::fs::read_to_string(&path) {
                    Ok(content) => Value::Variant("Ok".into(), vec![Value::String(content)]),
                    Err(e) => io_result_err(&e, &path),
                },
            )
        }
        "write_file" => {
            if args.len() != 2 {
                return Err(VmError::new("io.write_file takes 2 arguments".into()));
            }
            let (Value::String(path), Value::String(content)) = (&args[0], &args[1]) else {
                return Err(VmError::new(format!(
                    "io.write_file requires String, got ({}, {})",
                    value_kind(&args[0]),
                    value_kind(&args[1])
                )));
            };
            let path = path.clone();
            let content = content.clone();
            vm.submit_io_or_run(
                args,
                crate::value::IoCompletion::new(),
                &crate::value::io_unknown_timeout_err,
                move || match std::fs::write(&path, &content) {
                    Ok(()) => Value::Variant("Ok".into(), vec![Value::Unit]),
                    Err(e) => io_result_err(&e, &path),
                },
            )
        }
        "read_line" => vm.submit_io_or_run(
            args,
            crate::value::IoCompletion::new(),
            &crate::value::io_unknown_timeout_err,
            move || {
                let mut line = String::new();
                match std::io::stdin().read_line(&mut line) {
                    // Ok(0) means EOF — surface as Err(IoUnexpectedEof) so
                    // match-against-Err loops terminate cleanly instead of
                    // spinning on "".
                    Ok(0) => io_err(Value::Variant("IoUnexpectedEof".into(), vec![])),
                    Ok(_) => Value::Variant(
                        "Ok".into(),
                        vec![Value::String(line.trim_end().to_string())],
                    ),
                    Err(e) => io_result_err(&e, ""),
                }
            },
        ),
        "args" => {
            // Round-74: return only the program args explicitly forwarded
            // by the CLI past a `--` separator (e.g.
            // `silt run script.silt -- foo bar` → `["foo", "bar"]`).
            // Pre-fix this returned `std::env::args()` directly, leaking
            // the silt binary's own argv (`["silt", "run", "script.silt",
            // ...]`) to user programs, which then had to know to drop the
            // first 3. The `--` separator + dedicated forwarding channel
            // collapses that to one obvious shape.
            let args_list: Vec<Value> = program_args().into_iter().map(Value::String).collect();
            Ok(Value::List(Arc::new(args_list)))
        }
        _ => Err(VmError::new(format!("unknown io function: {name}"))),
    }
}

/// Dispatch `fs.<name>(args)`.
pub fn call_fs(_vm: &Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "exists" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.exists takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.exists requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            Ok(Value::Bool(std::path::Path::new(path).exists()))
        }
        "is_file" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.is_file takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.is_file requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            Ok(Value::Bool(std::path::Path::new(path).is_file()))
        }
        "is_dir" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.is_dir takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.is_dir requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            Ok(Value::Bool(std::path::Path::new(path).is_dir()))
        }
        "list_dir" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.list_dir takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.list_dir requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut items = Vec::new();
                    for entry in entries {
                        match entry {
                            Ok(e) => {
                                items.push(Value::String(
                                    e.file_name().to_string_lossy().into_owned(),
                                ));
                            }
                            Err(e) => {
                                return Ok(io_result_err(&e, path));
                            }
                        }
                    }
                    Ok(Value::Variant(
                        "Ok".into(),
                        vec![Value::List(Arc::new(items))],
                    ))
                }
                Err(e) => Ok(io_result_err(&e, path)),
            }
        }
        "mkdir" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.mkdir takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.mkdir requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            match std::fs::create_dir_all(path) {
                Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(io_result_err(&e, path)),
            }
        }
        "remove" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.remove takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.remove requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            let p = std::path::Path::new(path);
            let result = if p.is_dir() {
                std::fs::remove_dir(p)
            } else {
                std::fs::remove_file(p)
            };
            match result {
                Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(io_result_err(&e, path)),
            }
        }
        "rename" => {
            if args.len() != 2 {
                return Err(VmError::new("fs.rename takes 2 arguments".into()));
            }
            let (Value::String(from), Value::String(to)) = (&args[0], &args[1]) else {
                return Err(VmError::new(format!(
                    "fs.rename requires String, got ({}, {})",
                    value_kind(&args[0]),
                    value_kind(&args[1])
                )));
            };
            match std::fs::rename(from, to) {
                Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(io_result_err(&e, from)),
            }
        }
        "copy" => {
            if args.len() != 2 {
                return Err(VmError::new("fs.copy takes 2 arguments".into()));
            }
            let (Value::String(from), Value::String(to)) = (&args[0], &args[1]) else {
                return Err(VmError::new(format!(
                    "fs.copy requires String, got ({}, {})",
                    value_kind(&args[0]),
                    value_kind(&args[1])
                )));
            };
            match std::fs::copy(from, to) {
                Ok(_) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(io_result_err(&e, from)),
            }
        }
        "stat" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.stat takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.stat requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            // Use symlink_metadata so the returned stat describes the path
            // itself (and `is_symlink` reflects that), rather than the
            // target's metadata. Users who want the target's metadata can
            // call `fs.read_link` then `fs.stat` on the result.
            match std::fs::symlink_metadata(path) {
                Ok(md) => {
                    let ft = md.file_type();
                    let is_symlink = ft.is_symlink();
                    // When the entry is a symlink, symlink_metadata reports
                    // is_file=false / is_dir=false. Surface that directly so
                    // callers can see "this is a symlink, neither file nor
                    // dir" without a follow step.
                    let is_file = md.is_file();
                    let is_dir = md.is_dir();
                    // modified() can fail on platforms that don't track mtime
                    // (rare, but the API requires us to handle it). Fall back
                    // to 0 in that case rather than fail the whole stat call.
                    let modified = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let readonly = md.permissions().readonly();
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
                    let accessed = system_time_to_option_datetime(md.accessed());
                    let created = system_time_to_option_datetime(md.created());
                    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
                    fields.insert("size".into(), Value::Int(md.len() as i64));
                    fields.insert("is_file".into(), Value::Bool(is_file));
                    fields.insert("is_dir".into(), Value::Bool(is_dir));
                    fields.insert("is_symlink".into(), Value::Bool(is_symlink));
                    fields.insert("modified".into(), Value::Int(modified));
                    fields.insert("readonly".into(), Value::Bool(readonly));
                    fields.insert("mode".into(), Value::Int(mode));
                    fields.insert("accessed".into(), accessed);
                    fields.insert("created".into(), created);
                    let rec = Value::Record("FileStat".into(), Arc::new(fields));
                    Ok(fs_ok(rec))
                }
                Err(e) => Ok(io_result_err(&e, path)),
            }
        }
        "is_symlink" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.is_symlink takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.is_symlink requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            // Must use symlink_metadata here: `Path::is_symlink` would do
            // the same thing, but std has made it stable only recently.
            // Using symlink_metadata avoids a version-gate and is explicit.
            let b = std::fs::symlink_metadata(path)
                .map(|md| md.file_type().is_symlink())
                .unwrap_or(false);
            Ok(Value::Bool(b))
        }
        "read_link" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.read_link takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.read_link requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            match std::fs::read_link(path) {
                Ok(target) => Ok(fs_ok(Value::String(target.to_string_lossy().into_owned()))),
                Err(e) => Ok(io_result_err(&e, path)),
            }
        }
        "walk" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.walk takes 1 argument".into()));
            }
            let Value::String(root) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.walk requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            // Default: do NOT follow symlinks. This avoids infinite loops
            // on cyclic trees and matches the principle of least surprise
            // for build tooling (a symlink loop in node_modules should not
            // hang a build).
            let walker = walkdir::WalkDir::new(root).follow_links(false);
            let mut out: Vec<Value> = Vec::new();
            for entry in walker {
                match entry {
                    Ok(e) => {
                        if out.len() >= MAX_FS_WALK_ENTRIES {
                            return Ok(io_result_err_unknown(format!(
                                "fs.walk: exceeded {MAX_FS_WALK_ENTRIES} entries (cap)"
                            )));
                        }
                        // Use absolute path where possible so callers can
                        // pass the result straight into other fs.* calls
                        // without worrying about cwd drift. Fall back to
                        // the raw path if canonicalize fails (e.g. the
                        // entry was already removed between the walk and
                        // this call — a classic TOCTOU race — or it lives
                        // in a directory we don't have read access to).
                        let p = e.path();
                        let s = std::fs::canonicalize(p)
                            .map(|c| c.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| p.to_string_lossy().into_owned());
                        out.push(Value::String(s));
                    }
                    Err(err) => {
                        // walkdir::Error -> reconstruct an io::Error when
                        // possible so variant classification is accurate;
                        // fall back to IoUnknown when walkdir wraps a
                        // non-io cause (cycle detection etc.).
                        if let Some(io_err_ref) = err.io_error() {
                            return Ok(io_result_err(io_err_ref, root));
                        }
                        return Ok(io_result_err_unknown(err.to_string()));
                    }
                }
            }
            Ok(fs_ok(Value::List(Arc::new(out))))
        }
        "glob" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.glob takes 1 argument".into()));
            }
            let Value::String(pattern) = &args[0] else {
                return Err(VmError::new(format!(
                    "fs.glob requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            match glob::glob(pattern) {
                Ok(paths) => {
                    let mut out: Vec<Value> = Vec::new();
                    for entry in paths {
                        if out.len() >= MAX_FS_WALK_ENTRIES {
                            return Ok(io_result_err_unknown(format!(
                                "fs.glob: exceeded {MAX_FS_WALK_ENTRIES} entries (cap)"
                            )));
                        }
                        match entry {
                            Ok(p) => out.push(Value::String(p.to_string_lossy().into_owned())),
                            // glob's per-entry error wraps std::io::Error.
                            Err(e) => return Ok(io_result_err(e.error(), pattern)),
                        }
                    }
                    Ok(fs_ok(Value::List(Arc::new(out))))
                }
                // PatternError (bad glob pattern) is a user-input problem;
                // route to IoInvalidInput so callers can distinguish
                // "your pattern was malformed" from fs failures.
                Err(e) => Ok(io_err(Value::Variant(
                    "IoInvalidInput".into(),
                    vec![Value::String(e.to_string())],
                ))),
            }
        }
        _ => Err(VmError::new(format!("unknown fs function: {name}"))),
    }
}

/// Dispatch `env.<name>(args)`.
pub fn call_env(vm: &Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "get" => {
            if args.len() != 1 {
                return Err(VmError::new("env.get takes 1 argument".into()));
            }
            let Value::String(key) = &args[0] else {
                return Err(VmError::new(format!(
                    "env.get requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            match std::env::var(key) {
                Ok(val) => Ok(Value::Variant("Some".into(), vec![Value::String(val)])),
                Err(_) => Ok(Value::Variant("None".into(), vec![])),
            }
        }
        "set" => {
            if args.len() != 2 {
                return Err(VmError::new("env.set takes 2 arguments".into()));
            }
            if vm.is_scheduled_task {
                return Err(VmError::new(
                    "env.set cannot be called from a spawned task".into(),
                ));
            }
            let (Value::String(key), Value::String(val)) = (&args[0], &args[1]) else {
                return Err(VmError::new(format!(
                    "env.set requires String, got ({}, {})",
                    value_kind(&args[0]),
                    value_kind(&args[1])
                )));
            };
            // SAFETY: Only reachable from the main thread (guarded above).
            unsafe { std::env::set_var(key, val) };
            Ok(Value::Unit)
        }
        "remove" => {
            if args.len() != 1 {
                return Err(VmError::new("env.remove takes 1 argument".into()));
            }
            if vm.is_scheduled_task {
                // Same rationale as env.set: mutating the process-wide
                // environment from a spawned task races with any other
                // task reading the env, and libc's setenv/unsetenv are
                // not synchronized. Keep it to the main thread.
                return Err(VmError::new(
                    "env.remove cannot be called from a spawned task".into(),
                ));
            }
            let Value::String(key) = &args[0] else {
                return Err(VmError::new(format!(
                    "env.remove requires String, got {}",
                    value_kind(&args[0])
                )));
            };
            // Idempotent by contract: std::env::remove_var does not
            // error when the variable was not set, so we don't need to
            // pre-check with env::var. SAFETY: main thread (guarded).
            unsafe { std::env::remove_var(key) };
            Ok(Value::Unit)
        }
        "vars" => {
            if !args.is_empty() {
                return Err(VmError::new("env.vars takes 0 arguments".into()));
            }
            // std::env::vars() snapshots the environment at call time
            // into an iterator. The iteration order is unspecified (on
            // glibc it's roughly insertion order into `environ`; we
            // don't sort, to avoid lying about stability). Each entry
            // becomes a `(String, String)` tuple.
            let pairs: Vec<Value> = std::env::vars()
                .map(|(k, v)| Value::Tuple(vec![Value::String(k), Value::String(v)]))
                .collect();
            Ok(Value::List(Arc::new(pairs)))
        }
        _ => Err(VmError::new(format!("unknown env function: {name}"))),
    }
}
