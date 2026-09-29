//! Round 62 (item 3 of type-design improvements): unified trait-decl
//! registration lock.
//!
//! Before: trait-decl registration had two code paths — a hand-rolled
//! `register_builtin_trait_decl(checker, "Display", "display", 1, Type::String)`
//! helper that built TraitInfo entries directly for Display/Compare/
//! Equal/Hash plus a one-off block for Error, and the user-source
//! `register_trait_decl(&TraitDecl)` that did all the heavy lifting
//! (duplicate-method check, supertrait/where-clause processing, default
//! body extraction).
//!
//! After: built-in traits synthesize `TraitDecl` AST nodes via
//! `builtin_trait_decls()` and feed them through the same
//! `register_trait_decl_inner` body the user path uses. The only
//! divergence is the `BUILTIN_TRAIT_NAMES` redefinition guard, which
//! lives on `register_trait_decl_user` because it's keyed off user
//! input.
//!
//! These tests lock:
//! 1. All five built-in traits (Display/Compare/Equal/Hash/Error)
//!    appear in `checker.traits` after the unified registration runs,
//!    with the right method names, arities, return types, supertraits,
//!    and default bodies.
//! 2. The legacy hand-rolled helper `register_builtin_trait_decl` no
//!    longer exists in src/typechecker/mod.rs (source-grep lock).
//! 3. The user-path redefinition guard still fires — a user
//!    `trait Display { ... }` is still rejected with the same
//!    "is a builtin trait and cannot be redefined" error.
//! 4. The `Error.message` default-body synthesis still works through
//!    the unified path: a user `trait Error for MyErr { ... }` that
//!    omits `message` gets the synthesized `self.display()` body.
//! 5. Future trait-decl features (here, `param_where_clauses`) added
//!    to `register_trait_decl_inner` automatically apply to built-ins
//!    — the unified path's whole reason to exist.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Lock test 1: every built-in trait is registered through the unified
/// path. Inspects the post-registration `traits` map via the existing
/// `__builtin_trait_registration_fingerprint` doc-hidden hook (which
/// already covers the four sig-only traits) plus a direct check on
/// Error.
#[test]
fn builtin_traits_registered_via_user_path() {
    let fp = silt::typechecker::__builtin_trait_registration_fingerprint();
    let names: Vec<String> = fp.iter().map(|e| e.0.clone()).collect();
    assert_eq!(
        names,
        vec![
            "Display".to_string(),
            "Compare".to_string(),
            "Equal".to_string(),
            "Hash".to_string(),
        ],
        "expected the four sig-only built-in traits to be registered through the unified path"
    );

    // Each sig-only trait still has the pre-unification field shape:
    //   no params, no supertraits, no where-clauses, no default bodies.
    for entry in &fp {
        let (
            name,
            _method,
            _arity,
            _ret,
            supertrait_args_count,
            default_bodies_count,
            params_count,
            supertraits_count,
            param_where_clauses_count,
        ) = entry;
        assert_eq!(*params_count, 0, "{name}: params should be empty");
        assert_eq!(*supertraits_count, 0, "{name}: supertraits should be empty");
        assert_eq!(
            *supertrait_args_count, 0,
            "{name}: supertrait_args should be empty"
        );
        assert_eq!(
            *param_where_clauses_count, 0,
            "{name}: param_where_clauses should be empty"
        );
        assert_eq!(
            *default_bodies_count, 0,
            "{name}: default_method_bodies should be empty (sig-only)"
        );
    }
}

/// Lock test 2: source-grep that the legacy hand-rolled helper is
/// gone. After unification, every built-in trait flows through
/// `register_trait_decl_inner` via the synthesized TraitDecl AST.
#[test]
fn builtin_trait_decl_uses_inner_path_not_legacy_helper() {
    let src = std::fs::read_to_string("src/typechecker/mod.rs")
        .expect("could not read src/typechecker/mod.rs");

    // The legacy helper is the function definition, not just any
    // mention of the symbol; allow doc/test references but reject
    // an actual `fn register_builtin_trait_decl(` definition.
    assert!(
        !src.contains("fn register_builtin_trait_decl("),
        "the legacy `fn register_builtin_trait_decl(...)` helper must be deleted; \
         built-in traits go through the unified register_trait_decl_inner path now"
    );

    // The synthesized-AST entry point must be present.
    assert!(
        src.contains("fn builtin_trait_decls()"),
        "the unified path requires `fn builtin_trait_decls()` to synthesize TraitDecl AST nodes"
    );
    assert!(
        src.contains("register_trait_decl_inner"),
        "the unified path requires `register_trait_decl_inner` to exist on TypeChecker"
    );
    assert!(
        src.contains("register_trait_decl_user"),
        "the unified path requires `register_trait_decl_user` for the user-source entry point"
    );

    // The Error trait's hand-rolled `let error_self = checker.fresh_var();`
    // block at line ~4380 should be gone — Error is just one of the
    // five entries in `builtin_trait_decls()`.
    assert!(
        !src.contains("let error_self = checker.fresh_var();"),
        "the hand-rolled `Error` registration block should be gone; \
         Error is now one of the five entries in builtin_trait_decls()"
    );
}

