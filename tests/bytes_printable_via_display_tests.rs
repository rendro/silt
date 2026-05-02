//! Round-65 lock: `Bytes` is now printable through the standard
//! `Display` trait surface, and `Bytes` / `TcpListener` / `TcpStream`
//! are registered in `BUILTIN_TYPES` so trait-impl-target diagnostics
//! route through the orphan-rule path instead of "type not declared".
//!
//! Strategy:
//!   - Runtime lock (CLI subprocess): `silt run` of a program that
//!     prints a `bytes` value succeeds with the documented format
//!     (`bytes(<hex>, length: <n>)`). Reverting either the
//!     `BUILTIN_TYPES` registration or the `register_auto_derived_impls_for`
//!     stamp surfaces as a typecheck rejection — caught by the test.
//!   - Diagnostic-message lock (CLI subprocess): a user attempting to
//!     `trait Display for Bytes { ... }` gets the orphan-rule wording,
//!     not `target 'Bytes' is not a declared type`. Same for
//!     `TcpListener` / `TcpStream`. This pins both the BUILTIN_TYPES
//!     registration and the existing orphan-rule precedence.
//!   - Source-grep lock against `src/types/builtins.rs`: catches a
//!     future edit that drops the three new entries.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_bytes_printable_tests");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{prefix}_{n}.silt"));
    fs::write(&path, content).unwrap();
    path
}

#[test]
fn bytes_is_printable_via_println() {
    let path = temp_silt_file(
        "println_bytes",
        "import bytes\n\
         fn main() {\n\
             let b = bytes.from_string(\"hi\")\n\
             println(b)\n\
         }\n",
    );
    let output = silt_cmd().arg("run").arg(&path).output().expect("silt run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "silt run failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("bytes(68 69, length: 2)"),
        "expected hex preview + length, got stdout=`{stdout}`"
    );
}

#[test]
fn bytes_user_display_impl_hits_orphan_rule_not_undeclared_target() {
    let path = temp_silt_file(
        "bytes_orphan",
        "import bytes\n\
         trait Display for Bytes {\n\
             fn display(b) -> String = \"x\"\n\
         }\n\
         fn main() {}\n",
    );
    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "expected typecheck failure, got success: stderr={stderr}"
    );
    assert!(
        stderr.contains("orphan impl"),
        "expected orphan-rule wording for trait Display for Bytes, got stderr=`{stderr}`"
    );
    assert!(
        !stderr.contains("not a declared type"),
        "diagnostic regressed to 'not a declared type' wording — \
         Bytes must be in BUILTIN_TYPES so the trait-impl-target gate \
         routes to the orphan-rule path. Stderr: `{stderr}`"
    );
}

#[test]
fn tcp_listener_user_display_impl_hits_orphan_rule_not_undeclared_target() {
    let path = temp_silt_file(
        "tcp_listener_orphan",
        "trait Display for TcpListener {\n\
             fn display(t) -> String = \"x\"\n\
         }\n\
         fn main() {}\n",
    );
    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "expected typecheck failure, got success: stderr={stderr}"
    );
    assert!(
        stderr.contains("orphan impl"),
        "expected orphan-rule wording for trait Display for TcpListener, got stderr=`{stderr}`"
    );
    assert!(
        !stderr.contains("not a declared type"),
        "diagnostic regressed to 'not a declared type' wording — \
         TcpListener must be in BUILTIN_TYPES. Stderr: `{stderr}`"
    );
}

#[test]
fn tcp_stream_user_display_impl_hits_orphan_rule_not_undeclared_target() {
    let path = temp_silt_file(
        "tcp_stream_orphan",
        "trait Display for TcpStream {\n\
             fn display(t) -> String = \"x\"\n\
         }\n\
         fn main() {}\n",
    );
    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "expected typecheck failure, got success: stderr={stderr}"
    );
    assert!(
        stderr.contains("orphan impl"),
        "expected orphan-rule wording for trait Display for TcpStream, got stderr=`{stderr}`"
    );
    assert!(
        !stderr.contains("not a declared type"),
        "diagnostic regressed to 'not a declared type' wording — \
         TcpStream must be in BUILTIN_TYPES. Stderr: `{stderr}`"
    );
}

#[test]
fn builtin_types_source_still_lists_bytes_tcp_listener_tcp_stream() {
    // Source-grep lock: catches a future edit that drops the three
    // new entries from the BUILTIN_TYPES table without realizing the
    // diagnostic / printability invariants depend on them.
    const SRC: &str = include_str!("../src/types/builtins.rs");
    for needle in &["\"Bytes\"", "\"TcpListener\"", "\"TcpStream\""] {
        assert!(
            SRC.contains(needle),
            "src/types/builtins.rs missing BUILTIN_TYPES entry {needle}. \
             Re-add it (kind: Container, arity: Some(0)) — Bytes printability \
             via Display and the orphan-rule diagnostic for these three \
             types depend on this registration."
        );
    }
}
