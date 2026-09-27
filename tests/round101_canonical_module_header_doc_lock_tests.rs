//! Round-101 doc-drift lock: the module header of
//! `src/types/canonical.rs` must describe the module as it exists
//! today, not as the Phase-A design sketch shipped it.
//!
//! Pre-fix the header claimed:
//!   * "Today the only reduction is `Type::Range(t) -> Type::List(t)`"
//!     — false: user-alias expansion (Phase D) and `AssocProj`
//!     impl-binding reduction are live in the same file.
//!   * the module "is purely additive ... not yet wired into any
//!     caller" — false: the unifier (`src/typechecker/mod.rs`), the
//!     typechecker's `resolve_type_expr` / `type_name_for_impl`, the
//!     compiler's trait-impl name emission, and the VM's runtime
//!     dispatch all call into this module.
//!
//! The `canonicalize` fn doc additionally claimed a
//! `Type::Record(name, _)` alias-expansion arm that the body does not
//! have (the `Record` arm is pure structural recursion; a name cannot
//! be both a record and an alias).
//!
//! A prior round (86) locked the `canonicalize_type_name` FN doc only
//! (`round86_canonicalize_type_name_doc_parity_tests.rs`); the module
//! header and the `canonicalize` fn doc were separate, unlocked
//! locations of the same drift class. This lock grep-scans both.

const CANONICAL_SRC: &str = include_str!("../src/types/canonical.rs");

/// The leading run of `//!` inner-doc lines at the top of the file.
fn module_header(src: &str) -> String {
    src.lines()
        .take_while(|line| line.trim_start().starts_with("//!"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The contiguous `///` doc-comment block immediately above the first
/// occurrence of `signature_needle`.
fn extract_doc_above_fn(src: &str, signature_needle: &str) -> String {
    let fn_idx = src
        .find(signature_needle)
        .unwrap_or_else(|| panic!("could not find `{signature_needle}` in source"));
    let before = &src[..fn_idx];
    let mut lines: Vec<&str> = before.lines().collect();
    let mut doc_lines: Vec<&str> = Vec::new();
    while let Some(line) = lines.pop() {
        if line.trim_start().starts_with("///") {
            doc_lines.push(line);
        } else {
            break;
        }
    }
    doc_lines.reverse();
    doc_lines.join("\n")
}

#[test]
fn module_header_has_no_phase_a_era_stale_claims() {
    let header = module_header(CANONICAL_SRC);
    assert!(
        !header.is_empty(),
        "src/types/canonical.rs no longer starts with a `//!` module \
         header — this lock scans that header for stale Phase-A claims."
    );

    // Phrases from the Phase-A design sketch that are false now that
    // phases B/C/D (unifier/VM/compiler wiring, alias expansion,
    // AssocProj reduction) are live. None may reappear in the header.
    let stale_phrases = ["only reduction", "purely additive", "not yet wired"];
    let mut present: Vec<&str> = Vec::new();
    for phrase in &stale_phrases {
        if header.contains(phrase) {
            present.push(phrase);
        }
    }
    assert!(
        present.is_empty(),
        "src/types/canonical.rs module header contains stale Phase-A-era \
         phrase(s): {present:?}. The module has multiple live reductions \
         (Range->List, alias expansion, AssocProj) and is wired into the \
         typechecker, compiler, and VM — describe it as it is, or frame \
         Phase A explicitly as history.\n\nCurrent header:\n{header}"
    );
}

#[test]
fn module_header_enumerates_every_live_reduction() {
    let header = module_header(CANONICAL_SRC);

    // Each live reduction implemented in `canonicalize` must be named
    // in the module header. If a reduction is removed from the body,
    // update the header AND this list together.
    let required_terms = [
        // Range -> List collapse (Phase A).
        "Range",
        // User `type Foo = Bar` alias expansion (Phase D).
        "alias",
        // Associated-type projection reduction.
        "AssocProj",
    ];
    let mut missing: Vec<&str> = Vec::new();
    for term in &required_terms {
        if !header.contains(term) {
            missing.push(term);
        }
    }
    assert!(
        missing.is_empty(),
        "src/types/canonical.rs module header no longer mentions the \
         following live reduction term(s): {missing:?}. The header must \
         enumerate the actual reduction set implemented by \
         `canonicalize`: {required_terms:?}.\n\nCurrent header:\n{header}"
    );
}

#[test]
fn canonicalize_fn_doc_matches_its_body() {
    let doc = extract_doc_above_fn(CANONICAL_SRC, "pub fn canonicalize(");
    assert!(
        !doc.is_empty(),
        "`pub fn canonicalize(` in src/types/canonical.rs has no \
         doc-comment — expected one enumerating its reduction set."
    );

    // The body has no Record-alias arm: the `Type::Record` arm is pure
    // structural recursion (a name cannot be declared as both a record
    // and an alias). The pre-fix doc claimed alias expansion fired on
    // `Type::Record(name, _)` heads too.
    assert!(
        !doc.contains("Record(name, _)` whose"),
        "`canonicalize` doc-comment again claims a `Type::Record` \
         alias-expansion arm; the body's Record arm is pure structural \
         recursion. Current doc:\n{doc}"
    );

    // The body reduces `Type::AssocProj` via the impl-binding registry;
    // the doc's reduction-set list must mention it.
    assert!(
        doc.contains("AssocProj"),
        "`canonicalize` doc-comment does not mention the `AssocProj` \
         reduction its body performs. Current doc:\n{doc}"
    );
}
