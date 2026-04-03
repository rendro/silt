//! Bytecode disassembler for debugging.
//!
//! Pretty-prints compiled `Chunk` objects in human-readable form.

use std::fmt::Write;

use crate::bytecode::{Chunk, Function, Op};

// ── Op decoding ───────────────────────────────────────────────────

/// Convert a raw byte back to an `Op` variant.
///
/// The `Op` enum is `#[repr(u8)]` so the discriminant order is stable.
fn op_from_byte(byte: u8) -> Option<Op> {
    // Safety: we range-check before transmuting.
    // We use a manual match to avoid depending on unstable transmute semantics.
    let op = match byte {
        b if b == Op::Constant as u8 => Op::Constant,
        b if b == Op::Unit as u8 => Op::Unit,
        b if b == Op::True as u8 => Op::True,
        b if b == Op::False as u8 => Op::False,

        b if b == Op::Add as u8 => Op::Add,
        b if b == Op::Sub as u8 => Op::Sub,
        b if b == Op::Mul as u8 => Op::Mul,
        b if b == Op::Div as u8 => Op::Div,
        b if b == Op::Mod as u8 => Op::Mod,

        b if b == Op::Eq as u8 => Op::Eq,
        b if b == Op::Neq as u8 => Op::Neq,
        b if b == Op::Lt as u8 => Op::Lt,
        b if b == Op::Gt as u8 => Op::Gt,
        b if b == Op::Leq as u8 => Op::Leq,
        b if b == Op::Geq as u8 => Op::Geq,

        b if b == Op::Negate as u8 => Op::Negate,
        b if b == Op::Not as u8 => Op::Not,

        b if b == Op::And as u8 => Op::And,
        b if b == Op::Or as u8 => Op::Or,

        b if b == Op::StringConcat as u8 => Op::StringConcat,
        b if b == Op::DisplayValue as u8 => Op::DisplayValue,

        b if b == Op::GetLocal as u8 => Op::GetLocal,
        b if b == Op::SetLocal as u8 => Op::SetLocal,
        b if b == Op::GetGlobal as u8 => Op::GetGlobal,
        b if b == Op::SetGlobal as u8 => Op::SetGlobal,

        b if b == Op::GetUpvalue as u8 => Op::GetUpvalue,

        b if b == Op::Call as u8 => Op::Call,
        b if b == Op::TailCall as u8 => Op::TailCall,
        b if b == Op::Return as u8 => Op::Return,
        b if b == Op::CallBuiltin as u8 => Op::CallBuiltin,

        b if b == Op::MakeClosure as u8 => Op::MakeClosure,

        b if b == Op::MakeTuple as u8 => Op::MakeTuple,
        b if b == Op::MakeList as u8 => Op::MakeList,
        b if b == Op::MakeMap as u8 => Op::MakeMap,
        b if b == Op::MakeSet as u8 => Op::MakeSet,
        b if b == Op::MakeRecord as u8 => Op::MakeRecord,
        b if b == Op::MakeVariant as u8 => Op::MakeVariant,
        b if b == Op::RecordUpdate as u8 => Op::RecordUpdate,
        b if b == Op::MakeRange as u8 => Op::MakeRange,

        b if b == Op::GetField as u8 => Op::GetField,
        b if b == Op::GetIndex as u8 => Op::GetIndex,

        b if b == Op::Jump as u8 => Op::Jump,
        b if b == Op::JumpBack as u8 => Op::JumpBack,
        b if b == Op::JumpIfFalse as u8 => Op::JumpIfFalse,
        b if b == Op::JumpIfTrue as u8 => Op::JumpIfTrue,
        b if b == Op::Pop as u8 => Op::Pop,
        b if b == Op::PopN as u8 => Op::PopN,
        b if b == Op::Dup as u8 => Op::Dup,

        b if b == Op::TestTag as u8 => Op::TestTag,
        b if b == Op::TestEqual as u8 => Op::TestEqual,
        b if b == Op::TestTupleLen as u8 => Op::TestTupleLen,
        b if b == Op::TestListMin as u8 => Op::TestListMin,
        b if b == Op::TestListExact as u8 => Op::TestListExact,
        b if b == Op::TestIntRange as u8 => Op::TestIntRange,
        b if b == Op::TestFloatRange as u8 => Op::TestFloatRange,
        b if b == Op::TestBool as u8 => Op::TestBool,
        b if b == Op::DestructTuple as u8 => Op::DestructTuple,
        b if b == Op::DestructVariant as u8 => Op::DestructVariant,
        b if b == Op::DestructList as u8 => Op::DestructList,
        b if b == Op::DestructListRest as u8 => Op::DestructListRest,
        b if b == Op::DestructRecordField as u8 => Op::DestructRecordField,

        b if b == Op::LoopSetup as u8 => Op::LoopSetup,
        b if b == Op::Recur as u8 => Op::Recur,

        b if b == Op::QuestionMark as u8 => Op::QuestionMark,
        b if b == Op::Panic as u8 => Op::Panic,

        b if b == Op::ChanNew as u8 => Op::ChanNew,
        b if b == Op::ChanSend as u8 => Op::ChanSend,
        b if b == Op::ChanRecv as u8 => Op::ChanRecv,
        b if b == Op::ChanClose as u8 => Op::ChanClose,
        b if b == Op::ChanTrySend as u8 => Op::ChanTrySend,
        b if b == Op::ChanTryRecv as u8 => Op::ChanTryRecv,
        b if b == Op::ChanSelect as u8 => Op::ChanSelect,
        b if b == Op::TaskSpawn as u8 => Op::TaskSpawn,
        b if b == Op::TaskJoin as u8 => Op::TaskJoin,
        b if b == Op::TaskCancel as u8 => Op::TaskCancel,
        b if b == Op::Yield as u8 => Op::Yield,

        _ => return None,
    };
    Some(op)
}

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
        Op::MakeRange => "MakeRange",
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
        Op::LoopSetup => "LoopSetup",
        Op::Recur => "Recur",
        Op::QuestionMark => "QuestionMark",
        Op::Panic => "Panic",
        Op::ChanNew => "ChanNew",
        Op::ChanSend => "ChanSend",
        Op::ChanRecv => "ChanRecv",
        Op::ChanClose => "ChanClose",
        Op::ChanTrySend => "ChanTrySend",
        Op::ChanTryRecv => "ChanTryRecv",
        Op::ChanSelect => "ChanSelect",
        Op::TaskSpawn => "TaskSpawn",
        Op::TaskJoin => "TaskJoin",
        Op::TaskCancel => "TaskCancel",
        Op::Yield => "Yield",
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

