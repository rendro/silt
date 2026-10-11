//! Bytecode disassembler for debugging.
//!
//! Pretty-prints compiled `Chunk` objects in human-readable form.

use std::fmt::Write;

use crate::bytecode::{Chunk, Const, Function, Globals, Instr, decode};
use crate::value::Value;

// ── Helpers ───────────────────────────────────────────────────────

/// Format a constant value for a disassembly comment.
fn constant_comment(chunk: &Chunk, k: Const) -> String {
    match chunk.constant(k) {
        // A function is shown by its name; any other constant as a
        // program writes it.
        function @ (Value::VmClosure(_) | Value::BuiltinFn(_) | Value::HostFn(_)) => {
            format!("{function}")
        }
        constant => format!("{constant:?}"),
    }
}

/// The lines of a list of names: one continuation line per entry, with
/// `label` as the per-entry prefix (`"field"` or `"exclude"`).
fn name_lines(chunk: &Chunk, names: impl Iterator<Item = Const>, label: &str) -> String {
    let mut lines = String::new();
    for k in names {
        write!(
            lines,
            "\n      |  {label} {:<5} ; {}",
            k.index(),
            constant_comment(chunk, k)
        )
        .unwrap();
    }
    lines
}

// ── Instruction disassembly ───────────────────────────────────────

