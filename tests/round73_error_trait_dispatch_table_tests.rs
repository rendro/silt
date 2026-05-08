//! Round-73 BLOAT-3 (L6) regression locks: the 11 byte-near-identical
//! `<EnumName> => catch_builtin_panic("<EnumName>", AssertUnwindSafe(||
//! <module>::call_<x>_error_trait(func, args)))` arms in
//! `dispatch_builtin` were collapsed onto a single dispatch-table
//! lookup (`ERROR_TRAIT_DISPATCH`) keyed by enum name.
//!
//! Pre-fix each arm was a copy-paste of the surrounding arm with one
//! string and one function pointer changed — the canonical "11
//! byte-near-identical lines that should be a table" shape. Adding a
//! new typed-error enum required edits at four sites; with the table,
//! adding a new enum is one entry.
//!
//! Tests:
//!
//!   * Source-grep lock: dispatch.rs no longer contains the 11 per-enum
//!     `<EnumName> => catch_builtin_panic` arms — there should be at
//!     most a handful of `catch_builtin_panic("<module>"`-style match
//!     arms remaining (one per builtin module: io, string, ...), and
//!     none of them should target a typed-error enum name.
//!
//!   * Behavioural lock: every typed-error enum's `.message()`
//!     dispatch routes through the table and produces a typed message
//!     (no "unknown builtin namespace: <X>" leak).

use silt::module::builtin_error_enum_variants_with_arity;

const DISPATCH_SRC: &str = include_str!("../src/vm/dispatch.rs");

// ── Source-grep locks ────────────────────────────────────────────────

/// Each typed-error enum name must NOT appear as the LHS of a `<Name>
/// => catch_builtin_panic` match arm in dispatch.rs. The dispatch table
/// drives that resolution now.
#[test]
fn no_per_enum_catch_builtin_panic_arms() {
    for (enum_name, _) in builtin_error_enum_variants_with_arity() {
        // Pre-fix arm shape: `"IoError" => catch_builtin_panic(`
        let needle = format!("\"{enum_name}\" => catch_builtin_panic(");
        for (idx, line) in DISPATCH_SRC.lines().enumerate() {
            let trimmed = line.trim_start();
            let is_comment = trimmed.starts_with("//") || trimmed.starts_with("/*");
            if is_comment {
                continue;
            }
            assert!(
                !line.contains(&needle),
                "src/vm/dispatch.rs:{} re-introduces the pre-fix \
                 per-enum match arm `{needle}`. The round-73 BLOAT-3 \
                 fix collapsed all 11 such arms onto a single lookup \
                 against `ERROR_TRAIT_DISPATCH`. Line: {line}",
                idx + 1
            );
        }
    }
}

/// The `ERROR_TRAIT_DISPATCH` table itself must be present, must use
/// the canonical `ErrorTraitFn` typedef, and must be consulted by the
/// dispatch-fallback arm.
#[test]
fn dispatch_table_and_lookup_are_present() {
    assert!(
        DISPATCH_SRC.contains("static ERROR_TRAIT_DISPATCH"),
        "src/vm/dispatch.rs must declare `static ERROR_TRAIT_DISPATCH` \
         to hold the round-73 BLOAT-3 table."
    );
    assert!(
        DISPATCH_SRC.contains("type ErrorTraitFn"),
        "src/vm/dispatch.rs must declare `type ErrorTraitFn` so all \
         table entries share one signature."
    );
    assert!(
        DISPATCH_SRC.contains("error_trait_dispatch("),
        "src/vm/dispatch.rs must call `error_trait_dispatch(...)` from \
         the dispatch fallback to look up the table."
    );
}

/// Count parity: the dispatch table must contain one entry per
/// registered stdlib error enum (cfg-gated entries excluded by
/// `#[cfg(...)]` attributes when the matching feature is off, included
/// otherwise — same shape as the registry).
///
/// We approximate the count by scanning the `ERROR_TRAIT_DISPATCH`
/// slice literal for `("<Name>"` openers. The parser tolerates both
/// the always-present entries and the `#[cfg(feature = "...")]`-gated
/// entries; what matters is that every name in the registry appears
/// at least once as a table key.
#[test]
fn every_registry_enum_has_a_dispatch_table_entry() {
    // Locate the table literal.
    let start = DISPATCH_SRC
        .find("static ERROR_TRAIT_DISPATCH")
        .expect("ERROR_TRAIT_DISPATCH not found");
    // The table body ends at the closing `];` of the slice literal.
    let after = &DISPATCH_SRC[start..];
    let close = after.find("];").expect("ERROR_TRAIT_DISPATCH not closed");
    let table = &after[..close];

    // Round 79 follow-up: tolerate both single-line `("Foo", call_foo)`
    // and rustfmt-broken-up multi-line `( "Foo", call_foo, )` forms.
    // Compare on a whitespace-collapsed view of the table.
    let table_compact: String = table.chars().filter(|c| !c.is_whitespace()).collect();
    let mut missing: Vec<&'static str> = Vec::new();
    for (enum_name, _) in builtin_error_enum_variants_with_arity() {
        let key_lit = format!("(\"{enum_name}\",");
        if !table_compact.contains(&key_lit) {
            missing.push(enum_name);
        }
    }

    assert!(
        missing.is_empty(),
        "ERROR_TRAIT_DISPATCH is missing entries for: {missing:?}. \
         Every enum in `module::builtin_error_enum_variants_with_arity` \
         must have a matching `(\"<EnumName>\", ...)` table entry — \
         feature-gated enums use a `#[cfg(feature = \"...\")]` attribute \
         on the entry. (Round-73 BLOAT-3 invariant.)"
    );
}

