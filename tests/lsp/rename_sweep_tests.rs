//! The rename sweep (see `rename_sweep`) over the cases of
//! `tests/lsp/rename_sweep/`: every identifier, with a fresh name and
//! with names in use, and what the program prints compared.

use super::rename_sweep::{Kind, repo, sweep};

/// Sweep each of `entries` of the case `case`, with clashing names, and
/// fail with every broken rename.
fn sweep_case(case: &str, entries: &[&str]) {
    let dir = repo().join("tests/lsp/rename_sweep").join(case);
    let mut broken = Vec::new();
    let mut counts = String::new();
    for entry in entries {
        let outcome = sweep(&dir, entry, "main.silt", Kind::Case, true, 1);
        counts.push_str(&format!(
            "{case}/{entry}: {} asked, {} renamed, {} refused\n",
            outcome.asked, outcome.renamed, outcome.refused
        ));
        assert!(outcome.asked > 0 && outcome.renamed > 0, "{counts}");
        broken.extend(outcome.broken);
    }
    eprint!("{counts}");
    assert!(
        broken.is_empty(),
        "{} broken rename(s) in {case}:\n{}\n{counts}",
        broken.len(),
        broken.join("\n")
    );
}

macro_rules! cases {
    ($($test:ident: $case:literal, [$($entry:literal),+];)+) => {
        $(
            #[test]
            fn $test() {
                sweep_case($case, &[$($entry),+]);
            }
        )+
    };
}

cases! {
    g1_punned_record_binders: "g1", ["main.silt"];
    t1_trait_and_its_methods: "t1", ["main.silt"];
    t2_patterns_pins_guards: "t2", ["main.silt"];
    t3_across_files: "t3", ["main.silt", "geo.silt", "util.silt", "other.silt"];
    t4_names_in_scope: "t4", ["main.silt"];
    t5_importers_not_open: "t5", ["geo.silt"];
    t10_shadowing_a_documented_function: "t10", ["main.silt"];
    t11_many_constructs: "t11", ["main.silt"];
    t12_or_patterns: "t12", ["main.silt"];
    t13_record_patterns: "t13", ["main.silt"];
    t14_nested_blocks: "t14", ["main.silt"];
    t15_nested_block_lets: "t15", ["main.silt"];
    t16_closures: "t16", ["main.silt"];
    t17_loop_binders: "t17", ["main.silt"];
    t18_traits_across_files: "t18", ["main.silt", "shapes.silt", "impls.silt"];
    t19_associated_type_projections: "t19", ["main.silt"];
    pkg2_dependency_importers: "pkg2", ["main.silt", "mathutil/src/lib.silt"];
}

/// Sweep the file `entry` of the case `case`, a program with a syntax
/// error, and return the counts: asked, renamed, refused.
fn sweep_broken(case: &str, entry: &str) -> (usize, usize, usize) {
    let dir = repo().join("tests/lsp/rename_sweep").join(case);
    let outcome = sweep(&dir, entry, "main.silt", Kind::Broken, true, 1);
    assert!(
        outcome.broken.is_empty(),
        "{} broken rename(s) in {case}:\n{}",
        outcome.broken.len(),
        outcome.broken.join("\n")
    );
    (outcome.asked, outcome.renamed, outcome.refused)
}

/// A rename is the same program under another name, or it is refused:
/// in a file that has a declaration with a lex error, no function is
/// renamed (the names in that declaration are not known), and neither
/// is the parameter of the function that failed. The parameters of the
/// functions that are whole are (with a fresh name, and with the names
/// in use that do not clash): each is used in its function only.
#[test]
fn e1_a_lex_error_refuses_what_the_failed_declaration_could_name() {
    let (asked, renamed, refused) = sweep_broken("e1", "main.silt");
    assert!(renamed >= 3 && refused > 100, "{asked} {renamed} {refused}");
}

/// The same for a declaration with a parse error.
#[test]
fn e2_a_parse_error_refuses_what_the_failed_declaration_could_name() {
    let (asked, renamed, refused) = sweep_broken("e2", "main.silt");
    assert!(renamed >= 3 && refused > 100, "{asked} {renamed} {refused}");
}

/// A string with a wrong escape is a lex error that leaves every
/// declaration whole: the renames are complete, and each renamed copy
/// is the same program with the same error.
#[test]
fn e3_a_wrong_escape_leaves_the_renames_whole() {
    let (asked, renamed, _) = sweep_broken("e3", "main.silt");
    assert!(asked > 20 && renamed > 5, "{asked} {renamed}");
}

/// A file that imports the module of a definition and has a syntax
/// error could name the definition in the declaration that failed: a
/// rename asked in the module that declares it is refused, though that
/// module is whole and the importer is not open. What no other file
/// can name is renamed: the parameter `w` of each of its two
/// functions.
#[test]
fn e4_a_broken_importer_refuses_the_rename_of_what_it_imports() {
    let (asked, renamed, refused) = sweep_broken("e4", "geo.silt");
    assert!(
        asked > 10 && renamed == 2 && refused > 10,
        "{asked} {renamed} {refused}"
    );
}
