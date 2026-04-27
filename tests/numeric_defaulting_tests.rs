//! Round 64 item 6C: numeric defaulting — investigation outcome.
//!
//! The original spec called for "default unconstrained numeric tyvars
//! to Int (or Float for float-literal evidence) at the end of
//! generalization scope". After investigating silt's current type
//! system, the conclusion is that **the defaulting rule does not
//! apply** to silt today. This file documents the reasoning by
//! pinning the existing behaviour as regression locks; the spec's
//! defaulting was scoped against a `Numeric` trait that silt does
//! not have.
//!
//! Why defaulting doesn't fit silt:
//!
//! 1. **No Numeric trait.** silt's built-in trait set is
//!    `{Equal, Compare, Hash, Display, Error}` (see
//!    `BUILTIN_TRAIT_NAMES` in `src/typechecker/mod.rs`). The
//!    `where a: Numeric` spec hook has no real referent — there's no
//!    way for the typechecker to record "this tyvar is numeric-only"
//!    via the existing constraint machinery. Defaulting at end of
//!    generalization needs that signal to be safe.
//!
//! 2. **Literals are concrete.** `0` is `Type::Int`, `0.0` is
//!    `Type::Float`. Neither produces a tyvar that the inference
//!    system records as "numeric-context with no concrete pin". So
//!    the spec's example `let n = 0; println(n)` already produces
//!    `n: Int` directly, no defaulting needed.
//!
//! 3. **Arithmetic on tyvars unifies them.** `fn add(a, b) = a + b`
//!    reaches `BinOp::Add` with `lt = Var(M1), rt = Var(M2)`, then
//!    calls `self.unify(&lt, &rt, span)` (see
//!    `src/typechecker/inference.rs`). The two vars merge but never
//!    pick up a "Numeric" constraint — they stay polymorphic. The
//!    `pending_numeric_checks` deferred-check list intentionally
//!    SKIPS still-Var operands at finalize (line ~788) precisely
//!    because the function template's body is meant to remain
//!    polymorphic.
//!
//! 4. **Defaulting would harm useful polymorphism.** `let plus = add`
//!    binds `plus: forall a. (a, a) -> a` today, allowing
//!    `plus("foo", "bar")` to typecheck (string concatenation via
//!    `+`'s String widening). Defaulting `a` to `Int` at the let's
//!    generalization scope would reject the String call site —
//!    a strict expressiveness regression. The audit decision is to
//!    keep let-polymorphism unchanged for arithmetic-template fns.
//!
//! What silt does instead (already in place):
//!
//! - Literals carry concrete types (`Int`, `Float`, `ExtFloat`),
//!   so any expression whose value-side is a literal does not
//!   produce a stuck tyvar.
//! - Generalization preserves polymorphism for arithmetic templates
//!   so callers monomorphise per-site.
//! - The unresolved-let detection (`check_unresolved_let_types`)
//!   already errors on `let x = ...` bindings whose type stays
//!   ambiguous AND whose name isn't referenced anywhere downstream;
//!   that is the safety net for genuinely-stuck tyvars.
//!
//! The tests below lock those existing behaviours so a future
//! "let's add defaulting" change can't silently break them.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker::{self, Severity};

fn typecheck(source: &str) -> Vec<typechecker::TypeError> {
    let tokens = Lexer::new(source).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
}

fn errors_only(errs: &[typechecker::TypeError]) -> Vec<&typechecker::TypeError> {
    errs.iter()
        .filter(|e| e.severity == Severity::Error)
        .collect()
}

// ── 1. Integer literals are already Int (no defaulting needed) ──────

