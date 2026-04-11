//! Regression tests pinning the type-system audit fixes shipped alongside
//! this file. Each test was written to FAIL against the pre-fix codebase
//! and to PASS after the corresponding fix in `src/typechecker/*.rs`.
//!
//! These live in their own file (rather than `tests/error_tests.rs`) to
//! avoid edit collisions with the broader test-coverage strengthening
//! happening in parallel.

use silt::lexer::Span;
use silt::typechecker;
use silt::types::Severity;

// ── Helpers ─────────────────────────────────────────────────────────

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

fn type_errors_full(input: &str) -> Vec<(String, Span)> {
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
        .map(|e| (e.message, e.span))
        .collect()
}

fn assert_type_error(input: &str, pattern: &str) {
    let errs = type_errors(input);
    assert!(
        errs.iter().any(|e| e.contains(pattern)),
        "expected type error containing '{pattern}', got: {errs:?}"
    );
}

// ── BROKEN-1: RecordUpdate on Type::Generic base ───────────────────

#[test]
fn test_record_update_unknown_field_on_param_rejected() {
    // When a RecordUpdate's base was a function parameter, its type
    // surfaced to inference as `Type::Generic("Config", [])` and the
    // unknown-field branch silently accepted every field, dropping the
    // `nonexistent: ...` write at runtime.
    assert_type_error(
        r#"
type Config { host: String, port: Int }
fn update(c: Config) -> Config { c.{ nonexistent: "bogus", port: 9090 } }
fn main() {
  let c = Config { host: "h", port: 80 }
  let c2 = update(c)
  println("{c2.port} {c2.host}")
}
"#,
        "unknown field 'nonexistent'",
    );
}

// ── BROKEN-2: RecordUpdate on non-record base ──────────────────────

#[test]
fn test_record_update_unknown_field_on_non_record_rejected_at_typecheck() {
    // `(42).{ bogus: 1 }` previously type-checked and exploded only at
    // runtime. It must now be a compile-time error at the base expr.
    assert_type_error(
        r#"
fn main() { let y = (42).{ bogus: 1 } println("{y}") }
"#,
        "record update",
    );
}

// ── BROKEN-3: match Pattern::Record unknown field ───────────────────

#[test]
fn test_match_pattern_unknown_record_field_rejected() {
    assert_type_error(
        r#"
type Point { x: Int, y: Int }
fn check(p: Point) -> String {
  match p {
    Point { z: 5 } -> "matched z"
    _ -> "fallback"
  }
}
fn main() { println(check(Point { x: 1, y: 2 })) }
"#,
        "no field 'z'",
    );
}

// ── BROKEN-4: let-destructure Pattern::Record ───────────────────────

#[test]
fn test_let_destructure_unknown_record_field_rejected() {
    assert_type_error(
        r#"
type User { name: String, age: Int }
fn main() {
  let u = User { name: "a", age: 10 }
  let User { nonexistent } = u
  println(nonexistent)
}
"#,
        "no field 'nonexistent'",
    );
}

#[test]
fn test_let_destructure_record_pattern_on_non_record_rejected() {
    // `let NotDeclared { x } = 42` uses a record pattern against a
    // non-record value — rejected at compile time with at least one
    // diagnostic mentioning either the undefined record type or that
    // a record pattern needs a record value.
    let errs = type_errors(
        r#"
fn main() {
  let NotDeclared { x } = 42
  println(x)
}
"#,
    );
    // Both diagnostics fire: one for the undefined record type, one for the
    // record-pattern-on-non-record mismatch. Assert both exact phrases.
    assert!(
        errs.iter()
            .any(|e| e.contains("undefined record type 'NotDeclared' in pattern")),
        "expected undefined-record-type-'NotDeclared' diagnostic, got: {errs:?}"
    );
    assert!(
        errs.iter().any(|e| e
            .contains("record pattern requires a record value, but 'Int' is not a record type")),
        "expected record-pattern-on-non-record diagnostic, got: {errs:?}"
    );
}

// ── GAP-1: where-clause on function value ───────────────────────────

#[test]
fn test_where_display_rejects_function_value() {
    // `type_name_for_impl` used to return None for Type::Fun, which
    // silently skipped the trait_impl_set check and let `show(f)`
    // through. It now resolves to "Fun", which has no trait impls, so
    // the error fires.
    assert_type_error(
        r#"
fn show(x: a) -> String where a: Display { "{x}" }
fn main() {
  let f = fn() { 42 }
  println(show(f))
}
"#,
        "Display",
    );
}

// ── GAP-2: trait impl missing-method diagnostic span ────────────────

