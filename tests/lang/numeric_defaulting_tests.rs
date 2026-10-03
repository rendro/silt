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
//! 3. **Arithmetic on tyvars unifies them.** `fn add(a, b) { a + b }`
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
//!    binds `plus: forall a. (a, a) -> a` today, allowing both
//!    `plus(1, 2)` and `plus(1.5, 2.5)` to typecheck. Defaulting `a`
//!    to `Int` at the let's generalization scope would reject the
//!    Float call site — a strict expressiveness regression. The audit decision is to
//!    keep let-polymorphism unchanged for arithmetic-template fns.
//!
//! What silt does instead (already in place):
//!
//! - Literals carry concrete types (`Int`, `Float`),
//!   so any expression whose value-side is a literal does not
//!   produce a stuck tyvar.
//! - Generalization preserves polymorphism for arithmetic templates
//!   so callers monomorphise per-site.
//! - The unresolved-let detection (`check_unresolved_let_types`)
//!   already errors on `let x = ...` bindings whose type stays
//!   ambiguous AND whose name isn't referenced anywhere downstream;
//!   that is the safety net for genuinely-stuck tyvars.
//!
//! The locks are golden cases
//! `tests/golden/lang/typecheck/numeric_defaulting__*`, plus the test
//! below.

use silt::diagnostic::Severity;

fn typecheck(source: &str) -> Vec<silt::diagnostic::Diagnostic> {
    silt::session::testing::analyze_str(source).1
}

fn errors_only(errs: &[silt::diagnostic::Diagnostic]) -> Vec<&silt::diagnostic::Diagnostic> {
    errs.iter()
        .filter(|e| e.severity == Severity::Error)
        .collect()
}

// ── 3. Arithmetic-template fn keeps its polymorphic shape ───────────

#[test]
fn arithmetic_template_fn_can_be_bound_and_called_at_int() {
    // `fn add(a, b) { a + b }` infers `forall a. (a, a) -> a` and
    // every call-site instantiates `a` concretely. A defaulting
    // rule pinning `a` to `Int` at the `let plus = add` binding
    // site would reject the Float call `plus(1.5, 2.5)`. The status
    // quo permits both.
    let source = r#"
fn add(a, b) { a + b }

fn main() {
  let plus = add
  let n = plus(1, 2)
  let f = plus(1.5, 2.5)
  n
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "let-bound arithmetic template must stay polymorphic across Int & Float callers: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
