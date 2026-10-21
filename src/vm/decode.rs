//! Decoding a json document: `json.parse`, `json.parse_list`,
//! `json.parse_map`, `json.decode`, and `T.decode(value)` of a type with
//! no written impl.
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
//! part appended as a decoder's own errors have it. A program with no
//! written `json.Decode` impl, and a document with no such part, takes
//! no frame.

use std::sync::Arc;

use super::runtime::{Native, Step};
use super::{Vm, VmError};
use crate::builtins::decode::{self, Error, Hole, Holes, Reader};
use crate::builtins::json::RULES;
use crate::bytecode::Globals;
use crate::typeinfo::{FieldType, bv, ty};
use crate::value::Value;

/// How a document is read as a value of a field type: as one value
/// (`decode::value`), as a list (`decode::list`), or as a map
/// (`decode::map`) of them.
pub(crate) type Read = fn(&Reader, &serde_json::Value, &FieldType) -> Result<Value, Error>;

/// The frame that decodes a document with parts a written
/// `json.Decode` impl decodes.
struct Decoding {
    /// The table the parts were found with.
    table: Arc<Globals>,
    doc: serde_json::Value,
    expected: FieldType,
    read: Read,
    holes: Vec<Hole>,
    /// The `decode` of each part's type.
    impls: Vec<Value>,
    /// What the impls gave for the parts so far.
    decoded: Vec<Value>,
    /// The error the decoder met after the parts, if it met one.
    error: Option<Error>,
    /// Whether a call is under way.
    calling: bool,
}

impl Native for Decoding {
    fn name(&self) -> &str {
        "json.decode"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if self.calling {
            let at = &self.holes[self.decoded.len()].at;
            match &input {
                Value::Variant(result) if result.is(bv::OK) => {
                    self.decoded.push(result.fields()[0].clone());
                }
                Value::Variant(result) if result.is(bv::ERR) => {
                    let error = at_part(&result.fields()[0], at);
                    return Ok(Step::Done(Value::variant(bv::ERR, vec![error])));
                }
                other => {
                    return Err(VmError::type_confusion(format!(
                        "decode returned {}, not a Result",
                        vm.user_facing_type_name(other)
                    )));
                }
            }
        }
        let next = self.decoded.len();
        if let (Some(hole), Some(impl_)) = (self.holes.get(next), self.impls.get(next)) {
            self.calling = true;
            // (The VM's own copy of the impl's closure, as a call of
            // the impl's method would take.)
            let impl_ = vm.own.value(hole.slot, impl_.clone());
            return Ok(vm.call(impl_, [hole.json.clone()]));
        }
        // Every part decoded: the decoder's own error after them, or
        // the value with them.
        if let Some(error) = self.error.take() {
            return Ok(Step::Done(decode::result(&RULES, Err(error))));
        }
        let table = self.table.clone();
        let written = move |id| table.decoded(id);
        let values = std::mem::take(&mut self.decoded).into_iter();
        let reader = Reader::with_impls(&vm.runtime, &written, Holes::Supply(values));
        let decoded = (self.read)(&reader, &self.doc, &self.expected);
        Ok(Step::Done(decode::result(&RULES, decoded)))
    }
}

/// The `JsonError` `error`, which an impl gave for a part `at` a path,
/// as the decoder's error of a value there: a message ends with the
/// path, a missing field's path starts with it.
fn at_part(error: &Value, at: &str) -> Value {
    let Value::Variant(variant) = error else {
        return error.clone();
    };
    if at.is_empty() || !variant.of(ty::JSON_ERROR) {
        return error.clone();
    }
    let ended = |text: &Value| match text {
        Value::String(s) => Value::String(format!("{s} at {at}").into()),
        other => other.clone(),
    };
    let mut fields = variant.fields().to_vec();
    if variant.is(bv::JSON_MISSING_FIELD) {
        if let Some(Value::String(field)) = fields.first() {
            fields[0] = Value::String(decode::key_path(at, field).into());
        }
    } else if variant.is(bv::JSON_TYPE_MISMATCH) {
        if let Some(actual) = fields.get_mut(1) {
            *actual = ended(actual);
        }
    } else if let Some(message) = fields.first_mut() {
        // `Syntax(message, offset)`, `Other(message)`.
        *message = ended(message);
    }
    Value::variant(variant.tag(), fields)
}

impl Vm {
    /// The result of decoding the json document `doc` as `read` reads
    /// it into values of `expected`, honouring the `json.Decode` impls
    /// the program wrote: `Ok` of the value, or `Err` of the
    /// `JsonError`. A type a field of which no decoder fills is refused
    /// before the document is read (`read_doc` gives it, or the result
    /// of the call where there is none).
    pub(crate) fn decode_json(
        &mut self,
        expected: FieldType,
        read: Read,
        read_doc: impl FnOnce() -> Result<serde_json::Value, Value>,
    ) -> Result<Step, VmError> {
        let table = self.decoding_table();
        let written = |id| table.decoded(id);
        let reader = Reader::with_impls(&self.runtime, &written, Holes::Collect(Vec::new()));
        let target = decode::Target::Value(expected.clone());
        if let Some(refused) = decode::refused(&reader, &RULES, &target) {
            return Ok(Step::Done(refused));
        }
        let doc = match read_doc() {
            Ok(doc) => doc,
            Err(result) => return Ok(Step::Done(result)),
        };
        let decoded = read(&reader, &doc, &expected);
        let holes = reader.into_holes();
        if holes.is_empty() {
            return Ok(Step::Done(decode::result(&RULES, decoded)));
        }
        let impls = holes
            .iter()
            .map(|hole| self.written_decode(hole))
            .collect::<Result<Vec<Value>, VmError>>()?;
        Ok(Step::Run(Box::new(Decoding {
            table,
            doc,
            expected,
            read,
            holes,
            impls,
            decoded: Vec::new(),
            error: decoded.err(),
            calling: false,
        })))
    }

    /// The table a document is decoded by: this VM's, or, for a task of
    /// an earlier REPL entry, the newest, where a later entry may have
    /// written an impl.
    fn decoding_table(&mut self) -> Arc<Globals> {
        match self.wider_table(Globals::any_decoded) {
            Some(table) => table,
            None => self.view.table.clone(),
        }
    }

    /// The `decode` the program wrote for the type of the part `hole`.
    ///
    /// An impl of `json.Decode` is written in the module that declares
    /// its type, and a module's functions are set before any of its code
    /// runs: an impl that is not set yet is a bug of silt, and said so.
    fn written_decode(&self, hole: &Hole) -> Result<Value, VmError> {
        self.global(hole.slot).cloned().ok_or_else(|| {
            VmError::type_confusion("a `decode` written for a type is called before it is set")
        })
    }
}
