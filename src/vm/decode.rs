//! Decoding a document: `json.parse`, `json.parse_list`,
//! `json.parse_map`, `json.decode`, `T.decode(value)` of a type with no
//! written impl, and `toml.parse`, `toml.parse_list`, `toml.parse_map`.
//!
//! A document is decoded by the one decoder (`builtins::decode`), but a
//! part of a record or enum type the program wrote a `json.Decode` impl
//! for is decoded by the impl's `decode`, wherever it is in the
//! document. The impl is silt code, so decoding such a document is a
//! frame ([`Decoding`]): the decoder reads the document once, collecting
//! each such part (its place and its `json.Value`); the frame calls the
//! impl for each, in the order the decoder came to them; then the
//! decoder reads the document again and puts what the impls gave in
//! their places. An impl's `Err` is the result, with the place of its
//! part appended as a decoder's own errors have it, as the format's
//! error (`Rules::from_impl`). A program with no
//! written `json.Decode` impl, and a document with no such part, takes
//! no frame.

use std::sync::Arc;

use super::runtime::{Native, Step};
use super::{Vm, VmError};
use crate::builtins::decode::{self, Document, Error, Hole, Holes, Reader};
use crate::bytecode::Globals;
use crate::typeinfo::{FieldType, bv};
use crate::value::Value;

/// How a document of the format `D` is read as a value of a field
/// type: as one value (`decode::value`), as a list (`decode::list`), or
/// as a map (`decode::map`) of them.
pub(crate) type Read<D> = fn(&Reader, &D, &FieldType) -> Result<Value, Error>;

/// The frame that decodes a document with parts a written
/// `json.Decode` impl decodes.
struct Decoding<D: Document> {
    /// The builtin that decodes.
    name: &'static str,
    /// The table the parts were found with.
    table: Arc<Globals>,
    doc: D,
    expected: FieldType,
    read: Read<D>,
    holes: Vec<Hole>,
    /// What the impls gave for the parts so far.
    decoded: Vec<Value>,
    /// The error the decoder met after the parts, if it met one.
    error: Option<Error>,
    /// Whether a call is under way.
    calling: bool,
}

impl<D: Document + Send> Native for Decoding<D> {
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if self.calling {
            let at = &self.holes[self.decoded.len()].at;
            match vm.decode_returned(&input)? {
                Ok(value) => self.decoded.push(value),
                Err(error) => {
                    let error = (D::RULES.from_impl)(&error, at);
                    return Ok(Step::Done(Value::variant(bv::ERR, vec![error])));
                }
            }
        }
        let next = self.decoded.len();
        if let Some(hole) = self.holes.get(next) {
            self.calling = true;
            return vm.call_written_decode(hole.slot, hole.json.clone());
        }
        // Every part decoded: the decoder's own error after them, or
        // the value with them.
        if let Some(error) = self.error.take() {
            return Ok(Step::Done(decode::result(D::RULES, Err(error))));
        }
        let table = self.table.clone();
        let written = move |id| table.decoded(id);
        let values: Vec<_> = std::mem::take(&mut self.decoded)
            .into_iter()
            .map(Ok)
            .collect();
        let reader = Reader::with_impls(&vm.runtime, &written, Holes::Supply(values.into_iter()));
        let decoded = (self.read)(&reader, &self.doc, &self.expected);
        Ok(Step::Done(decode::result(D::RULES, decoded)))
    }
}

impl Vm {
    /// The result of `name`, decoding the document `read_doc` gives as
    /// `read` reads it into values of `expected`, honouring the
    /// `json.Decode` impls the program wrote: `Ok` of the value, or
    /// `Err` of the format's error. A type a field of which no decoder
    /// fills is refused before the document is read (`read_doc` gives
    /// it, or the result of the call where there is none).
    pub(crate) fn decode_document<D: Document + Send>(
        &mut self,
        name: &'static str,
        expected: FieldType,
        read: Read<D>,
        read_doc: impl FnOnce() -> Result<D, Value>,
    ) -> Result<Step, VmError> {
        let table = self.decoding_table();
        let written = |id| table.decoded(id);
        let reader = Reader::with_impls(&self.runtime, &written, Holes::Collect(Vec::new()));
        let target = decode::Target::Value(expected.clone());
        if let Some(refused) = decode::refused(&reader, D::RULES, &target) {
            return Ok(Step::Done(refused));
        }
        let doc = match read_doc() {
            Ok(doc) => doc,
            Err(result) => return Ok(Step::Done(result)),
        };
        let decoded = read(&reader, &doc, &expected);
        let holes = reader.into_holes();
        if holes.is_empty() {
            return Ok(Step::Done(decode::result(D::RULES, decoded)));
        }
        Ok(Step::Run(Box::new(Decoding {
            name,
            table,
            doc,
            expected,
            read,
            holes,
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
    /// a call of the impl's method would take.
    pub(crate) fn call_written_decode(&mut self, slot: u16, json: Value) -> Result<Step, VmError> {
        let impl_ = self.global(slot).cloned().ok_or_else(unset_decode)?;
        let impl_ = self.own.value(slot, impl_);
        Ok(self.call(impl_, [json]))
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
