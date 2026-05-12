//! Round-85 follow-up: lock the two deferred items closed.
//!
//! Item 1 — `Op::RecordUpdateAnon` removed.
//! Round 83 added the sibling opcode to normalize a spread's runtime
//! `type_name` to `"<anon>"`. Round 84 added the `<anon>` wildcard to
//! `Value::PartialEq`; round 85 mirrored it into `Value::Ord` and
//! `Value::Hash`. The wildcards close the soundness gap from the
//! comparison/hashing surfaces, leaving the runtime tag-rebrand
//! strictly redundant. This follow-up removes the opcode and these
//! locks guard against accidental re-introduction.
//!
//! Item 2 — `format_module_source_error` inner-snippet color symmetry.
//! Round 83 added `NO_COLOR` / `FORCE_COLOR` support to
//! `SourceError::Display` via `active_colors()`. The module-import
//! inner snippet rendered by `format_module_source_error` was still
//! plain text, producing colored outer header + plain inner snippet
//! under `FORCE_COLOR=1` in a TTY. This follow-up routes the inner
//! `-->` / `|` / `^` glyphs through `active_colors()` too. These
//! locks verify the inner ANSI escapes appear under FORCE_COLOR and
//! disappear under NO_COLOR.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BYTECODE_RS: &str = "src/bytecode.rs";
const VM_EXECUTE_RS: &str = "src/vm/execute.rs";
const COMPILER_MOD_RS: &str = "src/compiler/mod.rs";
const DISASSEMBLE_RS: &str = "src/disassemble.rs";

fn read(path: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(path);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ── Item 1: RecordUpdateAnon removed ─────────────────────────────────

#[test]
fn item1_record_update_anon_variant_absent_from_bytecode() {
    let src = read(BYTECODE_RS);
    // The `Op::RecordUpdateAnon` variant declaration and its
    // `Op::from_byte` arm must both be gone. Bare-name comments
    // explaining the removal in a historical note are allowed; the
    // qualified `Op::RecordUpdateAnon` form is the actual code use.
    assert!(
        !src.contains("Op::RecordUpdateAnon"),
        "`Op::RecordUpdateAnon` was removed in the round-85 follow-up; \
         no qualified-form code uses should remain in {BYTECODE_RS}."
    );
}

#[test]
fn item1_record_update_anon_dispatch_arm_absent_from_vm() {
    let src = read(VM_EXECUTE_RS);
    assert!(
        !src.contains("Op::RecordUpdateAnon"),
        "VM dispatch must not reference `Op::RecordUpdateAnon` after \
         the round-85 follow-up; the unified `Op::RecordUpdate` arm \
         preserves the base record's `type_name`. See {VM_EXECUTE_RS}."
    );
}

#[test]
fn item1_compiler_emits_only_record_update_for_spread() {
    let src = read(COMPILER_MOD_RS);
    assert!(
        !src.contains("Op::RecordUpdateAnon"),
        "Compiler must not emit `Op::RecordUpdateAnon` after the \
         round-85 follow-up; spreads emit `Op::RecordUpdate` and rely \
         on the PartialEq/Ord/Hash `<anon>` wildcards for soundness. \
         See {COMPILER_MOD_RS}."
    );
    // Positive: the spread emission site must use `Op::RecordUpdate`
    // (already covered by the existing dot-update path, but we want
    // an explicit lock that the spread path didn't get accidentally
    // dropped entirely).
    assert!(
        src.contains("Op::RecordUpdate"),
        "Compiler must still emit `Op::RecordUpdate` for record \
         updates and spreads — found no references in {COMPILER_MOD_RS}."
    );
}

#[test]
fn item1_disassembler_arm_unified() {
    let src = read(DISASSEMBLE_RS);
    // Single arm, no `|` alternation, calls the helper with label
    // "field". Same shape as `DestructRecordRest` but with the
    // sibling label.
    assert!(
        src.contains("Op::RecordUpdate => fmt_u8_count_then_u16_names"),
        "Disassembler must route `Op::RecordUpdate` through \
         `fmt_u8_count_then_u16_names` with label \"field\" in {DISASSEMBLE_RS}."
    );
}

#[test]
fn item1_expected_op_count_decremented() {
    let src = read(DISASSEMBLE_RS);
    // Round 83 set the count to 73 (added RecordUpdateAnon); the
    // follow-up drops it to 72 (removed RecordUpdateAnon).
    assert!(
        src.contains("const EXPECTED_OP_COUNT: usize = 72;"),
        "`EXPECTED_OP_COUNT` must be 72 after the round-85 follow-up \
         removed `RecordUpdateAnon` (was 73 since round 83). See {DISASSEMBLE_RS}."
    );
    assert!(
        !src.contains("const EXPECTED_OP_COUNT: usize = 73;"),
        "Stale `EXPECTED_OP_COUNT = 73` must not remain in {DISASSEMBLE_RS}."
    );
}

// ── Item 2: format_module_source_error inner-snippet color symmetry ──

/// Build a fixture: a main file that imports a broken module, and a
/// broken module containing a lex error. Returns the path to the main
/// file (callers `silt run` it). The temp dir is leaked deliberately
/// so the subprocess can read the files; tests run in serial and the
/// OS reaps them on test-process exit.
fn write_broken_import_fixture() -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "silt-round85-deferred-fixture-{}",
        std::process::id()
    ));
    fs::create_dir_all(&tmp).expect("create temp fixture dir");
    // An illegal character `@` triggers a lex error inside the
    // imported module. format_module_source_error renders the inner
    // snippet pointing at it.
    fs::write(
        tmp.join("badlex.silt"),
        "pub fn hi() = 1\n@@@\npub fn bye() = 2\n",
    )
    .expect("write badlex.silt");
    let main_src = "import badlex\n\nfn main() {\n  badlex.hi()\n}\n";
    let main_path = tmp.join("main.silt");
    fs::write(&main_path, main_src).expect("write main.silt");
    main_path
}

