//! Round 80 (L2/L3 BLOAT/parity) — `stream.rs` `require_*` helpers and the
//! local `type_name` shim now use the canonical kind-naming oracle and the
//! `"<fn> requires <Kind>, got <kind>"` shape established in round 75 across
//! `numeric.rs`, `string.rs`, `collections.rs`, `bytes.rs`, `crypto.rs`,
//! `encoding.rs`, `uuid.rs`, and (round 79) `tcp.rs`.
//!
//! Pre-fix `stream.rs` had three drift sites:
//!
//!   * `fn type_name` — third copy of `value_kind` whose `_ => "value"`
//!     fallthrough hid Map/Set/Range/Tuple/Record/Unit/Channel/Handle behind
//!     a generic word in the `stream.write_to_{tcp,file}` "expected Bytes,
//!     got X" diagnostics. Round 75 LATENT enforced TitleCase parity between
//!     `super::common::value_kind` and `vm::Vm::type_name`; this local copy
//!     was the missed third twin.
//!   * `require_channel` — `"{fn} requires Channel"` (no `, got <kind>` tail)
//!   * `require_callable` — `"{fn} requires a function"` (no tail; also drifted
//!     from canonical `Fn` Kind name used by `value_kind`/`Vm::type_name`)
//!
//! Round 80 deletes the local `type_name`, routes its four call sites
//! (`stream.write_to_{tcp,file}` Bytes-mismatch arms) through
//! `super::common::value_kind`, and rewrites the two `require_*` helpers to
//! follow the canonical `"<fn> requires <Kind>, got <kind>"` shape.
//!
//! ## Locking strategy
//!
//! A behavioural test triggers `require_channel` and `require_callable` at
//! runtime — the same `id` laundering trick used by round 79's
//! `tcp_accept_with_non_listener_emits_canonical_shape` — to prove the
//! canonical shape end-to-end. The programs are ill-typed on purpose (the
//! point is the runtime defence), so they stay in Rust rather than golden.

use std::sync::Arc;

// ── Test 3: behavioural — runtime path emits canonical shape ─────────

fn run_for_err(src: &str) -> String {
    let tokens = silt::lexer::Lexer::new(src).tokenize().expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

/// End-to-end: route an Int through a generic `id` function so the
/// typechecker can't reject the wrong-kind argument at the call site,
/// then call `stream.fold(id(123), 0, fn(a,b){a})`. The call reaches the
/// runtime and trips `require_channel`. The error must follow the
/// canonical `"<fn> requires <Kind>, got <kind>"` shape.
#[test]
fn stream_fold_with_non_channel_emits_canonical_shape() {
    let src = r#"
import stream

-- Generic identity function: returns whatever was passed in. The
-- typechecker infers a fresh type variable for `x`, so the call site
-- below can pass `id(123)` (an Int) where `stream.fold` expects a
-- Channel and the typechecker can't catch the mismatch — the error
-- surfaces at runtime through `require_channel`.
fn id(x) { x }

fn main() {
  stream.fold(id(123), 0, fn(a, b) { a })
}
"#;
    let msg = run_for_err(src);
    assert!(
        msg.contains("stream.fold requires Channel"),
        "expected canonical `\"stream.fold requires Channel\"` substring, \
         got: {msg}"
    );
    assert!(
        msg.contains(", got "),
        "expected canonical `\", got <kind>\"` tail naming the offending \
         kind, got: {msg}"
    );
    assert!(
        msg.contains("got Int"),
        "expected the offending kind to surface as `Int` (the laundered \
         value was `123: Int`), got: {msg}"
    );
}

/// End-to-end: route a String through a generic `id` function so the
/// typechecker can't reject the wrong-kind callable arg, then call
/// `stream.fold(ch, 0, id("not a fn"))`. The call reaches the runtime
/// and trips `require_callable`. The error must follow the canonical
/// `"<fn> requires Fn, got <kind>"` shape.
#[test]
fn stream_fold_with_non_callable_emits_canonical_shape() {
    let src = r#"
import stream

fn id(x) { x }

fn main() {
  stream.fold(stream.from_list([1, 2, 3]), 0, id("not a fn"))
}
"#;
    let msg = run_for_err(src);
    assert!(
        msg.contains("stream.fold requires Fn"),
        "expected canonical `\"stream.fold requires Fn\"` substring, \
         got: {msg}"
    );
    assert!(
        msg.contains(", got "),
        "expected canonical `\", got <kind>\"` tail naming the offending \
         kind, got: {msg}"
    );
    assert!(
        msg.contains("got String"),
        "expected the offending kind to surface as `String` (the laundered \
         value was `\"not a fn\": String`), got: {msg}"
    );
}
