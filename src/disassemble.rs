//! Bytecode disassembler for debugging.
//!
//! Pretty-prints compiled `Chunk` objects in human-readable form.

use std::fmt::Write;

use crate::bytecode::{Chunk, Function, Op};

// ── Op decoding ───────────────────────────────────────────────────

/// Human-readable name for an opcode.
fn op_name(op: Op) -> &'static str {
    match op {
        Op::Constant => "Constant",
        Op::Unit => "Unit",
        Op::True => "True",
        Op::False => "False",
        Op::Add => "Add",
        Op::Sub => "Sub",
        Op::Mul => "Mul",
        Op::Div => "Div",
        Op::Mod => "Mod",
        Op::Eq => "Eq",
        Op::Neq => "Neq",
        Op::Lt => "Lt",
        Op::Gt => "Gt",
        Op::Leq => "Leq",
        Op::Geq => "Geq",
        Op::Negate => "Negate",
        Op::Not => "Not",
        Op::And => "And",
        Op::Or => "Or",
        Op::StringConcat => "StringConcat",
        Op::DisplayValue => "DisplayValue",
        Op::GetLocal => "GetLocal",
        Op::SetLocal => "SetLocal",
        Op::GetGlobal => "GetGlobal",
        Op::SetGlobal => "SetGlobal",
        Op::GetUpvalue => "GetUpvalue",
        Op::Call => "Call",
        Op::TailCall => "TailCall",
        Op::Return => "Return",
        Op::CallBuiltin => "CallBuiltin",
        Op::MakeClosure => "MakeClosure",
        Op::MakeTuple => "MakeTuple",
        Op::MakeList => "MakeList",
        Op::MakeMap => "MakeMap",
        Op::MakeSet => "MakeSet",
        Op::MakeRecord => "MakeRecord",
        Op::MakeVariant => "MakeVariant",
        Op::RecordUpdate => "RecordUpdate",
        Op::RecordUpdateAnon => "RecordUpdateAnon",
        Op::MakeRange => "MakeRange",
        Op::ListConcat => "ListConcat",
        Op::GetField => "GetField",
        Op::GetIndex => "GetIndex",
        Op::Jump => "Jump",
        Op::JumpBack => "JumpBack",
        Op::JumpIfFalse => "JumpIfFalse",
        Op::JumpIfTrue => "JumpIfTrue",
        Op::Pop => "Pop",
        Op::PopN => "PopN",
        Op::Dup => "Dup",
        Op::TestTag => "TestTag",
        Op::TestEqual => "TestEqual",
        Op::TestTupleLen => "TestTupleLen",
        Op::TestListMin => "TestListMin",
        Op::TestListExact => "TestListExact",
        Op::TestIntRange => "TestIntRange",
        Op::TestFloatRange => "TestFloatRange",
        Op::TestBool => "TestBool",
        Op::DestructTuple => "DestructTuple",
        Op::DestructVariant => "DestructVariant",
        Op::DestructList => "DestructList",
        Op::DestructListRest => "DestructListRest",
        Op::DestructRecordField => "DestructRecordField",
        Op::DestructRecordRest => "DestructRecordRest",
        Op::TestRecordTag => "TestRecordTag",
        Op::TestMapHasKey => "TestMapHasKey",
        Op::DestructMapValue => "DestructMapValue",
        Op::LoopSetup => "LoopSetup",
        Op::Recur => "Recur",
        Op::QuestionMark => "QuestionMark",
        Op::Panic => "Panic",
        Op::CallMethod => "CallMethod",
        Op::NarrowFloat => "NarrowFloat",
    }
}

// ── Helpers ───────────────────────────────────────────────────────

/// Read a little-endian u16 from the code bytes at the given offset.
fn read_u16(code: &[u8], offset: usize) -> u16 {
    code[offset] as u16 | ((code[offset + 1] as u16) << 8)
}

/// Format a constant value for a disassembly comment.
fn constant_comment(chunk: &Chunk, index: u16) -> String {
    if (index as usize) < chunk.constants.len() {
        format!("{:?}", chunk.constants[index as usize])
    } else {
        format!("???[{index}]")
    }
}

