//! Writing a value as data: `json.stringify`, `json.pretty`,
//! `json.encode`, `.encode()`, `toml.stringify`, `toml.pretty` and the
//! parameters of a postgres statement.
//!
//! A value is written by the one writer (`builtins::data`), but a record
//! or variant of a type the program wrote a `json.Encode` impl for is
//! written as the `json.Value` the impl's `encode` gives, wherever it is
//! inside the value. The impl is silt code, so writing such a value is a
//! frame ([`Encoding`]): the writer leaves a hole for each such part, the
//! frame calls the impl for each, in the order the writer came to them,
//! and then fills the holes with what the impls gave. A program with no
//! written `json.Encode` impl, and a value with no such part, takes no
//! frame.

use std::cell::Cell;
use std::sync::Arc;

use super::runtime::{Native, Step};
use super::{Vm, VmError};
use crate::builtins::data::{self, Data, Format, Part};
use crate::bytecode::Globals;
use crate::typeinfo::ty;
use crate::value::Value;

/// What is done with the data of a value once it is written.
type Then = Box<dyn FnOnce(&mut Vm, Data) -> Result<Step, VmError> + Send>;

/// The frame that writes a value with parts a written `json.Encode`
/// impl writes.
struct Encoding {
    caller: String,
    format: Format,
    /// The value as data, with a hole for each part.
    data: Data,
    parts: Vec<Part>,
    /// The `encode` of each part's type.
    impls: Vec<Value>,
    /// What the impls gave for the parts so far.
    encoded: Vec<Value>,
    /// Whether a call is under way.
    calling: bool,
    then: Option<Then>,
}

impl Native for Encoding {
    fn name(&self) -> &str {
        "json.encode"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if self.calling {
            match &input {
                Value::Variant(variant) if variant.of(ty::JSON_VALUE) => self.encoded.push(input),
                other => {
                    return Err(VmError::type_confusion(format!(
                        "encode returned {}, not a json.Value",
                        vm.user_facing_type_name(other)
                    )));
                }
            }
        }
        let next = self.encoded.len();
        if let (Some(part), Some(encode)) = (self.parts.get(next), self.impls.get(next)) {
            self.calling = true;
            // (The VM's own copy of the impl's closure, as a call of
            // the impl's method would take.)
            let encode = vm.own.value(part.slot, encode.clone());
            return Ok(vm.call(encode, [part.value.clone()]));
        }
        let mut data = std::mem::replace(&mut self.data, Data::Null);
        data::fill(
            &self.caller,
            self.format,
            &mut data,
            &self.parts,
            &self.encoded,
        )?;
        let then = self.then.take().ok_or_else(|| {
            VmError::new("internal VM error: a written value was finished twice".into())
        })?;
        then(vm, data)
    }
}

impl Vm {
    /// `value` as data in `format`, for `caller`, and go on with `then`,
    /// which gets it: at once when no written `json.Encode` impl has a
    /// part in it, else as a frame that calls the impls first.
    pub(crate) fn encode(
        &mut self,
        caller: &str,
        format: Format,
        value: &Value,
        then: impl FnOnce(&mut Vm, Data) -> Result<Step, VmError> + Send + 'static,
    ) -> Result<Step, VmError> {
        // The table the value is written by, where it is not this VM's.
        let mut wider = None;
        if !self.view.table.any_encoded() {
            // No impl in this VM's table. Only a value that can hold a
            // record or a variant is asked about in a wider one.
            let holds_parts = matches!(
                value,
                Value::Record(_)
                    | Value::Variant(_)
                    | Value::List(_)
                    | Value::Tuple(_)
                    | Value::Map(_)
            );
            wider = holds_parts
                .then(|| self.wider_table(Globals::any_encoded))
                .flatten();
            if wider.is_none() {
                let data = data::data(caller, format, value)?;
                return then(self, data);
            }
        }
        let table = wider.unwrap_or_else(|| self.view.table.clone());
        let (mut data, mut parts, unwritten) = written_parts(caller, format, &table, value)?;
        if unwritten
            && Arc::ptr_eq(&table, &self.view.table)
            && let Some(table) = self.wider_table(Globals::any_encoded)
        {
            (data, parts, _) = written_parts(caller, format, &table, value)?;
        }
        if parts.is_empty() {
            return then(self, data);
        }
        let impls = parts
            .iter()
            .map(|part| self.written_encode(part))
            .collect::<Result<Vec<Value>, VmError>>()?;
        Ok(Step::Run(Box::new(Encoding {
            caller: caller.to_string(),
            format,
            data,
            parts,
            impls,
            encoded: Vec::new(),
            calling: false,
            then: Some(Box::new(then)),
        })))
    }

    /// The `encode` the program wrote for the type of `part`.
    ///
    /// An impl of `json.Encode` is written in the module that declares
    /// its type, and a module's functions are set before any of its code
    /// runs, so a value of the type is never there before the impl is:
    /// an impl that is not set yet is a bug of silt, and said so.
    fn written_encode(&self, part: &Part) -> Result<Value, VmError> {
        self.global(part.slot).cloned().ok_or_else(|| {
            VmError::type_confusion(format!(
                "the `encode` written for '{}' is called before it is set",
                self.user_facing_type_name(&part.value)
            ))
        })
    }
}

/// `value` as data with a hole for each part that a written
/// `json.Encode` impl of the table `table` writes; those parts; and
/// whether a record or variant in the value has no written impl in the
/// table.
fn written_parts(
    caller: &str,
    format: Format,
    table: &Globals,
    value: &Value,
) -> Result<(Data, Vec<Part>, bool), VmError> {
    let unwritten = Cell::new(false);
    let written = |part: &Value| {
        let ty = match part {
            Value::Record(record) => record.type_id(),
            Value::Variant(variant) => variant.type_id(),
            _ => return None,
        };
        let slot = table.encoded(ty);
        unwritten.set(unwritten.get() || slot.is_none());
        slot
    };
    let (data, parts) = data::data_with_holes(caller, format, value, &written)?;
    Ok((data, parts, unwritten.get()))
}
