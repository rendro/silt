//! The compilation session driven directly, without a front door: what
//! is cached, what an edit invalidates, and the test helpers.

use std::sync::Arc;

use silt::session::testing::{check_str, run_files, run_str, session_with, test_path};
use silt::value::Value;

/// An editor's change to an imported module re-checks that module and
/// its importers, and nothing else: an unrelated module keeps its check.
#[test]
fn set_overlay_rechecks_the_module_and_its_importers_only() {
    let (mut session, entry) = session_with(&[
        (
            "main.silt",
            "import util\nimport other\nfn main() {\n  println(util.f() + other.g())\n}\n",
        ),
        ("util.silt", "pub fn f() -> Int { 1 }\n"),
        ("other.silt", "pub fn g() -> Int { 2 }\n"),
    ]);
    assert!(
        !session.analyze(entry).has_errors(),
        "{:?}",
        session.analyze(entry).diagnostics
    );
    let graph_id = |session: &silt::session::Session, name: &str| {
        session
            .graph()
            .module_at(&test_path(name))
            .expect("the module is in the graph")
    };
    let (util, other, main) = (
        graph_id(&session, "util.silt"),
        graph_id(&session, "other.silt"),
        graph_id(&session, "main.silt"),
    );
    let other_ast = session.module_analysis(other).unwrap().ast.clone();

    session.set_overlay(
        &test_path("util.silt"),
        "pub fn f() -> String { \"one\" }\n".into(),
    );
    assert!(
        session.module_analysis(util).is_none(),
        "the edited module is re-checked"
    );
    assert!(
        session.module_analysis(main).is_none(),
        "its importer is re-checked"
    );
    assert!(
        session.module_analysis(other).is_some(),
        "an unrelated module keeps its check"
    );

    let analysis = session.analyze(entry).clone();
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.is_error() && d.message.contains("String")),
        "the importer's diagnostics follow the edit: {:?}",
        analysis.diagnostics
    );
    assert!(Arc::ptr_eq(
        &other_ast,
        &session.module_analysis(other).unwrap().ast
    ));
}

/// Asking for an analysis twice checks nothing twice, and every file is
/// in the source map once.
#[test]
fn analyze_is_cached_and_each_file_is_registered_once() {
    let (mut session, entry) = session_with(&[
        (
            "main.silt",
            "import util\nfn main() {\n  println(util.f())\n}\n",
        ),
        ("util.silt", "pub fn f() -> Int { 1 }\n"),
    ]);
    let first = session.analyze(entry).clone();
    let main = session.module_of(entry);
    let ast = session.module_analysis(main).unwrap().ast.clone();
    let second = session.analyze(entry).clone();
    assert_eq!(first.diagnostics, second.diagnostics);
    assert_eq!(first.modules, second.modules);
    assert!(Arc::ptr_eq(
        &ast,
        &session.module_analysis(main).unwrap().ast
    ));
    assert_eq!(session.sources().file_count(), 2);
}

/// The entry point is found by its type, however `main` is bound.
#[test]
fn main_is_found_by_type_however_it_is_bound() {
    assert_eq!(run_str("fn main() { 1 }"), Ok(Value::Int(1)));
    assert_eq!(run_str("let main = { -> 2 }"), Ok(Value::Int(2)));
    assert_eq!(
        run_files(&[
            ("main.silt", "import entry.{ main }"),
            ("entry.silt", "pub fn main() { 3 }"),
        ]),
        Ok(Value::Int(3))
    );
    let err = run_str("fn helper() { 1 }").unwrap_err();
    assert!(err.contains("no main() function"), "{err}");
    let err = run_str("fn real(n: Int) { n }\nlet main = real").unwrap_err();
    assert!(err.contains("must take no parameters"), "{err}");
}

/// `check_str` gives the static diagnostics without asking for `main`.
#[test]
fn check_str_reports_type_errors_and_needs_no_main() {
    assert!(check_str("fn helper(x: Int) -> Int { x + 1 }").is_empty());
    let diagnostics = check_str("fn helper(x: Int) -> String { x }");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.is_error() && d.message.contains("type mismatch")),
        "{diagnostics:?}"
    );
}

/// A module's type aliases are its own, and an edit of the module
/// replaces them: its importer is checked against the new alias.
#[test]
fn set_overlay_replaces_the_edited_modules_aliases() {
    let (mut session, entry) = session_with(&[
        ("main.silt", "import a\nfn main() {\n  println(a.f(1))\n}\n"),
        ("a.silt", "type Id = Int\npub fn f(x: Id) -> Id { x }\n"),
    ]);
    assert!(!session.analyze(entry).has_errors());
    session.set_overlay(
        &test_path("a.silt"),
        "type Id = String\npub fn f(x: Id) -> Id { x }\n".into(),
    );
    let analysis = session.analyze(entry).clone();
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.is_error() && d.message.contains("String")),
        "{:?}",
        analysis.diagnostics
    );
}
