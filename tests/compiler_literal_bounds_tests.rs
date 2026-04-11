//! Regression tests for the list/map/set literal element-count bounds in
//! `src/compiler/mod.rs`.
//!
//! Background (B2): The `MakeList`, `MakeMap`, and `MakeSet` opcodes
//! encode their element count in a u16 operand. Previously the compiler
//! did `let count = elems.len() as u16;` with no bounds check — a 65 537
//! -element list would be emitted with count = 1, and at runtime
//! `Op::MakeList` would `stack.truncate(stack.len() - 1)`, leaving the
//! other 65 536 compiled values orphaned on the stack where they would
//! corrupt every subsequent operation.
//!
//! The fix adds explicit `> u16::MAX` bounds checks that produce a
//! clean compile-time `CompileError`. These tests exercise both the
//! happy path (exactly u16::MAX elements still compiles) and the
//! rejection path for each literal kind plus the spread accumulator.
//!
//! Mutation reasoning: reverting any of the four bounds checks would
//! make the rejection-path tests expect a compile failure while the
//! compiler silently accepted the oversized literal. The happy-path
//! test at u16::MAX locks the boundary so an off-by-one tightening
//! would also be caught.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;

/// Attempt to compile `source`. Returns `Ok(())` if compilation
/// succeeded, or the `CompileError.message` string on failure. Parse /
/// lex errors panic — we only care about the compiler stage here.
fn try_compile(source: &str) -> Result<(), String> {
    let tokens = Lexer::new(source).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    match compiler.compile_program(&program) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.message),
    }
}

/// Build a source string of the form
/// `fn main() { let _xs = [1,1,1,...] }` with exactly `n` elements.
fn make_list_source(n: usize) -> String {
    let mut s = String::with_capacity(n * 3 + 32);
    s.push_str("fn main() {\n  let _xs = [");
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push('1');
    }
    s.push_str("]\n}\n");
    s
}

/// Build a map literal source with exactly `n` pairs:
/// `#{"k0": 0, "k1": 1, ...}`. silt map literals use the `#{ ... }`
/// (HashBrace) syntax — see parser.rs line 1559.
fn make_map_source(n: usize) -> String {
    let mut s = String::with_capacity(n * 16 + 32);
    s.push_str("fn main() {\n  let _m = #{");
    for i in 0..n {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!("\"k{i}\": {i}"));
    }
    s.push_str("}\n}\n");
    s
}

/// Build a set literal source with exactly `n` elements:
/// `#[0, 1, 2, ...]`. silt set literals use the `#[ ... ]`
/// (HashBracket) syntax — see parser.rs line 1584.
fn make_set_source(n: usize) -> String {
    let mut s = String::with_capacity(n * 6 + 32);
    s.push_str("fn main() {\n  let _s = #[");
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&i.to_string());
    }
    s.push_str("]\n}\n");
    s
}

// ── List literal bounds ────────────────────────────────────────────────

#[test]
fn test_compiler_accepts_list_literal_at_u16_max() {
    // Exactly 65535 elements is the maximum a u16 can encode — this
    // must still compile cleanly. Picks up off-by-one tightening of
    // the bound.
    let src = make_list_source(u16::MAX as usize);
    let result = try_compile(&src);
    assert!(
        result.is_ok(),
        "list literal with exactly {} elements should compile, got error: {:?}",
        u16::MAX,
        result.err(),
    );
}

#[test]
fn test_compiler_rejects_list_literal_over_65535() {
    // 65536 elements overflows the u16 count operand. Prior to the
    // fix, this compiled, then at runtime `Op::MakeList` executed
    // with count = 0 (wrap), leaving every compiled element orphaned
    // on the VM stack.
    let src = make_list_source(u16::MAX as usize + 1);
    let err = try_compile(&src).expect_err("oversized list literal must be rejected");
    assert!(
        err.contains("list literal too large"),
        "error must mention 'list literal too large', got: {err}"
    );
    assert!(
        err.contains("65536"),
        "error must mention the actual size (65536), got: {err}"
    );
}

// ── Map literal bounds ─────────────────────────────────────────────────

#[test]
fn test_compiler_rejects_map_literal_over_65535() {
    // 65536 pairs overflows the u16 pair_count in Op::MakeMap. Same
    // stack-corruption class of bug as the list path.
    let src = make_map_source(u16::MAX as usize + 1);
    let err = try_compile(&src).expect_err("oversized map literal must be rejected");
    assert!(
        err.contains("map literal too large"),
        "error must mention 'map literal too large', got: {err}"
    );
    assert!(
        err.contains("65536"),
        "error must mention the actual pair count (65536), got: {err}"
    );
}

// ── Set literal bounds ─────────────────────────────────────────────────

#[test]
fn test_compiler_rejects_set_literal_over_65535() {
    // 65536 elements overflows the u16 count in Op::MakeSet.
    let src = make_set_source(u16::MAX as usize + 1);
    let err = try_compile(&src).expect_err("oversized set literal must be rejected");
    assert!(
        err.contains("set literal too large"),
        "error must mention 'set literal too large', got: {err}"
    );
    assert!(
        err.contains("65536"),
        "error must mention the actual size (65536), got: {err}"
    );
}

// ── Spread accumulator bounds ──────────────────────────────────────────

#[test]
fn test_compiler_rejects_list_spread_accumulated_over_65535() {
    // The spread path in `ExprKind::List` has a separate `single_count`
    // accumulator for the chunk of singles between spreads. With 65536
    // singles followed by a spread, the single-count flush would wrap
    // its u16 counter before the fix.
    //
    // We build `[1, 1, ... 65536 times, ..empty]` where `empty` is a
    // previously-bound one-element list, so the parser sees a real
    // spread and takes the spread codepath. silt spread syntax inside
    // a list literal is `..expr` (DotDot), per parser.rs line 1534.
    let n = u16::MAX as usize + 1;
    let mut src = String::with_capacity(n * 3 + 64);
    src.push_str("fn main() {\n  let empty = [0]\n  let _xs = [");
    for i in 0..n {
        if i > 0 {
            src.push(',');
        }
        src.push('1');
    }
    src.push_str(", ..empty]\n}\n");

    let err = try_compile(&src).expect_err("oversized spread-path list literal must be rejected");
    // Either the outer list-bound fires (preferred — it catches the
    // total count before the spread accumulator even runs), or the
    // spread-accumulator-specific bound fires. Both are valid: what
    // matters is that the compiler refuses the oversized literal
    // instead of emitting a wrapped u16.
    assert!(
        err.contains("list literal too large"),
        "error must mention 'list literal too large', got: {err}"
    );
}
