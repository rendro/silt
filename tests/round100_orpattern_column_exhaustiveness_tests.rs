//! Round-100 BROKEN (well-typed program rejected): an or-pattern in a
//! NON-trailing match column was dropped during Maranget specialization,
//! so an exhaustive match was wrongly reported as non-exhaustive.
//!
//! `expand_or` flattened only a TOP-LEVEL `A | B`; an or buried inside a
//! tuple column (`(A | B, true)`), a constructor payload (`P(A | B, _)`),
//! or a list element survived into the specialization loops, whose arms
//! match rows only by `Tuple(ps)` / `Wildcard` / `Ident` and silently
//! dropped any row whose column was an `Or`. Dropping those rows shrank
//! coverage, so the checker rejected exhaustive matches.
//!
//! Direction is over-rejection only (never a runtime soundness hole), but
//! it refused legitimate, exhaustive matches. Fix: `expand_or_deep`
//! distributes ors at EVERY nesting level into separate or-free matrix
//! rows before specialization.
//!
//! Every test shells out to the compiled CLI binary, exercising the full
//! parser → typechecker (`check`) and, where a value is produced, the VM
//! (`run`) so the static checker and runtime are locked in agreement.

use std::process::Command;

fn silt(label: &str, sub: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_r100_orpat_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(sub)
        .arg(&tmp)
        .output()
        .expect("spawn silt");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn tuple_column_or_pattern_is_exhaustive_without_wildcard() {
    let src = "type T { A, B }\n\
               fn f(x) {\n\
               \x20 match x {\n\
               \x20   (A | B, true) -> 1\n\
               \x20   (A | B, false) -> 2\n\
               \x20 }\n\
               }\n\
               fn main() { print(f((A, true))) }\n";
    let (_, stderr, ok) = silt("tuple_check", "check", src);
    assert!(
        ok,
        "or-pattern in a tuple column makes the match exhaustive; \
         `check` must exit 0. stderr:\n{stderr}"
    );
}

#[test]
fn constructor_payload_or_pattern_is_exhaustive() {
    let src = "type T { A, B }\n\
               type W { P(T, Bool) }\n\
               fn f(x) {\n\
               \x20 match x {\n\
               \x20   P(A | B, true) -> 1\n\
               \x20   P(A | B, false) -> 2\n\
               \x20 }\n\
               }\n\
               fn main() { print(f(P(A, true))) }\n";
    let (_, stderr, ok) = silt("ctor_check", "check", src);
    assert!(
        ok,
        "or-pattern inside a constructor payload must be exhaustive; \
         `check` must exit 0. stderr:\n{stderr}"
    );
}

#[test]
fn tuple_column_or_pattern_runs_each_arm() {
    // The VM must agree with the checker: each branch selects the right
    // arm. This locks the runtime path, not just the typecheck.
    let src = "type T { A, B }\n\
               fn f(x) {\n\
               \x20 match x {\n\
               \x20   (A | B, true) -> 1\n\
               \x20   (A | B, false) -> 2\n\
               \x20 }\n\
               }\n\
               fn main() { print(f((B, false))) }\n";
    let (stdout, stderr, ok) = silt("tuple_run", "run", src);
    assert!(ok, "run must succeed. stderr:\n{stderr}");
    assert_eq!(
        stdout.trim(),
        "2",
        "f((B, false)) must match the `(A | B, false)` arm and return 2"
    );
}

#[test]
fn genuinely_non_exhaustive_or_column_is_still_rejected() {
    // The fix must NOT make everything exhaustive — a real gap (missing
    // the `false` case) is still reported.
    let src = "type T { A, B }\n\
               fn f(x) {\n\
               \x20 match x {\n\
               \x20   (A | B, true) -> 1\n\
               \x20 }\n\
               }\n\
               fn main() { print(f((A, true))) }\n";
    let (_, stderr, ok) = silt("nonexh", "check", src);
    assert!(
        !ok && stderr.contains("non-exhaustive"),
        "a genuinely non-exhaustive or-column match must still be rejected. \
         stderr:\n{stderr}"
    );
}