// ── Instruction disassembly ───────────────────────────────────────

/// Disassemble a single instruction at `offset`.
///
/// Returns `(formatted_line, next_offset)`.
pub fn disassemble_instruction(chunk: &Chunk, offset: usize) -> (String, usize) {
    let code = &chunk.code;
    let byte = code[offset];

    let Some(op) = op_from_byte(byte) else {
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
        | Op::QuestionMark
        | Op::Panic
        | Op::ChanNew
        | Op::ChanSend
        | Op::ChanRecv
        | Op::ChanClose
        | Op::ChanTrySend
        | Op::ChanTryRecv
        | Op::ChanSelect
        | Op::TaskSpawn
        | Op::TaskJoin
        | Op::TaskCancel
        | Op::Yield => {
            (format!("{offset:04}  {name}"), offset + 1)
        }

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
        | Op::LoopSetup
        | Op::Recur => {
            let operand = code[offset + 1];
            (format!("{offset:04}  {name:<20} {operand}"), offset + 2)
        }

        // TestBool: u8 (0=false, 1=true)
        Op::TestBool => {
            let operand = code[offset + 1];
            let val = if operand == 0 { "false" } else { "true" };
            (format!("{offset:04}  {name:<20} {operand}    ; {val}"), offset + 2)
        }

        // ── u16 operand with constant comment ─────────────────
        Op::Constant | Op::GetGlobal | Op::SetGlobal | Op::TestTag
        | Op::TestEqual | Op::GetField | Op::DestructRecordField => {
            let index = read_u16(code, offset + 1);
            let comment = constant_comment(chunk, index);
            (format!("{offset:04}  {name:<20} {index:<5} ; {comment}"), offset + 3)
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

        // ── u16 operand (range patterns via constants) ────────
        Op::TestIntRange | Op::TestFloatRange => {
            let index = read_u16(code, offset + 1);
            let comment = constant_comment(chunk, index);
            (format!("{offset:04}  {name:<20} {index:<5} ; {comment}"), offset + 3)
        }

        // ── Jump instructions: show target offset ─────────────
        Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue => {
            let jump_offset = read_u16(code, offset + 1) as usize;
            let target = offset + 3 + jump_offset;
            (format!("{offset:04}  {name:<20} {jump_offset:<5} -> {target:04}"), offset + 3)
        }

        Op::JumpBack => {
            let jump_offset = read_u16(code, offset + 1) as usize;
            let target = offset + 3 - jump_offset;
            (format!("{offset:04}  {name:<20} {jump_offset:<5} -> {target:04}"), offset + 3)
        }

        // ── u16 + u8: CallBuiltin(name_index, argc) ──────────
        Op::CallBuiltin => {
            let name_index = read_u16(code, offset + 1);
            let argc = code[offset + 3];
            let comment = constant_comment(chunk, name_index);
            (
                format!("{offset:04}  {name:<20} {name_index:<5} {argc:<3} ; {comment}"),
                offset + 4,
            )
        }

        // ── u16 + u8: MakeVariant(name_index, field_count) ───
        Op::MakeVariant => {
            let name_index = read_u16(code, offset + 1);
            let field_count = code[offset + 3];
            let comment = constant_comment(chunk, name_index);
            (
                format!("{offset:04}  {name:<20} {name_index:<5} {field_count:<3} ; {comment}"),
                offset + 4,
            )
        }

        // ── MakeClosure: u16 func_index, u8 upvalue_count, then descriptors
        Op::MakeClosure => {
            let func_index = read_u16(code, offset + 1);
            let upvalue_count = code[offset + 3];
            let comment = constant_comment(chunk, func_index);
            let mut line = format!(
                "{offset:04}  {name:<20} {func_index:<5} {upvalue_count:<3} ; {comment}"
            );
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
                write!(line, "\n      |  field {field_name_index:<5} ; {field_comment}").unwrap();
                next += 2;
            }
            (line, next)
        }

        // ── RecordUpdate: u8 field_count, then field_count x u16 field_name_index
        Op::RecordUpdate => {
            let field_count = code[offset + 1];
            let mut line = format!("{offset:04}  {name:<20} {field_count}");
            let mut next = offset + 2;
            for _ in 0..field_count {
                let field_name_index = read_u16(code, next);
                let field_comment = constant_comment(chunk, field_name_index);
                write!(line, "\n      |  field {field_name_index:<5} ; {field_comment}").unwrap();
                next += 2;
            }
            (line, next)
        }
    }
}

