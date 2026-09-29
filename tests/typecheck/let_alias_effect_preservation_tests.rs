//! Round 64 regression locks for the let-alias effect-preservation
//! BROKEN finding.
//!
//! Both `let`-binding paths in the typechecker call
//! `TypeChecker::generalize`, which hardcodes its result's
//! `Scheme::effects` to `EffectSet::TOP`. The doc-comment at
//! `src/typechecker/mod.rs:1593-1601` warns: "Every caller MUST
//! overwrite the field"; before round 64 the two `let`-binding callers
//! (top-level `let` in `mod.rs`, inline `let` in `inference.rs`) did
//! not. The visible symptom was that `let alias = doit` (where `doit`
//! is declared `!{io}`) widened the alias's declared effect set to the
//! full TOP row. A subsequent `alias()` call inside an enclosing fn
//! declared with anything narrower than the full five-effect row
//! tripped the body-subset check with a misleading
//! `effect '!{fs}' not declared` diagnostic.
//!
//! These locks exercise the silt CLI binary (not just `check_str`)
//! because the round-62 audit's weak-gate watch flagged that
//! typecheck-only locks for this bug class regress: the broken
//! behaviour was reachable through `silt check` despite the inner
//! invariants holding for some intermediate testing surface. CLI
//! locks pin the user-visible behaviour.

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
    let dir = std::env::temp_dir().join("silt_let_alias_effect_round64");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

// ── 1: the BROKEN repro from the audit must pass under --strict-effects ──

#[test]
fn let_alias_inside_fn_preserves_callee_io_effect() {
    // The exact repro from the round-64 audit. `doit` is declared
    // `!{io}`. `main` is declared `!{io}` and binds an alias to
    // `doit`, then calls the alias. Before the fix, the alias's
    // scheme inherited `EffectSet::TOP` from `generalize`, the body
    // walker saw the alias call as carrying every effect, and the
    // body-subset check rejected `main` with
    // `effect '!{fs}' not declared`. After the fix, the alias's
    // effects are propagated from `doit`'s scheme (typechecker side)
    // and from a let-binding alias map (effects walker side), so the
    // alias call carries `!{io}` — fits inside `main`'s `!{io}`
    // declaration.
    let path = temp_silt_file(
        "alias_preserves_io",
        r#"fn doit() !{io} { println("hi") }
fn main() !{io} {
    let alias = doit
    alias()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&path)
        .output()
        .expect("failed to run silt check --strict-effects");

    assert!(
        output.status.success(),
        "silt check --strict-effects rejected let-alias call inside !{{io}} fn:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── 2: alias call inside fn declared with FEWER effects must error ──

#[test]
fn let_alias_inside_pure_fn_still_errors_with_correct_effect() {
    // The alias preserves `doit`'s `!{io}`. In a fn that declares no
    // effects, the body-subset check must still complain — but it
    // must complain about `!{io}` specifically, not the noisy TOP
    // row that was the round-64 BROKEN symptom. The diagnostic is
    // the user-visible signal that the alias path is reporting the
    // right effect set.
    let path = temp_silt_file(
        "alias_in_pure_errors_io_only",
        r#"fn doit() !{io} { println("hi") }
fn main() {
    let alias = doit
    alias()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&path)
        .output()
        .expect("failed to run silt check --strict-effects");

    assert!(
        !output.status.success(),
        "expected --strict-effects to reject calling an !{{io}} alias from a pure-by-default fn"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}\n{stdout}");

    // Lock the diagnostic: it must mention `!{io}` (the actual
    // callee effect), not `!{fs, io, net, random, time}` (the
    // pre-fix TOP-widened row that named every effect).
    assert!(
        combined.contains("!{io}"),
        "expected diagnostic to name `!{{io}}` (the callee's actual effect set); got:\n{combined}"
    );
    assert!(
        !combined.contains("!{fs, io, net, random, time}"),
        "diagnostic still names the full TOP row — alias is widening to TOP again:\n{combined}"
    );
}

// ── 3: top-level `let alias = doit` preserves effects ─────────────────

#[test]
fn top_level_let_alias_preserves_effects_for_caller() {
    // The other generalize site is in `check_program` for top-level
    // `let` declarations. Bind a top-level alias to a !{io} fn, then
    // call the alias from inside another fn declared with !{io}.
    // Before the fix, the top-level let path (mod.rs:2814) called
    // `generalize` without overwriting `effects`, so the alias's
    // scheme entered the global env with TOP and every caller of the
    // alias saw a TOP-widened call.
    let path = temp_silt_file(
        "top_level_alias_preserves_io",
        r#"fn doit() !{io} { println("hi") }
let alias = doit
fn caller() !{io} {
    alias()
}
fn main() !{io} {
    caller()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg("--strict-effects")
        .arg(&path)
        .output()
        .expect("failed to run silt check --strict-effects");

    assert!(
        output.status.success(),
        "top-level let-alias widened a !{{io}} fn's effects to TOP:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── 4: under plain (non-strict) check, the repro must also pass ──────

#[test]
fn let_alias_repro_passes_plain_check() {
    // Mirror test 1 but without the `--strict-effects` flag. The
    // body-subset check runs unconditionally (it's not gated on
    // strict mode); the strict flag only changes how unannotated fns
    // are treated. The BROKEN diagnostic in the audit was reached
    // under plain `silt check` too, so lock both flag states.
    let path = temp_silt_file(
        "alias_preserves_plain",
        r#"fn doit() !{io} { println("hi") }
fn main() !{io} {
    let alias = doit
    alias()
}
"#,
    );

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt check");

    assert!(
        output.status.success(),
        "silt check (no flag) rejected let-alias call inside !{{io}} fn:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
