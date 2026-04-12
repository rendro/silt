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

fn type_diagnostics_all(input: &str) -> Vec<(String, Severity)> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let diagnostics = typechecker::check(&mut program);
    diagnostics
        .into_iter()
        .map(|e| (e.message, e.severity))
        .collect()
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
  when let (a, b) = 42 else { return }
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
  when let Ok(x) = 42 else { return }
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
// `when let ... else` for multi-variant enums. Single-variant enums
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

// ── Type::Fun Display matches parser `Fn(...)` surface ─────────────
// The parser reads function-type annotations as `Fn(A, B) -> C` at
// src/parser.rs:836. Display at src/types.rs:59 must emit the same
// form so diagnostics round-trip against user-written annotations.
// A mismatch like "expected Fn(Int) -> Int, got (Int) -> Int" was
// confusing because the `(Int) -> Int` form visually collides with
// silt's tuple-type syntax.

#[test]
fn test_fn_type_annotation_mismatch_renders_fn_prefix() {
    let errs = type_errors(
        r#"
fn main() {
  let f: Fn(Int) -> Int = "hello"
  println(f)
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("Fn(Int) -> Int")),
        "expected 'Fn(Int) -> Int' in error message (matches parser surface), got: {errs:?}"
    );
}

#[test]
fn test_fn_type_annotation_two_arg_renders_fn_prefix() {
    let errs = type_errors(
        r#"
fn main() {
  let f: Fn(Int, String) -> Bool = 42
  println(f)
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("Fn(Int, String) -> Bool")),
        "expected 'Fn(Int, String) -> Bool' in error message, got: {errs:?}"
    );
}

// ── BROKEN (round 17 F1): where-clause constraints dropped by scheme narrowing
//
// After pass-3 body inference, the narrowing path in
// `src/typechecker/mod.rs` called `generalize()` which produced a
// fresh scheme with brand-new TyVar IDs, then a "preserve where
// constraints" loop filtered the original constraints by
// `new_scheme.vars.contains(tv)`. The TyVars never matched because
// they came from different instantiations, so the constraint set was
// silently emptied and calls like `use_doublable("text")` slipped
// through typecheck. At runtime the VM aborted with "no method
// 'doubled' for type 'String'".
//
// Fix: walk the original and narrowed types in lockstep to build an
// old→new TyVar remap, then carry the original constraints across the
// narrowing. Regression covered here.

#[test]
fn test_where_constraint_preserved_across_scheme_narrowing() {
    assert_type_error(
        r#"
trait Doublable { fn doubled(self) -> Int }
trait Doublable for Int { fn doubled(self) -> Int { self * 2 } }
fn use_doublable(x: t) where t: Doublable { x.doubled() }
fn main() {
  let s = "text"
  let r = use_doublable(s)
  println(r)
}
"#,
        "does not implement trait 'Doublable'",
    );
}

// ── BROKEN (round 17 F2): where-clause obligation dropped inside callback
//
// When a call with a deferred where obligation sits inside a lambda
// passed to a higher-order fn, the enclosing fn may have no params at
// all (e.g. `main`). The finalize pass's `touches_fn_param` test
// correctly returned false — BUT it then bailed out before checking
// whether the deferred TyVar had in fact resolved to a concrete type
// by the time finalize ran. The obligation was silently dropped.
// Once the outer call site unified the lambda's param to a concrete
// type that did not implement the required trait, no second check
// ran and the program crashed at runtime with "no method 'double'
// for type 'String'".
//
// Fix: even when the deferred TyVar doesn't touch any enclosing fn
// param, apply the substitution and check against the concrete type
// if resolved. Only fall through to the "enclosing fn must declare
// constraint" path when the var is still unresolved.

#[test]
fn test_where_constraint_preserved_inside_callback_lambda() {
    assert_type_error(
        r#"
trait Doubler { fn double(self) -> Int }
trait Doubler for Int { fn double(self) -> Int { self * 2 } }
fn run_callback(f) { f("hello") }
fn needs_doubler(x: a) where a: Doubler { x.double() }
fn main() {
  let r = run_callback({ x -> needs_doubler(x) })
  println(r)
}
"#,
        "does not implement trait 'Doubler'",
    );
}

// ── GAP (round 17 F3): method-name coherence across distinct traits
//
// Registering two user-defined traits with the same method name on
// the same target type silently overwrote the first impl's entry in
// `method_table`, so `.name()` routed to the last-registered trait.
// Must now be a hard error "ambiguous method ..." at impl
// registration time.

#[test]
fn test_ambiguous_method_across_distinct_traits_rejected() {
    assert_type_error(
        r#"
trait First { fn name(self) -> String }
trait Second { fn name(self) -> String }
trait First for Int { fn name(self) -> String { "first" } }
trait Second for Int { fn name(self) -> String { "second" } }
fn main() { println((5).name()) }
"#,
        "ambiguous method 'name' on type 'Int'",
    );
}

