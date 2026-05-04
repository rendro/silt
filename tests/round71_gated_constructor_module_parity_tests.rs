//! Round-71 PARALLEL-ARRAY-DRIFT lock: `gated_constructor_module`
//! ↔ `builtin_enum_variants` parity.
//!
//! `src/module.rs::gated_constructor_module` (the helper that maps a
//! variant name like `IoNotFound` back to the module that has to be
//! imported for it to be constructable) hand-encodes ~50 variant
//! names that are also present in
//! `src/module.rs::builtin_enum_variants` (the authoritative
//! `(enum, &[variants])` registry).
//!
//! Pre-round 71 there was no parity lock asserting the two registries
//! agree on the gated set. The existing
//! `tests/builtin_constructor_parity_tests.rs:198-204` only asserts
//! that `gated_constructor_module` is *consulted* by the LSP dot-
//! completion path — it does NOT assert that every gated variant in
//! `builtin_enum_variants` returns `Some(_)` from
//! `gated_constructor_module`, and it does NOT assert the reverse.
//!
//! This file adds those two missing directions:
//!
//! 1. Every variant in the non-prelude entries of
//!    `builtin_enum_variants()` must return `Some(_)` from
//!    `gated_constructor_module` — otherwise compilation of
//!    `EnumName.Variant` lowers to a bare global that user code
//!    cannot refer to without an explicit import (or the variant
//!    silently lands in the always-available prelude bucket).
//!
//! 2. Every variant arm in `gated_constructor_module`'s match must be
//!    registered in `builtin_enum_variants` — otherwise typo'd or
//!    stale match arms ride along forever.
//!
//! Round-71 PARALLEL-ARRAY-DRIFT fix lock.

use silt::module::{builtin_enum_variants, gated_constructor_module};

// ── Direction 1: every gated variant in `builtin_enum_variants` returns `Some(_)` ─

/// The prelude enums whose variants are intentionally NOT gated —
/// always available without any `import`. `Result` (Ok/Err) and
/// `Option` (Some/None) are the canonical prelude shapes; `ChannelOp`
/// (Recv/Send) is also always-available because `channel.select`
/// passes a list of `ChannelOp` values without requiring users to
/// import the `channel` module. Every other enum's variants are
/// gated.
const PRELUDE_ENUMS: &[&str] = &["Result", "Option", "ChannelOp"];

