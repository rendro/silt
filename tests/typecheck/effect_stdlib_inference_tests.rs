//! Phase C inference spot-tests: with every stdlib builtin classified,
//! a user fn's inferred effect set must equal the union of its callees'
//! `Scheme.effects`. These tests use the existing
//! `TypeChecker::fn_body_effects_for` accessor (Phase A plumbing) to
//! confirm Phase C annotations propagate end-to-end.
//!
//! See `docs/proposals/effect-rows.md` Part 7 for the rollout plan.

use silt::intern::intern;
use silt::typechecker::TypeChecker;
use silt::types::effects::{Effect, EffectSet};

/// Drive the type checker on a source string and return the populated
/// checker so each test can introspect `fn_body_effects_for(<name>)`.
fn check(src: &str) -> TypeChecker {
    let tokens = silt::lexer::Lexer::new(src)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let mut checker = TypeChecker::new();
    checker.check_program(&mut program);
    checker
}

#[test]
fn pure_user_fn_calling_list_map_infers_pure() {
    // list.map is `!{}` post-Phase C, so a fn whose body is just a
    // call to list.map (with a pure inline lambda) must also infer
    // `!{}`. If a future refactor flips list.map to `!{io}`, this test
    // catches it.
    let checker = check(
        r#"
import list
fn f(xs: List(Int)) -> List(Int) { list.map(xs, fn(x) { x + 1 }) }
"#,
    );
    let got = checker
        .fn_body_effects_for(intern("f"))
        .expect("f registered");
    assert_eq!(
        got,
        EffectSet::EMPTY,
        "fn calling only list.map must infer pure",
    );
}

#[test]
fn user_fn_calling_io_read_file_infers_io_fs() {
    let checker = check(
        r#"
import io
fn f(p: String) { io.read_file(p) }
"#,
    );
    let got = checker
        .fn_body_effects_for(intern("f"))
        .expect("f registered");
    assert_eq!(
        got,
        EffectSet::io_fs(),
        "fn calling io.read_file must infer !{{io, fs}}",
    );
}

#[test]
fn user_fn_calling_uuid_v7_infers_io_time_random() {
    let checker = check(
        r#"
import uuid
fn f() -> String { uuid.v7() }
"#,
    );
    let got = checker
        .fn_body_effects_for(intern("f"))
        .expect("f registered");
    let expected = EffectSet::io_time().insert(Effect::Random);
    assert_eq!(
        got, expected,
        "fn calling uuid.v7 must infer !{{io, random, time}}",
    );
}

#[test]
fn user_fn_calling_math_random_infers_io_random() {
    let checker = check(
        r#"
import math
fn f() -> Float { math.random() }
"#,
    );
    let got = checker
        .fn_body_effects_for(intern("f"))
        .expect("f registered");
    assert_eq!(
        got,
        EffectSet::io_random(),
        "fn calling math.random must infer !{{io, random}}",
    );
}

#[test]
fn user_fn_calling_pure_math_infers_pure() {
    // Confirm that a fn calling only pure math (math.sqrt) keeps the
    // inferred set at EMPTY — the Phase C sweep didn't accidentally
    // taint trigonometry / etc with `io`. Catches a regression where
    // someone replaces `pure_mono` with `io_mono` in math.rs.
    let checker = check(
        r#"
import math
fn f(x: Float) { math.sqrt(x) }
"#,
    );
    let got = checker
        .fn_body_effects_for(intern("f"))
        .expect("f registered");
    assert_eq!(got, EffectSet::EMPTY, "math.sqrt must remain pure");
}

#[test]
fn user_fn_unioning_two_effects_combines() {
    // io.read_file is `!{io, fs}`; list.length is `!{}` (pure). The
    // union — what the body inference walks — must be `!{io, fs}`,
    // unchanged by the pure call. Locks union semantics across a
    // mixed-effect body.
    let checker = check(
        r#"
import io
import list
fn f(p: String) {
  let _s = io.read_file(p)
  list.length([1, 2, 3])
}
"#,
    );
    let got = checker
        .fn_body_effects_for(intern("f"))
        .expect("f registered");
    assert_eq!(
        got,
        EffectSet::io_fs(),
        "io.read_file ∪ list.length should equal io.read_file's set",
    );
}

#[test]
fn user_fn_calling_println_infers_io() {
    // println is the simplest io-only builtin (no fs/net refinement),
    // so this test pins that the bare `io` classification flows
    // through inference. If the io-mono helpers ever start
    // accidentally adding `fs` or another bit, this trips first.
    let checker = check(
        r#"
fn f() { println("hello") }
"#,
    );
    let got = checker
        .fn_body_effects_for(intern("f"))
        .expect("f registered");
    assert_eq!(
        got,
        EffectSet::io(),
        "fn calling println must infer !{{io}}",
    );
}

#[test]
fn user_fn_calling_time_now_infers_io_time() {
    let checker = check(
        r#"
import time
fn f() { time.now() }
"#,
    );
    let got = checker
        .fn_body_effects_for(intern("f"))
        .expect("f registered");
    assert_eq!(
        got,
        EffectSet::io_time(),
        "fn calling time.now must infer !{{io, time}}",
    );
}

#[test]
fn user_fn_calling_pure_time_helpers_stays_pure() {
    // time.format does not read the clock — it formats a passed-in
    // DateTime. Phase C marks it pure. A regression that flips it to
    // io_time would trip this test.
    let checker = check(
        r#"
import time
fn f(d: DateTime) { time.format(d, "%Y") }
"#,
    );
    let got = checker
        .fn_body_effects_for(intern("f"))
        .expect("f registered");
    assert_eq!(got, EffectSet::EMPTY, "time.format must be pure");
}
