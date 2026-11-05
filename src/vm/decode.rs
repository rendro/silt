//! Decoding a document: `json.parse`, `json.decode`, `T.decode(value)`
//! of a type with no written impl, and `toml.parse`, `toml.parse_list`,
//! `toml.parse_map`.
//!
//! A document is decoded by the one decoder (`builtins::decode::seed`),
//! which makes the value as it reads the document; but a part of a
//! record or enum type the program wrote a `json.Decode` impl for is
//! decoded by the impl's `decode`, wherever it is in the document. The
//! impl is silt code, so decoding such a document is a frame
//! ([`Reading`]): the decoder reads the document once, finding each such
//! part (its `json.Value`), in the decoder's order (fields as declared,
//! keys sorted, elements in order); the frame calls the impl for each,
//! in that order; then the decoder reads the document again and puts
//! what the impls gave in their places. An impl's `Err` is the result, with the place of its
//! part appended as a decoder's own errors have it, as the format's
//! error (`Rules::from_impl`). A program with no written `json.Decode`
//! impl, and a document with no such part, takes no frame.

use std::sync::Arc;

use super::runtime::{Native, Step};
use super::{Vm, VmError};
use crate::builtins::decode::seed::{self, Outcome, Parts};
use crate::builtins::decode::{self, Error, Hole, Holes, Reader, Rules};
use crate::bytecode::Globals;
use crate::typeinfo::{FieldType, bv};
use crate::value::Value;

/// A document a decoding reads, again for its second reading: its text,
/// or the document read.
pub(crate) trait Source: Send {
    /// What the document decodes into as a value of `expected`, read by
    /// `r`, its parts of a type with a written impl as `parts` says; or
    /// the result of the call where the text is no document.
    fn read(&self, r: &Reader, expected: &FieldType, parts: &Parts) -> Result<Outcome, Value>;
}

/// The frame that decodes a document with parts a written
/// `json.Decode` impl decodes.
struct Reading {
    /// The builtin that decodes.
    name: &'static str,
    /// The table the parts were found with.
    table: Arc<Globals>,
    rules: &'static Rules,
    source: Box<dyn Source>,
    expected: FieldType,
    holes: Vec<Hole>,
    /// The number of each part, in the order of `holes`.
    numbers: Vec<usize>,
    /// Whether the first reading kept every variant's fields.
    keep_fields: bool,
    /// What the impls gave for the parts so far: a value, or the
    /// `JsonError` of the last.
    decoded: Vec<Result<Value, Value>>,
    /// The error the decoder met after the parts, if it met one.
    error: Option<Error>,
    /// Whether a call is under way.
    calling: bool,
}

impl Native for Reading {
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if std::mem::take(&mut self.calling) {
            let given = vm.decode_returned(&input)?;
            let failed = given.is_err();
            self.decoded.push(given);
            // An impl's `Err` is the result: the document read again to
            // its part says where that is.
            if failed {
                return Ok(Step::Done(self.supplied(vm)));
            }
        }
        let next = self.decoded.len();
        if let Some(hole) = self.holes.get_mut(next) {
            self.calling = true;
            let json = std::mem::replace(&mut hole.json, Value::Unit);
            let ty = std::mem::replace(&mut hole.ty, Value::Unit);
            return vm.call_written_decode(hole.slot, ty, json);
        }
        // Every part decoded: the decoder's own error after them, or
        // the value with them.
        if let Some(error) = self.error.take() {
            return Ok(Step::Done(decode::result(self.rules, Err(error))));
        }
        Ok(Step::Done(self.supplied(vm)))
    }
}

impl Reading {
    /// The result of the document read again with what the impls gave
    /// in the places of its parts.
    fn supplied(&mut self, vm: &Vm) -> Value {
        let table = self.table.clone();
        let written = move |id, args: &[FieldType]| table.decoded(id, args);
        let reader = Reader::with_impls(&vm.runtime, &written, Holes::Collect(Vec::new()));
        let given = std::mem::take(&mut self.decoded);
        let parts = Parts::supplying(&self.numbers, given, self.keep_fields);
        match self.source.read(&reader, &self.expected, &parts) {
            Ok(decoded) => decode::result(self.rules, decoded),
            // (A text read once is a document.)
            Err(result) => result,
        }
    }
}

