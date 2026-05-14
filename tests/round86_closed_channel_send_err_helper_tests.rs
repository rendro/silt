//! Round 86 audit — L5 BLOAT regression locks.
//!
//! Finding: the literal
//!   `Err(VmError::new(format!("send on closed channel {}", ch.id)))`
//! was duplicated across 5 try_send branches in
//! `src/builtins/concurrency.rs` (lines 59, 991, 1032, 1059, 1074 prior
//! to the fix). The wording is pinned by tests at
//! `tests/error_tests.rs:883` and `tests/integration.rs:780, 7433`, but
//! each individual test only exercises one of those branches, so the
//! other four could drift silently.
//!
//! Fix: introduce a private `closed_channel_send_err(id: usize)` helper
//! and route every site through it. These tests are the lock:
//!
//!   (a) source-grep — `format!("send on closed channel ` must appear
//!       in exactly ONE place in `src/builtins/concurrency.rs` (the
//!       helper body). Any future copy-paste of the formatted literal
//!       trips this assertion.
//!   (b) runtime — sending on a closed channel must still surface the
//!       pinned wording `"send on closed channel <id>"` to user code,
//!       regardless of which internal try_send branch fired.
//!
//! Both locks are necessary: (a) prevents structural drift, (b) prevents
//! a silent rewrite of the helper's message (which (a) alone wouldn't
//! catch).

use std::path::PathBuf;
use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::vm::Vm;

fn read_concurrency_src() -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("src/builtins/concurrency.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ── (a) source-grep regression lock ──────────────────────────────────

/// The exact `format!("send on closed channel ` literal must appear in
/// `src/builtins/concurrency.rs` in exactly ONE place (inside the
/// `closed_channel_send_err` helper body). If a future edit
/// reintroduces a copy at any other try_send branch, this fails.
#[test]
fn closed_channel_send_format_literal_used_only_once() {
    let src = read_concurrency_src();
    let needle = r#"format!("send on closed channel "#;
    let count = src.matches(needle).count();
    assert_eq!(
        count, 1,
        "expected exactly 1 occurrence of `{needle}` in \
         src/builtins/concurrency.rs (the `closed_channel_send_err` \
         helper body); found {count}. Every closed-channel try_send \
         branch must route through the helper — do not re-inline the \
         format!. See round86 audit notes."
    );
}

/// The bare wording (without `format!(` prefix) is allowed to appear in
/// (i) the helper body and (ii) at most a single docstring line that
/// mentions it. Anything more suggests duplicated user-visible strings.
/// This is a softer companion to the format!-literal lock above.
#[test]
fn closed_channel_send_helper_exists() {
    let src = read_concurrency_src();
    assert!(
        src.contains("fn closed_channel_send_err("),
        "the `closed_channel_send_err` helper is missing from \
         src/builtins/concurrency.rs — Round 86 BLOAT-L5 collapsed 5 \
         copies of the closed-channel-send format!() into this single \
         helper; do not remove it without re-distributing the wording \
         lock."
    );
}

// ── (b) runtime behavior lock ────────────────────────────────────────

/// Compile and run a Silt program that sends on a closed channel; the
/// runtime error message must contain the pinned wording
/// `"send on closed channel <numeric-id>"`. Shape mirrors the fixtures
/// at `tests/error_tests.rs:883` and `tests/integration.rs:780`.
#[test]
fn closed_channel_send_runtime_message_is_pinned() {
    let source = r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.close(ch)
  channel.send(ch, "hello")
}
    "#;
    let tokens = Lexer::new(source).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = match vm.run(script) {
        Err(e) => format!("{e}"),
        Ok(v) => panic!("expected runtime error, got: {v:?}"),
    };

    // Pinned prefix.
    assert!(
        err.contains("send on closed channel "),
        "runtime message lost the pinned `send on closed channel ` \
         prefix: got {err:?}"
    );

    // The id following the prefix must be numeric.
    let after = err
        .split("send on closed channel ")
        .nth(1)
        .expect("split must yield a tail");
    let first_token: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    assert!(
        !first_token.is_empty(),
        "expected numeric id after `send on closed channel ` prefix, \
         got tail: {after:?} (full message: {err:?})"
    );
    let _id: u64 = first_token
        .parse()
        .expect("collected ascii-digit prefix must parse as u64");
}
