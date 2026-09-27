//! Execution-site locks for the container-of-`Fn` Compare/Equal/Hash gate.
//!
//! ## Background
//!
//! Round 97 (tests/round97_container_fn_compare_tests.rs) made the
//! CONCRETE forms a compile error: `[fn(x) { x }] < [fn(x) { x }]` is
//! rejected by `operand_builtin_trait_violation`, which recurses into
//! container element types. But silt does NOT statically enforce
//! inferred trait bounds on polymorphic templates —
//! `pending_numeric_checks` (src/typechecker/inference.rs) skips
//! operands whose type is still a `Var`, on the documented promise that
//! "the VM catches it at runtime". The call site instantiates fresh
//! vars, so a polymorphic wrapper launders a container of functions
//! straight past the round-97 gate:
//!
//! ```text
//! fn lt(a: x, b: x) -> Bool { a < b }
//! lt([fn(y) { y }], [fn(y) { y }])   // reached the VM pre-fix
//! ```
//!
//! Pre-fix, the `(List, List)` arm of `compare()`
//! (src/vm/arithmetic.rs) deferred to `Value::cmp` with no element
//! gate, and `Value::cmp` orders `VmClosure` by `Arc::as_ptr`
//! (src/value.rs) — the printed Bool depended on heap allocation order,
//! nondeterministic across runs. The equality twin (`fn eq(a: x, b: x)
//! -> Bool { a == b }`) silently returned an `Arc::ptr_eq`-based Bool
//! because `equality_operand_violation` (src/vm/execute.rs) rejected
//! only TOP-LEVEL function shapes, and the `"equal"` / `"compare"`
//! trait-method arms of `dispatch_trait_method` (src/vm/dispatch.rs)
//! had the same hole.
//!
//! The fix adds `Vm::value_contains_fn` (src/vm/mod.rs) — a value
//! walker that detects function-shaped leaves (VmClosure / BuiltinFn /
//! VariantConstructor) inside List / Tuple / Map / Set / Record /
//! Variant — and consults it from all three surfaces. Every rejection
//! test below FAILS on the pre-fix code (the program ran and produced a
//! pointer-derived Bool/Int) and PASSES post-fix; the positive controls
//! guard against over-rejection.
//!
//! ## Hash follow-up
//!
//! `hash()` was the ungated sibling: the `(Hash, List)` auto-derive
//! stamp is registered unconditionally
//! (`register_auto_derived_impls_for`, src/typechecker/mod.rs) without
//! walking element types, so `[fn(y) { y }].hash()` typechecked AND ran
//! — the `"hash"` arm of `dispatch_trait_method` (src/vm/dispatch.rs)
//! admitted List/Tuple receivers with no `value_contains_fn` backstop,
//! and `impl Hash for Value` (src/value.rs) hashes every closure as a
//! constant tag, so two distinct closures hashed identically
//! (`h([fn(y){y}]) == h([fn(z){z * 99}])` printed `true`). That
//! contradicts the policy source (`gate_field_supports_trait`,
//! src/typechecker/mod.rs: "Functions support none of
//! Equal/Compare/Hash"). The `"hash"` arm now carries the same
//! receiver gate as its `"equal"` / `"compare"` siblings; locked by
//! the Hash section below.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

// ── Runtime harness: typecheck is intentionally non-fatal (the
//    polymorphic programs typecheck clean and the behavior under test is
//    at the VM execution site), matching
//    tests/round96_eq_fn_runtime_tests.rs. ──────────────────────────────

fn run(input: &str) -> Result<Value, String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).map_err(|e| e.message)
}

/// Assert `src` errors at runtime with a message naming both `trait_name`
/// and `'Fn'` — the execution-site gate's canonical diagnostic.
fn assert_runtime_fn_gate(label: &str, src: &str, trait_name: &str) {
    let err = match run(src) {
        Ok(v) => {
            panic!("{label}: expected a runtime {trait_name} error, got a clean result: {v:?}")
        }
        Err(e) => e,
    };
    assert!(
        err.contains(&format!("does not implement {trait_name}")) && err.contains("'Fn'"),
        "{label}: expected a runtime {trait_name} error naming Fn, got: {err}"
    );
}

// ── Ordering: `<` on containers of functions errors at the exec site ─────

#[test]
fn polymorphic_lt_of_fn_lists_errors_at_runtime() {
    // The finding's exact repro. Pre-fix this printed an
    // ASLR-nondeterministic Bool and exited 0.
    let src = r#"
fn lt(a: x, b: x) -> Bool { a < b }
fn main() {
  let xs = [fn(y) { y }]
  let ys = [fn(y) { y }]
  println(lt(xs, ys))
}
"#;
    assert_runtime_fn_gate("lt_fn_lists", src, "Compare");
}

