//! Regression tests for GAP #2 (round 59): `ExtFloat` was missing from the
//! primitive type-descriptor registration in `src/typechecker/builtins.rs`
//! and `src/vm/dispatch.rs`. Round 36 added `ExtFloat` to auto-derived trait
//! impls but missed this sibling site, so any program referencing
//! `ExtFloat` as a descriptor (e.g. `json.parse("3.5", ExtFloat)`) failed
//! with `error[type]: undefined variable 'ExtFloat'`.
//!
//! These tests lock in the descriptor registration at the typechecker level
//! and — where the runtime supports it — at the VM dispatch level as well.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

/// Collect type-checker hard errors (Severity::Error) as message strings.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Compile and run a program; returns the main-function return value.
fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime")
}

// ── (1) Typecheck-level availability ───────────────────────────────────

/// Minimum bar: `ExtFloat` must resolve as a type-descriptor value in the
/// typechecker. Before this fix, the program below errored with
/// `undefined variable 'ExtFloat'` because `ExtFloat` was not registered
/// alongside `Int`, `Float`, `String`, `Bool` in `typechecker::builtins`.
#[test]
fn extfloat_descriptor_resolves_in_typechecker() {
    let errs = type_errors(
        r#"
        fn main() {
            let d = ExtFloat
        }
        "#,
    );
    assert!(
        errs.is_empty(),
        "expected ExtFloat to resolve as a type descriptor, got:\n{}",
        errs.join("\n")
    );
}

/// `ExtFloat` should additionally typecheck when passed to `json.parse` as
/// the `type a` argument. The typechecker does not care that the runtime
/// presently rejects primitive descriptors on `json.parse` — it just needs
/// to unify `ExtFloat` against the `type a` descriptor position.
#[test]
fn extfloat_descriptor_flows_through_json_parse_signature() {
    let errs = type_errors(
        r#"
        import json
        fn main() {
            let _ = json.parse("3.5", ExtFloat)
        }
        "#,
    );
    assert!(
        errs.is_empty(),
        "expected json.parse(_, ExtFloat) to typecheck, got:\n{}",
        errs.join("\n")
    );
}

// ── (2) Runtime descriptor registration ───────────────────────────────

/// `ExtFloat` must also be registered in the VM globals as a
/// `Value::PrimitiveDescriptor("ExtFloat")`, so that bare references to
/// the name resolve at runtime (mirroring `Int`, `Float`, `String`,
/// `Bool`). The compiler emits these as global lookups; at runtime we
/// inspect the descriptor's string name to confirm the wiring.
#[test]
fn extfloat_descriptor_exists_at_runtime() {
    // `type_name(ExtFloat)` returns the string "Type" for any descriptor
    // value (see `value::type_name`). More importantly, if the name
    // `ExtFloat` were missing from globals the VM would fail with a
    // runtime error before we could evaluate it. Running the program
    // below without a VmError proves the descriptor is registered.
    let result = run(r#"
        fn main() {
            let _d = ExtFloat
            "ok"
        }
        "#);
    assert_eq!(result, Value::String("ok".into()));
}

// ── (3) End-to-end json.parse_map with ExtFloat value type ────────────

/// `json.parse_map` already accepts `Value::PrimitiveDescriptor` as the
/// value-type argument. This test locks in that `ExtFloat` works as a
/// value-type for map parsing: JSON numbers decode into `Value::ExtFloat`.
#[test]
fn json_parse_map_with_extfloat_value_type() {
    let result = run(r#"
        import json
        fn main() {
            match json.parse_map("\{\"a\": 3.5\}", ExtFloat) {
                Ok(m) -> m
                Err(_) -> #{}
            }
        }
        "#);
    // The returned map must contain a single entry "a" -> ExtFloat(3.5).
    match &result {
        Value::Map(m) => {
            assert_eq!(m.len(), 1, "expected single-entry map, got {result:?}");
            let v = m.get(&Value::String("a".into())).expect("key 'a' missing");
            assert_eq!(v, &Value::ExtFloat(3.5));
        }
        other => panic!("expected Map, got {other:?}"),
    }
}

// ── (4) End-to-end json.parse with ExtFloat descriptor (GAP primary) ──

/// Primary regression for the GAP: `json.parse("3.5", ExtFloat)` must
/// succeed and yield `Ok(ExtFloat(3.5))`. Before the fix, `json.parse`
/// rejected `Value::PrimitiveDescriptor` outright ("type argument must
/// be a record type"), so even with `ExtFloat` registered in the
/// descriptor tables the call would fail at runtime. We also exercise
/// two related paths:
///
/// 1. Overflowing input `1e400` — serde_json rejects the literal with
///    "number out of range", so we expect `Err(JsonSyntax(...))`. The
///    important property is that the *descriptor* is accepted; JSON
///    itself forbids infinity/NaN literals (per RFC 8259), so this is
///    the correct outcome for both `Float` and `ExtFloat`. We pin the
///    behaviour here so future serde_json upgrades don't silently
///    change it.
/// 2. A non-numeric input pinned against the JsonTypeMismatch message.
#[test]
fn json_parse_accepts_extfloat_descriptor() {
    // (a) Happy path: exact value.
    let ok_result = run(r#"
        import json
        fn main() {
            match json.parse("3.5", ExtFloat) {
                Ok(x) -> x
                Err(e) -> "got err: {e.message()}"
            }
        }
        "#);
    assert_eq!(
        ok_result,
        Value::ExtFloat(3.5),
        "expected ExtFloat(3.5), got {ok_result:?}"
    );

    // (b) Overflow path: `1e400` exceeds f64::MAX. serde_json rejects the
    //     literal before we ever see a Number, so the result is a
    //     JsonSyntax error with "number out of range" in the message.
    //     This demonstrates that the ExtFloat descriptor routes through
    //     the shared JSON parser — JSON literals cannot express infinity,
    //     so both `Float` and `ExtFloat` reject `1e400` identically.
    let overflow_msg = run(r#"
        import json
        fn main() {
            match json.parse("1e400", ExtFloat) {
                Ok(_) -> "unexpected ok"
                Err(e) -> e.message()
            }
        }
        "#);
    match &overflow_msg {
        Value::String(s) => assert!(
            s.contains("number out of range"),
            "expected 'number out of range' in overflow error, got: {s}"
        ),
        other => panic!("expected String message, got {other:?}"),
    }

    // (c) Negative path: non-numeric input yields a JsonSyntax variant.
    //     Pin the exact serde_json error substring ("expected ident"),
    //     which is what the underlying parser emits for "not a number".
    //     Exact match only — no OR-chains.
    let bad_result = run(r#"
        import json
        fn main() {
            match json.parse("not a number", ExtFloat) {
                Ok(_) -> "unexpected ok"
                Err(JsonSyntax(msg, _)) -> msg
                Err(e) -> e.message()
            }
        }
        "#);
    match &bad_result {
        Value::String(s) => assert!(
            s.contains("expected ident"),
            "expected 'expected ident' substring, got: {s}"
        ),
        other => panic!("expected String message, got {other:?}"),
    }
}