impl Vm {
    /// The result of `name`, decoding a document into values of
    /// `expected` by the rules `rules`, honouring the `json.Decode`
    /// impls the program wrote: `Ok` of the value, or `Err` of the
    /// format's error. A type a field of which no decoder fills is
    /// refused before the document is read. `read` reads the document,
    /// giving what it decodes into, or the result of the call where
    /// there is no document; `source` is the document to read again
    /// where it has parts a written impl decodes. (An impl is the
    /// program's code: a step.)
    pub(crate) fn decode_read(
        &mut self,
        name: &'static str,
        rules: &'static Rules,
        expected: FieldType,
        read: impl Fn(&Reader, &FieldType, &Parts) -> Result<Outcome, Value>,
        source: impl FnOnce() -> Box<dyn Source>,
    ) -> Result<Step, VmError> {
        let table = self.decoding_table();
        let written = |id, args: &[FieldType]| table.decoded(id, args);
        let reader = Reader::with_impls(&self.runtime, &written, Holes::Collect(Vec::new()));
        let target = decode::Target::Value(expected.clone());
        if let Some(refused) = decode::refused(&reader, rules, &target) {
            return Ok(Step::Done(refused));
        }
        // Where a written impl may decode a part, the first reading only
        // finds the parts.
        let parts = match table.any_decoded() && seed::reaches_written(&reader, rules, &expected) {
            true => Parts::only_finding(),
            false => Parts::finding(),
        };
        let decoded = match read(&reader, &expected, &parts) {
            Ok(decoded) => decoded,
            Err(result) => return Ok(Step::Done(result)),
        };
        let built = parts.built();
        let (found, keep_fields) = parts.into_found();
        if found.is_empty() {
            // (A document with no such part, read again to make its
            // value where the first reading did not.)
            let decoded = match (built, decoded) {
                (false, Ok(_)) => {
                    let making = Parts::supplying(&[], Vec::new(), keep_fields);
                    match read(&reader, &expected, &making) {
                        Ok(decoded) => decoded,
                        Err(result) => return Ok(Step::Done(result)),
                    }
                }
                (_, decoded) => decoded,
            };
            return Ok(Step::Done(decode::result(rules, decoded)));
        }
        drop(reader);
        let (numbers, holes) = found
            .into_iter()
            .map(|found| (found.number, found.hole))
            .unzip();
        Ok(Step::Run(Box::new(Reading {
            name,
            table,
            rules,
            source: source(),
            expected,
            holes,
            numbers,
            keep_fields,
            decoded: Vec::new(),
            error: decoded.err(),
            calling: false,
        })))
    }

    /// The table a document is decoded by: this VM's, or, for a task of
    /// an earlier REPL entry, the newest, where a later entry may have
    /// written an impl.
    pub(crate) fn decoding_table(&mut self) -> Arc<Globals> {
        match self.wider_table(Globals::any_decoded) {
            Some(table) => table,
            None => self.view.table.clone(),
        }
    }

    /// The call of the `decode` the program wrote in the global slot
    /// `slot` with `json`: the VM's own copy of the impl's closure, as
    /// a call of the impl's method would take. The descriptor `ty` of a
    /// generic type (`Box(Int)`) is left in the slot of the callee, as
    /// `Box(Int).decode(v)` leaves it: the impl takes its type
    /// arguments from it (`TypeArg`).
    pub(crate) fn call_written_decode(
        &mut self,
        slot: u16,
        ty: Value,
        json: Value,
    ) -> Result<Step, VmError> {
        let impl_ = self.global(slot).cloned().ok_or_else(unset_decode)?;
        let impl_ = self.own.value(slot, impl_);
        let func_slot = self.stack.len();
        self.stack.push(ty);
        self.stack.push(json);
        Ok(Step::Call {
            callee: impl_,
            argc: self.stack.len() - func_slot - 1,
        })
    }
}

/// An impl of `json.Decode` is written in the module that declares its
/// type, and a module's functions are set before any of its code runs:
/// an impl that is not set yet is a bug of silt, and said so.
fn unset_decode() -> VmError {
    VmError::type_confusion("a `decode` written for a type is called before it is set")
}

impl Vm {
    /// What a call of a written `decode` returned, `given`: `Ok` of the
    /// value, or `Err` of the `JsonError` it returned.
    pub(crate) fn decode_returned(&self, given: &Value) -> Result<Result<Value, Value>, VmError> {
        match given {
            Value::Variant(result) if result.is(bv::OK) => Ok(Ok(result.fields()[0].clone())),
            Value::Variant(result) if result.is(bv::ERR) => Ok(Err(result.fields()[0].clone())),
            other => Err(VmError::type_confusion(format!(
                "decode returned {}, not a Result",
                self.user_facing_type_name(other)
            ))),
        }
    }
}

impl Vm {
    /// `TypeArg`: the descriptor of the `k`th type argument of the type
    /// whose descriptor is on top (`Int` of `Box(Int)`).
    pub(super) fn type_arg(&mut self, k: usize) -> Result<(), VmError> {
        let top = self.stack.len() - 1;
        // (A type of the program, a tuple, or a builtin container:
        // `List(Int)`, `Map(String, Int)`.)
        let arg = match &self.stack[top] {
            Value::TypeDescriptor(info) => match (&info.shape, k) {
                (
                    crate::typeinfo::Shape::Applied(
                        FieldType::Applied(_, args) | FieldType::Tuple(args),
                    ),
                    k,
                ) => args.get(k).cloned(),
                (crate::typeinfo::Shape::Applied(FieldType::List(t) | FieldType::Option(t)), 0) => {
                    Some((**t).clone())
                }
                (crate::typeinfo::Shape::Applied(FieldType::Map(_)), 0) => Some(FieldType::String),
                (crate::typeinfo::Shape::Applied(FieldType::Map(t)), 1) => Some((**t).clone()),
                _ => None,
            },
            _ => None,
        };
        let Some(arg) = arg else {
            return Err(VmError::type_confusion(
                "a written `decode` of a generic type is called without its type arguments",
            ));
        };
        self.stack[top] = decode::descriptor(&self.runtime, &arg);
        Ok(())
    }
}
