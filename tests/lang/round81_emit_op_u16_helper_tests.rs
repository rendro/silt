//! Round 81 audit — DEAD-F2 helper-method regression locks.
//!
//! Finding: the compiler emitted ~78 hand-written
//!   `chunk.emit_op(Op::X, span); chunk.emit_u16(idx, span);`
//! pairs in `src/compiler/mod.rs` (and ~31 more in
//! `src/compiler/patterns.rs`). silt's design principle is "one way
//! to do things" — equivalent dual shapes should collapse to a
//! single unified form. Round 81 introduced a single
//! `Chunk::emit_op_u16(op, idx, span)` helper (in `src/bytecode.rs`)
//! and rewrote every paired call site in `src/compiler/` to use it.
//!
//! These tests are source-grep + behavioral locks: they fail loudly
//! if the helper is removed, or if a future patch reintroduces the
//! two-line shape inside `src/compiler/`. The behavioral test
//! exercises Op::Constant + Op::GetLocal + Op::Jump + Op::SetGlobal
//! end-to-end so any accidental span/byte regression in the
//! refactor would surface as a runtime mismatch.
//!
//! Out of scope (intentionally not touched by Round 81):
//!   - `src/disassemble.rs` — its tests construct chunks for
//!     round-trip parity and may use the two-call form on purpose.
//!   - `src/lsp/`, etc. — separate spans for op vs payload may be
//!     legitimate there.

use std::path::PathBuf;
use std::process::Command;