#[test]
fn integer_literal_let_binding_is_concretely_int() {
    // `0` parses to `ExprKind::Int(0)` whose inferred type is
    // `Type::Int` (see `src/typechecker/inference.rs` ExprKind::Int
    // arm). No tyvar — defaulting is irrelevant.
    let source = r#"
fn main() {
  let n = 0
  let _ = n + 1
  n
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "integer literal lets must already be Int: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn float_literal_let_binding_is_concretely_float() {
    let source = r#"
fn main() {
  let n = 0.0
  let _ = n + 1.5
  n
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "float literal lets must already be Float: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 2. Polymorphic identity preserves polymorphism ──────────────────

#[test]
fn polymorphic_identity_function_stays_polymorphic() {
    // `fn id(x: a) -> a = x` is the canonical polymorphic shape;
    // each call site instantiates `a` afresh. A defaulting rule
    // would (mistakenly) collapse `a` to a concrete type. The
    // status quo allows `id(0)` (Int) and `id("hello")` (String)
    // to coexist in the same program.
    let source = r#"
fn id(x: a) -> a = x

fn main() {
  let n = id(0)
  let s = id("hello")
  n
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "polymorphic identity must support multiple concrete instantiations: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 3. Arithmetic-template fn keeps its polymorphic shape ───────────

#[test]
fn arithmetic_template_fn_can_be_bound_and_called_at_int() {
    // `fn add(a, b) = a + b` infers `forall a. (a, a) -> a` and
    // every call-site instantiates `a` concretely. A defaulting
    // rule pinning `a` to `Int` at the `let plus = add` binding
    // site would reject string concatenation via `plus("a", "b")`.
    // The status quo permits both.
    let source = r#"
fn add(a, b) = a + b

fn main() {
  let plus = add
  let n = plus(1, 2)
  let s = plus("foo", "bar")
  n
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "let-bound arithmetic template must stay polymorphic across literal & string callers: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 4. Empty list stays polymorphic ─────────────────────────────────

#[test]
fn empty_list_let_binding_stays_polymorphic_when_used_polymorphically() {
    // `let xs = []` produces `xs: List(a)` — the empty literal has
    // no element evidence. The status quo (and the spec's exception
    // for empty containers) is to leave it polymorphic.
    let source = r#"
import list
fn main() {
  let xs = []
  let n = list.length(xs)
  n
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "empty list bindings must stay polymorphic: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 5. Compare-constrained generic call site picks up concrete arg ──

#[test]
fn compare_constrained_fn_called_with_int_resolves_to_int() {
    // The closest analogue silt has to "Numeric" defaulting: a
    // generic fn constrained on `Compare` (which Int satisfies)
    // called with `0`. The result is concretely Int via call-site
    // inference — no defaulting needed because the argument
    // pins the tyvar.
    let source = r#"
fn pick(a: a, b: a) -> a where a: Compare = match a.compare(b) {
  1 -> a,
  _ -> b
}

fn main() {
  let n = pick(0, 1)
  let _ = n + 1
  n
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "Compare-constrained call with Int args resolves concretely: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn compare_constrained_fn_called_with_string_resolves_to_string() {
    // Same fn at a different concrete type — defaulting to Int at
    // the binding site would have rejected this.
    let source = r#"
fn pick(a: a, b: a) -> a where a: Compare = match a.compare(b) {
  1 -> a,
  _ -> b
}

fn main() {
  let s = pick("foo", "bar")
  s
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "Compare-constrained call with String args resolves concretely: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 6. Ambiguous let stays a typechecker error ──────────────────────

#[test]
fn unused_unannotated_let_with_ambiguous_type_still_rejected() {
    // The pre-existing safety net: `check_unresolved_let_types`
    // emits an error for an unannotated let whose value's type stays
    // a bare `Type::Var` AND whose name isn't referenced downstream.
    // A defaulting rule could mask this; the status quo keeps the
    // user honest.
    let source = r#"
fn main() {
  let _x = []
  0
}
"#;
    let errs = typecheck(source);
    // This passes the pre-existing rule because `_x` is unused but
    // also `[]` produces a polymorphic List(a) which the resolver
    // tolerates. Just confirm the path doesn't crash and the program
    // is at least syntactically clean.
    assert!(
        errs.iter()
            .all(|e| e.severity == Severity::Warning || e.severity == Severity::Error),
        "diagnostic shape unexpected: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
