//! Round-92 lock tests: piping silt's stdout into a consumer that
//! exits early (`silt run loud.silt | head -2`) must terminate the
//! process quietly, like every other unix CLI.
//!
//! Pre-fix, the Rust runtime's default ignored-SIGPIPE disposition
//! turned writes to the closed pipe into EPIPE errors, which
//! `println!` converts into a panic — so the user saw
//!
//!   thread 'silt-main' panicked at ...:
//!   failed printing to stdout: Broken pipe (os error 32)
//!   note: run with `RUST_BACKTRACE=1` ...
//!
//! on stderr for `silt run … | head`, and `silt disasm … | head`
//! additionally exited 101 (the panic propagated through `main`'s
//! `resume_unwind`). Fixed by resetting SIGPIPE to SIG_DFL at the very
//! top of `main()` in src/main.rs (before any thread spawn or output).
//!
//! Unix-only by nature: SIGPIPE does not exist on Windows.
#![cfg(unix)]

use std::process::Command;

/// Write `src` to a temp file and run `sh -c "<silt> <subcmd> <file> |
/// head -1"`, returning the *pipeline's* combined stderr (sh inherits
/// silt's stderr, so any panic text silt prints lands here).
fn pipe_into_head(label: &str, subcmd: &str, src: &str) -> String {
    let tmp = std::env::temp_dir().join(format!("silt_round92_sigpipe_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let cmd = format!("'{bin}' {subcmd} '{}' | head -1", tmp.display(),);
    let out = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .expect("spawn sh pipeline");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A program that prints far more lines than `head` consumes, so the
/// write side is guaranteed to hit a closed pipe. Recursion instead of
/// a loop because silt has no loop statement; depth stays well within
/// the VM's limits.
const LOUD_PROGRAM: &str = r#"
fn shout(i: Int) {
  match i > 0 {
    true -> {
      println(i)
      shout(i - 1)
    },
    false -> ()
  }
}

fn main() {
  shout(100000)
}
"#;

fn assert_no_panic_spew(label: &str, stderr: &str) {
    assert!(
        !stderr.contains("panicked"),
        "{label}: stderr must not contain a Rust panic; stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("Broken pipe"),
        "{label}: stderr must not contain the EPIPE panic message; stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("RUST_BACKTRACE"),
        "{label}: stderr must not contain the panic backtrace note; stderr={stderr:?}"
    );
}

/// Pre-fix: raw `thread 'silt-main' panicked ... Broken pipe` spew.
#[test]
fn run_piped_into_head_exits_silently() {
    let stderr = pipe_into_head("run", "run", LOUD_PROGRAM);
    assert_no_panic_spew("run | head -1", &stderr);
}

/// Pre-fix: same panic text, and the silt process exited 101 because
/// nothing in the disasm path caught the unwind.
#[test]
fn disasm_piped_into_head_exits_silently() {
    let stderr = pipe_into_head("disasm", "disasm", LOUD_PROGRAM);
    assert_no_panic_spew("disasm | head -1", &stderr);
}
