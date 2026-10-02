//! End-to-end tests for the `bytes` builtin module (v0.9 PR 1).
//!
//! Critical invariants locked here:
//! - Structural equality (two `from_string("x")` calls produce equal Bytes)
//! - Hash consistency with equality (Bytes works as Map/Set key)
//! - All 14 functions cover their happy path + key error cases
//! - Forward-compat: behavior here will not change when Bytes is later
//!   promoted to a language-level `Type::Bytes`.
//!
//! The rest of the module's behaviour is covered by the golden cases in
//! tests/golden/lang/stdlib/bytes_module__*.silt. The map-key test stays
//! here because `silt check` rejects Bytes as a map key (no `Hash` impl),
//! so it can only run through the typechecker-ignoring runner below.

use std::sync::Arc;

use silt::value::Value;

fn run(input: &str) -> Value {
    let tokens = silt::lexer::Lexer::new(silt::source::FileId::default(), input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens, input)
        .parse_program()
        .expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    vm.run(script).expect("runtime error")
}

#[test]
fn test_bytes_works_as_map_key() {
    // Hash + Eq consistency: two equal Bytes values used as Map keys must
    // collapse to one entry. This is the invariant that protects BTreeMap
    // / BTreeSet correctness.
    let v = run(r#"
import bytes
fn main() {
  let m = #{
    bytes.from_string("a"): 1,
    bytes.from_string("a"): 2,
    bytes.from_string("b"): 3,
  }
  -- Map literal evaluation order: later entries overwrite earlier;
  -- the "a" entry should be a single slot now holding 2.
  m
}
"#);
    let Value::Map(m) = v else {
        panic!("expected Map, got {v:?}")
    };
    assert_eq!(m.len(), 2, "duplicate Bytes keys must collapse");
}
