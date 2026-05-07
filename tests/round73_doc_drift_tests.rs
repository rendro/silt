//! Round-73 DOC agent locks for the seven doc drift findings.
//!
//! Findings covered:
//!
//! - B4: `docs/language/loops-and-pipes.md` and `docs/concurrency.md`
//!   no longer claim that `loop {}` (no bindings) repeats indefinitely.
//!   Reality: `loop` only re-enters when the body explicitly calls
//!   `loop(...)` (the labeled re-entry call). Bare `loop { body }`
//!   runs the body exactly once. Walker test: the corrected
//!   `loop _ = () { ... loop(()) }` snippet type-checks.
//! - B5: `docs/concurrency.md` correctly enumerates all four
//!   `ChannelResult` variants (`Message`, `Closed`, `Empty`, `Sent`)
//!   for both `channel.receive` and `channel.try_receive`. Source-grep
//!   lock against the old "two variants" / "three variants" claims.
//! - B6: `docs/language/error-handling.md` no longer claims
//!   `"{e}"` interpolation gives the same rendered message as
//!   `e.message()`. The runtime currently renders the variant
//!   constructor form for stdlib error enums; that fix is deferred,
//!   so the doc drops the false equivalence.
//! - B7: `docs/language/types.md` and `docs/language/modules.md` add
//!   commas between enum variant declarations. Walker test: the
//!   corrected snippet type-checks.
//! - B8: `docs/language/bindings-and-functions.md` no longer shows
//!   `let [a, b, c] = [1, 2, 3]` as a legal destructuring form
//!   (the round-36 soundness fix rejects refutable list patterns in
//!   `let`). Walker test: the corrected `match` / `when let ... else`
//!   snippet type-checks; the old form is rejected.
//! - B9: `docs/concurrency.md` no longer claims a deterministic
//!   round-robin pattern (1,4)/(2,5)/(3,6) for fan-out workers. The
//!   wording is now "fair, not ordered" — matching the existing
//!   correct claim at concurrency.md:434.
//! - L3: `docs/proposals/effect-rows.md` no longer uses the
//!   non-existent `++` concat operator, the non-existent
//!   `fs.read_to_string`, or bare `.unwrap()`. The corrected snippet
//!   uses `+`, `io.read_file`, and `.unwrap_or(...)`.
//!
//! For doc fixes the primary lock is a source-grep against the
//! committed file content (so the assertion isn't tautological — it
//! pins what the doc actually contains). Behavioral walkers compile
//! the corrected snippets via `silt check` to prove they are real,
//! working silt.

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn read_doc(rel: &str) -> String {
    let path = manifest_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round73_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

fn check_ok(dir_suffix: &str, src: &str) {
    let dir = scratch_dir(dir_suffix);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "silt check should succeed for {dir_suffix} snippet.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

fn check_err_contains(dir_suffix: &str, src: &str, needle: &str) {
    let dir = scratch_dir(dir_suffix);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "silt check should reject {dir_suffix} snippet; got success.\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains(needle),
        "silt check stderr for {dir_suffix} should contain {needle:?}.\nstderr:\n{stderr}"
    );
}

// ────────────────────────────────────────────────────────────────────
// B4: `loop {}` (no bindings) does NOT iterate indefinitely
// ────────────────────────────────────────────────────────────────────

const LOOPS_DOC_PATH: &str = "docs/language/loops-and-pipes.md";
const LOOPS_DOC_SRC: &str = include_str!("../docs/language/loops-and-pipes.md");

const CONCURRENCY_DOC_PATH: &str = "docs/concurrency.md";
const CONCURRENCY_DOC_SRC: &str = include_str!("../docs/concurrency.md");

#[test]
fn b4_loops_doc_does_not_claim_bare_loop_iterates() {
    let runtime = read_doc(LOOPS_DOC_PATH);
    assert_eq!(LOOPS_DOC_SRC, runtime, "include_str! and runtime read disagree");

    assert!(
        !LOOPS_DOC_SRC.contains("repeats its body indefinitely"),
        "loops-and-pipes.md must not claim `loop {{}}` `repeats its body indefinitely` — \
         only labeled re-entry via `loop(...)` triggers JumpBack."
    );
    assert!(
        !LOOPS_DOC_SRC.contains("A `loop` without bindings repeats"),
        "loops-and-pipes.md must not say `A loop without bindings repeats ...`."
    );
    assert!(
        LOOPS_DOC_SRC.contains("runs the body **once**"),
        "loops-and-pipes.md should explicitly state that bare `loop {{ body }}` runs the body once."
    );
    assert!(
        LOOPS_DOC_SRC.contains("loop(())"),
        "loops-and-pipes.md should show the corrected `loop(())` re-entry form."
    );
}

#[test]
fn b4_concurrency_doc_no_bare_loop_with_no_recur() {
    let runtime = read_doc(CONCURRENCY_DOC_PATH);
    assert_eq!(CONCURRENCY_DOC_SRC, runtime);

    // The corrected select example must show explicit `loop(())` re-entry.
    assert!(
        CONCURRENCY_DOC_SRC.contains("loop _ = () {"),
        "concurrency.md select snippet should bind a sentinel state via `loop _ = ()`."
    );
    assert!(
        CONCURRENCY_DOC_SRC.contains("loop(())"),
        "concurrency.md select snippet should re-enter via `loop(())`."
    );
}

#[test]
fn b4_loop_no_bindings_doc_corrected() {
    // Behavioral lock (round-75 L7 tightening): the corrected loop
    // pattern actually loops at *runtime*, not just type-check. We send
    // 3 messages, close the channel, and confirm the loop drains all 3
    // before exiting on `Closed`. Earlier this test only ran `silt
    // check`, which would still pass if the body ran zero or one times
    // — the doc claim "loop drains all 3 before exiting on Closed" is
    // behavioral, so we now exercise it via `silt run` and assert the
    // three printed values appear in order.
    let src = r#"
import channel

fn main() {
  let ch = channel.new(3)
  channel.send(ch, "a")
  channel.send(ch, "b")
  channel.send(ch, "c")
  channel.close(ch)
  loop _ = () {
    match channel.receive(ch) {
      Message(val) -> {
        println(val)
        loop(())
      }
      Closed -> ()
      _ -> loop(())
    }
  }
}
"#;
    let dir = scratch_dir("b4_loop_unit_run");
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("silt run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "silt run should succeed for b4_loop_unit snippet.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The loop must drain all three messages before the Closed arm
    // terminates. A revert that makes `loop _ = ()` run the body once
    // would print only "a" and silently drop "b" and "c" — caught here.
    assert!(
        stdout.contains("a") && stdout.contains("b") && stdout.contains("c"),
        "loop _ = () should drain all three channel messages; got \
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Order matters: channel sends are FIFO and the loop must process
    // them in send order. Lock the exact ordering so a regression that
    // reverses or skips messages is caught.
    let pa = stdout.find('a').expect("missing 'a'");
    let pb = stdout.find('b').expect("missing 'b'");
    let pc = stdout.find('c').expect("missing 'c'");
    assert!(
        pa < pb && pb < pc,
        "loop drain order should be a, b, c; got stdout:\n{stdout}"
    );

    // Behavioral lock #2: the recursive-fn alternative type-checks.
    let drain_src = r#"
import channel
fn process(v) { println("{v}") }
fn drain(ch) {
  match channel.receive(ch) {
    Message(val) -> {
      process(val)
      drain(ch)
    }
    Closed -> ()
    _ -> drain(ch)
  }
}
fn main() {
  let ch = channel.new(2)
  channel.send(ch, "x")
  channel.close(ch)
  drain(ch)
}
"#;
    check_ok("b4_drain_fn", drain_src);
}

// ────────────────────────────────────────────────────────────────────
// B5: ChannelResult has 4 variants
// ────────────────────────────────────────────────────────────────────

#[test]
fn b5_concurrency_doc_lists_all_four_channel_result_variants() {
    let doc = CONCURRENCY_DOC_SRC;

    // Old false claim: receive returns "one of two variants".
    assert!(
        !doc.contains("returns one of\ntwo variants"),
        "concurrency.md must not claim channel.receive returns `two variants`."
    );
    assert!(
        !doc.contains("Returns one of three variants"),
        "concurrency.md must not claim channel.try_receive returns `three variants` only."
    );

    // Positive shape: `channel.receive` prose mentions all four variants and the four-variant enum.
    assert!(
        doc.contains("four variants"),
        "concurrency.md should describe ChannelResult as having four variants."
    );
    // The receive section must name Sent and Empty as part of the shared enum.
    assert!(
        doc.contains("ChannelResult(a)") && doc.contains("`Sent`") && doc.contains("`Empty`"),
        "concurrency.md must reference ChannelResult(a) and call out Sent/Empty."
    );

    // Quick Reference rows must reference ChannelResult, not the truncated `Message or Closed` form.
    assert!(
        !doc.contains("`Message(val)` or `Closed` |"),
        "concurrency.md Quick Reference must not reduce ChannelResult to `Message or Closed`."
    );
    assert!(
        !doc.contains("`Message(val)`, `Empty`, or `Closed` |"),
        "concurrency.md Quick Reference must not reduce ChannelResult to three variants."
    );
    assert!(
        doc.contains("| Receive (blocking) | `channel.receive(ch)` | `ChannelResult(a)`"),
        "concurrency.md Quick Reference receive row should be typed as `ChannelResult(a)`."
    );
    assert!(
        doc.contains("| Try receive | `channel.try_receive(ch)` | `ChannelResult(a)`"),
        "concurrency.md Quick Reference try_receive row should be typed as `ChannelResult(a)`."
    );
}

#[test]
fn b5_channel_result_exhaustiveness_walker() {
    // Behavioral cross-check: silt's exhaustiveness checker requires all
    // four ChannelResult variants in any `match` over `channel.receive`.
    // A 2-arm match must therefore fail without a catch-all.
    let two_arm = r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.close(ch)
  match channel.receive(ch) {
    Message(_) -> ()
    Closed -> ()
  }
}
"#;
    let dir = scratch_dir("b5_two_arm");
    let main = write_main(&dir, two_arm);
    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        !output.status.success(),
        "silt check should reject a non-exhaustive `match` over ChannelResult \
         that omits Empty/Sent.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The diagnostic should mention non-exhaustiveness somehow; we keep
    // the assertion loose to avoid pinning the exact message.
    assert!(
        combined.to_lowercase().contains("exhaust")
            || combined.contains("not covered")
            || combined.contains("missing"),
        "silt diagnostic for non-exhaustive ChannelResult match should reference \
         exhaustiveness/missing arms.\noutput:\n{combined}"
    );

    // Four-arm match must succeed.
    let four_arm = r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.close(ch)
  match channel.receive(ch) {
    Message(_) -> ()
    Closed -> ()
    Empty -> ()
    Sent -> ()
  }
}
"#;
    check_ok("b5_four_arm", four_arm);
}

// ────────────────────────────────────────────────────────────────────
// B6: `"{e}"` interpolation is NOT equivalent to `e.message()`
// ────────────────────────────────────────────────────────────────────

const ERR_DOC_PATH: &str = "docs/language/error-handling.md";
const ERR_DOC_SRC: &str = include_str!("../docs/language/error-handling.md");

#[test]
fn b6_error_handling_doc_asserts_display_message_equivalence() {
    // Round 73 originally rewrote this passage to acknowledge the
    // dual shape between `"{e}"` and `e.message()`. Round 73
    // follow-up shipped the runtime fix (collapse to one form via
    // `vm::dispatch::render_stdlib_error_message`) so the doc reverts
    // to the principled equivalence claim — see
    // `tests/round73f_deferred_fixes_tests.rs::fix1_*` for the
    // sibling locks (runtime behavior + helper presence).
    let runtime = read_doc(ERR_DOC_PATH);
    assert_eq!(ERR_DOC_SRC, runtime);

    // Doc must claim equivalence in some honest form.
    assert!(
        ERR_DOC_SRC.contains("produce the same text"),
        "error-handling.md should claim `\"{{e}}\"` and `e.message()` produce the same text \
         now that the runtime fix backs the equivalence."
    );
    // The conservative caveat from round 73 (doc-only fix) must be gone.
    assert!(
        !ERR_DOC_SRC.contains("variant constructor form"),
        "error-handling.md should no longer warn about the \"variant constructor form\" \
         divergence — the runtime now collapses the dual shape."
    );
}

// ────────────────────────────────────────────────────────────────────
// B7: enum variant declarations need commas
// ────────────────────────────────────────────────────────────────────

const TYPES_DOC_PATH: &str = "docs/language/types.md";
const TYPES_DOC_SRC: &str = include_str!("../docs/language/types.md");

const MODULES_DOC_PATH: &str = "docs/language/modules.md";
const MODULES_DOC_SRC: &str = include_str!("../docs/language/modules.md");

#[test]
fn b7_types_md_enum_variants_use_commas() {
    let runtime = read_doc(TYPES_DOC_PATH);
    assert_eq!(TYPES_DOC_SRC, runtime);

    // Old broken Shape snippet had bare `Circle(Float)` followed by `Rect(Float, Float)` (no comma).
    assert!(
        !TYPES_DOC_SRC.contains("Circle(Float)\n  Rect(Float, Float)\n}"),
        "types.md Shape snippet must use commas between variants."
    );
    assert!(
        TYPES_DOC_SRC.contains("Circle(Float),\n  Rect(Float, Float),\n}"),
        "types.md Shape snippet should use commas between Circle and Rect."
    );

    // Old broken Expr snippet.
    assert!(
        !TYPES_DOC_SRC.contains("Num(Int)\n  Add(Expr, Expr)\n}"),
        "types.md Expr snippet must use commas between variants."
    );
    assert!(
        TYPES_DOC_SRC.contains("Num(Int),\n  Add(Expr, Expr),\n}"),
        "types.md Expr snippet should use commas between Num and Add."
    );
}

#[test]
fn b7_modules_md_pub_type_uses_commas() {
    let runtime = read_doc(MODULES_DOC_PATH);
    assert_eq!(MODULES_DOC_SRC, runtime);

    assert!(
        !MODULES_DOC_SRC.contains("Circle(Int)\n  Square(Int)\n}"),
        "modules.md `pub type Shape` snippet must use commas between variants."
    );
    assert!(
        MODULES_DOC_SRC.contains("Circle(Int),\n  Square(Int),\n}"),
        "modules.md `pub type Shape` snippet should use commas between Circle and Square."
    );
}

#[test]
fn b7_corrected_enum_snippets_typecheck() {
    // Behavioral lock: the corrected snippets are valid silt.
    let src = r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

type Color { Red, Green, Blue }

type Expr {
  Num(Int),
  Add(Expr, Expr),
}

fn main() {
  let _s = Circle(5.0)
  let _r = Rect(3.0, 4.0)
  let _c = Red
  let _e = Add(Num(1), Num(2))
  ()
}
"#;
    check_ok("b7_types", src);

    let mod_src = r#"
pub fn add(a, b) { a + b }
fn helper(x) { x * 2 }

pub type Point { x: Int, y: Int }
pub type Shape {
  Circle(Int),
  Square(Int),
}

fn main() {
  let _p = Point { x: 1, y: 2 }
  let _s = Circle(5)
  let _q = Square(3)
  let _h = helper(add(1, 2))
  ()
}
"#;
    check_ok("b7_modules", mod_src);
}

// ────────────────────────────────────────────────────────────────────
// B8: list destructuring in `let` is rejected
// ────────────────────────────────────────────────────────────────────

const BIND_DOC_PATH: &str = "docs/language/bindings-and-functions.md";
const BIND_DOC_SRC: &str = include_str!("../docs/language/bindings-and-functions.md");

#[test]
fn b8_bindings_doc_drops_let_list_destructure_example() {
    let runtime = read_doc(BIND_DOC_PATH);
    assert_eq!(BIND_DOC_SRC, runtime);

    // Old broken example.
    assert!(
        !BIND_DOC_SRC.contains("let [a, b, c] = [1, 2, 3]"),
        "bindings-and-functions.md must not show `let [a, b, c] = [1, 2, 3]` — \
         round-36 rejects refutable list patterns in `let`."
    );
    // Positive shape: the doc points at `match` / `when let ... else`.
    assert!(
        BIND_DOC_SRC.contains("when let [a, b, c]"),
        "bindings-and-functions.md should show `when let [a, b, c] = ... else {{ ... }}` as the alternative."
    );
    assert!(
        BIND_DOC_SRC.contains("refutable"),
        "bindings-and-functions.md should explain that list patterns are refutable."
    );
    // Tuple/record let destructuring should still be documented.
    assert!(
        BIND_DOC_SRC.contains("let (x, y) = (1, \"hello\")"),
        "bindings-and-functions.md should preserve the tuple `let` destructuring example."
    );
    assert!(
        BIND_DOC_SRC.contains("let User { name, age, .. } = user"),
        "bindings-and-functions.md should preserve the record `let` destructuring example."
    );
}

#[test]
fn b8_let_list_pattern_still_rejected() {
    // Behavioral lock: silt rejects `let [a, b, c] = xs` with the round-36 diagnostic.
    let src = r#"
fn main() {
  let xs = [1, 2, 3]
  let [a, b, c] = xs
  ()
}
"#;
    check_err_contains("b8_let_list", src, "refutable pattern in `let`");
}

#[test]
fn b8_corrected_match_and_when_let_typecheck() {
    let src = r#"
fn handle_other_shape() { 0 }
fn use(a, b, c) { a + b + c }

fn solve(xs) {
  let _r = match xs {
    [a, b, c] -> use(a, b, c)
    _ -> handle_other_shape()
  }

  when let [a, b, c] = xs else { return handle_other_shape() }
  use(a, b, c)
}

fn main() {
  println("{solve([1, 2, 3])}")
}
"#;
    check_ok("b8_match_when_let", src);
}

// ────────────────────────────────────────────────────────────────────
// B9: round-robin "exact pattern" claim
// ────────────────────────────────────────────────────────────────────

#[test]
fn b9_concurrency_doc_drops_deterministic_round_robin_claim() {
    let doc = CONCURRENCY_DOC_SRC;

    assert!(
        !doc.contains("each worker processes exactly two"),
        "concurrency.md must not claim `each worker processes exactly two`."
    );
    assert!(
        !doc.contains("worker 1 gets messages 1 and 4"),
        "concurrency.md must not claim a deterministic 1-and-4 / 2-and-5 / 3-and-6 split."
    );
    // Positive shape: the corrected wording emphasizes fairness, not
    // ordering — matching the existing correct claim at line ~434.
    assert!(
        doc.contains("fair, not ordered"),
        "concurrency.md fan-out section should describe distribution as `fair, not ordered`."
    );
    assert!(
        doc.contains("no worker is guaranteed exactly two"),
        "concurrency.md fan-out section should explicitly disclaim the deterministic split."
    );
}

// ────────────────────────────────────────────────────────────────────
// L3: effect-rows.md proposal uses current silt syntax
// ────────────────────────────────────────────────────────────────────

const EFFECT_DOC_PATH: &str = "docs/proposals/effect-rows.md";
const EFFECT_DOC_SRC: &str = include_str!("../docs/proposals/effect-rows.md");

#[test]
fn l3_effect_rows_doc_uses_current_silt_syntax() {
    let runtime = read_doc(EFFECT_DOC_PATH);
    assert_eq!(EFFECT_DOC_SRC, runtime);

    // No more `++` concat operator (silt uses `+` for String concat).
    assert!(
        !EFFECT_DOC_SRC.contains("env.home() ++ \""),
        "effect-rows.md must not use the non-existent `++` concat operator."
    );
    // No more non-existent `fs.read_to_string` (it's `io.read_file`).
    assert!(
        !EFFECT_DOC_SRC.contains("fs.read_to_string"),
        "effect-rows.md must not reference the non-existent `fs.read_to_string` function."
    );
    // No more bare `.unwrap()` (silt only has `.unwrap_or(...)`).
    assert!(
        !EFFECT_DOC_SRC.contains("toml.parse(raw).unwrap()"),
        "effect-rows.md must not call bare `.unwrap()` (silt only exposes `.unwrap_or`)."
    );

    // Round-74 follow-up: `env.home()` does not exist in silt's stdlib
    // (only `env.get`, `env.set`, `env.remove`, `env.vars`), and bare
    // `.unwrap_or(...)` is not a method (the stdlib uses module-qualified
    // `option.unwrap_or` / `result.unwrap_or`). The corrected snippet
    // pipes `env.get("HOME")` into `option.unwrap_or` and the result
    // values into `result.unwrap_or`.
    assert!(
        !EFFECT_DOC_SRC.contains("env.home("),
        "effect-rows.md must not call the non-existent `env.home()` — \
         use `env.get(\"HOME\") |> option.unwrap_or(...)` instead."
    );
    assert!(
        EFFECT_DOC_SRC.contains("env.get(\"HOME\")"),
        "effect-rows.md should call `env.get(\"HOME\")` (the actual stdlib function)."
    );
    assert!(
        EFFECT_DOC_SRC.contains("io.read_file(path)"),
        "effect-rows.md should call `io.read_file` (the actual stdlib function)."
    );
    assert!(
        EFFECT_DOC_SRC.contains("option.unwrap_or("),
        "effect-rows.md should use `option.unwrap_or(...)` for the `env.get` Option."
    );
    assert!(
        EFFECT_DOC_SRC.contains("result.unwrap_or("),
        "effect-rows.md should use `result.unwrap_or(...)` for Result values \
         (silt has no bare `.unwrap_or` method)."
    );
}

// ────────────────────────────────────────────────────────────────────
// Round-76 B4 (Doc1) extension: ```silt fences that contain non-silt
// syntax must be retagged ```text (or ```pseudo) so a reader knows
// they are illustrative pseudocode, not runnable silt. The three new
// offenders introduced by the original L3 fix were:
//   - `.await?` (silt has no async/await)
//   - `module config_loader exports load_defaults, save_user_pref`
//     (silt files are modules implicitly; no `module` keyword)
//   - `Repo, Report, AppError` (undefined stand-in types)
// ────────────────────────────────────────────────────────────────────

#[test]
fn round76_effect_rows_doc_pseudocode_blocks_retagged() {
    let doc = EFFECT_DOC_SRC;

    // The illustrative snippets must NOT be tagged ```silt —
    // `silt check` would fail on them. They live in ```text fences
    // now so a reader (and the doc walker below) knows they are
    // pseudocode.
    //
    // Walk fence-by-fence and assert no ```silt fence contains any
    // of the offender substrings.
    for offender in [
        ".await?",
        "module config_loader exports",
        "Repo, Report, AppError",
    ] {
        let mut in_silt_fence = false;
        let mut found_in_silt = false;
        for line in doc.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```silt") {
                in_silt_fence = true;
                continue;
            }
            if trimmed.starts_with("```") {
                in_silt_fence = false;
                continue;
            }
            if in_silt_fence && line.contains(offender) {
                found_in_silt = true;
                break;
            }
        }
        assert!(
            !found_in_silt,
            "effect-rows.md offender {offender:?} appears inside a ```silt fence — \
             retag the fence as ```text or rewrite the snippet to real silt."
        );
    }
}

