//! Round-36 parity locks for `builtins::data::call_regex`.
//!
//! Eight `call_regex` match arms (six 2-arg: `is_match`, `find`,
//! `find_all`, `split`, `captures`, `captures_all`, `captures_named`;
//! and two 3-arg: `replace`, `replace_all`) shared a nearly-identical
//! arity-check + typed-destructure prelude. Round-36 extracted that
//! prelude into two module-local helpers:
//!
//!   - `parse_regex_string_pair(op_name, args) -> (&str, &str)`
//!   - `parse_regex_string_triple(op_name, args) -> (&str, &str, &str)`
//!
//! These tests lock the refactor. Each asserts — for one of the eight
//! regex ops — that:
//!
//!   1. A valid call returns the expected `Value`.
//!   2. A wrong-arity call returns the *exact* pre-refactor error
//!      string (e.g. "regex.is_match takes 2 arguments (pattern, text)").
//!   3. A wrong-type call returns the *exact* pre-refactor error
//!      string (e.g. "regex.is_match requires string arguments").
//!
//! The error phrasing is the contract the helper must preserve. A
//! future refactor that silently shifts the wording — or that
//! forgets to emit the arity error, or that misreads `args[0]` as the
//! text rather than the pattern — will fail these tests.
//!
//! The `replace_all_with` arm has a distinct structural shape (it
//! takes a callback and uses `iterate_builtin`), so it was *not*
//! migrated to the helper and is *not* covered here — that's the
//! intended scope boundary.

use silt::builtins::data::call_regex;
use silt::value::Value;
use silt::vm::Vm;

// ── tiny harness ─────────────────────────────────────────────────────

fn ok(name: &str, args: Vec<Value>) -> Value {
    let mut vm = Vm::new();
    call_regex(&mut vm, name, &args).unwrap_or_else(|e| {
        panic!("regex.{name} unexpected error: {}", e.message);
    })
}

fn err_msg(name: &str, args: Vec<Value>) -> String {
    let mut vm = Vm::new();
    match call_regex(&mut vm, name, &args) {
        Ok(v) => panic!("regex.{name} expected error, got Ok({v:?})"),
        Err(e) => e.message,
    }
}

fn s(lit: &str) -> Value {
    Value::String(lit.to_string())
}

// ── 2-arg ops ────────────────────────────────────────────────────────

#[test]
fn is_match_valid_and_parity_errors() {
    // Valid call.
    assert_eq!(
        ok("is_match", vec![s(r"\d+"), s("abc 123 def")]),
        Value::Bool(true)
    );
    assert_eq!(
        ok("is_match", vec![s(r"\d+"), s("no digits here")]),
        Value::Bool(false)
    );

    // Wrong arity — pre-refactor string, verbatim.
    assert_eq!(
        err_msg("is_match", vec![s(r"\d+")]),
        "regex.is_match takes 2 arguments (pattern, text)"
    );
    assert_eq!(
        err_msg("is_match", vec![s("a"), s("b"), s("c"), s("d")]),
        "regex.is_match takes 2 arguments (pattern, text)"
    );

    // Wrong type.
    assert_eq!(
        err_msg("is_match", vec![Value::Int(1), s("text")]),
        "regex.is_match requires string arguments"
    );
    assert_eq!(
        err_msg("is_match", vec![s("pat"), Value::Int(1)]),
        "regex.is_match requires string arguments"
    );
}

