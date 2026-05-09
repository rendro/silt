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
//! ## Locking strategy: source-grep + behavioural
//!
//! Per the CLAUDE workflow note on error-message fixes ("source-grep is the
//! strong form for error-message fixes — tautological format-construction
//! tests just re-derive the format and prove nothing"), we lock the canonical
//! shape with two source-level greps over `stream.rs`:
//!
//!  1. The local `fn type_name` is gone (no more catch-all hiding kinds).
//!  2. Every line containing `requires` in a `VmError::new(format!(...))`
//!     payload also contains `, got` (canonical-shape tail).
//!
//! Plus a behavioural test that triggers `require_channel` and
//! `require_callable` at runtime — the same `id` laundering trick used by
//! round 79's `tcp_accept_with_non_listener_emits_canonical_shape` — to prove
//! the canonical shape end-to-end.

use std::sync::Arc;

const STREAM_RS: &str = include_str!("../src/builtins/stream.rs");

// ── Test 1: source-grep — local `type_name` shim is gone ─────────────

/// The local `fn type_name(v: &Value) -> &'static str` in `stream.rs` was
/// the third drifted copy of `value_kind`/`Vm::type_name`. Round 75 LATENT
/// enforced TitleCase parity between the other two; round 80 collapses the
/// stream copy by deleting it and routing its callers through the canonical
/// `super::common::value_kind`.
///
/// This test fails BEFORE the fix (the local shim still defines its own
/// match with a `_ => "value"` catch-all hiding Map/Set/Range/Tuple/Record/
/// Unit/Handle behind a generic word) and PASSES AFTER (the local shim is
/// deleted; the four call sites now reach `super::common::value_kind`).
#[test]
fn stream_rs_has_no_local_type_name_shim() {
    assert!(
        !STREAM_RS.contains("fn type_name("),
        "stream.rs still defines a local `fn type_name(...)` — round 80 \
         L2 deleted this third copy of `value_kind` so the canonical \
         oracle (`super::common::value_kind`, locked by \
         `tests/round75_kind_naming_canonical_tests.rs` and round 75 \
         LATENT) is the single source of truth. Routes for the four \
         `stream.write_to_{{tcp,file}}` Bytes-mismatch arms must use \
         `super::common::value_kind` instead."
    );
    assert!(
        !STREAM_RS.contains("_ => \"value\""),
        "stream.rs still has the `_ => \"value\"` catch-all that hid \
         Map/Set/Range/Tuple/Record/Unit/Handle behind a generic word in \
         `stream.write_to_{{tcp,file}}` diagnostics — round 75 LATENT \
         opposes silent-wrong-answer fallbacks in kind-naming."
    );
}

// ── Test 2: source-grep — canonical `, got` shape on `requires` lines ─

/// Every `format!("{fn_label} requires` line in `stream.rs` (the canonical
/// `require_*` helper pattern) must also contain `, got` — the round-75 /
/// round-79 canonical shape.
///
/// We intentionally scope to the `{fn_label} requires` helper pattern
/// rather than every "requires" substring: stream.rs has additional inline
/// "requires a TcpStream" / "requires a List of Channels" arms that are
/// out-of-scope for round 80 L3 (which targets the `require_channel` and
/// `require_callable` helpers). A broader grep would conflate those.
///
/// This test fails BEFORE the fix (`require_channel` emits
/// `"{fn_label} requires Channel"` and `require_callable` emits
/// `"{fn_label} requires a function"`, both without `, got`) and passes
/// AFTER (both helpers follow the canonical shape via
/// `super::common::value_kind`).
#[test]
fn stream_require_helpers_use_canonical_got_shape() {
    let lines: Vec<&str> = STREAM_RS.lines().collect();
    let bad: Vec<(usize, &&str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("{fn_label} requires") && !l.contains(", got"))
        .filter(|(_, l)| {
            // Skip doc-comments / line-comments.
            let t = l.trim_start();
            !t.starts_with("//")
        })
        .collect();
    assert!(
        bad.is_empty(),
        "stream.rs has `{{fn_label}} requires X` without `, got Y` at:\n{}\n\n\
         Round 75 standardised the shape `\"<fn> requires <Kind>, got <kind>\"` \
         across builtin require_* helpers and round 79 closed the same \
         drift in tcp.rs. Route through `super::common::value_kind` to \
         render the offending kind explicitly.",
        bad.iter()
            .map(|(i, l)| format!("  line {}: {}", i + 1, l.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Belt-and-braces: explicitly assert each canonical substring whose
/// error-string construction lives in `stream.rs` is present. A future
/// refactor that *removes* the substring without reintroducing the
/// canonical replacement still trips the lock.
#[test]
fn stream_canonical_got_strings_present_for_each_kind() {
    for needle in ["requires Channel, got ", "requires Fn, got "] {
        assert!(
            STREAM_RS.contains(needle),
            "stream.rs missing canonical-shape substring `{needle}`. \
             Round 80 unified the `require_*` helpers to the \
             `\"<fn> requires <Kind>, got <kind>\"` shape — a refactor \
             that drops one of these messages must reintroduce it."
        );
    }
}

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