/// Disassemble the instruction `instr`, decoded at `offset`; the one
/// after it is at `next`.
fn disassemble_instruction(
    chunk: &Chunk,
    globals: &Globals,
    offset: usize,
    instr: Instr,
    next: usize,
) -> String {
    let code = chunk.code();
    let name = instr.op().name();
    // The operand of a jump is its distance from the next instruction,
    // negative for a jump back.
    let jump = |target: usize| {
        let distance = match target >= next {
            true => (target - next).to_string(),
            false => format!("-{}", next - target),
        };
        format!("{offset:04}  {name:<20} {distance:<5} -> {target:04}")
    };
    // An instruction whose one operand is a constant.
    let with_constant = |k: Const| {
        let comment = constant_comment(chunk, k);
        format!("{offset:04}  {name:<20} {:<5} ; {comment}", k.index())
    };
    // An instruction whose one operand is a number.
    let with_number = |n: usize| format!("{offset:04}  {name:<20} {n}");
    // An instruction whose operands are a constant and a count.
    let with_constant_and_count = |k: Const, count: usize| {
        let comment = constant_comment(chunk, k);
        format!(
            "{offset:04}  {name:<20} {:<5} {count:<3} ; {comment}",
            k.index()
        )
    };

    match instr {
        // ── No operands ───────────────────────────────────────
        Instr::Unit
        | Instr::True
        | Instr::False
        | Instr::Add
        | Instr::Sub
        | Instr::Mul
        | Instr::Div
        | Instr::Mod
        | Instr::Eq
        | Instr::Neq
        | Instr::Lt
        | Instr::Gt
        | Instr::Leq
        | Instr::Geq
        | Instr::Negate
        | Instr::Not
        | Instr::DisplayValue
        | Instr::Return
        | Instr::Pop
        | Instr::Dup
        | Instr::MakeRange
        | Instr::ListConcat
        | Instr::QuestionMark
        | Instr::Panic => format!("{offset:04}  {name}"),

        // ── A count, an index or a slot ───────────────────────
        Instr::StringConcat { count: n }
        | Instr::GetUpvalue { index: n }
        | Instr::Call { argc: n }
        | Instr::TailCall { argc: n }
        | Instr::MakeTuple { count: n }
        | Instr::TestTupleLen { len: n }
        | Instr::TestListMin { len: n }
        | Instr::TestListExact { len: n }
        | Instr::DestructTuple { index: n }
        | Instr::DestructVariant { index: n }
        | Instr::DestructList { index: n }
        | Instr::DestructListRest { start: n }
        | Instr::GetField { index: n }
        | Instr::DestructRecordField { index: n }
        | Instr::GetLocal { slot: n }
        | Instr::SetLocal { slot: n }
        | Instr::Slide { slot: n }
        | Instr::MakeList { count: n }
        | Instr::MakeMap { pairs: n }
        | Instr::MakeSet { count: n } => with_number(n),

        Instr::Recur { argc, first } => {
            format!("{offset:04}  {name:<20} {argc}  slot {first}")
        }

        // ── A constant ────────────────────────────────────────
        // TestTag: the variant the test is for (its operand is the
        // variant's tag, kept as a constructor constant).
        Instr::TestTag { tag } => {
            let comment = format!("<variant:{}>", chunk.tag(tag));
            format!("{offset:04}  {name:<20} {:<5} ; {comment}", tag.index())
        }

        Instr::Constant { k }
        | Instr::TestEqual { k }
        | Instr::GetFieldNamed { name: k }
        | Instr::DestructRecordFieldNamed { name: k }
        | Instr::TestRecordTag { ty: k }
        | Instr::TestMapHasKey { key: k }
        | Instr::DestructMapValue { key: k } => with_constant(k),

        // ── A global slot, commented with its definition ──────
        Instr::GetGlobal { slot } | Instr::SetGlobal { slot } => {
            format!("{offset:04}  {name:<20} {slot:<5} ; {}", globals.name(slot))
        }

        // ── The two bounds of a range pattern ─────────────────
        Instr::TestIntRange { lo, hi } | Instr::TestFloatRange { lo, hi } => {
            let lo_comment = constant_comment(chunk, lo);
            let hi_comment = constant_comment(chunk, hi);
            format!(
                "{offset:04}  {name:<20} {:<5} {:<5} ; {lo_comment}..{hi_comment}",
                lo.index(),
                hi.index()
            )
        }

        // ── Jumps: the distance and the target offset ─────────
        Instr::Jump { to } | Instr::JumpIfFalse { to } | Instr::JumpIfTrue { to } => jump(to),

        // CallBuiltin: the number of arguments, and the builtin by its
        // name (its id is its place among the rows of this build).
        Instr::CallBuiltin { builtin, argc } => {
            format!("{offset:04}  {name:<20} {argc:<5} ; {builtin}")
        }
        // CallMethod: the method name, argc, and the trait whose method
        // it calls (shown after the name when the call names one).
        Instr::CallMethod { method, argc, of } | Instr::TailCallMethod { method, argc, of } => {
            let line = with_constant_and_count(method, argc);
            match globals.trait_name(of) {
                Some(t) => format!("{line} of {t}"),
                None => line,
            }
        }

        // ── MakeClosure: the function, then what it captures ──
        Instr::MakeClosure { f, captures } => {
            let mut line = with_constant_and_count(f, captures.len());
            for capture in captures.iter(code) {
                let locality = if capture.is_local { "local" } else { "upvalue" };
                write!(line, "\n      |  {locality} {}", capture.index).unwrap();
            }
            line
        }

        // ── MakeRecord: the type, then the field names ────────
        Instr::MakeRecord { ty, fields } => {
            with_constant_and_count(ty, fields.len())
                + &name_lines(chunk, fields.iter(code), "field")
        }

        // ── A list of names ───────────────────────────────────
        //   RecordUpdate:        label "field".
        //   DestructRecordRest:  label "exclude".
        Instr::RecordUpdate { fields } => {
            with_number(fields.len()) + &name_lines(chunk, fields.iter(code), "field")
        }
        Instr::DestructRecordRest { excluded } => {
            with_number(excluded.len()) + &name_lines(chunk, excluded.iter(code), "exclude")
        }
    }
}

// ── Chunk disassembly ─────────────────────────────────────────────

/// Disassemble a complete `Chunk`, returning the formatted output.
/// Global slots are named by `globals`.
fn disassemble_chunk(chunk: &Chunk, globals: &Globals, name: &str) -> String {
    let mut output = format!("== {name} ==\n");
    let mut offset = 0;
    while let Some((instr, next)) = decode(chunk.code(), offset) {
        output.push_str(&disassemble_instruction(
            chunk, globals, offset, instr, next,
        ));
        output.push('\n');
        offset = next;
    }
    output
}