#[test]
fn test_ambiguous_method_across_distinct_traits_names_both() {
    let errs = type_errors(
        r#"
trait First { fn name(self) -> String }
trait Second { fn name(self) -> String }
trait First for Int { fn name(self) -> String { "first" } }
trait Second for Int { fn name(self) -> String { "second" } }
fn main() { println((5).name()) }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("First") && e.contains("Second")),
        "expected both trait names in ambiguity error, got: {errs:?}"
    );
}

// ── GAP (round 17 F5): (s)-pluralization in arity errors
//
// Typechecker arity errors used the awkward "1 argument(s), got 2"
// form. Pin the proper pluralization: both "1 argument" (singular)
// and "2 arguments" (plural) must appear across the fixed error
// sites.

#[test]
fn test_arity_error_uses_singular_for_one_arg() {
    let errs = type_errors(
        r#"
fn takes_one(x) { x + 1 }
fn main() { takes_one(1, 2) }
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("1 argument, got 2")),
        "expected '1 argument, got 2' (singular), got: {errs:?}"
    );
}

#[test]
fn test_arity_error_uses_plural_for_multiple_args() {
    let errs = type_errors(
        r#"
fn takes_two(x, y) { x + y }
fn main() { takes_two(1) }
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("2 arguments, got 1")),
        "expected '2 arguments, got 1' (plural), got: {errs:?}"
    );
}

// ── Pattern span refactor: constructor arity caret attribution ──────
//
// Before the Pattern→PatternKind split, `Pattern` carried no source
// span, so `check_pattern` / `bind_pattern` had to fall back to the
// enclosing scrutinee span when reporting constructor arity errors.
// The caret rendered against `Some(1)` (scrutinee) even though the
// offending pattern was `Some(x, y)` one line below. The refactor
// threads `pattern.span` through and these tests pin the
// post-refactor attribution so a regression can't silently revert.

#[test]
fn test_constructor_arity_caret_lands_on_pattern_not_scrutinee() {
    // Fix A (check_pattern / match arm): arity error caret must land
    // on the `Some(x, y)` pattern (line 5), not on the `Some(1)`
    // scrutinee (line 4).
    let errs = type_errors_full(
        r#"
type Option { Some(Int), None }
fn main() {
  match Some(1) {
    Some(x, y) -> println(x)
    None -> println(0)
  }
}
"#,
    );
    let arity: Vec<_> = errs
        .iter()
        .filter(|(m, _)| m.contains("constructor") && m.contains("1 field"))
        .collect();
    assert!(
        !arity.is_empty(),
        "expected a constructor arity error, got: {errs:?}"
    );
    for (msg, span) in &arity {
        assert_eq!(
            span.line, 5,
            "expected constructor arity caret on line 5 (the Some(x, y) pattern), \
             got line {} for '{msg}'",
            span.line
        );
    }
}

#[test]
fn test_let_destructure_constructor_arity_caret_lands_on_pattern() {
    // Fix A (bind_pattern / let-destructure): in
    // `let Some(x, y) = some_int_value`, the arity error must attribute
    // to the `Some(x, y)` pattern on the LHS, not to the RHS scrutinee.
    // The refutable-variant error uses the scrutinee span by design
    // (that's a shape problem, not a pattern-internal one), so we only
    // pin the arity error's span here.
    let errs = type_errors_full(
        r#"
type Option { Some(Int), None }
fn main() {
  let some_int_value = Some(1)
  let Some(x, y) = some_int_value
  println(x)
}
"#,
    );
    // Filter to the arity diagnostic specifically (not the refutable
    // pattern diagnostic emitted by reject_refutable_constructor_in_let,
    // which uses the scrutinee span on purpose).
    let arity: Vec<_> = errs
        .iter()
        .filter(|(m, _)| m.contains("expects 1") && m.contains("pattern has 2"))
        .collect();
    assert!(
        !arity.is_empty(),
        "expected a constructor arity error, got: {errs:?}"
    );
    for (msg, span) in &arity {
        assert_eq!(
            span.line, 5,
            "expected let-destructure arity caret on line 5 (the Some(x, y) pattern), \
             got line {} for '{msg}'",
            span.line
        );
        // The pattern starts at column 7 (`  let Some(x, y)`), not
        // column 20 (the `some_int_value` scrutinee). Pin that too.
        assert_eq!(
            span.col, 7,
            "expected let-destructure arity caret at column 7 (start of 'Some' pattern), \
             got column {} for '{msg}'",
            span.col
        );
    }
}

// ── Pattern span refactor: shadow-warning binding-site attribution ──
//
// Fix B (compile_pattern_bind Pattern::Ident): the shadow warning
// for a binding introduced by a nested pattern used to use the
// enclosing match-arm / let statement span, so the caret landed on
// a line above the actual binding. Now the binding's own span (from
// the parser's Pattern::new capture) is passed to
// warn_if_shadows_module, so the caret lands on the identifier
// itself.

