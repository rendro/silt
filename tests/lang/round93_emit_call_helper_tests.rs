//! Round 93 lock: the tail/non-tail call emission was duplicated at five
//! sites in `src/compiler/mod.rs` (normal calls, qualified-variant calls,
//! module-qualified calls, and both pipe shapes); round 93 collapsed them
//! into a single `emit_call(argc, tail, span)` helper. The extraction must
//! be a behavioral no-op, so these tests pin the OBSERVABLE behavior of
//! each call shape through the real compile+execute pipeline:
//!
//!   - tail position  => `TailCall` (deep recursion completes, far past
//!     the VM's 100k MAX_FRAMES cap), and
//!   - non-tail value => `Call` (the call's result is actually usable,
//!     i.e. no frame was thrown away).
//!
//! Also greps the compiler source so the duplication cannot quietly come
//! back: `Op::TailCall` must be emitted from exactly one production site
//! (the helper).

use std::process::Command;

/// Run a Silt source program via the CLI; return (stdout, stderr, success).
fn run_silt(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!(
        "silt_round93_emitcall_{}_{}.silt",
        std::process::id(),
        label
    ));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

fn assert_tco(label: &str, stdout: &str, stderr: &str, ok: bool) {
    assert!(
        ok && !stderr.contains("stack overflow"),
        "{label}: 2M-deep tail recursion must complete via TailCall; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(stdout.trim(), "0", "{label}: stderr={stderr:?}");
}

// ── Shape 1: normal function call ────────────────────────────────────

#[test]
fn normal_call_shape_tco_and_value() {
    let (stdout, stderr, ok) = run_silt(
        "normal",
        r#"
fn countdown(n) {
  match n {
    0 -> 0
    _ -> countdown(n - 1)
  }
}

fn main() {
  let direct = countdown(3)
  println("{direct}")
  println("{countdown(2000000)}")
}
"#,
    );
    assert!(ok, "stderr={stderr:?}");
    assert!(!stderr.contains("stack overflow"), "stderr={stderr:?}");
    // Non-tail use returns a usable value; tail use recurses 2M deep.
    assert_eq!(stdout.trim(), "0\n0", "stderr={stderr:?}");
}

// ── Shape 2: pipe with call on the right (`val |> f(args)`) ──────────

#[test]
fn pipe_call_shape_tco_and_value() {
    let (stdout, stderr, ok) = run_silt(
        "pipe_call",
        r#"
fn dec(n, by) {
  countdown(n - by)
}

fn countdown(n) {
  match n {
    0 -> 0
    _ -> n |> dec(1)
  }
}

fn main() {
  println("{countdown(2000000)}")
}
"#,
    );
    assert_tco("pipe_call", &stdout, &stderr, ok);
}

// ── Shape 3: bare pipe (`val |> f`) ──────────────────────────────────

#[test]
fn pipe_bare_shape_tco_and_value() {
    let (stdout, stderr, ok) = run_silt(
        "pipe_bare",
        r#"
fn countdown(n) {
  match n {
    0 -> 0
    _ -> n - 1 |> countdown
  }
}

fn main() {
  println("{countdown(2000000)}")
}
"#,
    );
    assert_tco("pipe_bare", &stdout, &stderr, ok);
}

// ── Shape 4: module-qualified call (`mod.f(args)`) ───────────────────

#[test]
fn module_qualified_call_shape_tco_and_value() {
    let dir =
        std::env::temp_dir().join(format!("silt_round93_emitcall_mod_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(
        dir.join("helper.silt"),
        r#"
pub fn bounce(f, n) {
  f(n)
}
"#,
    )
    .expect("write helper module");
    let main_path = dir.join("main.silt");
    // Each step makes a module-qualified call (`helper.bounce`) in tail
    // position; `bounce` tail-calls back into `countdown`. 2M rounds only
    // complete if BOTH sites emit TailCall — a plain Call at the
    // module-qualified site would grow 2M frames and blow MAX_FRAMES.
    std::fs::write(
        &main_path,
        r#"
import helper

fn countdown(n) {
  match n {
    0 -> 0
    _ -> helper.bounce(countdown, n - 1)
  }
}

fn main() {
  let direct = helper.bounce(countdown, 3)
  println("{direct}")
  println("{countdown(2000000)}")
}
"#,
    )
    .expect("write main");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&main_path)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success() && !stderr.contains("stack overflow"),
        "module-qualified tail recursion must complete; stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(stdout.trim(), "0\n0", "stderr={stderr:?}");
}

// ── Shape 5: qualified variant call (`Enum.Variant(args)`) ───────────

/// Qualified-variant calls go through the same emit path; in non-tail
/// position the constructed value must be usable (i.e. `Call`, not a
/// frame-discarding `TailCall`).
#[test]
fn qualified_variant_call_shape_value() {
    let (stdout, stderr, ok) = run_silt(
        "variant",
        r#"
type Wrapped {
  Box(Int)
}

fn wrap(n) {
  let w = Wrapped.Box(n)
  match w {
    Box(v) -> v + 1
  }
}

fn main() {
  println("{wrap(41)}")
}
"#,
    );
    assert!(ok, "stderr={stderr:?}");
    assert_eq!(stdout.trim(), "42", "stderr={stderr:?}");
}

// ── Module-internal TCO (round 93 companion fix) ─────────────────────

/// Functions DEFINED IN an imported module must also get TCO. Pre-round-93
/// the module fn-decl compile path never set `in_tail_position` before
/// compiling the body (unlike the top-level `Decl::Fn` path), so
/// self-recursion inside a module blew MAX_FRAMES at 100k depth.
#[test]
fn module_internal_tail_recursion_eliminated() {
    let dir = std::env::temp_dir().join(format!(
        "silt_round93_emitcall_modself_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(
        dir.join("counter.silt"),
        r#"
pub fn selfcount(n) {
  match n {
    0 -> 0
    _ -> selfcount(n - 1)
  }
}
"#,
    )
    .expect("write counter module");
    let main_path = dir.join("main.silt");
    std::fs::write(
        &main_path,
        r#"
import counter

fn main() {
  println("{counter.selfcount(2000000)}")
}
"#,
    )
    .expect("write main");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&main_path)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success() && !stderr.contains("stack overflow"),
        "module-internal tail recursion must complete; stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(stdout.trim(), "0", "stderr={stderr:?}");
}

// ── Source-level lock: one emission site only ────────────────────────

/// `Op::TailCall` must be emitted from exactly ONE production site in the
/// compiler — the `emit_call` helper. (Test-module assertions referencing
/// the opcode are fine; new `emit_op(Op::TailCall, ...)` call sites are
/// not.) This pins the round-93 dedup of the five copy-pasted
/// tail/non-tail emission blocks.
#[test]
fn tailcall_emitted_from_single_helper_site() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/compiler/mod.rs"))
        .expect("read compiler source");
    let emit_sites = src
        .lines()
        .filter(|l| l.contains("emit_op(Op::TailCall"))
        .count();
    assert_eq!(
        emit_sites, 1,
        "Op::TailCall must be emitted only from the emit_call helper; \
         found {emit_sites} `emit_op(Op::TailCall` sites — route new call \
         shapes through emit_call(argc, tail, span) instead"
    );
}
