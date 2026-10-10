//! The rename sweep (see `rename_sweep`) over the cases of
//! `tests/lsp/rename_sweep/`: every identifier, with a fresh name and
//! with names in use, and what the program prints compared.

use super::rename_sweep::{repo, sweep};

/// Sweep each of `entries` of the case `case`, with clashing names, and
/// fail with every broken rename.
fn sweep_case(case: &str, entries: &[&str]) {
    let dir = repo().join("tests/lsp/rename_sweep").join(case);
    let mut broken = Vec::new();
    let mut counts = String::new();
    for entry in entries {
        let outcome = sweep(&dir, entry, "main.silt", true, true, 1);
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
