//! Round 64 item 6B: annotated polymorphic recursion.
//!
//! When a `fn` carries a fully-annotated signature (every parameter
//! has an explicit type AND the return type is declared), the
//! typechecker locks the registered scheme as authoritative — the
//! body's instantiation does NOT cause the scheme to narrow into a
//! monomorphic shape. As a result, a recursive call inside the same
//! body can instantiate the scheme afresh at a different concrete
//! type (Mycroft 1984's "annotated polymorphic recursion").
//!
//! Without the annotation, silt keeps its existing (decidable)
//! monomorphic-recursion behaviour and emits a one-line help note
//! pointing the user at the annotation escape hatch.

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

fn warnings_only(errs: &[typechecker::TypeError]) -> Vec<&typechecker::TypeError> {
    errs.iter()
        .filter(|e| e.severity == Severity::Warning)
        .collect()
}

// ── 1. Annotated poly-recursion accepted ────────────────────────────

#[test]
fn annotated_poly_recursion_accepted_recursive_call_at_different_concrete_type() {
    // `myfn` is fully annotated with `(p: a) -> Int where a: Display`.
    // The body uses `p` polymorphically (via `Display`), then recurses
    // with a String argument and an Int argument. Both calls
    // instantiate `a` afresh — exactly the polymorphic-recursion case
    // that should typecheck.
    let source = r#"
fn myfn(p: a) -> Int where a: Display = {
  let s = "{p}"
  let _ = myfn("hello")
  let _ = myfn(42)
  0
}

fn main() {
  myfn(("a", "b"))
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "annotated poly-recursion should typecheck cleanly, got: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn annotated_recursion_constraining_body_use_does_not_collapse_scheme() {
    // The body uses `p` via the `Display` constraint (`{p}` interpolation).
    // Pre-round-64-6B, the narrowing pass would collapse the scheme to
    // a concrete type once the body's `p` resolved. With the
    // annotated-fn lock, the scheme stays polymorphic and the
    // recursive call at String works.
    let source = r#"
fn announce(x: a) -> String where a: Display = {
  let label = "{x}"
  let _ = announce("nested")
  label
}

fn main() {
  announce(42)
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "annotated recursion with constraining body should typecheck: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 2. Unannotated case still rejected ───────────────────────────────

#[test]
fn unannotated_poly_recursion_rejected_at_recursive_call_site() {
    // Body uses `p + 1` (constrains p to Int). Recursive call passes a
    // String. Without annotation, silt's monomorphic recursion
    // restriction stands: the narrowed scheme is `(Int) -> Int` and
    // the recursive call must match it. Unify fails with a
    // type-mismatch.
    let source = r#"
fn myfn(p) = {
  let x = p + 1
  let _ = myfn("hello")
  0
}

fn main() {
  myfn(42)
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        !real_errors.is_empty(),
        "unannotated polymorphic recursion should still be rejected"
    );
    let msg_seen: Vec<&str> = real_errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        msg_seen.iter().any(|m| m.contains("type mismatch")),
        "expected a type mismatch on the recursive call site, got: {:?}",
        msg_seen
    );
}

// ── 3. Sanity: existing monomorphic recursion still typechecks ──────

#[test]
fn monomorphic_recursion_still_typechecks() {
    // `fact` is annotated and recurses at the same Int type — the
    // canonical monomorphic-recursion case that has always worked.
    let source = r#"
fn fact(n: Int) -> Int = match n {
  0 -> 1,
  _ -> n * fact(n - 1)
}

fn main() {
  fact(5)
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "monomorphic recursion must still typecheck: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn unannotated_monomorphic_recursion_still_typechecks() {
    // Same shape as above but without annotations — still should
    // typecheck (the body's use of `n` and the recursive call agree
    // on Int via inference).
    let source = r#"
fn fact(n) = match n {
  0 -> 1,
  _ -> n * fact(n - 1)
}

fn main() {
  fact(5)
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "unannotated monomorphic recursion must still typecheck: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 4. Mutual recursion ──────────────────────────────────────────────

#[test]
fn annotated_mutual_recursion_at_different_types_accepted() {
    // Two annotated fns calling each other with different types.
    // Both schemes are locked; instantiation at each call site is
    // fresh.
    let source = r#"
fn ping(x: a) -> Int where a: Display = {
  let _ = pong("string-arg")
  0
}

fn pong(x: b) -> Int where b: Display = {
  let _ = ping(42)
  0
}

fn main() {
  ping(true)
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "annotated mutual recursion at different types should typecheck: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 5. Diagnostic note present on unannotated case ───────────────────

#[test]
fn unannotated_recursion_diagnostic_includes_polymorphic_recursion_hint() {
    let source = r#"
fn myfn(p) = {
  let x = p + 1
  let _ = myfn("hello")
  0
}

fn main() {
  myfn(42)
}
"#;
    let errs = typecheck(source);
    let any_hint = errs.iter().any(|e| {
        e.message
            .contains("add explicit type annotations to enable polymorphic recursion")
    });
    assert!(
        any_hint,
        "expected the polymorphic-recursion-hint warning on unannotated mismatch, got: {:?}",
        errs.iter().map(|e| (&e.severity, &e.message)).collect::<Vec<_>>()
    );
    // The hint is a Warning severity, not an Error — it accompanies
    // the real type-mismatch error rather than replacing it.
    let warns = warnings_only(&errs);
    assert!(
        warns
            .iter()
            .any(|w| w.message.contains("polymorphic recursion")),
        "hint should be emitted as a Warning so it doesn't double-count as an error"
    );
}

// ── 6. Annotated path does NOT emit the hint on a clean call ────────

#[test]
fn annotated_recursion_does_not_emit_hint_for_a_well_typed_call() {
    let source = r#"
fn myfn(p: a) -> Int where a: Display = {
  let _ = myfn("nested")
  0
}

fn main() {
  myfn(0)
}
"#;
    let errs = typecheck(source);
    assert!(
        errs.iter()
            .all(|e| !e.message.contains("polymorphic recursion")),
        "annotated path must not emit the hint for a clean recursive call: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── Soundness probe (round 64 item 6B) ──────────────────────────────

#[test]
fn annotated_recursive_fn_body_pinning_param_emits_signature_mismatch() {
    // The user's signature claims `a` is polymorphic, AND the fn
    // recurses — so the scheme is locked. The body's `x + 1` then
    // pins `a` to Int via the `+` operator's unification, which
    // contradicts the locked polymorphic scheme. The lock must
    // surface this as a real error rather than silently rewriting
    // the user's annotation, otherwise a downstream caller passing
    // a non-numeric type would be wrongly accepted.
    //
    // Scope note: a non-recursive `fn f(x: a) -> Int = x + 1` is NOT
    // covered here — the legacy narrowing path silently
    // monomorphises that case and round 64 item 6B preserves the
    // status quo for non-recursive fns to keep existing test
    // invariants stable. Item 6B is scoped to the recursive case
    // only.
    let source = r#"
fn f(x: a) -> Int = {
  let _ = f(0)
  x + 1
}

fn main() {
  f(1)
}
"#;
    let errs = typecheck(source);
    let real_errors = errors_only(&errs);
    assert!(
        !real_errors.is_empty(),
        "annotated recursive fn with body pinning param to concrete type \
         must emit a signature-mismatch error"
    );
    assert!(
        real_errors
            .iter()
            .any(|e| e.message.contains("polymorphic signature")),
        "expected the polymorphic-signature error, got: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