#[test]
fn polymorphic_lt_of_fn_field_records_errors_at_runtime() {
    // Generic-record laundering: `Box(Fn)` values are constructible even
    // though the concrete `==`/`<` on them is a compile error (round 93).
    let src = r#"
type Box(a) { v: a }
fn lt(a: x, b: x) -> Bool { a < b }
fn main() {
  println(lt(Box{v: fn(y) { y }}, Box{v: fn(y) { y }}))
}
"#;
    assert_runtime_fn_gate("lt_fn_records", src, "Compare");
}

#[test]
fn polymorphic_lt_of_fn_payload_variants_errors_at_runtime() {
    let src = r#"
type W(a) { Wrap(a), Empty }
fn lt(a: x, b: x) -> Bool { a < b }
fn main() {
  println(lt(Wrap(fn(y) { y }), Wrap(fn(y) { y })))
}
"#;
    assert_runtime_fn_gate("lt_fn_variants", src, "Compare");
}

// ── Equality: `==` / `!=` on containers of functions error too ───────────

#[test]
fn polymorphic_eq_of_fn_lists_errors_at_runtime() {
    // Pre-fix this silently returned an `Arc::ptr_eq`-based Bool.
    let src = r#"
fn eq(a: x, b: x) -> Bool { a == b }
fn main() {
  let xs = [fn(y) { y }]
  let ys = [fn(y) { y }]
  println(eq(xs, ys))
}
"#;
    assert_runtime_fn_gate("eq_fn_lists", src, "Equal");
}

#[test]
fn polymorphic_neq_of_fn_tuples_errors_at_runtime() {
    let src = r#"
fn neq(a: x, b: x) -> Bool { a != b }
fn main() {
  let a = (1, fn(y) { y })
  let b = (1, fn(y) { y })
  println(neq(a, b))
}
"#;
    assert_runtime_fn_gate("neq_fn_tuples", src, "Equal");
}

#[test]
fn polymorphic_eq_gates_the_right_operand_too() {
    // A shared discriminant no longer implies a shared violation: the
    // empty list carries no function leaf, so the gate must also walk
    // the right-hand operand.
    let src = r#"
fn eq(a: x, b: x) -> Bool { a == b }
fn main() {
  println(eq([], [fn(y) { y }]))
}
"#;
    assert_runtime_fn_gate("eq_empty_vs_fn_list", src, "Equal");
}

// ── Trait-method surface: `.compare()` / `.equal()` laundering ───────────

#[test]
fn polymorphic_compare_method_on_fn_lists_errors_at_runtime() {
    // List receivers resolve in `dispatch_trait_method`'s `"compare"`
    // arm (no synth global for builtin List) — pre-fix it deferred to
    // `Value::cmp` and returned a pointer-derived Int.
    let src = r#"
fn cmpm(a: t, b: t) -> Int where t: Compare { a.compare(b) }
fn main() {
  println(cmpm([fn(y) { y }], [fn(y) { y }]))
}
"#;
    assert_runtime_fn_gate("compare_method_fn_lists", src, "Compare");
}

#[test]
fn polymorphic_equal_method_on_fn_lists_errors_at_runtime() {
    let src = r#"
fn eqm(a: t, b: t) -> Bool where t: Equal { a.equal(b) }
fn main() {
  println(eqm([fn(y) { y }], [fn(y) { y }]))
}
"#;
    assert_runtime_fn_gate("equal_method_fn_lists", src, "Equal");
}

// ── Positive controls: the accepted set is untouched ─────────────────────

#[test]
fn polymorphic_lt_of_int_lists_still_works() {
    let src = r#"
fn lt(a: x, b: x) -> Bool { a < b }
fn main() -> Bool { lt([1, 2, 3], [4, 5, 6]) }
"#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b, "[1,2,3] < [4,5,6] must be true"),
        Ok(other) => panic!("expected Bool, got {other:?}"),
        Err(e) => panic!("int-list ordering must not error, got: {e}"),
    }
}

#[test]
fn polymorphic_eq_of_int_lists_still_works() {
    let src = r#"
fn eq(a: x, b: x) -> Bool { a == b }
fn main() -> Bool { eq([1, 2, 3], [1, 2, 3]) }
"#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b, "[1,2,3] == [1,2,3] must be true"),
        Ok(other) => panic!("expected Bool, got {other:?}"),
        Err(e) => panic!("int-list equality must not error, got: {e}"),
    }
}