fn read_src(rel: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ── (a) helper exists ────────────────────────────────────────────────

/// `Chunk::emit_op_u16` must be defined in `src/bytecode.rs` with a
/// `pub fn emit_op_u16(` signature. If someone deletes the helper,
/// this fires.
#[test]
fn chunk_helper_method_exists() {
    let src = read_src("src/bytecode.rs");
    assert!(
        src.contains("pub fn emit_op_u16("),
        "Chunk::emit_op_u16 helper missing from src/bytecode.rs — \
         Round 81 DEAD-F2 collapsed the emit_op + emit_u16 pair into a \
         single helper; do not remove it."
    );
}

// ── (b) compiler uses the helper, not two-line pairs ────────────────

/// Count residual `chunk.emit_op(Op::*, span);` lines whose *next*
/// non-empty line is a matching `chunk.emit_u16(_, span);` against
/// the same chunk-receiver expression and the same span identifier.
///
/// After Round 81 this should be 0 in `src/compiler/mod.rs` and
/// `src/compiler/patterns.rs`. We allow a small slack (<= 5) per
/// file in case a future legitimate use-case crops up where op and
/// payload genuinely have different spans — but if you hit the
/// threshold, justify each remaining site.
fn count_two_call_pairs(rel: &str) -> usize {
    let src = read_src(rel);
    let lines: Vec<&str> = src.lines().collect();
    let mut count = 0usize;
    // Crude line-pair matcher: looks for `<prefix>.emit_op(Op::Foo, <span>);`
    // immediately followed by `<prefix>.emit_u16(<anything>, <span>);` with
    // identical leading whitespace, identical receiver prefix, and identical
    // span argument.
    let op_re = regex_lite_op();
    let u16_re = regex_lite_u16();
    for i in 0..lines.len().saturating_sub(1) {
        let l1 = lines[i];
        let l2 = lines[i + 1];
        if let Some((indent1, prefix1, span1)) = op_re(l1) {
            if let Some((indent2, prefix2, span2)) = u16_re(l2) {
                if indent1 == indent2 && prefix1 == prefix2 && span1 == span2 {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Tiny hand-rolled matcher for `^(\s*)(.+?)\.emit_op\(Op::\w+,\s*(.+?)\);\s*$`.
/// Returns (indent, prefix, span_arg) on match. We avoid pulling in a
/// regex crate so the test suite has no extra deps.
fn regex_lite_op() -> impl Fn(&str) -> Option<(String, String, String)> {
    |line: &str| {
        let trimmed = line.trim_end();
        let indent_len = trimmed.len() - trimmed.trim_start().len();
        let (indent, body) = trimmed.split_at(indent_len);
        let needle = ".emit_op(Op::";
        let dot = body.find(needle)?;
        let prefix = &body[..dot];
        let after_op = &body[dot + needle.len()..];
        let comma = after_op.find(',')?;
        // skip op name; require ", " then span until ");"
        let rest = after_op[comma + 1..].trim_start();
        let close = rest.find(");")?;
        let span = rest[..close].trim().to_string();
        if !rest[close + 2..].is_empty() {
            return None;
        }
        Some((indent.to_string(), prefix.to_string(), span))
    }
}

/// Tiny hand-rolled matcher for `^(\s*)(.+?)\.emit_u16\(.+?,\s*(.+?)\);\s*$`.
/// Returns (indent, prefix, span_arg) on match.
fn regex_lite_u16() -> impl Fn(&str) -> Option<(String, String, String)> {
    |line: &str| {
        let trimmed = line.trim_end();
        let indent_len = trimmed.len() - trimmed.trim_start().len();
        let (indent, body) = trimmed.split_at(indent_len);
        let needle = ".emit_u16(";
        let dot = body.find(needle)?;
        let prefix = &body[..dot];
        let after = &body[dot + needle.len()..];
        // last comma before "));" or ");"
        let close = after.rfind(");")?;
        let inside = &after[..close];
        let comma = inside.rfind(',')?;
        let span = inside[comma + 1..].trim().to_string();
        if !after[close + 2..].is_empty() {
            return None;
        }
        Some((indent.to_string(), prefix.to_string(), span))
    }
}

#[test]
fn compiler_uses_helper_not_two_calls_in_mod() {
    let n = count_two_call_pairs("src/compiler/mod.rs");
    assert!(
        n <= 5,
        "src/compiler/mod.rs still has {n} `emit_op + emit_u16` two-line \
         pairs; Round 81 DEAD-F2 collapsed all of these into \
         `chunk.emit_op_u16(op, idx, span)`. If you re-introduce one, \
         please document why op and payload need separate spans."
    );
    // tighter expectation: after Round 81 we expect exactly zero.
    assert_eq!(
        n, 0,
        "src/compiler/mod.rs should have zero residual two-line pairs after Round 81"
    );
}

#[test]
fn compiler_uses_helper_not_two_calls_in_patterns() {
    let n = count_two_call_pairs("src/compiler/patterns.rs");
    assert!(
        n <= 5,
        "src/compiler/patterns.rs still has {n} `emit_op + emit_u16` \
         two-line pairs; Round 81 collapsed all of these into the \
         `emit_op_u16` helper."
    );
    assert_eq!(
        n, 0,
        "src/compiler/patterns.rs should have zero residual two-line pairs after Round 81"
    );
}

/// The compiler should now be calling the helper at scale. Lock that
/// at least 50 helper invocations exist between mod.rs and
/// patterns.rs combined — defends against accidental mass-revert.
#[test]
fn compiler_actually_invokes_helper_at_scale() {
    let mod_src = read_src("src/compiler/mod.rs");
    let pat_src = read_src("src/compiler/patterns.rs");
    let total = mod_src.matches(".emit_op_u16(").count() + pat_src.matches(".emit_op_u16(").count();
    assert!(
        total >= 50,
        "expected >= 50 `.emit_op_u16(` invocations across compiler; \
         got {total}. Has someone reverted Round 81?"
    );
}

// ── (c) behavioral test — bytecode emission unchanged ───────────────

/// Run a Silt program that exercises `Op::Constant` + `Op::GetLocal`
/// + `Op::Jump` (via `if`) + `Op::SetGlobal` (via top-level `let`),
/// and assert the visible output matches the expected sequence.
/// This is a semantic no-op canary for the refactor — if the helper
/// somehow scrambled byte ordering, the loop wouldn't print the
/// right numbers.
#[test]
fn bytecode_emission_unchanged_after_refactor() {
    // - `let tag = "..."` (top-level) exercises Op::SetGlobal + emit_u16.
    // - String literals + the interpolation `{n}` exercise Op::Constant
    //   + emit_u16.
    // - `match` arms compile to a chain of test+JumpIfFalse / Jump
    //   opcodes — every Jump / JumpIfFalse uses emit_u16 for its
    //   patch operand. Hitting at least three arms exercises this
    //   path multiple times per call.
    // - `n` parameter usage inside `classify` exercises Op::GetLocal +
    //   emit_u16.
    // - `1..6 |> list.each { ... }` evaluates `classify` six times so
    //   we get six lines of stable expected output.
    let src = r#"
import list

let tag = "round81"

fn classify(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _ -> "{n}"
  }
}

fn main() {
  1..6
  |> list.each { n -> println("{tag}:{classify(n)}") }
}
"#;
    let tmp = std::env::temp_dir().join("silt_round81_emit_op_u16.silt");
    std::fs::write(&tmp, src).expect("write tmp silt program");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "silt run failed; stdout={stdout}; stderr={stderr}"
    );
    // Exact-output lock: post-refactor must produce these lines in order.
    // silt's `lo..hi` range is inclusive on both ends (see Value::Range
    // doc in src/value.rs), so 1..6 yields [1,2,3,4,5,6] -> classify each.
    let expected_substrings = [
        "round81:1",
        "round81:2",
        "round81:Fizz",
        "round81:4",
        "round81:Buzz",
        "round81:Fizz",
    ];
    let mut cursor = 0usize;
    for needle in expected_substrings {
        let slice = &stdout[cursor..];
        let pos = slice.find(needle).unwrap_or_else(|| {
            panic!(
                "expected substring {:?} after offset {cursor} in stdout:\n{stdout}",
                needle
            )
        });
        cursor += pos + needle.len();
    }
}