#[test]
fn round76_effect_rows_all_silt_fences_parse() {
    // Positive assertion: every ```silt fence in effect-rows.md must
    // parse via `silt check` (or be tagged ```text/```pseudo). The
    // three problem fences from B4 were retagged to ```text in
    // round 76; this test guards against future drift in either
    // direction (un-retagging, or new ```silt fences with bad
    // syntax).
    let doc = EFFECT_DOC_SRC;

    let mut current: Option<String> = None;
    let mut snippets: Vec<String> = Vec::new();
    for line in doc.lines() {
        let trimmed = line.trim_start();
        if let Some(buf) = current.as_mut() {
            if trimmed.starts_with("```") {
                snippets.push(std::mem::take(buf));
                current = None;
            } else {
                buf.push_str(line);
                buf.push('\n');
            }
        } else if trimmed.starts_with("```silt") {
            current = Some(String::new());
        }
    }

    for (idx, snippet) in snippets.iter().enumerate() {
        let dir = scratch_dir(&format!("round76_effect_silt_{idx}"));
        let main = write_main(&dir, snippet);
        let output = silt_bin()
            .args(["check", main.to_str().unwrap()])
            .output()
            .expect("silt check");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "effect-rows.md ```silt fence #{idx} did NOT parse via `silt check`. \
             Either retag it to ```text/```pseudo (illustrative pseudocode) or \
             rewrite the snippet to runnable silt.\n\
             snippet:\n{snippet}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}

// ────────────────────────────────────────────────────────────────────
// Round-76 G3 (Doc4): docs/concurrency.md task.cancel section must
// enumerate the third "queued but not yet running" case, matching
// the wording in src/typechecker/builtins/docs.rs:812-816.
// ────────────────────────────────────────────────────────────────────

#[test]
fn round76_concurrency_doc_task_cancel_third_case() {
    let doc = CONCURRENCY_DOC_SRC;

    // The doc must mention the queued-not-yet-scheduled case in the
    // task.cancel section, matching the builtin doc at
    // src/typechecker/builtins/docs.rs:812-816 ("queued but not yet
    // scheduled — handle resolves to Err immediately, but a slice
    // may still run if scheduler picks it up").
    assert!(
        doc.contains("queued but not yet"),
        "concurrency.md task.cancel section should enumerate the \
         `queued but not yet running/scheduled` case (third bullet) — \
         matching the builtin doc at src/typechecker/builtins/docs.rs."
    );
    // The doc should specifically reference the first-writer-wins
    // semantics shared with the builtin doc.
    assert!(
        doc.contains("first-writer-wins"),
        "concurrency.md task.cancel section should reference \
         `first-writer-wins` semantics."
    );
}