/// Format an instruction whose operands are a u16 constant-index followed by
/// a u8 (e.g. `CallMethod`, `CallBuiltin`, `MakeVariant`). The u16 is
/// commented with the resolved constant; the u8 is printed as a bare number.
///
/// Returns `(formatted_line, next_offset)`. The next offset is `offset + 4`.
fn fmt_u16_u8_with_const(
    chunk: &Chunk,
    code: &[u8],
    offset: usize,
    name: &str,
) -> (String, usize) {
    let index = read_u16(code, offset + 1);
    let byte_operand = code[offset + 3];
    let comment = constant_comment(chunk, index);
    (
        format!("{offset:04}  {name:<20} {index:<5} {byte_operand:<3} ; {comment}"),
        offset + 4,
    )
}

/// Format an instruction whose operands are a u8 count followed by
/// `count` x u16 name-index entries (e.g. `RecordUpdate`,
/// `RecordUpdateAnon`, `DestructRecordRest`). Each name-index is rendered on
/// its own continuation line using `label_literal` as the per-entry prefix
/// (e.g. `"field"` or `"exclude"`).
///
/// Returns `(formatted_line, next_offset)`.
fn fmt_u8_count_then_u16_names(
    chunk: &Chunk,
    code: &[u8],
    offset: usize,
    name: &str,
    label_literal: &str,
) -> (String, usize) {
    let field_count = code[offset + 1];
    let mut line = format!("{offset:04}  {name:<20} {field_count}");
    let mut next = offset + 2;
    for _ in 0..field_count {
        let field_name_index = read_u16(code, next);
        let field_comment = constant_comment(chunk, field_name_index);
        write!(
            line,
            "\n      |  {label_literal} {field_name_index:<5} ; {field_comment}"
        )
        .unwrap();
        next += 2;
    }
    (line, next)
}

// ── Instruction disassembly ───────────────────────────────────────

