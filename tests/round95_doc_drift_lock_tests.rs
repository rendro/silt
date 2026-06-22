//! Round 95 doc-drift lock: `docs/why-silt.md` once described nominal records
//! with the un-annotated field syntax `type User { name, age }`, which does NOT
//! parse — record fields require type annotations. This drift escaped the
//! doc-walker because it lives in prose, not a ```silt fenced block. These
//! source-grep locks fail the moment the bad form returns.

const WHY_SILT: &str = include_str!("../docs/why-silt.md");

#[test]
fn why_silt_does_not_contain_unannotated_record_fields() {
    assert!(
        !WHY_SILT.contains("type User { name, age }"),
        "docs/why-silt.md regressed: the un-annotated record form \
         `type User {{ name, age }}` does not parse — record fields require \
         type annotations (e.g. `type User {{ name: String, age: Int }}`)."
    );
}

#[test]
fn why_silt_uses_annotated_record_fields() {
    assert!(
        WHY_SILT.contains("type User { name: String, age: Int }"),
        "docs/why-silt.md should describe nominal records with the annotated \
         form `type User {{ name: String, age: Int }}`."
    );
}
