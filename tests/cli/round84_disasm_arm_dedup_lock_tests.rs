//! Round 84 lock: disassembler operand-decode helper dedup.
//!
//! Round 84 collapsed five byte-identical disassembler arms into two
//! shared helpers in `src/disassemble.rs`:
//!
//! * `fmt_u16_u8_with_const` covers `Op::CallMethod`, `Op::CallBuiltin`,
//!   and `Op::MakeVariant` (u16 constant-index + u8 byte operand,
//!   commented with the resolved constant).
//! * `fmt_u8_count_then_u16_names` covers `Op::RecordUpdate`,
//!   `Op::RecordUpdateAnon`, and `Op::DestructRecordRest` (u8 count
//!   followed by `count` × u16 name-index, label `"field"` for the
//!   first two and `"exclude"` for the last).
//!
//! The real semantic lock is the round-trip + format tests inside
//! `src/disassemble.rs` (see `test_record_update`, `test_record_update_anon`,
//! `test_destruct_record_rest_label`, `test_call_method_format`,
//! `test_make_variant_format`, `test_call_builtin`,
//! `test_op_from_byte_roundtrip`). This file is a lightweight
//! source-grep parity check that guards against accidentally
//! re-inlining the duplicated operand decodes in a future edit.

use std::fs;

const DISASSEMBLE_RS: &str = "src/disassemble.rs";

/// Read the disassembler source as a single string.
fn read_disasm() -> String {
    fs::read_to_string(DISASSEMBLE_RS)
        .expect("Failed to read src/disassemble.rs from the workspace root")
}

#[test]
fn disasm_u16_u8_helper_is_present() {
    let src = read_disasm();
    assert!(
        src.contains("fn fmt_u16_u8_with_const"),
        "Helper `fmt_u16_u8_with_const` must exist in {DISASSEMBLE_RS}. \
         Round 84 introduced this helper to dedup the CallMethod / \
         CallBuiltin / MakeVariant operand-decode arms — deleting it \
         re-introduces the BLOAT."
    );
}

#[test]
fn disasm_u8_count_then_u16_names_helper_is_present() {
    let src = read_disasm();
    assert!(
        src.contains("fn fmt_u8_count_then_u16_names"),
        "Helper `fmt_u8_count_then_u16_names` must exist in {DISASSEMBLE_RS}. \
         Round 84 introduced this helper to dedup the RecordUpdate / \
         RecordUpdateAnon / DestructRecordRest operand-decode arms — \
         deleting it re-introduces the BLOAT."
    );
}

#[test]
fn disasm_consolidated_u16_u8_arm_uses_helper() {
    let src = read_disasm();
    // The three opcodes that share `fmt_u16_u8_with_const` must appear
    // together in a single combined match arm. Spelling out the exact
    // alternation locks the dedup against drift back to three arms.
    assert!(
        src.contains("Op::CallMethod | Op::CallBuiltin | Op::MakeVariant"),
        "Combined match arm `Op::CallMethod | Op::CallBuiltin | Op::MakeVariant` \
         must be present in {DISASSEMBLE_RS}. Round 84 collapsed three \
         byte-identical arms into this single arm calling `fmt_u16_u8_with_const`."
    );
}

#[test]
fn disasm_record_update_arm_uses_helper() {
    let src = read_disasm();
    // RecordUpdate must use `fmt_u8_count_then_u16_names` with label
    // "field"; DestructRecordRest uses the same helper with label
    // "exclude". (Round 85 follow-up removed the sibling
    // `RecordUpdateAnon` that round 83 had added — see
    // src/bytecode.rs for the dual-closure note.)
    assert!(
        src.contains("Op::RecordUpdate => fmt_u8_count_then_u16_names"),
        "RecordUpdate must route through `fmt_u8_count_then_u16_names` \
         with label \"field\" in {DISASSEMBLE_RS}."
    );
    assert!(
        !src.contains("Op::RecordUpdateAnon"),
        "`Op::RecordUpdateAnon` was removed in the round-85 follow-up; \
         no code-level uses (qualified `Op::RecordUpdateAnon`) should \
         remain in {DISASSEMBLE_RS}. (Historical comments mentioning \
         the bare name `RecordUpdateAnon` for context are allowed.)"
    );
}

#[test]
fn disasm_no_inlined_argc_decode_after_dedup() {
    // Anti-pattern check: the old inlined `argc = code[offset + 3]`
    // shape lived only inside the CallMethod / CallBuiltin arms. If
    // that exact byte-fetch literal reappears alongside an `argc =`
    // binding, someone has re-inlined the decode that
    // `fmt_u16_u8_with_const` is supposed to own. Scoped to "argc"
    // specifically to avoid false positives — MakeRecord and
    // MakeClosure legitimately read a u8 at offset+3 but bind it to
    // `field_count` / `upvalue_count`.
    let src = read_disasm();
    assert!(
        !src.contains("let argc = code[offset + 3]"),
        "Found re-inlined `let argc = code[offset + 3]` in {DISASSEMBLE_RS}. \
         Round 84 routed CallMethod and CallBuiltin through \
         `fmt_u16_u8_with_const`; the inlined byte-fetch must not return."
    );
}

#[test]
fn disasm_no_inlined_method_name_index_decode() {
    let src = read_disasm();
    // Anti-pattern: the `method_name_index` local only existed inside
    // the now-removed CallMethod arm. Its return signals that the
    // CallMethod arm was re-inlined.
    assert!(
        !src.contains("let method_name_index = read_u16"),
        "Found re-inlined `let method_name_index = read_u16(...)` in \
         {DISASSEMBLE_RS}. Round 84 routed CallMethod through \
         `fmt_u16_u8_with_const`; the inlined u16 fetch must not return."
    );
}
