//! Round-77 REPL-1 GAP lock: REPL tab completion offers user-defined
//! enum *variant* names, not just the type name.
//!
//! Background: in `src/repl.rs::eval_declaration`, after a successful
//! eval the REPL appends every newly-declared name to its completion
//! `names` list. Pre-fix, the `Decl::Type(t)` arm only pushed `t.name`
//! and never iterated `TypeBody::Enum(variants)`. So a session like
//!
//!     > type Status { Active, Idle }
//!     > Acti<TAB>
//!
//! produced no completion for `Active`, even though every BUILTIN enum
//! constructor (`Ok`, `Some`, `Recv`, `Monday`, ...) DOES surface — it
//! flows through `module::all_builtin_constructor_names` which is
//! consulted by `repl::builtin_names`. User enums silently differed.
//!
//! The fix extracts the post-eval name-collection loop into a public
//! helper `repl::collect_decl_completion_names` and, in the
//! `Decl::Type` arm, additionally walks `TypeBody::Enum(variants)` and
//! emits every variant name. This test pins the fix at the data-source
//! level (the helper that feeds the REPL's `names` Rc).
//!
//! We exercise the actual completion path by:
//!   1. Parsing a `type Status { Active, Idle }` declaration through
//!      the same `Lexer` -> `Parser` pipeline the REPL uses.
//!   2. Calling `collect_decl_completion_names` on the resulting decls
//!      — this is the function the REPL calls in `eval_declaration` to
//!      populate its completion list.
//!   3. Filtering by `starts_with("Acti")`, mirroring the exact logic
//!      `SiltHelper::complete` uses on the populated names vector
//!      (see `src/repl.rs` lines 138-144).
//!
//! A regression test confirms builtin enum variants still complete via
//! `repl::completion_candidates_for_prefix`.

use silt::ast::Program;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::repl::{builtin_names, collect_decl_completion_names, completion_candidates_for_prefix};

/// Drive the same lex+parse pipeline `eval_declaration` runs. A failure
/// here means the test fixture itself is broken, not the completion
/// helper — we report it as a panic so the test author notices.
fn parse_program(src: &str) -> Program {
    let tokens = Lexer::new(src)
        .tokenize()
        .unwrap_or_else(|e| panic!("test fixture failed to lex: {}", e.message));
    Parser::new(tokens)
        .parse_program()
        .unwrap_or_else(|e| panic!("test fixture failed to parse: {}", e.message))
}

/// Mirror of `SiltHelper::complete`'s filter (src/repl.rs:138-144):
/// `names.iter().filter(|n| n.starts_with(prefix)).collect()`.
fn completions_for(names: &[String], prefix: &str) -> Vec<String> {
    names
        .iter()
        .filter(|n| n.starts_with(prefix))
        .cloned()
        .collect()
}

#[test]
fn round77_user_enum_variant_names_are_offered_for_completion() {
    // Define `type Status { Active, Idle }` exactly as a REPL user
    // would type it, then run it through the SAME helper
    // `eval_declaration` calls.
    let program = parse_program("type Status { Active, Idle }");
    let collected = collect_decl_completion_names(&program.decls);

    // The type name must be present (this was the pre-fix behavior;
    // we keep the regression check so a future refactor that drops
    // the type name is also caught).
    assert!(
        collected.iter().any(|s| s == "Status"),
        "expected `Status` in collected completion names, got: {collected:?}"
    );

    // The fix: every enum variant must also be present.
    for v in ["Active", "Idle"] {
        assert!(
            collected.iter().any(|s| s == v),
            "expected variant `{v}` in collected completion names \
             (REPL-1 GAP fix: `Decl::Type` arm must walk `TypeBody::Enum`), \
             got: {collected:?}"
        );
    }

    // Exercise the actual completion data-structure path: simulate the
    // REPL's populated `names` list (builtin_names + collected) and run
    // the same `starts_with` filter `SiltHelper::complete` uses for the
    // user's typed prefix `Acti`. `Active` must surface.
    let mut names = builtin_names();
    names.extend(collected);
    let matches = completions_for(&names, "Acti");
    assert!(
        matches.iter().any(|s| s == "Active"),
        "completion for prefix `Acti` must include user-defined enum variant \
         `Active`; got matches: {matches:?}"
    );
}

#[test]
fn round77_user_enum_with_payload_variants_are_offered_for_completion() {
    // Enum variants with payload fields must also surface — the fix
    // pushes `v.name` regardless of `v.fields`. A regression that
    // narrowed the loop to only nullary variants would fail here.
    let program = parse_program("type Shape { Circle(int), Square(int, int) }");
    let collected = collect_decl_completion_names(&program.decls);

    for v in ["Shape", "Circle", "Square"] {
        assert!(
            collected.iter().any(|s| s == v),
            "expected `{v}` in collected completion names, got: {collected:?}"
        );
    }
}

#[test]
fn round77_record_type_does_not_leak_field_names_into_completion() {
    // The fix must NOT spill record FIELD names into the completion
    // namespace — those are accessed via record syntax `r.field`, not
    // unqualified. A regression that walked `TypeBody::Record(fields)`
    // here would incorrectly add `x`/`y` and shadow user variables.
    let program = parse_program("type Point { x: int, y: int }");
    let collected = collect_decl_completion_names(&program.decls);

    assert!(
        collected.iter().any(|s| s == "Point"),
        "expected `Point` in collected names, got: {collected:?}"
    );
    for f in ["x", "y"] {
        assert!(
            !collected.iter().any(|s| s == f),
            "record field `{f}` must NOT appear in unqualified-completion \
             names (would shadow user bindings); got: {collected:?}"
        );
    }
}

#[test]
fn round77_builtin_enum_variants_still_complete_regression_check() {
    // Regression check: the builtin path was always working — variants
    // come from `module::all_builtin_constructor_names()` via
    // `builtin_names()` and surface through `completion_candidates_for_prefix`.
    // If the fix accidentally broke the builtin path (e.g. by replacing
    // rather than augmenting the namespace), this guard fires.
    //
    // We probe canonical entries from each gating slice:
    //   * prelude:   `Ok`, `Some`
    //   * channels:  `Recv`
    //   * time:      `Monday`
    let probes = [("Ok", "Ok"), ("Som", "Some"), ("Rec", "Recv"), ("Mon", "Monday")];
    for (prefix, expected) in probes {
        let matches = completion_candidates_for_prefix(prefix);
        assert!(
            matches.iter().any(|s| s == expected),
            "regression: builtin enum variant `{expected}` no longer surfaces \
             for prefix `{prefix}`; got matches: {matches:?}"
        );
    }
}
