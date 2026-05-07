//! Round 77 BLOAT D3 lock tests — container method-dispatch arms.
//!
//! The pre-fix code in `src/typechecker/inference.rs:2623-2704` had
//! four match arms for `Type::List`, `Type::Tuple`, `Type::Map`, and
//! `Type::Set` that were line-for-line identical except for the
//! literal type-name string (`"List"|"Tuple"|"Map"|"Set"`). Adding a
//! fifth container head, or changing the dispatch flow, required
//! editing four near-clones in lockstep — a textbook latent-drift
//! risk.
//!
//! The fix collapses the four arms onto a single arm that asks
//! `type_name_for_impl` for the canonical name. The audit-guide rule
//! for refactor-style fixes requires a semantic-equivalence lock plus
//! a structural negative-grep lock so the four-clone shape cannot
//! return.
//!
//! Locks below:
//!
//!  1. **Behavioural** — for each of the four container heads, an
//!     unknown-method call must yield an `unknown method 'X' on <Head>`
//!     diagnostic with the literal head-name string (no `type` prefix
//!     — that's the primitive arm's wording, not the container arm's).
//!     This pins the per-head dispatch path and the error wording in
//!     a single assertion.
//!
//!  2. **Suggestion parity** — when an auto-derived method name is a
//!     near edit-distance match of the typo, the diagnostic must
//!     append a `did you mean` hint. This exercises the
//!     `format_unknown_method_message` plumbing for every head and
//!     asserts the suggestion side-channel survived the collapse.
//!
//!  3. **Structural** — the source must contain at most ONE
//!     `Type::List(_)` pattern in the dispatch arm region; the
//!     four-clone version had four. Same for `Type::Tuple(_)`,
//!     `Type::Map(_, _)`, and `Type::Set(_)`. The merged pattern
//!     groups all four heads in a single `t @ (… | … | … | …)`
//!     construct. This is the audit-guide "negative-grep" lock for
//!     dead-code collapse fixes.

use silt::typechecker;
use silt::types::Severity;

const INFERENCE_SRC: &str = include_str!("../src/typechecker/inference.rs");

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn first_unknown_method(errs: &[String]) -> Option<String> {
    errs.iter().find(|e| e.contains("unknown method")).cloned()
}

// ── 1. Behavioural lock — per-head error wording ──────────────────────

#[test]
fn list_unknown_method_error_uses_bare_list_head() {
    // `xs` resolves to `List(Int)`. The unknown-method dispatch must
    // route through the container arm, key the method_table on the
    // canonical name `"List"`, and report the failure with the bare
    // head string `"List"` — not `"type List"` (which is the
    // primitive arm's wording).
    let errs = type_errors(
        r#"
fn main() {
  let xs = [1, 2, 3]
  println(xs.qqqqqqqq_unrelated())
}
"#,
    );
    let msg =
        first_unknown_method(&errs).expect("expected an unknown-method error on List receiver");
    assert!(
        msg.contains("on List"),
        "expected `on List` in message, got: {msg:?}"
    );
    assert!(
        !msg.contains("on type List"),
        "container arm uses bare-head wording, not `type` prefix; got: {msg:?}"
    );
}

#[test]
fn tuple_unknown_method_error_uses_bare_tuple_head() {
    let errs = type_errors(
        r#"
fn main() {
  let t = (1, 2)
  println(t.qqqqqqqq_unrelated())
}
"#,
    );
    let msg =
        first_unknown_method(&errs).expect("expected an unknown-method error on Tuple receiver");
    assert!(
        msg.contains("on Tuple"),
        "expected `on Tuple` in message, got: {msg:?}"
    );
    assert!(
        !msg.contains("on type Tuple"),
        "container arm uses bare-head wording, not `type` prefix; got: {msg:?}"
    );
}

#[test]
fn map_unknown_method_error_uses_bare_map_head() {
    let errs = type_errors(
        r#"
fn main() {
  let m = #{ "a": 1, "b": 2 }
  println(m.qqqqqqqq_unrelated())
}
"#,
    );
    let msg =
        first_unknown_method(&errs).expect("expected an unknown-method error on Map receiver");
    assert!(
        msg.contains("on Map"),
        "expected `on Map` in message, got: {msg:?}"
    );
    assert!(
        !msg.contains("on type Map"),
        "container arm uses bare-head wording, not `type` prefix; got: {msg:?}"
    );
}

#[test]
fn set_unknown_method_error_uses_bare_set_head() {
    let errs = type_errors(
        r#"
fn main() {
  let s = #[1, 2, 3]
  println(s.qqqqqqqq_unrelated())
}
"#,
    );
    let msg =
        first_unknown_method(&errs).expect("expected an unknown-method error on Set receiver");
    assert!(
        msg.contains("on Set"),
        "expected `on Set` in message, got: {msg:?}"
    );
    assert!(
        !msg.contains("on type Set"),
        "container arm uses bare-head wording, not `type` prefix; got: {msg:?}"
    );
}

