//! Round-90 STALE-DOC lock: the doc comment on `combine_hash_expr` must
//! describe the formula the function actually emits.
//!
//! `combine_hash_expr` (src/typechecker/auto_derive.rs) lowers to the silt
//! expression `(a mod P) * 31 + (b mod P)` — there is NO outer
//! `mod (i64-safe bound)` wrapper, because each side is already reduced
//! modulo the prime and `(P * 31) + P` fits in i64 with room to spare.
//!
//! A prior revision of the doc claimed `... mod (i64-safe bound)`, an outer
//! reduction that the code never applies. This is a source-grep lock: it
//! fails the instant that phantom clause reappears in the source tree.

const AUTO_DERIVE_SRC: &str = include_str!("../src/typechecker/auto_derive.rs");

#[test]
fn combine_hash_doc_does_not_claim_phantom_outer_mod() {
    assert!(
        !AUTO_DERIVE_SRC.contains("mod (i64-safe bound)"),
        "combine_hash_expr doc claims an outer `mod (i64-safe bound)` that the \
         code (src/typechecker/auto_derive.rs::combine_hash_expr) never emits — \
         it lowers to `(a mod P) * 31 + (b mod P)` with no outer reduction. \
         Update the doc to match the emitted expression."
    );
}

#[test]
fn combine_hash_doc_describes_actual_emitted_formula() {
    assert!(
        AUTO_DERIVE_SRC.contains("(a mod P) * 31 + (b mod P)"),
        "combine_hash_expr doc should state the exact formula it emits, \
         `(a mod P) * 31 + (b mod P)`, so the prose stays in lock-step with \
         the lowering."
    );
}
