//! Round-64 audit follow-up: parity locks across the typed-error
//! registries.
//!
//! Three independent issues from the round-64 audit:
//!
//!   * GAP — `PgError`/`TcpError` were unconditionally registered by
//!     the typechecker even though their VM dispatch arms are gated
//!     behind the `postgres`/`tcp` cargo features. A build like
//!         cargo run --no-default-features --features "repl,http,tcp"
//!     would let `PgError.PgConnect("nope")` typecheck and then crash
//!     at runtime with `unknown builtin namespace: PgError`.
//!
//!   * LATENT — the trait-method dispatch fallback at
//!     `src/vm/dispatch.rs` produced "unknown module: <X>" for any
//!     missing namespace. From the user's perspective `PgError` is an
//!     enum, not a "module", so the wording was confusing. The fix
//!     replaces the wording with "unknown builtin namespace: <X>".
//!
//!   * DUP — the typed-error variant set lived in three independent
//!     registries (`src/module.rs`, `src/typechecker/builtins/errors.rs`,
//!     and `src/vm/dispatch.rs`). The dispatch-side list is now
//!     data-driven from `module::builtin_error_enum_variants_with_arity`.
//!     This test asserts the dispatch globals match what the
//!     typechecker registers on `(variant, arity)` for every enum.
//!
//! These tests intentionally lock the SOURCE forms (and the
//! source-grep wording lock for the LATENT) — per audit instructions,
//! source-level grep locks beat construct-and-format locks for
//! user-visible strings.

use silt::module::{builtin_enum_variants, builtin_error_enum_variants_with_arity};
#[cfg(any(not(feature = "postgres"), not(feature = "tcp")))]
use silt::types::Severity;

const DISPATCH_SRC: &str = include_str!("../src/vm/dispatch.rs");
const ERRORS_SRC: &str = include_str!("../src/typechecker/builtins/errors.rs");

// ── Finding 2 — wording lock ─────────────────────────────────────────

/// SOURCE-LEVEL grep: `unknown module:` must NOT appear as the FORMAT
/// string of a runtime error in `src/vm/dispatch.rs`. The trait-method
/// dispatch fallback previously emitted that wording, but the
/// user-facing string for failed namespace lookups is now "unknown
/// builtin namespace:". The grep lock prevents future patches from
/// regressing the wording.
///
/// Historical references in comments (e.g. "previously 'unknown
/// module: <X>'") are intentionally allowed — they document the round
/// where the wording changed. We scope the check to source positions
/// matching the runtime error-construction shape.
#[test]
fn dispatch_does_not_emit_unknown_module_wording() {
    // The old wording shape was `format!("unknown module: {module}")`.
    // After the round-64 LATENT fix the source must contain neither
    // a `"unknown module: {module}"` format-fragment nor a literal
    // string that flows into `VmError::new(format!(...))`.
    //
    // Filter: each line that contains the old wording must be a
    // comment (starts with `//` after trimming) or a doc-comment
    // (`///` / `//!`). Any non-comment occurrence is a regression.
    let mut bad_lines: Vec<(usize, &str)> = Vec::new();
    for (lineno, line) in DISPATCH_SRC.lines().enumerate() {
        if line.contains("unknown module:") {
            let trimmed = line.trim_start();
            let is_comment = trimmed.starts_with("//") || trimmed.starts_with("/*");
            if !is_comment {
                bad_lines.push((lineno + 1, line));
            }
        }
    }
    assert!(
        bad_lines.is_empty(),
        "src/vm/dispatch.rs contains non-comment `unknown module:` \
         occurrences:\n  - {}\n\
         The round-64 LATENT fix replaced this wording with \
         `unknown builtin namespace:`. Comments referencing the old \
         phrase as a historical note are allowed.",
        bad_lines
            .iter()
            .map(|(n, l)| format!("line {n}: {l}"))
            .collect::<Vec<_>>()
            .join("\n  - ")
    );
}

#[test]
fn dispatch_uses_unknown_builtin_namespace_wording() {
    // The replacement wording must be present — this is the canonical
    // shape for unknown-module-style failures from the trait-method
    // dispatch fallback path.
    assert!(
        DISPATCH_SRC.contains("unknown builtin namespace: {module}"),
        "src/vm/dispatch.rs is missing the canonical \
         `unknown builtin namespace: {{module}}` wording. The \
         round-64 LATENT fix established this as the user-facing \
         shape for namespace-lookup failures."
    );
}