// ── Behavioural lock ────────────────────────────────────────────────

/// Calling `.message()` on every registered enum's first variant must
/// dispatch through the table and return a non-empty `String` (i.e.
/// the typed-message helper actually ran).
#[test]
fn every_registry_enum_message_routes_through_table() {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join("silt_round73_error_trait_dispatch_table");
        fs::create_dir_all(&dir).unwrap();
        let name = format!("{prefix}_{n}.silt");
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    // Mirror of the per-variant constructor table used in
    // tests/round73_error_enum_runtime_dedup_tests.rs — one known-good
    // variant per enum is enough to exercise the dispatch table.
    fn ctor_call_for(_enum_name: &str, variant: &str, _arity: usize) -> Option<String> {
        match variant {
            "ParseEmpty" | "ChannelTimeout" => Some(variant.to_string()),
            "IoNotFound" | "HttpConnect" | "PgConnect" | "TcpConnect" | "TimeParseFormat" => {
                Some(format!(r#"{variant}("x")"#))
            }
            "BytesInvalidUtf8" => Some(format!(r#"{variant}(0)"#)),
            "JsonSyntax" | "TomlSyntax" | "RegexInvalidPattern" => {
                Some(format!(r#"{variant}("x", 0)"#))
            }
            _ => None,
        }
    }

    let skip = |enum_name: &str| -> bool {
        match enum_name {
            #[cfg(not(feature = "postgres"))]
            "PgError" => true,
            #[cfg(not(feature = "tcp"))]
            "TcpError" => true,
            _ => false,
        }
    };

    for (enum_name, variants) in builtin_error_enum_variants_with_arity() {
        if skip(enum_name) {
            continue;
        }
        let chosen = variants
            .iter()
            .find_map(|(v, a)| ctor_call_for(enum_name, v, *a).map(|c| (*v, c)));
        let (variant, call) = chosen.unwrap_or_else(|| {
            panic!(
                "round-73 BLOAT-3 test: no variant of {enum_name} has a \
                 constructor template in `ctor_call_for`. Add one — \
                 every registered stdlib error enum must be exercised \
                 here to lock the dispatch-table wiring."
            )
        });
        let module = silt::module::gated_constructor_module(variant)
            .expect("every stdlib error variant maps to a module");

        // The script prints the raw `.message()` output. We don't pin
        // the exact wording (that's a typed-message detail owned by the
        // per-module `call_*_error_trait` helper) — we only require the
        // dispatch path produced *some* non-empty string and exited
        // cleanly. A regression in the table (wrong fn pointer, missing
        // entry) would surface as either a non-zero exit, an "unknown
        // builtin namespace" error, or empty stdout.
        let src = format!(
            "import {module}\n\
             fn main() {{\n\
                 let e = {call}\n\
                 println(e.message())\n\
             }}\n"
        );
        let path = temp_silt_file(&format!("trait_{enum_name}"), &src);

        let output = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("run")
            .arg(&path)
            .output()
            .expect("failed to run silt");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "`silt run` failed for {enum_name}.{variant} — the \
             round-73 BLOAT-3 dispatch table must wire up every \
             registered enum's `.message()`.\nsrc:\n{src}\n\
             stdout: {stdout}\nstderr: {stderr}"
        );
        // The stdout should be non-empty (the typed message). println
        // always appends a newline, so non-empty stdout is a tight
        // check.
        let trimmed = stdout.trim();
        assert!(
            !trimmed.is_empty(),
            "{enum_name}.{variant}.message() produced empty stdout — \
             the table entry's fn pointer is likely returning the \
             wrong shape. Full stdout: {stdout:?}, stderr: {stderr:?}"
        );
        assert!(
            !stderr.contains("unknown builtin namespace"),
            "{enum_name}.{variant}.message() leaked the dispatch \
             fallback wording 'unknown builtin namespace' — the table \
             lookup did not find the entry.\nstderr: {stderr}"
        );
    }
}
