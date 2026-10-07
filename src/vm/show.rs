//! Showing a value: `println`, `print`, interpolation, `.display()`,
//! `string.from`, a panic's message.
//!
//! A value is shown by the VM's formatter (`Value`'s `Display`), but a
//! record or variant of a type the program wrote a `Display` impl for is
//! shown as the impl says, wherever it is inside the value. The impl is
//! silt code, so showing such a value is a frame ([`Showing`]): it calls
//! the impl for each such part, in the order the parts are written, and
//! then writes the value with what the impls gave. A program with no
//! written `Display` impl, and a value with no such part, takes no
//! frame.

use std::cell::{Cell, RefCell};

use super::runtime::{Native, Step};
use super::{Vm, VmError};
use crate::value::{Shown, Value};

/// What is done with the text of a value once it is shown.
type Then = Box<dyn FnOnce(&mut Vm, String) -> Result<Step, VmError> + Send>;

/// The frame that shows a value with parts a written `Display` impl
/// shows.
struct Showing {
    value: Value,
    /// Each such part, with its type's `display`, in the order the
    /// formatter comes to them.
    parts: Vec<(Value, Value)>,
    /// What the impls gave for the parts so far.
    texts: Vec<String>,
    /// Whether a call is under way.
    calling: bool,
    then: Option<Then>,
}

impl Native for Showing {
    fn name(&self) -> &str {
        "display"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if self.calling {
            match input {
                Value::String(text) => self.texts.push(text),
                other => {
                    return Err(VmError::type_confusion(format!(
                        "display returned {}, not a String",
                        vm.user_facing_type_name(&other)
                    )));
                }
            }
        }
        if let Some((part, display)) = self.parts.get(self.texts.len()) {
            self.calling = true;
            return Ok(vm.call(display.clone(), [part.clone()]));
        }
        let next = Cell::new(0);
        let texts = &self.texts;
        let text = Shown(&self.value, &|part, f| {
            vm.written_display(part)?;
            let text = &texts[next.get()];
            next.set(next.get() + 1);
            Some(f.write_str(text))
        })
        .to_string();
        let then = self.then.take().ok_or_else(|| {
            VmError::new("internal VM error: a shown value was finished twice".into())
        })?;
        then(vm, text)
    }
}

impl Vm {
    /// The `display` the program wrote for the type of `value`, a record
    /// or a variant.
    fn written_display(&self, value: &Value) -> Option<Value> {
        let ty = match value {
            Value::Record(ty, _) => ty.id,
            Value::Variant(tag, _) => tag.type_id(),
            _ => return None,
        };
        let slot = self.global_slots.shown(ty)?;
        self.globals.get(usize::from(slot)).cloned().flatten()
    }

    /// Show `value` and go on with `then`, which gets its text: at once
    /// when no written `Display` impl has a part in it, else as a frame
    /// that calls the impls first.
    pub(crate) fn show(
        &mut self,
        value: &Value,
        then: impl FnOnce(&mut Vm, String) -> Result<Step, VmError> + Send + 'static,
    ) -> Result<Step, VmError> {
        if !self.global_slots.any_shown() {
            let text = self.display_value(value);
            return then(self, text);
        }
        // What the formatter writes, with nothing for the parts a
        // written impl shows: complete if there is no such part.
        let parts: RefCell<Vec<(Value, Value)>> = RefCell::new(Vec::new());
        let text = Shown(value, &|part, _| {
            let display = self.written_display(part)?;
            parts.borrow_mut().push((part.clone(), display));
            Some(Ok(()))
        })
        .to_string();
        let parts = parts.into_inner();
        if parts.is_empty() {
            return then(self, text);
        }
        Ok(Step::Run(Box::new(Showing {
            value: value.clone(),
            parts,
            texts: Vec::new(),
            calling: false,
            then: Some(Box::new(then)),
        })))
    }

    /// [`Vm::show`] for what gives the text as its value.
    pub(crate) fn shown(&mut self, value: &Value) -> Result<Step, VmError> {
        self.show(value, |_, text| Ok(Step::Done(Value::String(text))))
    }
}
