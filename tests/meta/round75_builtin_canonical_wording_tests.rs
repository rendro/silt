//! Round 75 — io / data / postgres builtins adopt the canonical
//! `"<fn> requires <Kind>, got <kind>"` wording.
//!
//! Round 75 finished the sweep across `io.rs`, the data builtins, and
//! `postgres.rs` — ~50 sites that previously read e.g. `"requires a
//! string path"`, `"requires Int days"`, `"expected a Date record"`.
//! Each terse form lacked the offending `got <kind>` suffix that lets
//! users diagnose without checking docs.
//!
//! The typechecker rejects these calls in user programs, so the runtime
//! defence is driven directly through the builtin dispatchers with a
//! wrong-kind argument, for a sample of the migrated sites.

use silt::value::Value;
use silt::vm::Vm;

fn err_msg(result: Result<Value, silt::vm::VmError>) -> String {
    match result {
        Ok(v) => panic!("expected an error, got Ok({v:?})"),
        Err(e) => e.message,
    }
}

#[test]
fn io_read_file_non_string_path_says_canonical_form() {
    let mut vm = Vm::new(silt::HostIo::process());
    let msg = err_msg(silt::builtins::io::call(
        &mut vm,
        "read_file",
        &[Value::Int(42)],
    ));
    assert_eq!(msg, "io.read_file requires String, got Int");
}

#[test]
fn fs_exists_non_string_path_says_canonical_form() {
    let vm = Vm::new(silt::HostIo::process());
    let msg = err_msg(silt::builtins::io::call_fs(
        &vm,
        "exists",
        &[Value::Int(42)],
    ));
    assert_eq!(msg, "fs.exists requires String, got Int");
}

#[test]
fn extract_date_non_record_says_canonical_form() {
    // `time.add_days` reads its Date argument through `extract_date`.
    let mut vm = Vm::new(silt::HostIo::process());
    let msg = err_msg(silt::builtins::time::call_time(
        &mut vm,
        "add_days",
        &[Value::Int(42), Value::Int(1)],
    ));
    assert_eq!(msg, "extract_date requires Date, got Int");
}