/// Disassemble a single instruction at `offset`.
///
/// Returns `(formatted_line, next_offset)`.
fn disassemble_instruction(chunk: &Chunk, offset: usize) -> (String, usize) {
    let code = &chunk.code;
    let byte = code[offset];

    let Some(op) = Op::from_byte(byte) else {
        return (format!("{offset:04}  <unknown {byte:#04x}>"), offset + 1);
    };

    let name = op_name(op);

    match op {
        // ── No operands ───────────────────────────────────────
        Op::Unit
        | Op::True
        | Op::False
        | Op::Add
        | Op::Sub
        | Op::Mul
        | Op::Div
        | Op::Mod
        | Op::Eq
        | Op::Neq
        | Op::Lt
        | Op::Gt
        | Op::Leq
        | Op::Geq
        | Op::Negate
        | Op::Not
        | Op::And
        | Op::Or
        | Op::DisplayValue
        | Op::Return
        | Op::Pop
        | Op::Dup
        | Op::MakeRange
        | Op::ListConcat
        | Op::QuestionMark
        | Op::Panic => (format!("{offset:04}  {name}"), offset + 1),

        // ── u8 operand ────────────────────────────────────────
        Op::StringConcat
        | Op::GetUpvalue
        | Op::Call
        | Op::TailCall
        | Op::MakeTuple
        | Op::PopN
        | Op::GetIndex
        | Op::TestTupleLen
        | Op::TestListMin
        | Op::TestListExact
        | Op::DestructTuple
        | Op::DestructVariant
        | Op::DestructList
        | Op::DestructListRest
        | Op::LoopSetup => {
            let operand = code[offset + 1];
            (format!("{offset:04}  {name:<20} {operand}"), offset + 2)
        }

        // Recur: u8 arg_count, u16 first_slot
        Op::Recur => {
            let arg_count = code[offset + 1];
            let first_slot = read_u16(code, offset + 2);
            (
                format!("{offset:04}  {name:<20} {arg_count}  slot {first_slot}"),
                offset + 4,
            )
        }

        // TestBool: u8 (0=false, 1=true)
        Op::TestBool => {
            let operand = code[offset + 1];
            let val = if operand == 0 { "false" } else { "true" };
            (
                format!("{offset:04}  {name:<20} {operand}    ; {val}"),
                offset + 2,
            )
        }

        // ── u16 operand with constant comment ─────────────────
        Op::Constant
        | Op::GetGlobal
        | Op::SetGlobal
        | Op::TestTag
        | Op::TestEqual
        | Op::GetField
        | Op::DestructRecordField
        | Op::TestRecordTag
        | Op::TestMapHasKey
        | Op::DestructMapValue => {
            let index = read_u16(code, offset + 1);
            let comment = constant_comment(chunk, index);
            (
                format!("{offset:04}  {name:<20} {index:<5} ; {comment}"),
                offset + 3,
            )
        }

        // ── u16 operand (slot, no constant comment) ───────────
        Op::GetLocal | Op::SetLocal => {
            let slot = read_u16(code, offset + 1);
            (format!("{offset:04}  {name:<20} {slot}"), offset + 3)
        }

        // ── u16 operand (count, no constant comment) ──────────
        Op::MakeList | Op::MakeMap | Op::MakeSet => {
            let count = read_u16(code, offset + 1);
            (format!("{offset:04}  {name:<20} {count}"), offset + 3)
        }

        // ── 2x u16 operand (range patterns via constants) ────────
        Op::TestIntRange | Op::TestFloatRange => {
            let lo_index = read_u16(code, offset + 1);
            let hi_index = read_u16(code, offset + 3);
            let lo_comment = constant_comment(chunk, lo_index);
            let hi_comment = constant_comment(chunk, hi_index);
            (
                format!(
                    "{offset:04}  {name:<20} {lo_index:<5} {hi_index:<5} ; {lo_comment}..{hi_comment}"
                ),
                offset + 5,
            )
        }

        // ── Jump instructions: show target offset ─────────────
        Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue | Op::NarrowFloat => {
            let jump_offset = read_u16(code, offset + 1) as usize;
            let target = offset + 3 + jump_offset;
            (
                format!("{offset:04}  {name:<20} {jump_offset:<5} -> {target:04}"),
                offset + 3,
            )
        }

        Op::JumpBack => {
            let jump_offset = read_u16(code, offset + 1) as usize;
            let target = offset + 3 - jump_offset;
            (
                format!("{offset:04}  {name:<20} {jump_offset:<5} -> {target:04}"),
                offset + 3,
            )
        }

        // ── u16 + u8 (with constant comment on the u16) ──────
        //   CallMethod(method_name_index, argc)
        //   CallBuiltin(name_index, argc)
        //   MakeVariant(name_index, field_count)
        // The three arms shared a byte-identical operand-decode shape
        // (round 84 audit): consolidated into `fmt_u16_u8_with_const`.
        Op::CallMethod | Op::CallBuiltin | Op::MakeVariant => {
            fmt_u16_u8_with_const(chunk, code, offset, name)
        }

        // ── MakeClosure: u16 func_index, u8 upvalue_count, then descriptors
        Op::MakeClosure => {
            let func_index = read_u16(code, offset + 1);
            let upvalue_count = code[offset + 3];
            let comment = constant_comment(chunk, func_index);
            let mut line =
                format!("{offset:04}  {name:<20} {func_index:<5} {upvalue_count:<3} ; {comment}");
            let mut next = offset + 4;
            for _ in 0..upvalue_count {
                let is_local = code[next];
                let index = code[next + 1];
                let locality = if is_local != 0 { "local" } else { "upvalue" };
                write!(line, "\n      |  {locality} {index}").unwrap();
                next += 2;
            }
            (line, next)
        }

        // ── MakeRecord: u16 type_name_index, u8 field_count, then field names
        Op::MakeRecord => {
            let type_name_index = read_u16(code, offset + 1);
            let field_count = code[offset + 3];
            let comment = constant_comment(chunk, type_name_index);
            let mut line = format!(
                "{offset:04}  {name:<20} {type_name_index:<5} {field_count:<3} ; {comment}"
            );
            let mut next = offset + 4;
            for _ in 0..field_count {
                let field_name_index = read_u16(code, next);
                let field_comment = constant_comment(chunk, field_name_index);
                write!(
                    line,
                    "\n      |  field {field_name_index:<5} ; {field_comment}"
                )
                .unwrap();
                next += 2;
            }
            (line, next)
        }

        // ── u8-count + count x u16 name_index (round 84) ─────
        //   RecordUpdate / RecordUpdateAnon: label "field". The two
        //     opcodes share an operand layout — they differ only in the
        //     resulting Value's `type_name` (base's name vs `"<anon>"`).
        //   DestructRecordRest:              label "exclude".
        Op::RecordUpdate | Op::RecordUpdateAnon => {
            fmt_u8_count_then_u16_names(chunk, code, offset, name, "field")
        }
        Op::DestructRecordRest => {
            fmt_u8_count_then_u16_names(chunk, code, offset, name, "exclude")
        }
    }
}

// ── Chunk disassembly ─────────────────────────────────────────────