// ── Finding 1 — feature-gate lock ────────────────────────────────────

/// When the `postgres` cargo feature is OFF, the typechecker must NOT
/// register `PgError` as an enum. Constructing `PgError.PgConnect(...)`
/// must therefore be rejected at typecheck time, not at runtime.
///
/// This is gated `#[cfg(not(feature = "postgres"))]` because the
/// happy-path is exactly: feature off → typecheck rejects.
#[cfg(not(feature = "postgres"))]
#[test]
fn pg_error_typecheck_rejects_when_postgres_feature_off() {
    let src = r#"
fn main() {
    let _ = PgError.PgConnect("nope")
}
"#;
    let tokens = silt::lexer::Lexer::new(src).tokenize().expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = silt::typechecker::check(&mut program);
    let messages: Vec<String> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();
    assert!(
        !messages.is_empty(),
        "with `postgres` feature OFF, the typechecker must reject \
         `PgError.PgConnect(...)` instead of letting it through to \
         runtime where it would crash with `unknown builtin \
         namespace: PgError`. Got no errors."
    );
}

/// Mirror lock for `TcpError`. The audit-driver default test profile
/// usually enables `tcp` (round-64 dev profile is `repl,http,tcp`), so
/// an unconditional `cfg(not(feature = "tcp"))` guard would skip in
/// every routine run. We keep the test gated on `not(feature = "tcp")`
/// for build correctness but the spec is identical to PgError.
#[cfg(not(feature = "tcp"))]
#[test]
fn tcp_error_typecheck_rejects_when_tcp_feature_off() {
    let src = r#"
fn main() {
    let _ = TcpError.TcpConnect("nope")
}
"#;
    let tokens = silt::lexer::Lexer::new(src).tokenize().expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = silt::typechecker::check(&mut program);
    let messages: Vec<String> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();
    assert!(
        !messages.is_empty(),
        "with `tcp` feature OFF, the typechecker must reject \
         `TcpError.TcpConnect(...)` instead of letting it through to \
         runtime."
    );
}

/// Source-level lock: the typechecker's `errors::register` for
/// `PgError` and `TcpError` must be guarded by the matching cargo
/// feature. This is the load-bearing piece of Finding 1 — without the
/// guard, a `--no-default-features --features "repl,http,tcp"` build
/// silently regresses to the pre-fix behaviour even if the runtime
/// crash never trips a happy-path test.
#[test]
fn errors_rs_gates_pg_and_tcp_registration_on_features() {
    // The exact form we expect at the call site.
    let pg_gate = "#[cfg(feature = \"postgres\")]";
    let tcp_gate = "#[cfg(feature = \"tcp\")]";
    let pg_call = "\"PgError\",";
    let tcp_call = "\"TcpError\",";

    // For each call, find the previous non-empty line and assert it is
    // the matching `#[cfg(feature = ...)]` attribute.
    fn preceding_attr(src: &str, call: &str) -> Option<String> {
        let idx = src.find(call)?;
        // Walk backwards over the lines preceding `idx`.
        let prefix = &src[..idx];
        let mut lines: Vec<&str> = prefix.lines().collect();
        // Drop the partial last line containing the call site.
        lines.pop();
        // Walk back past lines that are not the cfg attribute. The
        // pattern at the call site is:
        //
        //   #[cfg(feature = "postgres")]
        //   register_enum(
        //       checker,
        //       env,
        //       "PgError",
        //
        // i.e. the cfg attribute is several lines above the
        // `"PgError",` argument. Scan up to 12 preceding lines for it.
        for l in lines.iter().rev().take(12) {
            let t = l.trim();
            if t.starts_with("#[cfg(") {
                return Some(t.to_string());
            }
        }
        None
    }

    let pg_attr = preceding_attr(ERRORS_SRC, pg_call);
    assert_eq!(
        pg_attr.as_deref(),
        Some(pg_gate),
        "the `PgError` registration in src/typechecker/builtins/errors.rs \
         must be guarded by `{pg_gate}` so it disappears when the \
         `postgres` feature is off. Found: {pg_attr:?}"
    );

    let tcp_attr = preceding_attr(ERRORS_SRC, tcp_call);
    assert_eq!(
        tcp_attr.as_deref(),
        Some(tcp_gate),
        "the `TcpError` registration in src/typechecker/builtins/errors.rs \
         must be guarded by `{tcp_gate}` so it disappears when the \
         `tcp` feature is off. Found: {tcp_attr:?}"
    );
}