#[test]
fn find_valid_and_parity_errors() {
    // Valid call — match.
    match ok("find", vec![s(r"\d+"), s("abc 123 def")]) {
        Value::Variant(tag, payload) => {
            assert_eq!(tag, "Some");
            assert_eq!(payload.len(), 1);
            match &payload[0] {
                Value::String(m) => assert_eq!(m, "123"),
                other => panic!("expected String, got {other:?}"),
            }
        }
        other => panic!("expected Variant, got {other:?}"),
    }
    // Valid call — no match.
    match ok("find", vec![s(r"\d+"), s("no digits here")]) {
        Value::Variant(tag, payload) => {
            assert_eq!(tag, "None");
            assert!(payload.is_empty());
        }
        other => panic!("expected Variant, got {other:?}"),
    }

    // Wrong arity.
    assert_eq!(
        err_msg("find", vec![s(r"\d+")]),
        "regex.find takes 2 arguments (pattern, text)"
    );
    assert_eq!(
        err_msg("find", vec![s("a"), s("b"), s("c"), s("d")]),
        "regex.find takes 2 arguments (pattern, text)"
    );

    // Wrong type.
    assert_eq!(
        err_msg("find", vec![Value::Bool(true), s("text")]),
        "regex.find requires string arguments"
    );
}

#[test]
fn find_all_valid_and_parity_errors() {
    // Valid call.
    match ok("find_all", vec![s(r"\d+"), s("a1 b22 c333")]) {
        Value::List(items) => {
            assert_eq!(items.len(), 3);
            for (got, want) in items.iter().zip(["1", "22", "333"].iter()) {
                match got {
                    Value::String(m) => assert_eq!(m, want),
                    other => panic!("expected String, got {other:?}"),
                }
            }
        }
        other => panic!("expected List, got {other:?}"),
    }

    // Wrong arity.
    assert_eq!(
        err_msg("find_all", vec![s(r"\d+")]),
        "regex.find_all takes 2 arguments (pattern, text)"
    );
    assert_eq!(
        err_msg("find_all", vec![s("a"), s("b"), s("c"), s("d")]),
        "regex.find_all takes 2 arguments (pattern, text)"
    );

    // Wrong type.
    assert_eq!(
        err_msg("find_all", vec![s("pat"), Value::Float(1.0)]),
        "regex.find_all requires string arguments"
    );
}

#[test]
fn split_valid_and_parity_errors() {
    // Valid call.
    match ok("split", vec![s(r"\s+"), s("a b   c")]) {
        Value::List(items) => {
            assert_eq!(items.len(), 3);
            let got: Vec<&str> = items
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.as_str(),
                    _ => panic!("non-string in split list"),
                })
                .collect();
            assert_eq!(got, vec!["a", "b", "c"]);
        }
        other => panic!("expected List, got {other:?}"),
    }

    // Wrong arity.
    assert_eq!(
        err_msg("split", vec![s(r"\s+")]),
        "regex.split takes 2 arguments (pattern, text)"
    );
    assert_eq!(
        err_msg("split", vec![s("a"), s("b"), s("c"), s("d")]),
        "regex.split takes 2 arguments (pattern, text)"
    );

    // Wrong type.
    assert_eq!(
        err_msg("split", vec![Value::Int(0), s("text")]),
        "regex.split requires string arguments"
    );
}

#[test]
fn captures_valid_and_parity_errors() {
    // Valid call — match.
    match ok("captures", vec![s(r"(\d+)-(\w+)"), s("42-foo tail")]) {
        Value::Variant(tag, payload) => {
            assert_eq!(tag, "Some");
            match &payload[0] {
                Value::List(groups) => {
                    // 3 groups: whole match, (\d+), (\w+).
                    assert_eq!(groups.len(), 3);
                }
                other => panic!("expected List payload, got {other:?}"),
            }
        }
        other => panic!("expected Variant, got {other:?}"),
    }

    // Wrong arity.
    assert_eq!(
        err_msg("captures", vec![s(r"\d+")]),
        "regex.captures takes 2 arguments (pattern, text)"
    );
    assert_eq!(
        err_msg("captures", vec![s("a"), s("b"), s("c"), s("d")]),
        "regex.captures takes 2 arguments (pattern, text)"
    );

    // Wrong type.
    assert_eq!(
        err_msg("captures", vec![s("pat"), Value::Bool(false)]),
        "regex.captures requires string arguments"
    );
}

