//! REPL tab completion offers what an input sees, as the session's scope
//! of the input binds it: the variants of an enum the session declared
//! (round-77 REPL-1), not a builtin module's variants or functions until
//! the module is imported, and then through it.

use silt::repl::{Repl, builtin_names, completion_candidates_for_prefix};
use silt::session::ProjectSetup;

/// The names <Tab> offers after `inputs` ran, each of which must run.
fn names_after(inputs: &[&str]) -> Vec<String> {
    let mut repl = Repl::new(ProjectSetup::None);
    for input in inputs {
        let evaluation = repl.eval(input);
        assert!(
            evaluation.committed,
            "`{input}` must run: {:?}",
            evaluation.diagnostics
        );
    }
    repl.completion_names()
}

/// The names are made when <Tab> asks for them, not after each input.
/// What is offered is what the last input that ran to its end left: an
/// input that fails, however it fails, adds no name and takes none
/// away, and what an earlier input declared stays offered after it.
#[test]
fn the_names_offered_are_those_of_the_last_input_that_ran() {
    let mut repl = Repl::new(ProjectSetup::None);
    assert_eq!(repl.completion_names(), builtin_names());
    let offered = |repl: &Repl, name: &str| repl.completion_names().iter().any(|n| n == name);

    assert!(repl.eval("type Status { Active, Idle }").committed);
    assert!(repl.eval("import string").committed);
    let after_ran = repl.completion_names();
    for name in ["Status", "Active", "Status.Idle", "string", "string.trim"] {
        assert!(offered(&repl, name), "missing `{name}`: {after_ran:?}");
    }
    assert!(!offered(&repl, "list.map"));

    // An input that does not check, one that does not parse, and
    // inputs that fail when they run: statements, a declaration, and a
    // declaration behind an import that was checked.
    for failing in [
        "let wrong: Int = \"s\"",
        "fn (",
        "1 / 0",
        "let late = 1 / 0",
        "import list\nlet boom = 1 / 0",
    ] {
        assert!(!repl.eval(failing).committed, "`{failing}` must fail");
        assert_eq!(repl.completion_names(), after_ran, "after `{failing}`");
    }
    for name in ["wrong", "late", "boom", "list.map"] {
        assert!(!offered(&repl, name), "`{name}` is offered");
    }

    // The next input that runs is seen at once, with what was there.
    assert!(repl.eval("let fresh = 1").committed);
    for name in ["fresh", "Status", "Idle", "string.trim"] {
        assert!(offered(&repl, name), "missing `{name}`");
    }
    assert_ne!(repl.completion_names(), after_ran);
    // A name bound again is offered once.
    assert!(repl.eval("let fresh = 2").committed);
    let fresh = repl.completion_names();
    assert_eq!(fresh.iter().filter(|n| *n == "fresh").count(), 1);
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

#[test]
fn an_ambiguous_variant_is_not_offered_and_an_enum_offers_its_variants() {
    // After `type C { Red }` and `type D { Red }`, a bare `Red` is
    // ambiguous for the next input: it is not offered; `C.Red` and
    // `D.Red` are.
    let names = names_after(&["type C { Red, Green }", "type D { Red, Blue }"]);
    assert!(!names.iter().any(|s| s == "Red"), "{names:?}");
    for present in [
        "C", "D", "Green", "Blue", "C.Red", "C.Green", "D.Red", "D.Blue",
    ] {
        assert!(
            names.iter().any(|s| s == present),
            "missing `{present}`: {names:?}"
        );
    }
    let mut repl = Repl::new(ProjectSetup::None);
    repl.eval("type C { Red, Green }");
    repl.eval("type D { Red, Blue }");
    assert!(!repl.eval("Red").committed, "a bare `Red` is ambiguous");
}

#[test]
fn an_enum_of_an_imported_module_offers_its_variants() {
    let names = names_after(&["import time"]);
    let matches: Vec<&String> = names
        .iter()
        .filter(|s| s.starts_with("time.Weekday.T"))
        .collect();
    assert_eq!(
        matches,
        vec!["time.Weekday.Thursday", "time.Weekday.Tuesday"],
        "{names:?}"
    );
}