// ── Chunk disassembly ─────────────────────────────────────────────

/// Disassemble a complete `Chunk`, returning the formatted output.
pub fn disassemble_chunk(chunk: &Chunk, name: &str) -> String {
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
pub fn disassemble_function(func: &Function) -> String {
    let header = format!(
        "{} (arity={}, upvalues={})",
        func.name, func.arity, func.upvalue_count
    );
    disassemble_chunk(&func.chunk, &header)
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
        let idx = chunk.add_constant(Value::Int(42));
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
        chunk.emit_op(Op::Unit, span);   // 0000
        chunk.emit_op(Op::Pop, span);    // 0001
        chunk.emit_op(Op::Unit, span);   // 0002
        chunk.emit_op(Op::Pop, span);    // 0003
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
        let idx = chunk.add_constant(func);

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

        let type_idx = chunk.add_constant(Value::String("Point".into()));
        let x_idx = chunk.add_constant(Value::String("x".into()));
        let y_idx = chunk.add_constant(Value::String("y".into()));

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

        let name_idx = chunk.add_constant(Value::String("print".into()));
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

        let x_idx = chunk.add_constant(Value::String("x".into()));

        chunk.emit_op(Op::RecordUpdate, span);
        chunk.emit_u8(1, span); // 1 field
        chunk.emit_u16(x_idx, span);
        chunk.emit_op(Op::Return, span);

        let output = disassemble_chunk(&chunk, "update");
        assert!(output.contains("RecordUpdate"));
        assert!(output.contains("\"x\""));
    }

    #[test]
    fn test_op_from_byte_roundtrip() {
        // Every Op variant should round-trip through its u8 discriminant.
        let all_ops = [
            Op::Constant, Op::Unit, Op::True, Op::False,
            Op::Add, Op::Sub, Op::Mul, Op::Div, Op::Mod,
            Op::Eq, Op::Neq, Op::Lt, Op::Gt, Op::Leq, Op::Geq,
            Op::Negate, Op::Not, Op::And, Op::Or,
            Op::StringConcat, Op::DisplayValue,
            Op::GetLocal, Op::SetLocal, Op::GetGlobal, Op::SetGlobal,
            Op::GetUpvalue,
            Op::Call, Op::TailCall, Op::Return, Op::CallBuiltin,
            Op::MakeClosure,
            Op::MakeTuple, Op::MakeList, Op::MakeMap, Op::MakeSet,
            Op::MakeRecord, Op::MakeVariant, Op::RecordUpdate, Op::MakeRange,
            Op::GetField, Op::GetIndex,
            Op::Jump, Op::JumpBack, Op::JumpIfFalse, Op::JumpIfTrue,
            Op::Pop, Op::PopN, Op::Dup,
            Op::TestTag, Op::TestEqual, Op::TestTupleLen, Op::TestListMin,
            Op::TestListExact, Op::TestIntRange, Op::TestFloatRange, Op::TestBool,
            Op::DestructTuple, Op::DestructVariant, Op::DestructList,
            Op::DestructListRest, Op::DestructRecordField,
            Op::LoopSetup, Op::Recur,
            Op::QuestionMark, Op::Panic,
            Op::ChanNew, Op::ChanSend, Op::ChanRecv, Op::ChanClose,
            Op::ChanTrySend, Op::ChanTryRecv, Op::ChanSelect,
            Op::TaskSpawn, Op::TaskJoin, Op::TaskCancel, Op::Yield,
        ];
        for op in all_ops {
            let byte = op as u8;
            let decoded = op_from_byte(byte);
            assert_eq!(decoded, Some(op), "round-trip failed for {op:?} (byte {byte})");
        }
        // Invalid byte should return None.
        assert_eq!(op_from_byte(255), None);
    }
}
