//! Round-76 V1 lock: IoPool worker-panic recovery must surface a
//! TYPED `Err` shape (the one the caller's `IoCompletion` factory
//! produces), not the legacy untyped `Err(String)` that bypassed
//! every typed match arm.
//!
//! Pre-fix (src/vm/runtime.rs:344): when the closure submitted to
//! `IoPool::submit_with` panicked, the recovery branch unconditionally
//! built `Value::Variant("Err".into(), vec![Value::String(msg)])` —
//! the legacy untyped shape. Module-specific completions (postgres,
//! tcp, http) typecheck their callers as `Result(T, PgError)` /
//! `Result(T, TcpError)` / `Result(T, IoError)` etc, so user `match`
//! arms over those typed enums never observed the stringly-shaped
//! panic Err — the program would hit the catch-all `_` arm or, worse,
//! fail at runtime when the typechecker saw the value differ from the
//! declared shape on a propagating path.
//!
//! Post-fix: the panic branch routes the message through
//! `IoCompletion::build_timeout_err(prefixed)` (with a `"panic: "`
//! prefix so the message text still preserves the panic vs timeout
//! distinction). This is the same factory the scheduler watchdog
//! uses on deadline-cancel (scheduler.rs:253), so panic and
//! deadline-cancel now produce the SAME typed shape — exactly what
//! the caller's typecheck signature expects.
//!
//! Lock strategy: drive the actual `IoPool::submit_with` worker-panic
//! path via the test-only hook `silt::vm::submit_panicking_io_for_test`
//! (gated on `cfg(any(test, feature = "test-hooks"))`). The hook
//! submits a closure that `panic!`s, blocks on the completion, and
//! returns the resulting Value. We supply an `IoCompletion` whose
//! `timeout_err` factory produces a known typed shape and assert the
//! returned Value matches that factory's output (with the "panic: "
//! prefix in the carried message).
//!
//! This exercises the runtime panic-recovery path (NOT typecheck, NOT
//! scheduler watchdog) — exactly the path V1 reports as broken.
//!
//! Gated on the `test-hooks` Cargo feature so the introspection helper
//! `silt::vm::submit_panicking_io_for_test` is exposed. Run with:
//!
//! ```sh
//! cargo test --features test-hooks --test round76_iopool_panic_typed_err_tests
//! ```

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use silt::value::{IoCompletion, Value};
use silt::vm::{Vm, submit_panicking_io_for_test};

/// Build a typed factory that wraps any message in
/// `Err(SyntheticTypedTimeout(msg))`. Distinct from any production
/// factory so we can be certain the routing went through THIS factory
/// (and therefore through `build_timeout_err` rather than the legacy
/// untyped `Err(String)` path).
fn synthetic_typed_factory(msg: &str) -> Value {
    Value::Variant(
        "Err".into(),
        vec![Value::Variant(
            "SyntheticTypedTimeout".into(),
            vec![Value::String(msg.to_string())],
        )],
    )
}

#[test]
fn iopool_worker_panic_produces_typed_err_via_completion_factory() {
    let vm = Vm::new();
    let completion = IoCompletion::with_timeout_err(Arc::new(synthetic_typed_factory));
    let result = submit_panicking_io_for_test(&vm, completion);

    // Post-fix shape: Err(SyntheticTypedTimeout("panic: <msg>")).
    // Pre-fix shape: Err(String("<msg>")) — fails the outer-arm
    // pattern match below.
    let inner = match &result {
        Value::Variant(tag, args) if tag.as_str() == "Err" && args.len() == 1 => &args[0],
        other => panic!(
            "expected Err(_) variant from IoPool worker-panic recovery, got {:?}",
            other
        ),
    };

    let (typed_tag, typed_args) = match inner {
        Value::Variant(tag, args) => (tag.as_str(), args),
        Value::String(_) => panic!(
            "REGRESSION: IoPool worker-panic recovery produced legacy untyped \
             Err(String) shape — typed callers' match arms will not fire. \
             Got: {:?}",
            inner
        ),
        other => panic!("expected typed inner variant, got {:?}", other),
    };

    assert_eq!(
        typed_tag, "SyntheticTypedTimeout",
        "panic recovery must route through IoCompletion::build_timeout_err \
         (the caller-supplied factory), so the inner tag must be the one \
         this test installed. Got tag: {}",
        typed_tag
    );

    assert_eq!(typed_args.len(), 1, "expected exactly one payload arg");
    let msg = match &typed_args[0] {
        Value::String(s) => s.as_str(),
        other => panic!("expected String payload, got {:?}", other),
    };
    assert!(
        msg.starts_with("panic: "),
        "panic recovery must prefix the carried message with 'panic: ' so \
         readers can still distinguish a panic from a deadline-cancel timeout \
         (both share the typed shape post-fix). Got: {:?}",
        msg
    );
    assert!(
        msg.contains("synthetic IO worker panic for round-76 lock"),
        "carried message must include the original panic payload. Got: {:?}",
        msg
    );
}

/// Belt-and-suspenders: also exercise the default (`IoCompletion::new()`)
/// factory path, which produces `Err(IoUnknown(msg))`. This is the path
/// the io/fs family of builtins actually uses, so a regression here
/// would silently break user code that pattern-matches `Err(IoUnknown)`.
#[test]
fn iopool_worker_panic_with_default_completion_produces_io_unknown() {
    let vm = Vm::new();
    let completion = IoCompletion::new();
    let result = submit_panicking_io_for_test(&vm, completion);

    let inner = match &result {
        Value::Variant(tag, args) if tag.as_str() == "Err" && args.len() == 1 => &args[0],
        other => panic!("expected Err(_), got {:?}", other),
    };

    match inner {
        Value::Variant(tag, args) if tag.as_str() == "IoUnknown" && args.len() == 1 => {
            match &args[0] {
                Value::String(s) => {
                    assert!(
                        s.starts_with("panic: "),
                        "expected 'panic: ' prefix; got {:?}",
                        s
                    );
                }
                other => panic!("expected String inside IoUnknown, got {:?}", other),
            }
        }
        Value::String(_) => panic!(
            "REGRESSION: default IoCompletion panic recovery produced legacy \
             untyped Err(String) shape. Got: {:?}",
            inner
        ),
        other => panic!("expected IoUnknown variant, got {:?}", other),
    }
}