#[test]
fn every_non_prelude_variant_is_gated() {
    let mut missing: Vec<(&'static str, &'static str)> = Vec::new();
    for (enum_name, variants) in builtin_enum_variants() {
        if PRELUDE_ENUMS.contains(enum_name) {
            continue;
        }
        for variant in variants.iter() {
            if gated_constructor_module(variant).is_none() {
                missing.push((enum_name, variant));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "PARALLEL-ARRAY-DRIFT: the following gated variants are \
         registered in `module::builtin_enum_variants` but \
         `module::gated_constructor_module` returns `None` for them. \
         Add a match arm to `gated_constructor_module` for each:\n\
         {}",
        missing
            .iter()
            .map(|(e, v)| format!("  - {e}.{v}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ── Direction 2: every gated variant in the match is in `builtin_enum_variants` ─

/// The full set of variant names that `gated_constructor_module` maps
/// to a module — mirrored here as the reverse-direction lock. Every
/// arm in the `match name { ... }` body must land in this list, AND
/// every entry in this list must appear in `builtin_enum_variants`.
///
/// If a new gated variant is added to `gated_constructor_module`,
/// add it here; the parity test at the bottom of this file then
/// asserts the registry catches up.
const GATED_VARIANTS: &[&str] = &[
    // list (Step)
    "Stop",
    "Continue",
    // channel (ChannelResult, ChannelError)
    "Message",
    "Closed",
    "Empty",
    "Sent",
    "ChannelTimeout",
    "ChannelClosed",
    // time (Weekday, TimeError)
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
    "TimeParseFormat",
    "TimeOutOfRange",
    // http (Method, HttpError)
    "GET",
    "POST",
    "PUT",
    "PATCH",
    "DELETE",
    "HEAD",
    "OPTIONS",
    "HttpConnect",
    "HttpTls",
    "HttpTimeout",
    "HttpInvalidUrl",
    "HttpInvalidResponse",
    "HttpClosedEarly",
    "HttpStatusCode",
    "HttpUnknown",
    // io (IoError)
    "IoNotFound",
    "IoPermissionDenied",
    "IoAlreadyExists",
    "IoInvalidInput",
    "IoInterrupted",
    "IoUnexpectedEof",
    "IoWriteZero",
    "IoUnknown",
    // json (JsonError)
    "JsonSyntax",
    "JsonTypeMismatch",
    "JsonMissingField",
    "JsonUnknown",
    // toml (TomlError)
    "TomlSyntax",
    "TomlTypeMismatch",
    "TomlMissingField",
    "TomlUnknown",
    // int / float (ParseError)
    "ParseEmpty",
    "ParseInvalidDigit",
    "ParseOverflow",
    "ParseUnderflow",
    // regex (RegexError)
    "RegexInvalidPattern",
    "RegexTooBig",
    // postgres (PgError)
    "PgConnect",
    "PgTls",
    "PgAuthFailed",
    "PgQuery",
    "PgTypeMismatch",
    "PgNoSuchColumn",
    "PgClosed",
    "PgTimeout",
    "PgTxnAborted",
    "PgUnknown",
    // tcp (TcpError)
    "TcpConnect",
    "TcpTls",
    "TcpClosed",
    "TcpTimeout",
    "TcpUnknown",
    // bytes (BytesError)
    "BytesInvalidUtf8",
    "BytesInvalidHex",
    "BytesInvalidBase64",
    "BytesByteOutOfRange",
    "BytesOutOfBounds",
];

#[test]
fn every_gated_variant_in_match_is_registered_in_enum_variants() {
    let mut all_registered: std::collections::BTreeSet<&'static str> =
        std::collections::BTreeSet::new();
    for (_enum_name, variants) in builtin_enum_variants() {
        for v in variants.iter() {
            all_registered.insert(*v);
        }
    }

    let mut unregistered: Vec<&'static str> = Vec::new();
    for variant in GATED_VARIANTS {
        if !all_registered.contains(variant) {
            unregistered.push(variant);
        }
    }
    assert!(
        unregistered.is_empty(),
        "PARALLEL-ARRAY-DRIFT: the following variants appear in \
         `module::gated_constructor_module`'s match (mirrored by the \
         GATED_VARIANTS list in this test) but are NOT registered in \
         `module::builtin_enum_variants`. Either add them to the \
         registry or drop the stale arm:\n  - {}",
        unregistered.join("\n  - ")
    );
}

#[test]
fn gated_variants_list_matches_gated_constructor_module() {
    // Every entry in `GATED_VARIANTS` must indeed be gated, i.e.
    // `gated_constructor_module` must return `Some(_)` for it. This
    // catches typos in the test fixture itself.
    for variant in GATED_VARIANTS {
        assert!(
            gated_constructor_module(variant).is_some(),
            "GATED_VARIANTS in this test contains `{variant}` but \
             `gated_constructor_module(\"{variant}\")` returns None — \
             likely a typo in the test fixture."
        );
    }

    // And every non-prelude variant in `builtin_enum_variants` must
    // appear in `GATED_VARIANTS` (sanity-checked from the other
    // direction; otherwise this test would be vacuous on additions).
    let listed: std::collections::BTreeSet<&'static str> =
        GATED_VARIANTS.iter().copied().collect();
    let mut missing: Vec<(&'static str, &'static str)> = Vec::new();
    for (enum_name, variants) in builtin_enum_variants() {
        if PRELUDE_ENUMS.contains(enum_name) {
            continue;
        }
        for variant in variants.iter() {
            if !listed.contains(variant) {
                missing.push((enum_name, variant));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "GATED_VARIANTS list in this test is missing the following \
         variants registered by `builtin_enum_variants` — round-71 \
         PARALLEL-ARRAY-DRIFT lock requires the test fixture to enumerate \
         every gated variant:\n{}",
        missing
            .iter()
            .map(|(e, v)| format!("  - {e}.{v}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
