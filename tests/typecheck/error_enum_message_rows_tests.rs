//! `.message()` of a builtin error enum is a row of the builtin
//! registry, found by the enum's type (`Registry::error_module`).
//!
//! Behavioural lock: every error enum of
//! `builtin_error_enum_variants_with_arity` has a `.message()` that
//! gives a text. The test iterates the registry, so a newly registered
//! enum whose module says nothing of its variants fails.

use silt::module::builtin_error_enum_variants_with_arity;

// ── Behavioural lock ────────────────────────────────────────────────

/// Calling `.message()` on a variant of every registered enum gives a
/// non-empty `String`: the enum has a row, and its module a text for
/// the variant.
#[test]
fn every_builtin_error_enum_has_a_message_row() {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join("silt_error_enum_message_rows");
        fs::create_dir_all(&dir).unwrap();
        let name = format!("{prefix}_{n}.silt");
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    // One known-good variant per enum is enough to exercise its row:
    // the enum, its module, and a variant of it as source text.
    const SAMPLES: &[(&str, &str, &str)] = &[
        ("IoError", "io", r#"NotFound("x")"#),
        ("IntError", "int", "Empty"),
        ("FloatError", "float", "Empty"),
        ("RegexError", "regex", r#"InvalidPattern("x", 0)"#),
        ("JsonError", "json", r#"Syntax("x", 0)"#),
        ("TomlError", "toml", r#"Syntax("x", 0)"#),
        ("TimeError", "time", r#"ParseFormat("x")"#),
        ("HttpError", "http", r#"Connect("x")"#),
        ("PostgresError", "postgres", r#"Connect("x")"#),
        ("BytesError", "bytes", "InvalidUtf8(0)"),
        ("TcpError", "tcp", r#"Connect("x")"#),
        ("CryptoError", "crypto", "InvalidLength(-1)"),
        ("EncodingError", "encoding", r#"InvalidPercent("x")"#),
        ("UuidError", "uuid", r#"Invalid("x")"#),
    ];

    let skip = |enum_name: &str| -> bool {
        match enum_name {
            #[cfg(not(feature = "postgres"))]
            "PostgresError" => true,
            #[cfg(not(feature = "tcp"))]
            "TcpError" => true,
            _ => false,
        }
    };

    for (enum_name, _) in builtin_error_enum_variants_with_arity() {
        if skip(enum_name) {
            continue;
        }
        let Some((_, module, call)) = SAMPLES.iter().find(|(name, _, _)| name == enum_name) else {
            panic!(
                "{enum_name} has no sample in `SAMPLES`. Add one: every \
                 registered stdlib error enum is exercised here."
            )
        };
        let variant = call.split('(').next().unwrap_or(call);

        // The script prints the raw `.message()` output. We don't pin
        // the exact wording (that is the module's `error_text`) — we
        // only require that the call gave *some* non-empty string and
        // exited cleanly. An enum with no text would surface as a
        // non-zero exit or empty stdout.
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
            "`silt run` failed for {enum_name}.{variant}: every \
             registered enum has a `.message()`.\nsrc:\n{src}\n\
             stdout: {stdout}\nstderr: {stderr}"
        );
        // The stdout should be non-empty (the typed message). println
        // always appends a newline, so non-empty stdout is a tight
        // check.
        let trimmed = stdout.trim();
        assert!(
            !trimmed.is_empty(),
            "{enum_name}.{variant}.message() produced empty stdout. \
             Full stdout: {stdout:?}, stderr: {stderr:?}"
        );
    }
}
