//! Round-73 BLOAT-3 (L6) regression lock: the 11 byte-near-identical
//! per-enum `catch_builtin_panic` arms in `dispatch_builtin` were
//! collapsed onto a single dispatch-table lookup
//! (`ERROR_TRAIT_DISPATCH`) keyed by enum name.
//!
//! Behavioural lock: every typed-error enum registered in
//! `builtin_error_enum_variants_with_arity` has its `.message()`
//! dispatch routed through the table and produces a typed message (no
//! "unknown builtin namespace: <X>" leak). The test iterates the
//! registry, so a newly registered enum without a table entry fails.

use silt::module::builtin_error_enum_variants_with_arity;

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

    // One known-good variant per enum is enough to exercise the
    // dispatch table.
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
        let module = silt::module::builtin_variant_module(variant)
            .expect("every stdlib error variant belongs to a module");

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
                 let e = {module}.{call}\n\
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