#[test]
fn test_trait_impl_missing_method_has_real_span() {
    // The "missing method" diagnostic previously used Span::new(0, 0)
    // when the impl block had no methods to borrow a span from. It
    // must now be reported at the impl block's real location (non-zero
    // line number).
    let errs = type_errors_full(
        r#"
trait Foo {
  fn a(self) -> Int
  fn b(self) -> Int
}
trait Foo for Int { }
fn main() { 42 }
"#,
    );
    let missing: Vec<_> = errs
        .iter()
        .filter(|(m, _)| m.contains("missing method"))
        .collect();
    assert!(
        !missing.is_empty(),
        "expected at least one 'missing method' error, got: {errs:?}"
    );
    for (_, span) in &missing {
        assert!(
            span.line > 0,
            "missing-method diagnostic has sentinel span (line 0): {missing:?}"
        );
    }
}

// ── BROKEN (round 15): bind_pattern skipped scrutinee unification ──
//
// `Pattern::Tuple` and `Pattern::Constructor` arms of
// `Checker::bind_pattern` used to resolve the scrutinee type and,
// if it wasn't already a tuple/enum, silently fall through to
// creating fresh vars for each sub-pattern — never unifying. That
// let `let (a, b) = 42` and `let Ok(x) = 42` slip past `silt
// check` and blow up at runtime with `DestructTuple on non-tuple`
// / `DestructVariant on non-variant`. Both pattern arms now build
// the expected shape and unify it against the scrutinee before
// recursing.

#[test]
fn test_let_tuple_destructure_on_non_tuple_rejected_at_typecheck() {
    assert_type_error(
        r#"
fn main() {
  let (a, b) = 42
  let y: Int = a
  println(y)
}
"#,
        "got Int",
    );
}

#[test]
fn test_let_constructor_pattern_on_non_variant_rejected_at_typecheck() {
    assert_type_error(
        r#"
fn main() {
  let Ok(x) = 42
  let y: Int = x
  println(y)
}
"#,
        "got Int",
    );
}

#[test]
fn test_let_tuple_destructure_wrong_arity_still_rejected() {
    // Sanity check that the rewrite of the Tuple arm still catches
    // arity mismatches (now via the Tuple/Tuple branch of unify).
    assert_type_error(
        r#"
fn main() {
  let (a, b, c) = (1, 2)
  println("{a} {b} {c}")
}
"#,
        "tuple length mismatch: expected 3, got 2",
    );
}

#[test]
fn test_when_tuple_destructure_on_non_tuple_rejected() {
    assert_type_error(
        r#"
fn main() {
  when (a, b) = 42 else { return }
  println("{a} {b}")
}
"#,
        "got Int",
    );
}

#[test]
fn test_when_constructor_pattern_on_non_variant_rejected() {
    assert_type_error(
        r#"
fn main() {
  when Ok(x) = 42 else { return }
  println("{x}")
}
"#,
        "got Int",
    );
}

#[test]
fn test_stmt_let_constructor_inside_fn_body_still_rejected() {
    // Same bug, same shape, nested inside a helper fn body to make
    // sure the fix isn't just firing at top-level `main`.
    assert_type_error(
        r#"
fn helper() -> Int {
  let Ok(x) = 99
  x
}
fn main() { println("{helper()}") }
"#,
        "got Int",
    );
}

#[test]
fn test_valid_tuple_destructure_still_typechecks() {
    // Positive case: a well-typed tuple destructure must still have
    // zero type errors after the fix.
    let errs = type_errors(
        r#"
fn main() {
  let (a, b) = (1, "hi")
  let i: Int = a
  let s: String = b
  println("{i} {s}")
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for valid tuple destructure, got: {errs:?}"
    );
}

// ── R1 (round 15): Record pattern destructure through fn boundary ──
//
// `resolve_type_expr` maps user record type annotations (like `Pair`
// in a parameter or return type) to `Type::Generic(name, args)`. Before
// the round-15 fix, `bind_pattern`'s `Pattern::Record` arm only
// handled `Type::Record(..)` and rejected `Type::Generic(..)` with a
// misleading "record pattern requires a record value, but 'Pair' is
// not a record type" error, breaking any `let Name { f } = x` where
// `x` came from a function call or parameter.

#[test]
fn test_record_destructure_through_fn_boundary() {
    // Regression: let Pair { a, b } = mkpair() must typecheck cleanly
    // and the runtime must print 3 (1 + 2). A stray error from the
    // old Record arm would fail both the type-error-empty assertion
    // AND the runtime output assertion.
    let src = r#"
type Pair { a: Int, b: Int }
fn mkpair() -> Pair { Pair { a: 1, b: 2 } }
fn main() {
  let p = mkpair()
  let Pair { a, b } = p
  println(a + b)
}
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "expected no type errors for fn-boundary record destructure, got: {errs:?}"
    );
}

