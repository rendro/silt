//! Round-52 regression: the formatter must preserve the source's
//! trailing-comma state on the last element of a delimited block. The
//! fuzz invariant "significant token count unchanged" (see
//! `src/fuzz_invariants.rs::check_formatter_invariants` and
//! `significant_token_count`) counts `,` as significant, so silently
//! stripping a trailing comma the user wrote — or adding one they
//! didn't — corrupts the count and trips the fuzz harness.
//!
//! The original blocking input was `examples/cross_module_errors.silt`
//! (seeded into `fuzz/corpus/fuzz_formatter/` by `fuzz/seed.sh`): its
//! `type AppError { … Custom(String), }` had a trailing comma on the
//! last variant, but the old formatter always dropped it, yielding a
//! 207 → 206 token delta that crashed libFuzzer in CI.

use silt::formatter;
use silt::fuzz_invariants::check_formatter_invariants;

/// Last-variant trailing comma must survive a format pass.
#[test]
fn type_enum_preserves_trailing_comma_on_last_variant() {
    let src = "type AppError {\n  A(Int),\n  B(String),\n}\n";
    let out = formatter::format(src).expect("format should succeed");
    assert!(
        out.contains("B(String),"),
        "last-variant trailing comma was stripped; formatter output:\n{out}"
    );
    check_formatter_invariants(src, &out).expect("formatter invariants must hold");
}

/// Last-variant with NO trailing comma in source must NOT have one
/// inserted (would add a significant token). This guards the opposite
/// direction of the round-52 fix.
#[test]
fn type_enum_does_not_insert_trailing_comma_when_source_has_none() {
    let src = "type E {\n  A,\n  B\n}\n";
    let out = formatter::format(src).expect("format should succeed");
    assert!(
        !out.contains("B,"),
        "formatter inserted a trailing comma that the source did not have; \
         output:\n{out}"
    );
    check_formatter_invariants(src, &out).expect("formatter invariants must hold");
}

/// The specific shape from `examples/cross_module_errors.silt` that
/// triggered the libFuzzer crash: an enum with parameterized variants,
/// last variant has a trailing comma.
#[test]
fn cross_module_errors_shape_survives_fuzz_invariants() {
    let src = "type AppError {\n  \
              ConfigRead(IoError),\n  \
              ConfigParse(JsonError),\n  \
              ApiCall(HttpError),\n  \
              ApiResponse(JsonError),\n  \
              Custom(String),\n\
              }\n";
    let out = formatter::format(src).expect("format should succeed");
    check_formatter_invariants(src, &out).expect(
        "the invariant the CI fuzz corpus replay was failing on must pass; \
         before the round-52 fix the last-variant `,` was stripped, producing \
         a 207 → 206 significant-token-count violation",
    );
}

/// Single-line form with trailing comma must also preserve it. The
/// source scanner must correctly identify the `,` as the last
/// meaningful byte when the `{` and `}` are on the same line or when
/// the `}` is on a line with preceding content.
#[test]
fn single_line_enum_preserves_trailing_comma() {
    let src = "type E { A, B, }\n";
    let out = formatter::format(src).expect("format should succeed");
    check_formatter_invariants(src, &out).expect("invariants on single-line form");
}


/// Idempotency: a second format pass on an already-formatted source
/// with a trailing comma must not strip it.
#[test]
fn trailing_comma_is_idempotent_across_two_passes() {
    let src = "type AppError {\n  A(Int),\n  B(String),\n}\n";
    let once = formatter::format(src).expect("first pass");
    let twice = formatter::format(&once).expect("second pass");
    assert_eq!(
        once, twice,
        "formatter must be idempotent on trailing commas"
    );
    assert!(
        twice.contains("B(String),"),
        "two passes stripped the trailing comma; round-trip:\n{twice}"
    );
}
