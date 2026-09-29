//! Round-52 audit lock: `channel.select` requires `ChannelOp(a)` elements.
//!
//! Commit `2f86aeb` ("stdlib: unify channel.select element shape via
//! ChannelOp(a)") replaced the old dual shape — bare `Channel(a)` for
//! receive arms and raw `(Channel(a), a)` tuples for send arms — with a
//! single tagged `ChannelOp(a)` form constructed by `Recv(ch)` or
//! `Send(ch, value)`. Every positive test covers the happy path (they
//! typecheck and run). None of them asserted that the OLD shapes are
//! now rejected.
//!
//! These tests lock the negative side: a regression that silently
//! re-accepted bare channels or raw tuples in `channel.select` would
//! pass the rest of the suite but fail here.
//!
//! Signature source of truth: `src/typechecker/builtins/channel.rs:98-124`
//!   `channel.select: List(ChannelOp(a)) -> (Channel(a), ChannelResult(a))`
//! Element constructors: `src/typechecker/builtins.rs:342-398`
//!   `Recv : Channel(a) -> ChannelOp(a)`
//!   `Send : Channel(a), a -> ChannelOp(a)`
//! Runtime parser: `src/builtins/concurrency.rs:816-842` (`parse_select_ops`).
//!
//! The unification error messages from the typechecker take the shape
//! `type mismatch: expected ChannelOp, got Channel` (or `got Tuple`),
//! produced by `src/typechecker/mod.rs:635-690`'s `unify` default
//! branch. Each negative assertion pins the substring `"ChannelOp"` so
//! the diagnostic must specifically name the expected element type,
//! not just any stray mismatch.

use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

/// Run lex + parse + typecheck; return all hard-error messages.
fn type_errors(src: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(src)
        .tokenize()
        .expect("lexer error in negative-shape test source");
    let mut program = Parser::new(tokens)
        .parse_program()
        .expect("parse error in negative-shape test source");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Assert the typechecker produced at least one hard error whose message
/// contains `needle`, pinning the diagnostic to the expected shape.
fn assert_err_mentions(src: &str, needle: &str) {
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|e| e.contains(needle)),
        "expected a typecheck error mentioning `{needle}`, got: {errs:?}"
    );
}

// ── Test A: bare channel in select list rejected ──────────────────────
//
// Pre-round-52 this shape was accepted (the receive-only form). After
// unification the only legal receive element is `Recv(ch)`. Passing a
// bare `Channel(Int)` where `ChannelOp(Int)` is expected must produce
// a hard type error whose message names `ChannelOp` — otherwise we
// have silently re-admitted the pre-unification shape.
#[test]
fn bare_channel_in_select_list_rejected() {
    let src = r#"
import channel
fn main() {
  let ch: Channel(Int) = channel.new(1)
  let _ = channel.select([ch])
  ()
}
"#;
    assert_err_mentions(src, "ChannelOp");
}

// ── Test B: raw tuple in select list rejected ─────────────────────────
//
// Pre-round-52 a `(Channel(a), a)` tuple encoded a send arm. After
// unification every send must be `Send(ch, value)`. A raw tuple has
// type `(Channel(Int), Int)` which unifies against `ChannelOp(Int)`
// via the default branch of `unify`, yielding `type mismatch: expected
// ChannelOp, got (Channel(Int), Int)`.
#[test]
fn raw_tuple_in_select_list_rejected() {
    let src = r#"
import channel
fn main() {
  let ch: Channel(Int) = channel.new(1)
  let _ = channel.select([(ch, 1)])
  ()
}
"#;
    assert_err_mentions(src, "ChannelOp");
}

// ── Test C: mixed list with a bare channel rejected ───────────────────
//
// A list that mixes `Recv(ch)` with a bare `ch` must still be rejected.
// The list's element type is inferred from the `Recv(ch)` element as
// `ChannelOp(Int)`, so the bare `ch` term (type `Channel(Int)`) fails
// to unify against it. Guards against a partial-regression that only
// rejects fully-bare lists but admits bare elements alongside
// `ChannelOp` ones.
#[test]
fn mixed_bare_and_recv_in_select_list_rejected() {
    let src = r#"
import channel
fn main() {
  let ch1: Channel(Int) = channel.new(1)
  let ch2: Channel(Int) = channel.new(1)
  let _ = channel.select([Recv(ch1), ch2])
  ()
}
"#;
    assert_err_mentions(src, "ChannelOp");
}

// ── Test D: positive control ──────────────────────────────────────────
//
// A well-formed `channel.select([Recv(ch), Send(ch, 1)])` must
// typecheck cleanly. Without this, an over-broad rejection (e.g. a
// typechecker change that accidentally rejects any list element at all
// going into `channel.select`) could make tests A–C pass while
// breaking legitimate usage. This test is the "no false positives"
// counterpart to the negative locks above.
#[test]
fn recv_and_send_in_select_list_typechecks() {
    let src = r#"
import channel
fn main() {
  let ch: Channel(Int) = channel.new(1)
  let _ = channel.select([Recv(ch), Send(ch, 1)])
  ()
}
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "a well-formed channel.select([Recv(ch), Send(ch, 1)]) must \
         typecheck with no errors, got: {errs:?}"
    );
}
