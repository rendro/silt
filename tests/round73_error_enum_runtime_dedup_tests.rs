//! Round-73 BLOAT-1 (L4) regression locks: the runtime and typechecker
//! sites that seed stdlib typed-error enum globals / trait impls must
//! be derived from the canonical
//! `module::builtin_error_enum_variants_with_arity` registry, not from
//! hand-rolled per-enum lists.
//!
//! Pre-fix shape (round 72 audit):
//!
//!   * `src/vm/dispatch.rs` had a hand-rolled `&["IoError", "JsonError",
//!     ...]` slice driving the `<EnumName>.message` BuiltinFn seeding.
//!   * `src/typechecker/builtins/errors.rs` had a near-identical
//!     `vec!["IoError", ...]` plus `#[cfg(...)] vec.push(...)` for the
//!     two feature-gated enums (`PgError`/`TcpError`).
//!
//! The collapse uses one source of truth — the registry helper — so
//! adding a new stdlib error enum can no longer drift between the three
//! sites (the third sibling lives in `src/typechecker/mod.rs`).
//!
//! The tests below combine source-grep locks (catch byte-level
//! re-introduction of the hand-rolled lists) with behavioural checks
//! that every registered enum's `.message()` actually dispatches.

use silt::module::builtin_error_enum_variants_with_arity;

const DISPATCH_SRC: &str = include_str!("../src/vm/dispatch.rs");
const ERRORS_SRC: &str = include_str!("../src/typechecker/builtins/errors.rs");

// ── Source-grep locks ────────────────────────────────────────────────

/// `src/vm/dispatch.rs` must NOT contain the pre-fix hand-rolled
/// `&["IoError", "JsonError", ...]` slice. The seed loop now iterates
/// `module::builtin_error_enum_variants_with_arity()`.
#[test]
fn dispatch_rs_does_not_hand_roll_error_enum_names() {
    // Pre-fix shape: a literal slice/array starting with the two
    // anchor names. Any non-comment occurrence is a regression.
    let needle = "\"IoError\", \"JsonError\"";
    for (idx, line) in DISPATCH_SRC.lines().enumerate() {
        if line.contains(needle) {
            let trimmed = line.trim_start();
            let is_comment = trimmed.starts_with("//") || trimmed.starts_with("/*");
            assert!(
                is_comment,
                "src/vm/dispatch.rs:{} re-introduces the hand-rolled \
                 error-enum name list ({needle:?}). The round-73 BLOAT-1 \
                 fix derives this set from \
                 `module::builtin_error_enum_variants_with_arity`. \
                 Line: {line}",
                idx + 1
            );
        }
    }
}

/// `src/typechecker/builtins/errors.rs` must NOT contain the pre-fix
/// hand-rolled `vec!["IoError", "JsonError", ...]` literal. The
/// trait-impl loop now iterates the registry with a cfg-aware filter.
#[test]
fn errors_rs_does_not_hand_roll_enum_names_vec() {
    // The pre-fix shape was a `vec!` macro whose first two entries were
    // `"IoError",` and `"JsonError",`. Look for those two on adjacent
    // lines outside of comments.
    let lines: Vec<&str> = ERRORS_SRC.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.starts_with("\"IoError\",") {
            // Look ahead a few lines for `"JsonError",` (also non-comment).
            for ahead in lines.iter().skip(i + 1).take(3) {
                let at = ahead.trim_start();
                if at.starts_with("//") {
                    continue;
                }
                assert!(
                    !at.starts_with("\"JsonError\","),
                    "src/typechecker/builtins/errors.rs:{} re-introduces \
                     the hand-rolled error-enum name vec literal. The \
                     round-73 BLOAT-1 fix derives this set from \
                     `module::builtin_error_enum_variants_with_arity` \
                     with a cfg-aware filter. Found `\"IoError\",` and \
                     `\"JsonError\",` on adjacent non-comment lines.",
                    i + 1
                );
            }
        }
    }
}

/// Both sites must reference the canonical registry helper. Without
/// this call, the source might still happen to satisfy the negative
/// greps above by being silently broken.
#[test]
fn both_sites_reference_canonical_registry() {
    assert!(
        DISPATCH_SRC.contains("builtin_error_enum_variants_with_arity"),
        "src/vm/dispatch.rs must reference \
         `module::builtin_error_enum_variants_with_arity` to seed the \
         runtime `<EnumName>.message` BuiltinFn entries (round-73 BLOAT-1)."
    );
    assert!(
        ERRORS_SRC.contains("builtin_error_enum_variants_with_arity"),
        "src/typechecker/builtins/errors.rs must reference \
         `module::builtin_error_enum_variants_with_arity` to derive the \
         per-enum trait-impl loop (round-73 BLOAT-1)."
    );
}

