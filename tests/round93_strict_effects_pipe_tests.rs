//! Round 93 soundness locks: `--strict-effects` must see effects
//! performed via the pipe operator.
//!
//! GAP (round 93): the Phase-A effects walker's `ExprKind::Pipe` arm
//! was `walk(l) ∪ walk(r)`. But a pipe is a call in disguise —
//! `a |> f(b)` desugars to `f(a, b)` (inference.rs Pipe arm) — and a
//! bare-ident/dotted-path RHS contributed EMPTY via the Ident /
//! FieldAccess arms, never charging the callee's `Scheme::effects`.
//! Result: `fn shout() { "hi" |> println }` passed
//! `silt check --strict-effects` while `fn shout() { println("hi") }`
//! was correctly rejected. The fix routes both the direct-Call arm and
//! the Pipe arm through one `call_effects` helper
//! (src/typechecker/effects_infer.rs) so the two forms can never
//! diverge again.
//!
//! Each rejection test here FAILED before the fix (the file passed
//! strict check) and PASSES after.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Tempdir per test name — same zero-dependency pattern as
/// `tests/cli_strict_effects_flag_tests.rs`.
fn make_tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_r93_strict_pipe_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `src` to a file and run `silt check --strict-effects` on it.
fn strict_check(name: &str, src: &str) -> std::process::Output {
    let dir = make_tempdir(name);
    let main = dir.join("main.silt");
    std::fs::write(&main, src).unwrap();
    silt_cmd()
        .args(["check", "--strict-effects", main.to_str().unwrap()])
        .output()
        .expect("failed to run silt check --strict-effects")
}

// ── 1. bare-ident pipe RHS is charged the callee's effects ────────

#[test]
fn pure_fn_piping_into_println_is_rejected() {
    let out = strict_check(
        "bare_ident",
        "fn shout() { \"hi\" |> println }\nfn main() = ()\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`\"hi\" |> println` performs !{{io}}; strict mode must reject the \
         unannotated fn exactly like `println(\"hi\")` does, got success"
    );
    assert!(
        stderr.contains("uses effect !{io}") && stderr.contains("declared pure"),
        "expected the standard strict-mode diagnostic naming !{{io}}, got: {stderr}"
    );
    assert!(
        stderr.contains("'shout'"),
        "diagnostic must name the offending fn 'shout', got: {stderr}"
    );
}

// ── 2. module-qualified (dotted-path) pipe RHS ─────────────────────

#[test]
fn pure_fn_piping_into_module_qualified_io_is_rejected() {
    let out = strict_check(
        "dotted_path",
        "import io\nfn touch() { \"f.txt\" |> io.read_file }\nfn main() = ()\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`\"f.txt\" |> io.read_file` performs !{{fs, io}}; strict mode must \
         reject the unannotated fn, got success"
    );
    assert!(
        stderr.contains("uses effect !{fs, io}") && stderr.contains("declared pure"),
        "expected the standard strict-mode diagnostic naming !{{fs, io}}, got: {stderr}"
    );
}

// ── 3. chained pipe with an effectful stage ────────────────────────

#[test]
fn chained_pipe_with_effectful_stage_is_rejected() {
    let out = strict_check(
        "chained",
        "import string\nfn chain() { \"hi\" |> string.to_upper |> println }\nfn main() = ()\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "chained pipe ends in println (!{{io}}); strict mode must reject the \
         unannotated fn, got success"
    );
    assert!(
        stderr.contains("uses effect !{io}") && stderr.contains("declared pure"),
        "expected !{{io}} from the println stage (and ONLY io — \
         string.to_upper is pure), got: {stderr}"
    );
}

// ── 4. effectful-declared fn using pipes passes ────────────────────

#[test]
fn annotated_effectful_fn_using_pipes_passes() {
    let out = strict_check(
        "annotated_ok",
        "fn shout() !{io} { \"hi\" |> println }\nfn main() !{io} { shout() }\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "fn declared !{{io}} covers the piped println; strict check must \
         pass, got stderr: {stderr}"
    );
}

// ── 5. pure pipes stay green — no over-rejection ───────────────────

#[test]
fn pure_fn_with_pure_pipes_still_passes() {
    let out = strict_check(
        "pure_ok",
        "import list\nimport string\n\
         fn bump(xs: List(Int)) -> List(Int) { xs |> list.map({x -> x + 1}) }\n\
         fn up(s: String) -> String { s |> string.to_upper }\n\
         fn main() = ()\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "list.map and string.to_upper are pure; piping into them must not \
         be over-charged under strict mode, got stderr: {stderr}"
    );
}

// ── 6. parity lock: pipe form and direct-call form agree ───────────

#[test]
fn lambda_rhs_pipe_matches_direct_immediate_call_verdict() {
    // Phase A treats a computed callee (here: an immediately-invoked
    // lambda) as TOP — the conservative higher-order default. The pipe
    // spelling of the same call must reach the same verdict; before the
    // round-93 fix the pipe form passed while the direct form was
    // rejected.
    let piped = strict_check(
        "lambda_piped",
        "fn lam(x: Int) -> Int { x |> {y -> y + 1} }\nfn main() = ()\n",
    );
    let direct = strict_check(
        "lambda_direct",
        "fn lam(x: Int) -> Int { ({y -> y + 1})(x) }\nfn main() = ()\n",
    );
    assert_eq!(
        piped.status.success(),
        direct.status.success(),
        "pipe and direct-call spellings of the same call must agree under \
         --strict-effects; piped stderr: {}\ndirect stderr: {}",
        String::from_utf8_lossy(&piped.stderr),
        String::from_utf8_lossy(&direct.stderr)
    );
    assert!(
        !direct.status.success(),
        "higher-order/computed callee is TOP in Phase A; both forms are \
         expected to be rejected in a pure-declared fn"
    );
}