#[test]
fn captures_all_valid_and_parity_errors() {
    // Valid call.
    match ok("captures_all", vec![s(r"(\d+)"), s("1 and 22 and 333")]) {
        Value::List(items) => {
            assert_eq!(items.len(), 3);
        }
        other => panic!("expected List, got {other:?}"),
    }

    // Wrong arity.
    assert_eq!(
        err_msg("captures_all", vec![s(r"\d+")]),
        "regex.captures_all takes 2 arguments (pattern, text)"
    );
    assert_eq!(
        err_msg("captures_all", vec![s("a"), s("b"), s("c"), s("d")]),
        "regex.captures_all takes 2 arguments (pattern, text)"
    );

    // Wrong type.
    assert_eq!(
        err_msg("captures_all", vec![Value::Int(7), s("text")]),
        "regex.captures_all requires string arguments"
    );
}

#[test]
fn captures_named_valid_and_parity_errors() {
    // Valid call — named capture matches.
    match ok(
        "captures_named",
        vec![s(r"(?P<year>\d{4})-(?P<month>\d{2})"), s("2026-04 tail")],
    ) {
        Value::Variant(tag, payload) => {
            assert_eq!(tag, "Some");
            match &payload[0] {
                Value::Map(map) => {
                    assert_eq!(map.len(), 2);
                }
                other => panic!("expected Map payload, got {other:?}"),
            }
        }
        other => panic!("expected Variant, got {other:?}"),
    }

    // Valid call — no named groups → None.
    match ok("captures_named", vec![s(r"\d+"), s("42")]) {
        Value::Variant(tag, payload) => {
            assert_eq!(tag, "None");
            assert!(payload.is_empty());
        }
        other => panic!("expected Variant, got {other:?}"),
    }

    // Wrong arity.
    assert_eq!(
        err_msg("captures_named", vec![s(r"\d+")]),
        "regex.captures_named takes 2 arguments (pattern, text)"
    );
    assert_eq!(
        err_msg("captures_named", vec![s("a"), s("b"), s("c"), s("d")]),
        "regex.captures_named takes 2 arguments (pattern, text)"
    );

    // Wrong type.
    assert_eq!(
        err_msg("captures_named", vec![s("pat"), Value::Int(42)]),
        "regex.captures_named requires string arguments"
    );
}

// ── 3-arg ops ────────────────────────────────────────────────────────

#[test]
fn replace_valid_and_parity_errors() {
    // Valid call — replaces only the first match.
    match ok("replace", vec![s(r"\d+"), s("a1 b2 c3"), s("X")]) {
        Value::String(out) => assert_eq!(out, "aX b2 c3"),
        other => panic!("expected String, got {other:?}"),
    }

    // Wrong arity (arity-3 error message, NOT the arity-2 message!).
    assert_eq!(
        err_msg("replace", vec![s(r"\d+")]),
        "regex.replace takes 3 arguments (pattern, text, replacement)"
    );
    assert_eq!(
        err_msg(
            "replace",
            vec![s("a"), s("b"), s("c"), s("d")]
        ),
        "regex.replace takes 3 arguments (pattern, text, replacement)"
    );

    // Wrong type (on any of the three positions).
    assert_eq!(
        err_msg(
            "replace",
            vec![Value::Int(0), s("text"), s("repl")]
        ),
        "regex.replace requires string arguments"
    );
    assert_eq!(
        err_msg(
            "replace",
            vec![s("pat"), Value::Int(0), s("repl")]
        ),
        "regex.replace requires string arguments"
    );
    assert_eq!(
        err_msg(
            "replace",
            vec![s("pat"), s("text"), Value::Int(0)]
        ),
        "regex.replace requires string arguments"
    );
}