/// Disassemble a compiled `Function`, returning the formatted output.
/// Global slots are named by `globals`, the program's.
///
/// Recursively disassembles nested functions found as `VmClosure` constants.
pub fn disassemble_function(func: &Function, globals: &Globals) -> String {
    let header = format!(
        "{} (arity={}, upvalues={})",
        func.name(),
        func.arity(),
        func.upvalue_count()
    );
    let mut output = disassemble_chunk(func.chunk(), globals, &header);

    // Recurse into nested functions stored as VmClosure constants.
    for constant in func.chunk().constants() {
        if let Value::VmClosure(closure) = constant {
            output.push('\n');
            output.push_str(&disassemble_function(&closure.function, globals));
        }
    }

    output
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Asm, Emitter, Op, UpvalueDesc, VmClosure};
    use crate::source::Span;
    use std::sync::Arc;

    fn span() -> Span {
        Span::BUILTIN
    }

    /// The disassembly of the function `build` emits, which takes
    /// `arity` arguments and has `upvalues` upvalues.
    fn disassembly(arity: usize, upvalues: usize, build: impl FnOnce(&mut Emitter)) -> String {
        let mut emitter = Emitter::new("test".into(), arity, span()).unwrap();
        build(&mut emitter);
        let function = emitter.finish(upvalues).unwrap_or_else(|e| panic!("{e:?}"));
        disassemble_function(&function, &Globals::default())
    }

    #[test]
    fn test_simple_ops() {
        let output = disassembly(0, 0, |e| {
            e.emit(Asm::True, span()).unwrap();
            e.emit(Asm::False, span()).unwrap();
            e.emit(Asm::Eq, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert!(output.contains("== test (arity=0, upvalues=0) =="));
        assert!(output.contains("0000  True"));
        assert!(output.contains("0001  False"));
        assert!(output.contains("0002  Eq"));
        assert!(output.contains("0003  Return"));
    }

    #[test]
    fn test_constant_op() {
        let output = disassembly(0, 0, |e| {
            let k = e.constant(Value::Int(42), span()).unwrap();
            e.emit(Asm::Constant { k }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert!(
            output.contains("0000  Constant             0     ; 42"),
            "{output}"
        );
    }

    #[test]
    fn test_jump_targets() {
        let output = disassembly(0, 0, |e| {
            let start = e.label();
            let end = e.label();
            e.bind(start, span()).unwrap(); // 0000
            e.emit(Asm::True, span()).unwrap(); // 0000
            e.emit(Asm::JumpIfTrue { to: end }, span()).unwrap(); // 0001
            e.emit(Asm::Jump { to: start }, span()).unwrap(); // 0004
            e.bind(end, span()).unwrap(); // 0009
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        // A jump shows its distance from the next instruction, then its
        // target.
        assert!(
            output.contains("0001  JumpIfTrue           5     -> 0009"),
            "{output}"
        );
        assert!(
            output.contains("0004  Jump                 -9    -> 0000"),
            "{output}"
        );
    }

    #[test]
    fn test_make_closure() {
        let inner = {
            let mut e = Emitter::new("inner".into(), 0, span()).unwrap();
            e.emit(Asm::GetUpvalue { index: 1 }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
            e.finish(2).unwrap()
        };
        let output = disassembly(4, 1, |e| {
            let f = e
                .constant(
                    Value::VmClosure(Arc::new(VmClosure {
                        function: Arc::new(inner),
                        upvalues: vec![],
                    })),
                    span(),
                )
                .unwrap();
            let captures = [
                UpvalueDesc {
                    is_local: true,
                    index: 3,
                },
                UpvalueDesc {
                    is_local: false,
                    index: 0,
                },
            ];
            e.emit(
                Asm::MakeClosure {
                    f,
                    captures: &captures,
                },
                span(),
            )
            .unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert!(
            output.contains("MakeClosure          0     2   ; "),
            "{output}"
        );
        assert!(
            output.contains("\n      |  local 3\n      |  upvalue 0\n"),
            "{output}"
        );
        // The function among the constants is disassembled too.
        assert!(
            output.contains("== inner (arity=0, upvalues=2) =="),
            "{output}"
        );
    }

    #[test]
    fn test_make_record() {
        let output = disassembly(2, 0, |e| {
            let anon = crate::typeinfo::builtin_type(crate::typeinfo::ty::ANON_RECORD);
            let ty = e
                .constant(Value::TypeDescriptor(anon.clone()), span())
                .unwrap();
            let x = e.constant(Value::String("x".into()), span()).unwrap();
            let y = e.constant(Value::String("y".into()), span()).unwrap();
            e.emit(
                Asm::MakeRecord {
                    ty,
                    fields: &[x, y],
                },
                span(),
            )
            .unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert!(
            output.contains("0000  MakeRecord           0     2   ; "),
            "{output}"
        );
        assert!(
            output.contains("\n      |  field 1     ; \"x\""),
            "{output}"
        );
        assert!(
            output.contains("\n      |  field 2     ; \"y\""),
            "{output}"
        );
        // The next instruction is decoded after the field names.
        assert!(output.contains("0008  Return"), "{output}");
    }

    #[test]
    fn test_call_builtin() {
        let output = disassembly(1, 0, |e| {
            let builtin = crate::builtins::registry::registry()
                .named("print")
                .expect("print")
                .id;
            e.emit(Asm::CallBuiltin { builtin, argc: 1 }, span())
                .unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert!(
            output.contains("0000  CallBuiltin          1     ; print"),
            "{output}"
        );
    }

    #[test]
    fn test_number_operands() {
        let output = disassembly(3, 0, |e| {
            e.emit(Asm::MakeTuple { count: 2 }, span()).unwrap();
            e.emit(Asm::Call { argc: 1 }, span()).unwrap();
            e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::Slide { slot: 0 }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert!(
            output.contains("0000  MakeTuple            2\n"),
            "{output}"
        );
        assert!(
            output.contains("0002  Call                 1\n"),
            "{output}"
        );
        assert!(
            output.contains("0004  GetLocal             0\n"),
            "{output}"
        );
        assert!(
            output.contains("0007  Slide                0\n"),
            "{output}"
        );
        assert!(output.contains("0010  Return"), "{output}");
    }

    #[test]
    fn test_record_update() {
        let output = disassembly(2, 0, |e| {
            let x = e.constant(Value::String("x".into()), span()).unwrap();
            e.emit(Asm::RecordUpdate { fields: &[x] }, span()).unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert!(
            output.contains("0000  RecordUpdate         1\n"),
            "{output}"
        );
        // The per-entry label is "field", not "exclude" (the
        // DestructRecordRest label).
        assert!(output.contains("      |  field 0     ; \"x\""), "{output}");
    }

    #[test]
    fn test_destruct_record_rest_label() {
        let output = disassembly(1, 0, |e| {
            let z = e.constant(Value::String("z".into()), span()).unwrap();
            e.emit(Asm::DestructRecordRest { excluded: &[z] }, span())
                .unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert!(
            output.contains("      |  exclude 0     ; \"z\""),
            "{output}"
        );
        // Must NOT use the sibling label.
        assert!(!output.contains("field "), "{output}");
    }

    #[test]
    fn test_call_method_format() {
        let output = disassembly(1, 0, |e| {
            let method = e.constant(Value::String("len".into()), span()).unwrap();
            e.emit(
                Asm::CallMethod {
                    method,
                    argc: 1,
                    of: 0,
                },
                span(),
            )
            .unwrap();
            e.emit(Asm::Return, span()).unwrap();
        });
        assert!(
            output.contains("0000  CallMethod           0     1   ; \"len\"\n"),
            "{output}"
        );
        // The next instruction is decoded after the trait operand.
        assert!(output.contains("0006  Return"), "{output}");
    }

    #[test]
    fn test_every_opcode_has_a_name() {
        for op in Op::ALL {
            assert!(!op.name().is_empty());
        }
    }
}