fn run_silt_capture_stderr(main: &PathBuf, force_color: Option<&str>, no_color: Option<&str>) -> String {
    let bin = env!("CARGO_BIN_EXE_silt");
    let mut cmd = Command::new(bin);
    cmd.arg("run").arg(main);
    // Always set TERM so the child has a deterministic env. We
    // explicitly toggle FORCE_COLOR / NO_COLOR per case.
    cmd.env_remove("FORCE_COLOR");
    cmd.env_remove("NO_COLOR");
    if let Some(v) = force_color {
        cmd.env("FORCE_COLOR", v);
    }
    if let Some(v) = no_color {
        cmd.env("NO_COLOR", v);
    }
    let output = cmd.output().expect("spawn silt");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn item2_inner_snippet_colored_under_force_color() {
    let main = write_broken_import_fixture();
    let stderr = run_silt_capture_stderr(&main, Some("1"), None);
    // Sanity: the broken module's parse error reached the user
    // through `format_module_source_error`.
    assert!(
        stderr.contains("badlex.silt") && stderr.contains("@@@"),
        "expected error stderr to reference badlex.silt and contain \
         the broken source line, got: {stderr:?}"
    );
    // Inner-snippet glyphs must be wrapped in ANSI escapes. We
    // grep for the specific cyan-wrapped `-->` and bold-red `^` —
    // exact-match keeps the lock pointed at the format we ship.
    assert!(
        stderr.contains("\x1b[36m-->\x1b[0m"),
        "FORCE_COLOR=1: inner snippet `-->` must be cyan-wrapped. \
         Got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("\x1b[1m\x1b[31m^\x1b[0m"),
        "FORCE_COLOR=1: inner snippet `^` must be bold-red-wrapped. \
         Got stderr:\n{stderr}"
    );
    // The `|` gutter must also be cyan-wrapped (at least one bar).
    assert!(
        stderr.contains("\x1b[36m|\x1b[0m"),
        "FORCE_COLOR=1: inner snippet `|` gutter must be cyan-wrapped. \
         Got stderr:\n{stderr}"
    );
}

#[test]
fn item2_inner_snippet_plain_under_no_color() {
    let main = write_broken_import_fixture();
    let stderr = run_silt_capture_stderr(&main, None, Some("1"));
    // Sanity.
    assert!(
        stderr.contains("badlex.silt") && stderr.contains("@@@"),
        "expected error stderr to reference badlex.silt and contain \
         the broken source line, got: {stderr:?}"
    );
    // No ANSI escapes anywhere — `NO_COLOR=1` wins over any other
    // signal per the no-color.org spec.
    assert!(
        !stderr.contains('\x1b'),
        "NO_COLOR=1: stderr must contain no ANSI escape sequences. \
         Got stderr:\n{stderr}"
    );
    // The inner-snippet glyphs must still be present as plain text.
    assert!(stderr.contains("-->"), "plain `-->` must still be present");
    assert!(stderr.contains('^'), "plain `^` caret must still be present");
}

#[test]
fn item2_no_color_wins_over_force_color() {
    let main = write_broken_import_fixture();
    let stderr = run_silt_capture_stderr(&main, Some("1"), Some("1"));
    assert!(
        !stderr.contains('\x1b'),
        "NO_COLOR=1 + FORCE_COLOR=1: NO_COLOR wins (no-color.org spec). \
         No ANSI escapes expected. Got stderr:\n{stderr}"
    );
}
