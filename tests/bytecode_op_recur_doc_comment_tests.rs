//! Lock test: Op::Recur doc-comment must accurately reflect its 3-byte
//! operand encoding (u8 arg_count + u16 first_slot). A round-64 audit
//! found the doc-comment understated the operand width as just "u8
//! arg_count", which would mislead a future contributor into walking the
//! IP off-by-two.
//!
//! These are SOURCE-GREP locks (per audit weak-gate watch — doc-comment
//! fixes are exactly the class where source-grep is the right strength):
//! we verify the doc-comment text and parity with the disassembler /
//! compiler emission so any future encoding change forces all sites to
//! stay in sync.

const SRC: &str = include_str!("../src/bytecode.rs");

#[test]
fn op_recur_doc_comment_mentions_first_slot() {
    // The Op::Recur doc comment must mention `first_slot` (or u16) so a
    // future contributor reading the enum knows the operand width is
    // u8 + u16, not just u8.
    let recur_block = SRC
        .lines()
        .skip_while(|l| !l.contains("Recur,"))
        .take(2)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        recur_block.contains("first_slot") || recur_block.contains("u16"),
        "src/bytecode.rs Op::Recur doc-comment understates operand width \
         — actual encoding is u8 arg_count + u16 first_slot \
         (see src/disassemble.rs:271-278 and src/vm/execute.rs:1993-1994)"
    );
}

#[test]
fn op_recur_encoding_is_three_operand_bytes() {
    let dis = include_str!("../src/disassemble.rs");
    let comp = include_str!("../src/compiler/mod.rs");
    // disassemble.rs ~272: reads chunk.code[ip + 1] (u8) and a u16 from ip+2,ip+3
    // compiler/mod.rs ~2777: emits Op::Recur, then u8 arg_count, then u16 first_slot
    assert!(
        dis.contains("Op::Recur") && (dis.contains("first_slot") || dis.contains("u16")),
        "src/disassemble.rs Recur handling does not reference u16 first_slot — \
         encoding may have drifted; bytecode.rs doc-comment and disassembler \
         must stay in sync"
    );
    assert!(
        comp.contains("Op::Recur")
            && comp.contains("emit_u8")
            && comp.contains("emit_u16"),
        "src/compiler/mod.rs Recur emission no longer emits u8 + u16 — \
         encoding may have drifted; bytecode.rs doc-comment and compiler \
         must stay in sync"
    );
}