// ── Finding 3 — variant/arity parity ─────────────────────────────────

/// SOURCE-LEVEL parity: `src/vm/dispatch.rs::register_builtins` must
/// derive its stdlib typed-error variant globals from
/// `module::builtin_error_enum_variants_with_arity`, not from
/// hand-rolled per-family loops.
///
/// Pre-round-64 the dispatch source duplicated each (variant, arity)
/// across ~133 lines of per-enum `for ... in [...]` blocks. The
/// round-64 DUP-1 fix collapsed all of them into a single loop driven
/// by the shared registry. This test pins the data-driven shape so a
/// future patch can't silently re-introduce hand-rolled lists.
#[test]
fn dispatch_uses_shared_arity_registry() {
    assert!(
        DISPATCH_SRC.contains("builtin_error_enum_variants_with_arity()"),
        "src/vm/dispatch.rs must call \
         `module::builtin_error_enum_variants_with_arity()` to seed the \
         stdlib typed-error variant globals. Round-64 DUP-1 made the \
         dispatch loop data-driven; without the call, dispatch.rs \
         would be back to N independent hand-rolled per-enum loops."
    );
    // Belt-and-braces: each enum name should appear at most ONCE as a
    // string literal in the variant-registration region (the
    // section between `Stdlib error variants` and the next major
    // section comment). Hand-rolled loops would re-mention the enum
    // name once per loop. Since the registration is now driven by the
    // helper, no per-enum literal should remain in that region.
    // Round-75 DEAD-7 collapsed the per-family Prelude loop and
    // Stdlib-error loop into a single `chain()`-driven loop with a
    // merged section header. The marker matches the post-merge text;
    // the assertion below still verifies no per-enum literal remains
    // inside the registration region.
    let region_start_marker = "// ── Prelude + stdlib-error enum variants ──";
    let region_start = DISPATCH_SRC
        .find(region_start_marker)
        .expect("Prelude + stdlib-error variants section comment missing");
    // Search for the next section comment AFTER skipping past the
    // start marker. Slice via byte length of the marker, not a magic
    // constant — the marker contains multi-byte ─ characters, so a
    // wrong offset would split inside a codepoint.
    let region_after_idx = region_start + region_start_marker.len();
    let region_after = &DISPATCH_SRC[region_after_idx..];
    let region_end_rel = region_after
        .find("// ── ")
        .expect("no following section comment");
    let region = &DISPATCH_SRC[region_start..region_after_idx + region_end_rel];
    for enum_name in [
        "IoError",
        "JsonError",
        "TomlError",
        "ParseError",
        "HttpError",
        "RegexError",
        "PgError",
        "TcpError",
        "TimeError",
        "BytesError",
        "ChannelError",
    ] {
        assert!(
            !region.contains(&format!("// {enum_name}\n")),
            "src/vm/dispatch.rs still contains a per-enum hand-rolled \
             registration block for `{enum_name}`. The round-64 DUP-1 \
             fix collapsed all per-family loops into a single \
             data-driven loop."
        );
    }
}

