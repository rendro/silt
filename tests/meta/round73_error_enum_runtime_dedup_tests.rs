//! Round-73 BLOAT-1 (L4) regression lock: the runtime and typechecker
//! sites that seed stdlib typed-error enum globals / trait impls are
//! derived from the canonical
//! `module::builtin_error_enum_variants_with_arity` registry. This test
//! walks the registry and checks that every registered enum's
//! `.message()` actually dispatches through `silt run`.

use silt::module::builtin_error_enum_variants_with_arity;

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
            "IoNotFound" | "HttpConnect" | "PgConnect" | "TcpConnect" | "TimeParseFormat" => {
                Some(format!(r#"{variant}("x")"#))
            }
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

        // The module that declares the variant: the constructor is
        // reached through it.
        let module = silt::module::builtin_variant_module(variant)
            .expect("every stdlib error variant belongs to a module");

        let src = format!(
            "import {module}\n\
             fn main() {{\n\
                 let e = {module}.{call}\n\
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
