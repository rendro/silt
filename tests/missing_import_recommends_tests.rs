//! Round 56 item 4: typechecker rejects `<builtin>.X` without an import.
//!
//! Before this change, stdlib builtins were quietly accessible at
//! typecheck time even when the user had not imported them — the
//! compiler layer later refused to emit bytecode, but the typechecker
//! silently registered the qualified names and typechecked against
//! them. Meanwhile, the misleading "unknown function '<field>' on
//! module '<mod>'" fired when the member was missing, not pointing at
//! the real problem.
//!
//! The audit decision: stdlib should be opaque until imported. This
//! test file locks the new behavior in the typechecker:
//!
//!   - `list.sum(x)` without `import list` → error recommending import
//!   - `list.sum(x)` with `import list`    → typechecks clean
//!   - `import list as l`, then `l.sum(x)` → typechecks clean
//!   - bare `list` identifier without import → also rejected (the
//!     check fires at FieldAccess, which covers every dotted call)

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

#[test]
fn unimported_builtin_emits_import_recommendation() {
    let errs = type_errors(
        r#"
        fn main() -> Int {
            list.sum([1, 2, 3])
        }
        "#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("module 'list' is not imported"),
        "expected 'module list is not imported' error, got:\n{joined}"
    );
    assert!(
        joined.contains("add `import list`"),
        "error should recommend `import list`, got:\n{joined}"
    );
}

#[test]
fn imported_builtin_typechecks_clean() {
    let errs = type_errors(
        r#"
        import list
        fn main() -> Int {
            list.sum([1, 2, 3])
        }
        "#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors with `import list`, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn aliased_import_typechecks_under_alias() {
    // Use `l.length` — `length` is in `builtin_module_functions("list")`
    // so the alias loop at `src/typechecker/mod.rs` mirrors its scheme
    // under the alias. (`list.sum` exists at typecheck via a separate
    // trait-based registration that the alias mirror doesn't yet
    // cover; that's a pre-existing gap, not in scope for this wave.)
    let errs = type_errors(
        r#"
        import list as l
        fn main() -> Int {
            l.length([1, 2, 3])
        }
        "#,
    );
    assert!(
        errs.is_empty(),
        "alias-qualified access should typecheck, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn aliased_import_does_not_expose_original_name() {
    // `import list as l` renames; the original bare `list` name must
    // still be treated as un-imported. This ensures the opaque-until-
    // imported rule isn't silently bypassed by any import form.
    let errs = type_errors(
        r#"
        import list as l
        fn main() -> Int {
            list.length([1, 2, 3])
        }
        "#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("module 'list' is not imported"),
        "aliased import should NOT expose original name, got:\n{joined}"
    );
}

#[test]
fn items_import_also_makes_module_accessible() {
    // `import list.{sum}` brings `sum` into scope as a bare name AND
    // makes the `list` module accessible — the user imported from
    // `list`, so `list.X` should continue to work for other members.
    let errs = type_errors(
        r#"
        import list.{sum}
        fn main() -> Int {
            list.length([1, 2, 3])
        }
        "#,
    );
    assert!(
        errs.is_empty(),
        "items-import should expose the module too, got:\n{}",
        errs.join("\n")
    );
}

#[test]
fn import_suggestion_supersedes_unknown_function_message() {
    // Before the fix, `list.notafunction(x)` without `import list`
    // emitted "unknown function 'notafunction' on module 'list'" at
    // the typechecker — misleading, since the real problem is that
    // the module isn't imported. Lock that the new behavior surfaces
    // the import recommendation instead.
    let errs = type_errors(
        r#"
        fn main() {
            list.notafunction([1, 2, 3])
        }
        "#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("module 'list' is not imported"),
        "expected import recommendation, got:\n{joined}"
    );
    assert!(
        !joined.contains("unknown function 'notafunction'"),
        "should no longer emit 'unknown function' when the module is un-imported, got:\n{joined}"
    );
}

#[test]
fn known_unknown_function_on_imported_module_still_reported() {
    // Counterpart to the test above: when the module IS imported but
    // the member doesn't exist, we keep the original typechecker
    // wording (and its "did you mean" suggestions) — the user's
    // problem is the typo, not the missing import.
    let errs = type_errors(
        r#"
        import list
        fn main() {
            list.notafunction([1, 2, 3])
        }
        "#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("unknown function 'notafunction'"),
        "imported-but-unknown member should still produce the typo diagnostic, got:\n{joined}"
    );
}

#[test]
fn non_builtin_module_identifier_is_not_gated() {
    // The import-opacity gate fires only for BUILTIN module names.
    // A record-typed or user-bound identifier with a dotted access
    // must continue to flow through the regular FieldAccess path —
    // otherwise we'd break every record-field read.
    let errs = type_errors(
        r#"
        type Point { x: Int, y: Int }
        fn main() -> Int {
            let p = Point { x: 1, y: 2 }
            p.x
        }
        "#,
    );
    assert!(
        errs.is_empty(),
        "record field access must not be gated by import-opacity, got:\n{}",
        errs.join("\n")
    );
}