/// Disassemble a complete `Chunk`, returning the formatted output.
fn disassemble_chunk(chunk: &Chunk, name: &str) -> String {
    let mut output = format!("== {name} ==\n");
    let mut offset = 0;
    while offset < chunk.code.len() {
        let (line, next) = disassemble_instruction(chunk, offset);
        output.push_str(&line);
        output.push('\n');
        offset = next;
    }
    output
}

/// Disassemble a compiled `Function`, returning the formatted output.
///
/// Recursively disassembles nested functions found as `VmClosure` constants.
pub fn disassemble_function(func: &Function) -> String {
    let header = format!(
        "{} (arity={}, upvalues={})",
        func.name, func.arity, func.upvalue_count
    );
    let mut output = disassemble_chunk(&func.chunk, &header);

    // Recurse into nested functions stored as VmClosure constants.
    for constant in &func.chunk.constants {
        if let crate::value::Value::VmClosure(closure) = constant {
            output.push('\n');
            output.push_str(&disassemble_function(&closure.function));
        }
    }

    output
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Chunk, Function, Op};
    use crate::lexer::Span;
    use crate::value::Value;

    fn dummy_span() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn test_simple_ops() {
        let mut chunk = Chunk::new();
        let span = dummy_span();
        chunk.emit_op(Op::True, span);
        chunk.emit_op(Op::False, span);
        chunk.emit_op(Op::Add, span);
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "test");
        assert!(output.contains("== test =="));
        assert!(output.contains("0000  True"));
        assert!(output.contains("0001  False"));
        assert!(output.contains("0002  Add"));
        assert!(output.contains("0003  Return"));
    }

    #[test]
    fn test_constant_op() {
        let mut chunk = Chunk::new();
        let span = dummy_span();
        let idx = chunk.add_constant(Value::Int(42)).unwrap();
        chunk.emit_op(Op::Constant, span);
        chunk.emit_u16(idx, span);
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "constants");
        assert!(output.contains("Constant"));
        assert!(output.contains("42"));
    }

    #[test]
    fn test_jump_target() {
        let mut chunk = Chunk::new();
        let span = dummy_span();
        // Emit Jump with offset 10 -> target = 0 + 3 + 10 = 13
        chunk.emit_op(Op::Jump, span);
        chunk.emit_u16(10, span);
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "jumps");
        assert!(output.contains("Jump"));
        assert!(output.contains("-> 0013"));
    }

    #[test]
    fn test_jump_back_target() {
        let mut chunk = Chunk::new();
        let span = dummy_span();
        // Pad with some ops so we can jump back
        chunk.emit_op(Op::Unit, span); // 0000
        chunk.emit_op(Op::Pop, span); // 0001
        chunk.emit_op(Op::Unit, span); // 0002
        chunk.emit_op(Op::Pop, span); // 0003
        // JumpBack at offset 4, with offset 5 -> target = 4 + 3 - 5 = 2
        chunk.emit_op(Op::JumpBack, span);
        chunk.emit_u16(5, span);

        let output = disassemble_chunk(&chunk, "jumpback");
        assert!(output.contains("JumpBack"));
        assert!(output.contains("-> 0002"));
    }

    #[test]
    fn test_make_closure() {
        let mut chunk = Chunk::new();
        let span = dummy_span();

        // Add a function constant
        let func = Value::String("<fn>".into());
        let idx = chunk.add_constant(func).unwrap();

        chunk.emit_op(Op::MakeClosure, span);
        chunk.emit_u16(idx, span);
        chunk.emit_u8(2, span); // 2 upvalues
        // upvalue 0: local slot 3
        chunk.emit_u8(1, span);
        chunk.emit_u8(3, span);
        // upvalue 1: upvalue index 0
        chunk.emit_u8(0, span);
        chunk.emit_u8(0, span);
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "closure");
        assert!(output.contains("MakeClosure"));
        assert!(output.contains("local 3"));
        assert!(output.contains("upvalue 0"));
    }

    #[test]
    fn test_make_record() {
        let mut chunk = Chunk::new();
        let span = dummy_span();

        let type_idx = chunk.add_constant(Value::String("Point".into())).unwrap();
        let x_idx = chunk.add_constant(Value::String("x".into())).unwrap();
        let y_idx = chunk.add_constant(Value::String("y".into())).unwrap();

        chunk.emit_op(Op::MakeRecord, span);
        chunk.emit_u16(type_idx, span);
        chunk.emit_u8(2, span); // 2 fields
        chunk.emit_u16(x_idx, span);
        chunk.emit_u16(y_idx, span);
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "record");
        assert!(output.contains("MakeRecord"));
        assert!(output.contains("\"Point\""));
        assert!(output.contains("\"x\""));
        assert!(output.contains("\"y\""));
    }

    #[test]
    fn test_call_builtin() {
        let mut chunk = Chunk::new();
        let span = dummy_span();

        let name_idx = chunk.add_constant(Value::String("print".into())).unwrap();
        chunk.emit_op(Op::CallBuiltin, span);
        chunk.emit_u16(name_idx, span);
        chunk.emit_u8(1, span);
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "builtin");
        assert!(output.contains("CallBuiltin"));
        assert!(output.contains("\"print\""));
    }

    #[test]
    fn test_disassemble_function() {
        let mut func = Function::new("add".into(), 2);
        let span = dummy_span();
        func.upvalue_count = 1;
        func.chunk.emit_op(Op::GetLocal, span);
        func.chunk.emit_u16(0, span);
        func.chunk.emit_op(Op::GetLocal, span);
        func.chunk.emit_u16(1, span);
        func.chunk.emit_op(Op::Add, span);
        func.chunk.emit_op(Op::Return, span);

        let output = disassemble_function(&func);
        assert!(output.contains("== add (arity=2, upvalues=1) =="));
        assert!(output.contains("GetLocal"));
        assert!(output.contains("Add"));
        assert!(output.contains("Return"));
    }

    #[test]
    fn test_u8_operand_ops() {
        let mut chunk = Chunk::new();
        let span = dummy_span();
        chunk.emit_op(Op::MakeTuple, span);
        chunk.emit_u8(3, span);
        chunk.emit_op(Op::Call, span);
        chunk.emit_u8(2, span);
        chunk.emit_op(Op::PopN, span);
        chunk.emit_u8(5, span);

        let output = disassemble_chunk(&chunk, "u8ops");
        assert!(output.contains("MakeTuple"));
        assert!(output.contains("Call"));
        assert!(output.contains("PopN"));
    }

    #[test]
    fn test_record_update() {
        let mut chunk = Chunk::new();
        let span = dummy_span();

        let x_idx = chunk.add_constant(Value::String("x".into())).unwrap();

        chunk.emit_op(Op::RecordUpdate, span);
        chunk.emit_u8(1, span); // 1 field
        chunk.emit_u16(x_idx, span);
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "update");
        assert!(output.contains("RecordUpdate"));
        assert!(output.contains("\"x\""));
        // Round 84: the operand decode for RecordUpdate is now shared with
        // RecordUpdateAnon via `fmt_u8_count_then_u16_names`. Lock the
        // per-entry label literal to guard against accidentally swapping
        // it with "exclude" (the DestructRecordRest label).
        assert!(output.contains("field "));
    }

    #[test]
    fn test_record_update_anon() {
        // Sibling lock for `Op::RecordUpdateAnon` (round 83 added the
        // discriminant; round 84 consolidates its disassembler arm with
        // `RecordUpdate`). Verifies both the discriminant byte path and
        // the rendered output (op name + "field" per-entry label).
        let mut chunk = Chunk::new();
        let span = dummy_span();

        let y_idx = chunk.add_constant(Value::String("y".into())).unwrap();

        chunk.emit_op(Op::RecordUpdateAnon, span);
        chunk.emit_u8(1, span); // 1 field
        chunk.emit_u16(y_idx, span);
        chunk.emit_op(Op::Return, span);

        // Discriminant sanity: the byte we just emitted must round-trip
        // back through `Op::from_byte` to `Op::RecordUpdateAnon`. This
        // catches a future regression where the emitter and decoder
        // disagree on the byte assigned to `RecordUpdateAnon`.
        let discrim_byte = chunk.code[0];
        assert_eq!(
            Op::from_byte(discrim_byte),
            Some(Op::RecordUpdateAnon),
            "RecordUpdateAnon discriminant round-trip failed",
        );

        let output = disassemble_chunk(&chunk, "update_anon");
        assert!(output.contains("RecordUpdateAnon"));
        assert!(output.contains("\"y\""));
        // Same per-entry label as RecordUpdate (they share the helper).
        assert!(output.contains("field "));
    }

    #[test]
    fn test_destruct_record_rest_label() {
        // Round 84: locks the `"exclude"` label used by the shared helper
        // `fmt_u8_count_then_u16_names` for `Op::DestructRecordRest`.
        // The other two opcodes routed through that helper use `"field"`
        // — this test guards against accidentally collapsing the label
        // argument or swapping the two literals.
        let mut chunk = Chunk::new();
        let span = dummy_span();

        let name_idx = chunk.add_constant(Value::String("z".into())).unwrap();

        chunk.emit_op(Op::DestructRecordRest, span);
        chunk.emit_u8(1, span);
        chunk.emit_u16(name_idx, span);
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "rest");
        assert!(output.contains("DestructRecordRest"));
        assert!(output.contains("exclude "));
        // Must NOT use the sibling label — guards against the helper
        // being called with the wrong label literal.
        assert!(
            !output.contains("field "),
            "DestructRecordRest should render label 'exclude', not 'field'. Output:\n{output}"
        );
    }

    #[test]
    fn test_call_method_format() {
        // Round 84: locks the formatted output for `Op::CallMethod`,
        // which now shares the operand-decode helper
        // `fmt_u16_u8_with_const` with `CallBuiltin` and `MakeVariant`.
        let mut chunk = Chunk::new();
        let span = dummy_span();

        let method_idx = chunk.add_constant(Value::String("len".into())).unwrap();

        chunk.emit_op(Op::CallMethod, span);
        chunk.emit_u16(method_idx, span);
        chunk.emit_u8(0, span); // argc = 0
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "method");
        assert!(output.contains("CallMethod"));
        assert!(output.contains("\"len\""));
    }

    #[test]
    fn test_make_variant_format() {
        // Round 84: locks the formatted output for `Op::MakeVariant`,
        // which now shares the operand-decode helper
        // `fmt_u16_u8_with_const` with `CallMethod` and `CallBuiltin`.
        let mut chunk = Chunk::new();
        let span = dummy_span();

        let name_idx = chunk.add_constant(Value::String("Some".into())).unwrap();

        chunk.emit_op(Op::MakeVariant, span);
        chunk.emit_u16(name_idx, span);
        chunk.emit_u8(1, span); // field_count = 1
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "variant");
        assert!(output.contains("MakeVariant"));
        assert!(output.contains("\"Some\""));
    }

    #[test]
    fn test_op_from_byte_roundtrip() {
        // Hand-locked count of Op variants. Bumping the Op enum without
        // bumping this constant fails the test on purpose: it forces a
        // conscious update to both `Op::from_byte` and any disassembler
        // tables. Last verified: 73 variants (round 83 audit added
        // `RecordUpdateAnon` to fix the spread-vs-anon-literal `==`
        // divergence — see src/bytecode.rs for the design note).
        const EXPECTED_OP_COUNT: usize = 73;

        // Sweep every possible byte value. For each one that decodes,
        // verify the round-trip discriminant matches. This catches both
        // (a) deletion of an arm from `Op::from_byte` (count drops) and
        // (b) any future opcode whose discriminant doesn't survive
        // round-tripping (unlikely with `#[repr(u8)]`, but locked in).
        let mut decoded_count = 0usize;
        for byte in 0u8..=255 {
            if let Some(op) = Op::from_byte(byte) {
                assert_eq!(
                    op as u8, byte,
                    "round-trip failed for byte {byte}: decoded to {op:?} but discriminant is {}",
                    op as u8
                );
                decoded_count += 1;
            }
        }
        assert_eq!(
            decoded_count, EXPECTED_OP_COUNT,
            "Op::from_byte decoded {decoded_count} bytes; expected {EXPECTED_OP_COUNT}. \
             If you added/removed an Op variant, update both Op::from_byte and EXPECTED_OP_COUNT."
        );
        // Sanity: a byte well past the highest discriminant must not decode.
        assert_eq!(Op::from_byte(255), None);
    }

    #[test]
    fn test_op_name_exhaustive_via_from_byte() {
        // For every byte that decodes to an Op, `op_name` must produce
        // a non-empty, non-placeholder label. This guards against
        // accidentally regressing `op_name` to a fallthrough that
        // returns "<unknown>" or "???" for some valid opcode.
        for byte in 0u8..=255 {
            if let Some(op) = Op::from_byte(byte) {
                let name = op_name(op);
                assert!(!name.is_empty(), "op_name({op:?}) returned empty string");
                assert!(
                    !name.contains("unknown") && !name.contains("???"),
                    "op_name({op:?}) returned placeholder: {name:?}"
                );
            }
        }
    }
}