#[test]
fn replace_all_valid_and_parity_errors() {
    // Valid call — replaces every match.
    match ok("replace_all", vec![s(r"\d+"), s("a1 b2 c3"), s("X")]) {
        Value::String(out) => assert_eq!(out, "aX bX cX"),
        other => panic!("expected String, got {other:?}"),
    }

    // Wrong arity.
    assert_eq!(
        err_msg("replace_all", vec![s(r"\d+")]),
        "regex.replace_all takes 3 arguments (pattern, text, replacement)"
    );
    assert_eq!(
        err_msg(
            "replace_all",
            vec![s("a"), s("b"), s("c"), s("d")]
        ),
        "regex.replace_all takes 3 arguments (pattern, text, replacement)"
    );

    // Wrong type (on any of the three positions).
    assert_eq!(
        err_msg(
            "replace_all",
            vec![Value::Bool(true), s("text"), s("repl")]
        ),
        "regex.replace_all requires string arguments"
    );
    assert_eq!(
        err_msg(
            "replace_all",
            vec![s("pat"), Value::Bool(true), s("repl")]
        ),
        "regex.replace_all requires string arguments"
    );
    assert_eq!(
        err_msg(
            "replace_all",
            vec![s("pat"), s("text"), Value::Bool(true)]
        ),
        "regex.replace_all requires string arguments"
    );
}

// ── arity vs type precedence ─────────────────────────────────────────
//
// The helper checks arity *before* types. If a future refactor swaps
// that order, callers who pass both wrong arity AND wrong types would
// see a different error — we lock the current order.

#[test]
fn arity_check_runs_before_type_check_pair() {
    // 1 arg and wrong type → arity error wins.
    assert_eq!(
        err_msg("is_match", vec![Value::Int(1)]),
        "regex.is_match takes 2 arguments (pattern, text)"
    );
    // 4 args and wrong types → arity error wins.
    assert_eq!(
        err_msg(
            "find",
            vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)]
        ),
        "regex.find takes 2 arguments (pattern, text)"
    );
}

#[test]
fn arity_check_runs_before_type_check_triple() {
    // 1 arg and wrong type → arity error wins.
    assert_eq!(
        err_msg("replace", vec![Value::Int(1)]),
        "regex.replace takes 3 arguments (pattern, text, replacement)"
    );
    // 4 non-string args → arity error wins.
    assert_eq!(
        err_msg(
            "replace_all",
            vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)]
        ),
        "regex.replace_all takes 3 arguments (pattern, text, replacement)"
    );
}

// ── arg-index lock (pattern is index 0, text is index 1) ─────────────
//
// If the helper ever confused the indices — reading args[1] as pattern
// — the semantics would flip. We prove the current direction by using
// a pattern that matches the text but NOT vice versa.

#[test]
fn pair_helper_uses_args_in_order_pattern_then_text() {
    // Pattern `abc` matches text `xyz abc 123`.
    assert_eq!(
        ok("is_match", vec![s("abc"), s("xyz abc 123")]),
        Value::Bool(true)
    );
    // Swapped: pattern `xyz abc 123` as a regex does NOT match text `abc`.
    assert_eq!(
        ok("is_match", vec![s("xyz abc 123"), s("abc")]),
        Value::Bool(false)
    );
}

#[test]
fn triple_helper_uses_args_in_order_pattern_text_replacement() {
    // Pattern=`\d+`, text="a1 b2", replacement="X" → "aX bX" (after replace_all).
    match ok("replace_all", vec![s(r"\d+"), s("a1 b2"), s("X")]) {
        Value::String(out) => assert_eq!(out, "aX bX"),
        other => panic!("expected String, got {other:?}"),
    }
    // If the helper swapped pattern/text, this would fail: pattern `a1 b2`
    // does not match text `\d+`, so nothing would change.
    match ok("replace_all", vec![s("a1 b2"), s(r"\d+"), s("X")]) {
        // Pattern `a1 b2` literal-matches "a1 b2" inside text `\d+`, which
        // it does NOT contain — so no substitution; result = original text.
        Value::String(out) => assert_eq!(out, r"\d+"),
        other => panic!("expected String, got {other:?}"),
    }
}