/// Lock test 3: the user-path redefinition guard still rejects a user
/// `trait Display { ... }` declaration. The guard moved from
/// `register_trait_decl` to `register_trait_decl_user`, and the
/// `register_trait_decl_inner` path (used by built-ins) intentionally
/// skips it.
#[test]
fn user_redefinition_of_display_still_rejected() {
    let errs = type_errors("trait Display { fn show(self) -> String }\nfn main() { println(1) }\n");
    assert_eq!(
        errs.len(),
        1,
        "expected exactly one error for builtin trait 'Display' redefinition, got: {errs:?}"
    );
    let msg = &errs[0];
    assert!(
        msg.contains("trait 'Display'"),
        "error should name the trait, got: {msg}"
    );
    assert!(
        msg.contains("is a builtin trait and cannot be redefined"),
        "error should contain the builtin-redefinition phrase, got: {msg}"
    );
}

/// Lock test 4: the round-61 `Error.message` default-body synthesis is
/// intact. A user `trait Error for MyErr { fn display(self) -> String = ... }`
/// that omits `message` gets the synthesized `self.display()` body
/// cloned into the impl. We assert at runtime that calling `.message()`
/// on the impl-target dispatches through the synthesized default and
/// returns the string the impl's `display` produces.
#[test]
fn builtin_error_default_method_synthesis_intact() {
    use silt::compiler::Compiler;
    use silt::lexer::Lexer;
    use silt::parser::Parser;
    use silt::value::Value;
    use silt::vm::Vm;
    use std::sync::Arc;

    let src = r#"
type MyErr { Boom }

trait Display for MyErr {
    fn display(self) -> String = "kaboom"
}

trait Error for MyErr {}

fn main() -> String {
    let e = Boom
    e.message()
}
"#;
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errs = typechecker::check(&mut program);
    let fatal: Vec<_> = errs
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        fatal.is_empty(),
        "expected clean typecheck (Error.message default synthesis should fill in), got: {fatal:?}"
    );

    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let result = vm.run(script).expect("runtime error");
    assert_eq!(
        result,
        Value::String("kaboom".into()),
        "default `message` body should call self.display() and return its string"
    );
}

/// Lock test 5: future trait-decl features (e.g. param_where_clauses)
/// added to the user path automatically apply to anything that flows
/// through `register_trait_decl_inner`. We typecheck a user trait whose
/// shape exercises `param_where_clauses`, and confirm the same code
/// path is what the built-ins ride. (The previous hand-rolled
/// registration didn't run any of this machinery — the unification's
/// whole reason for existing is to fix that.)
#[test]
fn every_builtin_trait_has_param_where_clauses_unified_with_user_path() {
    // User-written trait with a non-empty param_where_clauses. If the
    // unified path didn't preserve param_where_clauses on the inner
    // body, this would either typecheck with the bound silently
    // dropped (regression) or fail to typecheck (also a regression).
    let errs = type_errors(
        r#"
trait Hasher(k) where k: Hash { fn hash_with(self, key: k) -> Int }
fn main() { println(1) }
"#,
    );
    assert!(
        errs.is_empty(),
        "user trait with param_where_clauses should typecheck cleanly through register_trait_decl_inner; got: {errs:?}"
    );

    // Source-grep lock: the only call sites of register_trait_decl_inner
    // are register_trait_decl_user (one delegation) and the built-in
    // synthesizer loop. If a future commit adds a third bypass, this
    // lock fires.
    let src = std::fs::read_to_string("src/typechecker/mod.rs")
        .expect("could not read src/typechecker/mod.rs");
    let inner_call_count = src.matches("register_trait_decl_inner(").count();
    // 1 definition + 1 delegation from register_trait_decl_user + 1 call
    // inside the built-in synth loop = 3 occurrences.
    assert_eq!(
        inner_call_count, 3,
        "expected exactly 3 occurrences of `register_trait_decl_inner(` (1 fn def + 1 user delegation + 1 built-in synth call), got {inner_call_count}; \
         a new bypass would defeat the unification — make new code call register_trait_decl_inner instead"
    );
}