#[test]
fn test_record_destructure_through_fn_param_annotated() {
    // The parameter's declared type `Pair` resolves to Type::Generic
    // ("Pair", []), so `let Pair { a, b } = p` in the body must still
    // bind sub-patterns instead of rejecting the scrutinee.
    let errs = type_errors(
        r#"
type Pair { a: Int, b: Int }
fn sum(p: Pair) -> Int {
  let Pair { a, b } = p
  a + b
}
fn main() { println(sum(Pair { a: 10, b: 20 })) }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for fn-param record destructure, got: {errs:?}"
    );
}

#[test]
fn test_record_destructure_nested_through_fn_boundary() {
    // Nested-record destructuring through two fn hops. Inner `Inner`
    // field must be destructured after pulling it out of Outer.
    let errs = type_errors(
        r#"
type Inner { x: Int }
type Outer { inner: Inner, tag: Int }
fn g(o: Outer) -> Int {
  let Outer { inner, tag } = o
  let Inner { x } = inner
  x + tag
}
fn mk() -> Outer { Outer { inner: Inner { x: 5 }, tag: 3 } }
fn main() { println(g(mk())) }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for nested fn-boundary record destructure, got: {errs:?}"
    );
}

#[test]
fn test_parameterized_record_destructure_through_fn_boundary() {
    // Parameterized record `Box(a)` — the field template references
    // the record's type parameter, which must get substituted with
    // the concrete type arg from `Box(Int)` before the sub-pattern is
    // bound, so `value` has type `Int` and not `?a`.
    let errs = type_errors(
        r#"
type Box(a) { value: a }
fn mk() -> Box(Int) { Box { value: 42 } }
fn main() {
  let b = mk()
  let Box { value } = b
  let x: Int = value
  println(x)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for parameterized fn-boundary record destructure, got: {errs:?}"
    );
}

// ── B1 (round 15): exhaustiveness for records reached via Type::Generic ──
//
// `is_wildcard_useful` / `missing_description` only consulted the
// enums table when the scrutinee was `Type::Generic(name, _)`. Records
// passed through fn boundaries surface under the same constructor and
// would fall through as "exhaustive", deferring the failure to a
// runtime "non-exhaustive match: no arm matched" crash.

#[test]
fn test_exhaustiveness_record_fn_param_missing_arm_rejected() {
    // Match covers only `Pair { a: 1, b: 1 }`. The wildcard case
    // (e.g. `Pair { a: 1, b: 2 }`) must be surfaced at TYPECHECK,
    // not the runtime.
    let errs = type_errors(
        r#"
type Pair { a: Int, b: Int }
fn check(p: Pair) -> String {
  match p {
    Pair { a: 1, b: 1 } -> "one"
  }
}
fn main() { println(check(Pair { a: 1, b: 2 })) }
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("non-exhaustive match")),
        "expected 'non-exhaustive match' error, got: {errs:?}"
    );
}

// ════════════════════════════════════════════════════════════════════
// AUDIT FINDINGS: round 16
// ════════════════════════════════════════════════════════════════════

// ── B1 (round 16): refutable variant pattern in `let` is type-unsound
//
// `let Square(n) = circle_value` used to silently destructure the
// Circle's payload into `n` and produce nonsense downstream (Circle's
// Int payload was read into `n`, and the error cascaded into a
// misleading `+ Int String` at the first use of `n`). The typechecker
// now rejects refutable Constructor patterns in `let` outright with
// a clean, actionable diagnostic — the user must use `match` or
// `when ... else` for multi-variant enums. Single-variant enums
// (e.g. `type Wrapper { Wrap(Int) }`) remain irrefutable and are
// allowed.

#[test]
fn test_let_refutable_variant_pattern_rejected_at_typecheck() {
    let errs = type_errors(
        r#"
type Shape { Circle(Int), Square(String) }
fn main() {
  let s = Circle(5)
  let Square(n) = s
  println(n)
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(
            "refutable pattern in `let`: constructor 'Square' is only one of 2 variants of enum 'Shape'"
        )),
        "expected refutable-pattern error for let Square(n) = circle, got: {errs:?}"
    );
}

