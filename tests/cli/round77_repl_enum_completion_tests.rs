//! REPL tab completion offers what an input sees, as the session's scope
//! of the input binds it: the variants of an enum the session declared
//! (round-77 REPL-1), not a builtin module's variants or functions until
//! the module is imported, and then through it.

use silt::repl::{Repl, builtin_names, completion_candidates_for_prefix};
use silt::session::ProjectSetup;

/// The names <Tab> offers after `inputs` ran, each of which must run.
fn names_after(inputs: &[&str]) -> Vec<String> {
    let mut repl = Repl::new(ProjectSetup::None);
    let mut names = builtin_names();
    for input in inputs {
        let evaluation = repl.eval(input);
        assert!(
            evaluation.committed,
            "`{input}` must run: {:?}",
            evaluation.diagnostics
        );
        names = builtin_names();
        names.extend(evaluation.names);
    }
    names
}

#[test]
fn round77_user_enum_variants_are_offered_for_completion() {
    let names = names_after(&["type Status { Active, Idle }"]);
    for v in ["Status", "Active", "Idle"] {
        assert!(names.iter().any(|s| s == v), "missing `{v}`: {names:?}");
    }
}

#[test]
fn round77_user_enum_with_payload_variants_are_offered_for_completion() {
    let names = names_after(&["type Shape { Circle(Int), Square(Int, Int) }", "1"]);
    for v in ["Shape", "Circle", "Square"] {
        assert!(names.iter().any(|s| s == v), "missing `{v}`: {names:?}");
    }
}

#[test]
fn round77_record_type_does_not_leak_field_names_into_completion() {
    let names = names_after(&["type Point { x: Int, y: Int }"]);
    assert!(names.iter().any(|s| s == "Point"), "{names:?}");
    for f in ["x", "y"] {
        assert!(
            !names.iter().any(|s| s == f),
            "field `{f}` offered: {names:?}"
        );
    }
}

#[test]
fn builtin_module_names_complete_only_through_an_import() {
    for (prefix, absent) in [("Mon", "Monday"), ("Rec", "Recv"), ("list.m", "list.map")] {
        let matches = completion_candidates_for_prefix(prefix);
        assert!(
            !matches.iter().any(|s| s == absent),
            "`{absent}` offered: {matches:?}"
        );
    }
    for (prefix, present) in [("Ok", "Ok"), ("Som", "Some")] {
        let matches = completion_candidates_for_prefix(prefix);
        assert!(
            matches.iter().any(|s| s == present),
            "`{present}` missing: {matches:?}"
        );
    }
    let names = names_after(&["import time", "import list as l", "import channel.{ Recv }"]);
    for present in ["time", "time.Monday", "time.Weekday", "l", "l.map", "Recv"] {
        assert!(
            names.iter().any(|s| s == present),
            "missing `{present}`: {names:?}"
        );
    }
    for absent in ["Monday", "list.map", "Message"] {
        assert!(
            !names.iter().any(|s| s == absent),
            "offered `{absent}`: {names:?}"
        );
    }
}
