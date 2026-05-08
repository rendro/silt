//! Round 79 (F5 BLOAT/parity) — `tcp.rs` `require_*` helpers now use the
//! canonical `"<fn> requires <Kind>, got <kind>"` shape established in
//! round 75 across `numeric.rs`, `string.rs`, `collections.rs`,
//! `bytes.rs`, `crypto.rs`, `encoding.rs`, and `uuid.rs`.
//!
//! Pre-fix `tcp.rs` had four drift sites:
//!
//!   * `require_bool` — `"{fn} requires Bool"` (no `, got` tail)
//!   * `require_bytes` (cfg-gated) — `"{fn} requires Bytes"` (no tail)
//!   * `require_listener` — `"{fn} requires TcpListener"` (no tail)
//!   * `require_stream` — `"{fn} requires TcpStream"` (no tail)
//!
//! Plus an inline `tcp.write` arm that hand-rolled the same truncated
//! string. Round 79 routes Bool/Bytes through `super::common::{require_bool*}`
//! / `super::common::require_bytes` and rewrites the listener/stream
//! arms to use `super::common::value_kind` for the offending kind.
//!
//! ## Locking strategy: source-grep + behavioural
//!
//! Per the CLAUDE workflow note on error-message fixes ("source-grep is
//! the strong form for error-message fixes — tautological format-
//! construction tests just re-derive the format and prove nothing"), we
//! lock the canonical shape with a source-level grep over `tcp.rs`. Any
//! line that contains `"requires"` inside a `VmError::new(format!(...))`
//! payload must also contain `, got` so the diagnostic always names the
//! offending kind (round 73f / round 75 principle).
//!
//! A complementary behavioural test exercises the runtime path: it
//! laundering-casts a non-listener through a generic `id` function and
//! calls `tcp.accept(id(123))`. The typechecker can't pin `id`'s return
//! type to TcpListener, so the call reaches the runtime and trips
//! `require_listener`. We then assert the error contains both
//! `"requires TcpListener"` and `"got "` — proving the canonical shape
//! end-to-end.

#![cfg(feature = "tcp")]

use std::sync::Arc;

const TCP_RS: &str = include_str!("../src/builtins/tcp.rs");

// ── Test 1: source-grep canonical-shape lock ────────────────────────

/// Every `"requires X"` substring in `tcp.rs` must also contain `, got Y`
/// (the canonical round-75 shape). Filter out doc-comments and prose
/// (which mention `requires` for documentation/context purposes) so the
/// test only fires on actual error-string construction.
///
/// This is a stronger lock than building a `VmError` and asserting the
/// rendered text matches the format — the latter just re-derives the
/// format and tells us nothing about the source-level shape.
#[test]
fn tcp_require_helpers_use_canonical_got_shape() {
    let lines: Vec<&str> = TCP_RS.lines().collect();
    let bad: Vec<(usize, &&str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("requires") && !l.contains(", got"))
        .filter(|(_, l)| {
            // Skip doc-comments / line-comments. The drift previously
            // affected real `VmError::new(format!(...))` payloads, not
            // prose. We're conservative here: anything starting with
            // `//` (after leading whitespace) is documentation context,
            // including module-level `//!` and item-level `///` forms.
            let t = l.trim_start();
            !t.starts_with("//")
        })
        .collect();
    assert!(
        bad.is_empty(),
        "tcp.rs has 'requires X' without ', got Y' at:\n{}\n\n\
         Round 75 standardised the shape `\"<fn> requires <Kind>, got <kind>\"` \
         across builtin require_* helpers. Route through \
         `super::common::{{require_bool, require_bytes, require_string, require_int}}` \
         or render the offending kind explicitly via `super::common::value_kind`.",
        bad.iter()
            .map(|(i, l)| format!("  line {}: {}", i + 1, l.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Belt-and-braces: explicitly assert each canonical substring whose
/// error-string construction lives in `tcp.rs` is present. A future
/// refactor that *removes* the substring without reintroducing the
/// canonical replacement still trips the lock.
///
/// `Bytes` is intentionally NOT pinned here: round 79 routes the tcp
/// `require_bytes` wrapper through `super::common::require_bytes`, so
/// the actual format-string lives in `common.rs`, not `tcp.rs`. The
/// `common.rs` Bytes shape is locked by
/// `tests/round75_kind_naming_canonical_tests.rs`.
#[test]
fn tcp_canonical_got_strings_present_for_each_kind() {
    // We do not pin the exact `<fn>` prefix because there are many
    // (`tcp.read`, `tcp.write`, `tcp.set_nodelay`, etc.). We pin the
    // type-token + `, got ` tail.
    for needle in [
        "requires Bool, got ",
        "requires TcpListener, got ",
        "requires TcpStream, got ",
    ] {
        assert!(
            TCP_RS.contains(needle),
            "tcp.rs missing canonical-shape substring `{needle}`. \
             Round 79 unified the `require_*` helpers to the \
             `\"<fn> requires <Kind>, got <kind>\"` shape — a refactor \
             that drops one of these messages must reintroduce it."
        );
    }
}

// ── Test 2: behavioural — runtime path emits canonical shape ────────

/// End-to-end: route a non-listener through a generic `id` function so
/// the typechecker can't reject the wrong-kind argument at the call
/// site, then call `tcp.accept(id(123))`. The call reaches the runtime
/// and trips `require_listener`. The error must contain both
/// `"requires TcpListener"` (the helper's invariant) and `"got "` (the
/// canonical-shape tail naming the offending kind).
///
/// We use `tcp.accept` rather than `tcp.set_nodelay` (which exercises
/// `require_bool`) because `set_nodelay` requires a real `TcpStream` as
/// arg 0 — reaching its `require_bool` on arg 1 would need a full
/// `listen → accept → connect` sequence with cross-task coordination,
/// purely to set up a single error-message check. `tcp.accept` only
/// requires one argument and the `require_listener` path covers the
/// same canonical-shape invariant.
#[test]
fn tcp_accept_with_non_listener_emits_canonical_shape() {
    let src = r#"
import tcp

-- Generic identity function: returns whatever was passed in. The
-- typechecker infers a fresh type variable for `x`, so the call site
-- below can pass `id(123)` (an Int) where `tcp.accept` expects a
-- TcpListener and the typechecker can't catch the mismatch — the
-- error surfaces at runtime through `require_listener`.
fn id(x) { x }

fn main() {
  tcp.accept(id(123))
}
"#;
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
    let msg = format!("{err}");
    assert!(
        msg.contains("requires TcpListener"),
        "expected canonical `\"<fn> requires TcpListener\"` substring, got: {msg}"
    );
    assert!(
        msg.contains("got "),
        "expected canonical `\", got <kind>\"` tail naming the \
         offending kind, got: {msg}"
    );
    assert!(
        msg.contains("got Int"),
        "expected the offending kind to surface as `Int` (the laundered \
         value was `123: Int`), got: {msg}"
    );
}
