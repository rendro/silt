//! Round 50 audit: after removing the never-read informational
//! fields (`_name`, `_params`) from `EnumInfo`, `RecordInfo`, and
//! `TraitInfo`, these assertions lock the struct sizes so that
//! accidentally re-adding a purely informational field (which
//! would bloat the typechecker HashMaps storing thousands of
//! these per compilation) fails fast at test time.
//!
//! If you INTENTIONALLY add a field, update the expected size
//! below. If the size changed because the underlying Vec/HashMap
//! layout changed in a Rust release, that's also fine — bump the
//! numbers once, and the lock continues to protect against
//! accidental re-introduction of dead fields.
use super::{EnumInfo, RecordInfo, TraitInfo};

#[test]
fn enum_info_size_locked() {
    // Round 63 item 5 added `defined_in: Symbol` to track the
    // owning package for the trait-orphan rule.
    assert_eq!(
        std::mem::size_of::<EnumInfo>(),
        80,
        "EnumInfo size changed — see module doc"
    );
}

#[test]
fn record_info_size_locked() {
    // Round 63 item 5 added `defined_in: Symbol` to track the
    // owning package for the trait-orphan rule.
    assert_eq!(
        std::mem::size_of::<RecordInfo>(),
        32,
        "RecordInfo size changed — see module doc"
    );
}

#[test]
fn trait_info_size_locked() {
    // Round 63 item 5 added `defined_in: Symbol` to track the
    // owning package for the trait-orphan rule; stage 5 shrank its
    // spans from 24 to 12 bytes; stage 6 added `self_var` and
    // `var_names`, which a default body and an impl are checked with.
    assert_eq!(
        std::mem::size_of::<TraitInfo>(),
        264,
        "TraitInfo size changed — see module doc"
    );
}
