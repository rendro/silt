//! Round-26 B3 regression test.
//!
//! Before the fix, a user trait declaration that shadowed one of the
//! builtin trait names (`Equal`, `Compare`, `Hash`, `Display`) would
//! overwrite the compiler's preregistered `TraitInfo` entry. The new
//! body's method name (e.g. `eq`) then mismatched the auto-derived
//! method name (`equal`) already inserted into `method_table` for every
//! primitive / builtin container, causing `validate_trait_impls` to
//! iterate the preregistered `trait_impl_set` and emit 15+ cascade
//! diagnostics of the form `trait impl 'Equal' for 'Int' is missing
//! method 'eq'` — every one with `dummy_span` (no caret).
//!
//! The fix rejects redeclaration of any builtin trait name outright in
//! `register_trait_decl`, emitting a single clean diagnostic at the
//! user's `trait` keyword span. This test locks in both:
//!   - Each of the four builtin traits produces exactly one error
//!     containing `is a builtin trait and cannot be redefined`, and no
//!     `missing method` cascade appears.
//!   - Non-builtin trait names (e.g. `MyEqual`) continue to register
//!     cleanly.
//!   - The preregistered builtin impls themselves still work: `1 == 1`
//!     typechecks, compiles, and evaluates to `true`.

use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;

/// Typecheck-only: collect hard-error messages.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Typecheck + compile + run to a Value.
fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errs = typechecker::check(&mut program);
    let fatal: Vec<_> = errs
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(fatal.is_empty(), "type errors: {fatal:?}");
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

/// Asserts the single-diagnostic shape for a user redeclaration of
/// builtin trait `name`. Exactly one error is produced, it mentions the
/// trait by name + the "builtin trait" phrase, and no "missing method"
/// cascade leaks through.
fn assert_builtin_redef_rejected(name: &str) {
    let src = format!(
        "trait {name} {{ fn foo(self) -> Bool }}\nfn main() {{ println(1) }}\n"
    );
    let errs = type_errors(&src);
    assert_eq!(
        errs.len(),
        1,
        "expected exactly one error for builtin trait '{name}' redefinition, got {} errors: {errs:?}",
        errs.len()
    );
    let msg = &errs[0];
    assert!(
        msg.contains(&format!("trait '{name}'")),
        "error should name the trait, got: {msg}"
    );
    assert!(
        msg.contains("is a builtin trait and cannot be redefined"),
        "error should contain the builtin-redefinition phrase, got: {msg}"
    );
    // Critical: the old bug produced a flood of "missing method" errors.
    // Verify none of those leak through.
    for e in &errs {
        assert!(
            !e.contains("missing method"),
            "no 'missing method' cascade should appear, found: {e}"
        );
    }
}

#[test]
fn test_user_cannot_redefine_builtin_trait_equal() {
    assert_builtin_redef_rejected("Equal");
}

#[test]
fn test_user_cannot_redefine_builtin_trait_compare() {
    assert_builtin_redef_rejected("Compare");
}

#[test]
fn test_user_cannot_redefine_builtin_trait_hash() {
    assert_builtin_redef_rejected("Hash");
}

#[test]
fn test_user_cannot_redefine_builtin_trait_display() {
    assert_builtin_redef_rejected("Display");
}

/// Control: declaring a trait whose name does NOT collide with a
/// builtin — even if its method is called `eq` — must typecheck clean.
/// This confirms the guard is keyed on the trait NAME, not the method
/// name.
#[test]
fn test_non_builtin_trait_name_typechecks_clean() {
    let errs = type_errors(
        r#"
trait MyEqual { fn eq(self) -> Bool }
fn main() { println(1) }
"#,
    );
    assert!(
        errs.is_empty(),
        "non-builtin trait name should typecheck clean, got: {errs:?}"
    );
}

/// Control: the preregistered builtin `Equal` impl on `Int` must still
/// work — `1 == 1` typechecks, compiles, runs, and returns `true`. If
/// the fix accidentally wiped out the auto-derived impls, this would
/// regress.
#[test]
fn test_builtin_equal_on_int_still_works() {
    let v = run("fn main() -> Bool { 1 == 1 }");
    assert_eq!(v, Value::Bool(true));
}
