//! Round 15 / 17 audit lock for `time.to_datetime`: extremal
//! epoch/offset probes must never surface a Rust panic.
//!
//! The load-bearing locks of this file (negative-epoch subsecond
//! handling, offset overflow as a clean error, strftime specifier
//! validation, `time.nanos` / `time.micros`) are golden cases
//! `tests/golden/lang/time/time_builtin_bounds__*`. This probe matrix
//! stays in Rust because each probe may legitimately succeed OR fail;
//! it only asserts that no panic text leaks.

use silt::vm::Vm;
use std::sync::Arc;

#[test]
fn test_time_to_datetime_extremal_probes_never_panic() {
    let probes: &[(&str, i64, i64)] = &[
        (
            "near-max epoch_ns, i32::MAX offset",
            9_000_000_000_000_000_000,
            2_147_483_647,
        ),
        (
            "near-min epoch_ns, i32::MIN offset",
            -9_000_000_000_000_000_000,
            -2_147_483_647,
        ),
        (
            "near-max epoch_ns, i32::MIN offset",
            9_000_000_000_000_000_000,
            -2_147_483_647,
        ),
        (
            "near-min epoch_ns, i32::MAX offset",
            -9_000_000_000_000_000_000,
            2_147_483_647,
        ),
    ];

    for (label, epoch_ns, offset) in probes {
        let src = format!(
            r#"
import time
fn main() -> Int {{
  let inst = Instant {{ epoch_ns: {epoch_ns} }}
  let dt = time.to_datetime(inst, {offset})
  dt.date.year
}}
"#
        );
        // Use the error path (this may succeed or fail; we just care
        // that no panic noise surfaces). Drive the VM manually so we
        // tolerate both success and VmError outcomes.
        let tokens = silt::lexer::Lexer::new(&src)
            .tokenize()
            .expect("lexer error");
        let mut program = silt::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let _ = silt::typechecker::check(&mut program);
        let mut compiler = silt::compiler::Compiler::new();
        let functions = compiler.compile_program(&program).expect("compile error");
        let script = Arc::new(functions.into_iter().next().unwrap());
        let mut vm = Vm::new();
        match vm.run(script) {
            Ok(_) => { /* valid datetime — fine */ }
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    !msg.contains("panicked"),
                    "{label}: to_datetime surfaced a panic string: {msg}"
                );
                assert!(
                    !msg.to_lowercase().contains("overflowed"),
                    "{label}: to_datetime surfaced chrono's 'overflowed' \
                     expect message: {msg}"
                );
            }
        }
    }
}