#[test]
fn polymorphic_lt_of_derivable_records_still_works() {
    // Exercises the gated Record arm of `compare()` positively: the
    // walker finds no function leaf and defers to `Value::cmp`.
    let src = r#"
type P { x: Int, name: String }
fn lt(a: t, b: t) -> Bool { a < b }
fn main() -> Bool { lt(P{x: 1, name: "a"}, P{x: 2, name: "b"}) }
"#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b, "P{{1,a}} < P{{2,b}} must be true"),
        Ok(other) => panic!("expected Bool, got {other:?}"),
        Err(e) => panic!("derivable record ordering must not error, got: {e}"),
    }
}

#[test]
fn polymorphic_lt_of_derivable_variants_still_works() {
    // Exercises the gated Variant arm of `compare()` positively
    // (declaration-order comparison, round 61).
    let src = r#"
type Shape { Dot, Line(Int) }
fn lt(a: t, b: t) -> Bool { a < b }
fn main() -> Bool { lt(Dot, Line(1)) }
"#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b, "Dot < Line(1) must be true"),
        Ok(other) => panic!("expected Bool, got {other:?}"),
        Err(e) => panic!("derivable variant ordering must not error, got: {e}"),
    }
}

#[test]
fn compare_method_on_int_lists_still_works() {
    // Exercises the gated `"compare"` dispatch arm positively.
    let src = r#"
fn cmpm(a: t, b: t) -> Int where t: Compare { a.compare(b) }
fn main() -> Int { cmpm([1, 2], [3, 4]) }
"#;
    match run(src) {
        Ok(Value::Int(n)) => assert_eq!(n, -1, "[1,2].compare([3,4]) must be -1"),
        Ok(other) => panic!("expected Int, got {other:?}"),
        Err(e) => panic!("int-list compare() must not error, got: {e}"),
    }
}

// ── Hash: `.hash()` on containers of functions errors at the exec site ───

#[test]
fn polymorphic_hash_of_fn_lists_errors_at_runtime() {
    // The finding's exact repro shape. Pre-fix `h([fn(y){y}])` and
    // `h([fn(z){z * 99}])` both returned the SAME contentless Int
    // (every closure hashes as a constant tag in `impl Hash for
    // Value`), so this ran clean and the two hashes compared equal.
    let src = r#"
fn h(a: x) -> Int where x: Hash { a.hash() }
fn main() {
  println(h([fn(y) { y }]) == h([fn(z) { z * 99 }]))
}
"#;
    assert_runtime_fn_gate("hash_fn_lists", src, "Hash");
}

#[test]
fn polymorphic_hash_of_fn_tuples_errors_at_runtime() {
    let src = r#"
fn h(a: x) -> Int where x: Hash { a.hash() }
fn main() {
  println(h((fn(y) { y }, 1)) == h((fn(z) { z + 5 }, 1)))
}
"#;
    assert_runtime_fn_gate("hash_fn_tuples", src, "Hash");
}

#[test]
fn concrete_hash_of_fn_list_errors_at_runtime() {
    // Unlike Equal/Compare, the CONCRETE form also slips the static
    // layer (the `(Hash, List)` auto-derive stamp never walks element
    // types), so the runtime gate is the load-bearing rejection here.
    // The harness's non-fatal typecheck keeps this lock valid even if
    // a future static gate starts rejecting it earlier.
    let src = r#"
fn main() {
  let xs = [fn(y) { y }]
  println(xs.hash())
}
"#;
    assert_runtime_fn_gate("hash_fn_list_concrete", src, "Hash");
}

// ── Hash positive controls: fn-free receivers still hash ─────────────────

#[test]
fn polymorphic_hash_of_int_lists_still_works() {
    // Same-content lists must agree; hashing must not error.
    let src = r#"
fn h(a: x) -> Int where x: Hash { a.hash() }
fn main() -> Bool { h([1, 2, 3]) == h([1, 2, 3]) }
"#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b, "equal int lists must hash equal"),
        Ok(other) => panic!("expected Bool, got {other:?}"),
        Err(e) => panic!("int-list hash() must not error, got: {e}"),
    }
}

#[test]
fn concrete_hash_of_tuple_and_string_still_works() {
    // Exercises the gated `"hash"` dispatch arm positively for the
    // Tuple and String receivers named in the finding's controls.
    let src = r#"
fn main() -> Bool { (1, "a").hash() == (1, "a").hash() && "abc".hash() == "abc".hash() }
"#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b, "equal tuples/strings must hash equal"),
        Ok(other) => panic!("expected Bool, got {other:?}"),
        Err(e) => panic!("tuple/string hash() must not error, got: {e}"),
    }
}