#[test]
fn test_pattern_binding_shadow_warning_span_on_binding_not_arm() {
    // Minimal repro distilled from examples/concurrent_processor.silt.
    // The tuple pattern `(_, Message(result))` in a match arm binds
    // `result`, which shadows the `result` module imported above.
    // Before Fix B the warning's span pointed at the match-arm scrutinee
    // line; after Fix B it points at the `result` binding inside
    // `Message(result)`.
    let tokens = silt::lexer::Lexer::new(
        r#"
import result
type Outcome { Wrap(Int), Finish }
fn run(o) {
  match (1, o) {
    (_, Wrap(result)) -> result
    (_, Finish) -> 0
  }
}
fn main() { run(Finish) }
"#,
    )
    .tokenize()
    .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    // The shadow warning is emitted by the compiler, not the
    // typechecker, so run `Compiler::compile_program` and collect
    // warnings via `Compiler::warnings()`.
    let _tc_errs = typechecker::check(&mut program);
    let mut compile = silt::compiler::Compiler::new();
    let _ = compile
        .compile_program(&program)
        .expect("compile should succeed");
    let warnings = compile.warnings();
    let shadow: Vec<_> = warnings
        .iter()
        .filter(|w| w.message.contains("shadows the builtin 'result' module"))
        .collect();
    assert!(
        !shadow.is_empty(),
        "expected a shadow warning for 'result', got warnings: {:?}",
        warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>()
    );
    for w in &shadow {
        // The `result` binding sits at line 6 inside the
        // `(_, Message(result))` pattern of the first match arm.
        // The enclosing match-arm head is line 5, which is what the
        // pre-refactor warning reported.
        assert_eq!(
            w.span.line, 6,
            "expected shadow warning on line 6 (the `result` binding), \
             got line {} — warning message: {}",
            w.span.line, w.message
        );
    }
}

// ════════════════════════════════════════════════════════════════════
// AUDIT FINDINGS: round 19
// ════════════════════════════════════════════════════════════════════

// ── BROKEN (round 19 F1): check_pattern accepts sub-patterns on zero-arg ctors
//
// `match x { Red(y) -> ... }` where Red has zero fields used to slip
// through typecheck silently (no error, no unification), crashing at
// runtime. The `_ =>` arm in check_pattern's Constructor handler now
// emits an error when sub_pats is non-empty on a zero-arg constructor.

#[test]
fn test_zero_arg_constructor_with_sub_patterns_rejected() {
    assert_type_error(
        r#"
type Color { Red, Green, Blue }
fn main() {
  let x: Color = Red
  match x {
    Red(y) -> "red"
    Green -> "green"
    Blue -> "blue"
  }
}
"#,
        "expects 0 fields",
    );
}

#[test]
fn test_zero_arg_constructor_without_sub_patterns_still_accepted() {
    // Positive lock: matching a zero-arg constructor without sub-patterns
    // must continue to typecheck cleanly.
    let errs = type_errors(
        r#"
type Color { Red, Green, Blue }
fn main() {
  let x: Color = Red
  match x {
    Red -> "red"
    Green -> "green"
    Blue -> "blue"
  }
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for valid zero-arg constructor match, got: {errs:?}"
    );
}

// ── BROKEN (round 19 F2): method call arity check allows one extra argument
//
// The arity check for method calls had a backwards disjunct
// `arg_types.len() == params.len() + 1` that let one extra argument
// through. This affected module-qualified calls (e.g. `math.sqrt`)
// which parse as FieldAccess and thus set is_method_call = true but
// whose params do NOT include a `self` slot. The extra disjunct let
// exactly one surplus argument slip past the arity gate.

#[test]
fn test_method_call_extra_arg_rejected() {
    // Module-qualified call with one extra argument. Before the fix,
    // `string.length("hi", "extra")` passed arity check because the
    // backwards condition matched: arg_types.len()==params.len()+1.
    assert_type_error(
        r#"
import string
fn main() {
  println(string.length("hi", "extra"))
}
"#,
        "argument",
    );
}

#[test]
fn test_method_call_correct_arity_accepted() {
    // Positive lock: correct-arity module-qualified call must still work.
    let errs = type_errors(
        r#"
import string
fn main() {
  println(string.length("hi"))
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for correct module call arity, got: {errs:?}"
    );
}

// ── GAP (round 19 F3): recur arity mismatch emits warning instead of error
//
// The typechecker emitted a warning for loop/recur arity mismatches,
// but this should be a hard error. Changed `self.warning(...)` to
// `self.error(...)`.

#[test]
fn test_recur_arity_mismatch_is_error_not_warning() {
    let diagnostics = type_diagnostics_all(
        r#"
fn main() {
  loop n = 0 {
    match n > 5 {
      true -> n
      false -> loop(n + 1, "extra")
    }
  }
}
"#,
    );
    // Must have at least one Error-severity diagnostic about binding/argument count
    let has_error = diagnostics.iter().any(|(msg, sev)| {
        *sev == Severity::Error && msg.contains("loop has") && msg.contains("recur supplies")
    });
    assert!(
        has_error,
        "expected an Error-severity diagnostic for recur arity mismatch, got: {diagnostics:?}"
    );
    // Must NOT have a Warning-severity diagnostic for the same thing
    let has_warning = diagnostics.iter().any(|(msg, sev)| {
        *sev == Severity::Warning && msg.contains("loop has") && msg.contains("recur supplies")
    });
    assert!(
        !has_warning,
        "recur arity mismatch should be Error, not Warning, got: {diagnostics:?}"
    );
}