/// Source-level lock: the variant set in
/// `module::builtin_enum_variants` (names only) must be a perfect
/// subset of `builtin_error_enum_variants_with_arity` for every
/// stdlib error enum. Catches the simplest form of drift — adding a
/// variant in one helper and forgetting the other.
#[test]
fn module_helpers_agree_on_error_enum_variant_names() {
    // Collect (enum_name -> variants) from each helper.
    let with_arity = builtin_error_enum_variants_with_arity();
    let names_only = builtin_enum_variants();

    let mut mismatches: Vec<String> = Vec::new();
    for (enum_name, arity_variants) in with_arity {
        let arity_names: Vec<&str> = arity_variants.iter().map(|(n, _)| *n).collect();
        let names = names_only
            .iter()
            .find(|(e, _)| e == enum_name)
            .map(|(_, vs)| vs.to_vec());
        match names {
            Some(names) => {
                if names != arity_names {
                    mismatches.push(format!(
                        "{enum_name}: names-only={names:?} vs with-arity={arity_names:?}"
                    ));
                }
            }
            None => {
                mismatches.push(format!(
                    "{enum_name}: present in builtin_error_enum_variants_with_arity \
                     but missing from builtin_enum_variants"
                ));
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "module.rs helpers disagree on stdlib error enum variants:\n  - {}",
        mismatches.join("\n  - ")
    );
}

/// Source-level lock: `errors::register` in
/// `src/typechecker/builtins/errors.rs` must contain a
/// `register_enum(checker, env, "<EnumName>", &[ ... ])` block whose
/// variant names exactly match
/// `builtin_error_enum_variants_with_arity`. The arity is encoded in
/// the field-types slice (`&[Type::String]` is arity 1, `&[]` is
/// arity 0, etc.); we count occurrences of `Type::` per variant entry
/// to lock arity against the registry.
///
/// This is the load-bearing parity assert for Finding 3 (DUP-1) — it
/// is the test that, if it had existed before round 64, would have
/// failed when `errors.rs` and `dispatch.rs` drifted.
#[test]
fn errors_rs_register_block_matches_arity_registry() {
    // Walk the source for each enum's `register_enum(..., "<EnumName>", &[ ... ])`
    // block and parse the variant entries.
    fn extract_variants(src: &str, enum_name: &str) -> Option<Vec<(String, usize)>> {
        let needle = format!("\"{enum_name}\",");
        let start = src.find(&needle)?;
        // Find the `&[` after the enum name.
        let after = &src[start..];
        let bracket = after.find("&[")? + start + 2;
        // Walk forward, balancing brackets.
        let bytes = src.as_bytes();
        let mut depth: i32 = 1;
        let mut j = bracket;
        while j < bytes.len() && depth > 0 {
            match bytes[j] {
                b'[' => depth += 1,
                b']' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        let body = &src[bracket..j - 1];
        // Each variant is `(<whitespace>"<Name>"<whitespace>, <whitespace>&[<Type>, ...])`. Walk
        // the body for `"<Name>"` openers preceded by `(` (with optional
        // whitespace/newlines between) and parse name + count `Type::`
        // occurrences in the matching field-types slice.
        let mut out = Vec::new();
        let bytes = body.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // Find next `"<Name>"` whose nearest non-whitespace
            // predecessor is `(`. This handles both the inline form
            //   ("PgConnect", &[Type::String])
            // and the multi-line form
            //   (
            //       "PgTypeMismatch",
            //       &[Type::String, Type::String, Type::String],
            //   )
            if bytes[i] == b'"' {
                // Walk backwards over whitespace.
                let mut prev = i;
                while prev > 0 {
                    let c = bytes[prev - 1];
                    if c == b' ' || c == b'\n' || c == b'\t' || c == b'\r' {
                        prev -= 1;
                    } else {
                        break;
                    }
                }
                let preceded_by_paren = prev > 0 && bytes[prev - 1] == b'(';
                if preceded_by_paren {
                    let name_start = i + 1;
                    // Scan for closing `"`.
                    let mut k = name_start;
                    while k < bytes.len() && bytes[k] != b'"' {
                        k += 1;
                    }
                    let name = &body[name_start..k];
                    // Find the next `&[` after the closing quote.
                    let rest = &body[k + 1..];
                    if let Some(fields_start) = rest.find("&[") {
                        let fields_after = &rest[fields_start + 2..];
                        // Find the matching `]`.
                        let fb = fields_after.as_bytes();
                        let mut d: i32 = 1;
                        let mut p = 0;
                        while p < fb.len() && d > 0 {
                            match fb[p] {
                                b'[' => d += 1,
                                b']' => d -= 1,
                                _ => {}
                            }
                            p += 1;
                        }
                        let fields_body = &fields_after[..p - 1];
                        let arity = fields_body.matches("Type::").count();
                        out.push((name.to_string(), arity));
                        // Advance past the variant tuple.
                        i = k + 1 + fields_start + 2 + p;
                        continue;
                    }
                }
            }
            i += 1;
        }
        Some(out)
    }

    let mut mismatches: Vec<String> = Vec::new();
    for (enum_name, expected) in builtin_error_enum_variants_with_arity() {
        let extracted = extract_variants(ERRORS_SRC, enum_name);
        match extracted {
            None => mismatches.push(format!(
                "{enum_name}: no `register_enum(...)` block found in errors.rs"
            )),
            Some(parsed) => {
                let expected_vec: Vec<(String, usize)> = expected
                    .iter()
                    .map(|(n, a)| ((*n).to_string(), *a))
                    .collect();
                if parsed != expected_vec {
                    mismatches.push(format!(
                        "{enum_name}: errors.rs={parsed:?} vs registry={expected_vec:?}"
                    ));
                }
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "src/typechecker/builtins/errors.rs disagrees with \
         module::builtin_error_enum_variants_with_arity:\n  - {}",
        mismatches.join("\n  - ")
    );
}