#[test]
fn test_let_single_variant_constructor_still_accepted() {
    // Positive lock: when the enum has exactly one variant, the
    // Constructor pattern is irrefutable and must still typecheck
    // cleanly. This guards against a regression that over-rejects.
    let errs = type_errors(
        r#"
type Wrapper { Wrap(Int) }
fn main() {
  let w = Wrap(5)
  let Wrap(r) = w
  println(r + 1)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for let Wrap(r) = w, got: {errs:?}"
    );
}

#[test]
fn test_let_nested_refutable_variant_rejected() {
    // Nested: `let (a, Some(x)) = (1, opt)` — the inner Some is
    // refutable (Option has Some | None). Walk must descend into
    // the Tuple and surface the inner refutable constructor.
    let errs = type_errors(
        r#"
fn main() {
  let tup = (1, Some(42))
  let (_, Some(x)) = tup
  println(x)
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(
            "refutable pattern in `let`: constructor 'Some' is only one of 2 variants of enum 'Option'"
        )),
        "expected refutable-pattern error for nested Some(x), got: {errs:?}"
    );
}

// ── B2 (round 16): wrong-arity record type annotation silently accepted
//
// `type Box(a) { value: a }; fn takes(b: Box(Int, String)) -> Int { ... }`
// used to typecheck without any error — the extra type arg was dropped
// silently, and the first field access (`b.value + 1`) exploded at
// runtime as "cannot apply '+' to Int and String". The typechecker
// now catches the arity mismatch when resolving the annotation.

#[test]
fn test_box_type_annotation_extra_arg_rejected() {
    let errs = type_errors(
        r#"
type Box(a) { value: a }
fn takes(b: Box(Int, String)) -> Int { b.value + 1 }
fn main() { println(takes(Box { value: 5 })) }
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(
            "type argument count mismatch for record 'Box': expected 1, got 2"
        )),
        "expected arity mismatch error for Box(Int, String), got: {errs:?}"
    );
}

#[test]
fn test_box_type_annotation_correct_arity_accepted() {
    // Positive lock: `Box(Int)` with a matching argument list still
    // typechecks cleanly.
    let errs = type_errors(
        r#"
type Box(a) { value: a }
fn takes(b: Box(Int)) -> Int { b.value + 1 }
fn main() { println(takes(Box { value: 5 })) }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for Box(Int), got: {errs:?}"
    );
}

// ── B3 (round 16): exhaustiveness misses Int-column in tuple-of-(Int, Enum)
//
// `match (t: (Int, Color)) { (0, Red) -> _, (_, Green) -> _, (_, Blue) -> _ }`
// used to typecheck — the checker specialized the Int column on a
// single wildcard constructor, kept every row, and declared the Color
// column exhaustive. The `(1, Red)` case then blew up at runtime with
// `non-exhaustive match: no arm matched`. The checker now splits the
// Int column into `{literals seen} ∪ {witness not in matrix}` and
// surfaces the missing case at typecheck time.

#[test]
fn test_exhaustiveness_int_color_tuple_missing_arm_rejected() {
    let errs = type_errors(
        r#"
type Color { Red, Green, Blue }
fn classify(t: (Int, Color)) -> String {
  match t {
    (0, Red) -> "zero red"
    (_, Green) -> "green"
    (_, Blue) -> "blue"
  }
}
fn main() { println(classify((1, Red))) }
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("non-exhaustive match")),
        "expected 'non-exhaustive match' error for (Int, Color) tuple, got: {errs:?}"
    );
}

#[test]
fn test_exhaustiveness_int_color_tuple_with_wildcard_still_passes() {
    // Positive lock: adding `(_, Red) -> _` covers the missing case,
    // so the match must typecheck cleanly.
    let errs = type_errors(
        r#"
type Color { Red, Green, Blue }
fn classify(t: (Int, Color)) -> String {
  match t {
    (0, Red) -> "zero red"
    (_, Red) -> "other red"
    (_, Green) -> "green"
    (_, Blue) -> "blue"
  }
}
fn main() { println(classify((1, Red))) }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for fully covered (Int, Color) match, got: {errs:?}"
    );
}

// ── B4 (round 16): where-clause dropped when param stays an unresolved TyVar
//
// `fn indirect(x: a) -> String { show(x) }` used to typecheck even
// when `show` required `a: Display`. The call-site check applied
// `a` into a still-unresolved Var, `type_name_for_impl` returned
// None, and the constraint was silently dropped. Soundness fix:
// the enclosing function must declare the same constraint or the
// call is rejected.

#[test]
fn test_where_constraint_propagation_rejects_missing_declaration() {
    let errs = type_errors(
        r#"
trait Display {
  fn display(self) -> String
}
fn show(x: a) -> String where a: Display { x.display() }
fn indirect(x: a) -> String { show(x) }
fn main() { println(indirect(5)) }
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains(
            "enclosing function does not declare constraint required by call to 'show': `a: Display`"
        )),
        "expected constraint-propagation error, got: {errs:?}"
    );
}

#[test]
fn test_where_constraint_propagation_accepts_declared_constraint() {
    // Positive lock: when the enclosing fn declares the same
    // constraint, the indirect call typechecks.
    let errs = type_errors(
        r#"
trait Display {
  fn display(self) -> String
}
fn show(x: a) -> String where a: Display { x.display() }
fn indirect(x: a) -> String where a: Display { show(x) }
fn main() { println(indirect(5)) }
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors when enclosing fn declares the constraint, got: {errs:?}"
    );
}