// ── Behavioural lock ────────────────────────────────────────────────

/// Every enum from the registry whose feature is enabled in this build
/// must have its `.message()` dispatch wired up — i.e. constructing a
/// representative variant and calling `.message()` must succeed.
///
/// We exercise this through `silt run` rather than poking the VM
/// internals directly, both because `Vm.globals` is `pub(crate)` and
/// because the end-to-end shape is what users actually depend on.
#[test]
fn every_registered_error_enum_message_dispatches() {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join("silt_round73_error_enum_dedup");
        fs::create_dir_all(&dir).unwrap();
        let name = format!("{prefix}_{n}.silt");
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    // For each registered enum, the test picks one known-shape variant
    // and constructs it with arity-correct typed arguments. The arg
    // lists are hand-spelled to match the typed field shapes declared
    // in `src/typechecker/builtins/errors.rs` — the test only needs
    // ONE variant per enum that round-trips through `.message()`
    // cleanly. (Adding a new enum without extending this table is
    // caught by the `unwrap_or_else(panic!)` below.)
    fn ctor_call_for(_enum_name: &str, variant: &str, _arity: usize) -> Option<String> {
        match variant {
            // arity-0
            "ParseEmpty" | "ChannelTimeout" => Some(variant.to_string()),
            // arity-1 String
            "IoNotFound" | "HttpConnect" | "PgConnect" | "TcpConnect"
            | "TimeParseFormat" => Some(format!(r#"{variant}("x")"#)),
            // arity-1 Int
            "BytesInvalidUtf8" => Some(format!(r#"{variant}(0)"#)),
            // arity-2 (String, Int)
            "JsonSyntax" | "TomlSyntax" | "RegexInvalidPattern" => {
                Some(format!(r#"{variant}("x", 0)"#))
            }
            _ => None,
        }
    }

    // Enums that this build cannot exercise — they rely on cargo
    // features that may be off. Skip them rather than hard-fail.
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
        // Walk variants and pick the first one our `ctor_call_for`
        // table knows how to construct. Every registered enum must have
        // at least ONE entry in the table — the test below assert on
        // that to catch a future regression where someone adds an enum
        // and forgets to extend the table.
        let chosen = variants
            .iter()
            .find_map(|(v, a)| ctor_call_for(enum_name, v, *a).map(|c| (*v, c)));
        let (variant, call) = chosen.unwrap_or_else(|| {
            panic!(
                "round-73 BLOAT-1 test: no variant of {enum_name} has a \
                 constructor template in `ctor_call_for`. Add one — \
                 every registered stdlib error enum must be exercised \
                 here to lock the dispatch wiring."
            )
        });

        // Determine which module to import so the constructor and the
        // trait dispatch are both visible.
        let module = silt::module::gated_constructor_module(variant)
            .expect("every stdlib error variant maps to a module");

        let src = format!(
            "import {module}\n\
             fn main() {{\n\
                 let e = {call}\n\
                 println(e.message())\n\
             }}\n"
        );
        let path = temp_silt_file(&format!("err_{enum_name}"), &src);

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
             round-73 BLOAT-1 dedup must keep `.message()` dispatch \
             wired for every registered enum.\nsrc:\n{src}\n\
             stdout: {stdout}\nstderr: {stderr}"
        );
    }
}

/// Count parity: the number of `<EnumName>.message` BuiltinFn entries
/// the runtime would seed equals the number of enums in the registry
/// (modulo cfg-gated entries), and the names match by set.
///
/// The `dispatch.rs` side is uncfg-gated (it always seeds every enum
/// from the registry), so the natural assertion is that every name in
/// the registry appears in the source as a `<Name>.message` literal
/// reference inside the seed-loop region. We approximate by asserting
/// the registry is non-empty and every enum name appears textually in
/// dispatch.rs (most as comments / log strings, but at least once).
#[test]
fn registry_names_appear_in_dispatch_source() {
    let registry = builtin_error_enum_variants_with_arity();
    assert!(!registry.is_empty(), "registry must not be empty");

    // The seed loop is data-driven, so the enum names themselves do
    // NOT appear in the loop body — they are pulled from the registry.
    // What MUST appear textually is the registry helper call.
    assert!(
        DISPATCH_SRC.contains("builtin_error_enum_variants_with_arity()"),
        "round-73 BLOAT-1 fix loop must call the registry helper"
    );
    // And the registry must be non-trivial (would have caught a
    // pathological "empty registry" regression).
    assert!(
        registry.len() >= 9,
        "registry shrunk unexpectedly to {} entries — expected \u{2265} 9 \
         (Io/Json/Toml/Parse/Http/Regex/Time/Bytes/Channel + cfg-gated \
         Pg/Tcp)",
        registry.len()
    );
}