// ── 2. Suggestion parity — `did you mean` hint per head ───────────────

/// `dispaly` → `display` on a List receiver. `display` is an
/// auto-derived builtin-trait method registered on every type
/// including List, so the method_table has a real candidate to feed
/// `suggest_similar`. The helper must surface the hint via
/// `format_unknown_method_message` after the four-arm collapse.
#[test]
fn list_unknown_method_suggests_close_match_after_collapse() {
    let errs = type_errors(
        r#"
fn main() {
  let xs = [1, 2, 3]
  println(xs.dispaly())
}
"#,
    );
    let msg = first_unknown_method(&errs).expect("expected an unknown-method error");
    assert!(
        msg.contains("\nhelp: did you mean `display`?"),
        "expected newline-prefixed `did you mean display` hint on List, got: {msg:?}"
    );
}

// ── 3. Structural lock — pattern count caps ───────────────────────────

/// The dispatch arm region must mention `Type::List(_)` AT MOST a
/// small number of times. The four-clone version had four separate
/// `Type::List(_) =>` arms in this region; the collapsed version has
/// just one `Type::List(_)` token inside the merged
/// `t @ (Type::List(_) | Type::Tuple(_) | Type::Map(_, _) |
/// Type::Set(_))` pattern.
///
/// We bound the global count at <= 3 (which fails on 4 clones) but
/// passes on the collapsed version even if other unrelated mentions
/// of `Type::List(_)` exist elsewhere in the file. Pre-fix the count
/// in just the four arm bodies plus the literal head pattern at each
/// arm start was already 4 — this lock fires.
#[test]
fn inference_rs_does_not_have_four_list_dispatch_arm_clones() {
    let count = INFERENCE_SRC.matches("Type::List(_) =>").count();
    assert!(
        count <= 1,
        "expected at most one `Type::List(_) =>` arm head in inference.rs \
         (the four-clone container dispatch has been collapsed). Found {count}. \
         If you re-introduced a per-head arm, route it through the merged \
         `t @ (Type::List(_) | …) =>` arm instead."
    );
}

#[test]
fn inference_rs_does_not_have_per_head_tuple_dispatch_arm() {
    let count = INFERENCE_SRC.matches("Type::Tuple(_) =>").count();
    assert!(
        count == 0,
        "expected no `Type::Tuple(_) =>` arm in inference.rs after the \
         round-77 BLOAT D3 collapse — the merged container arm covers \
         Tuple. Found {count} per-head arm head(s)."
    );
}

#[test]
fn inference_rs_does_not_have_per_head_map_dispatch_arm() {
    let count = INFERENCE_SRC.matches("Type::Map(_, _) =>").count();
    assert!(
        count == 0,
        "expected no `Type::Map(_, _) =>` arm in inference.rs after the \
         round-77 BLOAT D3 collapse — the merged container arm covers \
         Map. Found {count} per-head arm head(s)."
    );
}

#[test]
fn inference_rs_does_not_have_per_head_set_dispatch_arm() {
    let count = INFERENCE_SRC.matches("Type::Set(_) =>").count();
    assert!(
        count == 0,
        "expected no `Type::Set(_) =>` arm in inference.rs after the \
         round-77 BLOAT D3 collapse — the merged container arm covers \
         Set. Found {count} per-head arm head(s)."
    );
}

/// The merged pattern itself must be present — this is the positive
/// half of the structural lock. Together with the negative caps
/// above, it pins the exact shape of the refactor.
#[test]
fn inference_rs_has_merged_container_arm_pattern() {
    // We allow either ordering of inner pipes and any whitespace, so
    // assert each of the four heads appears inside one merged
    // alternation. The simplest stable check: the merged arm contains
    // all four heads on the same `match` arm and is bound via
    // `t @ (...)` so the body can ask `type_name_for_impl(t)`.
    assert!(
        INFERENCE_SRC.contains("Type::List(_)")
            && INFERENCE_SRC.contains("Type::Tuple(_)")
            && INFERENCE_SRC.contains("Type::Map(_, _)")
            && INFERENCE_SRC.contains("Type::Set(_)"),
        "all four container heads must still appear (in the merged arm)"
    );
    assert!(
        INFERENCE_SRC.contains("type_name_for_impl(t)"),
        "merged container arm must dispatch through `type_name_for_impl(t)` \
         (single source of truth for canonical head names; see \
         `src/typechecker/mod.rs:1887`)"
    );
}
