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
                Value::String(text) => self.texts.push(text.to_string()),
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
            vm.written_slot(part)?;
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
    /// Where the `display` the program wrote for the type of `value`, a
    /// record or a variant, is kept.
    fn written_slot(&self, value: &Value) -> Option<u16> {
        let ty = match value {
            Value::Record(record) => record.type_id(),
            Value::Variant(variant) => variant.type_id(),
            _ => return None,
        };
        self.global_slots.shown(ty)
    }

    /// The `display` the program wrote for the type of `value`.
    ///
    /// An impl of `Display` is written in the module that declares its
    /// type, and a module's functions are set before any of its code
    /// runs, so a value of the type is never there before the impl is:
    /// an impl that is not set yet is a bug of silt, and said so, not
    /// taken for a type without one.
    fn written_display(&self, value: &Value) -> Result<Option<Value>, VmError> {
        let Some(slot) = self.written_slot(value) else {
            return Ok(None);
        };
        match self.globals.get(usize::from(slot)).cloned().flatten() {
            Some(display) => Ok(Some(display)),
            None => Err(VmError::type_confusion(format!(
                "the `display` written for '{}' is called before it is set",
                self.user_facing_type_name(value)
            ))),
        }
    }

    /// Show `value` and go on with `then`, which gets its text: at once
    /// when no written `Display` impl has a part in it, else as a frame
    /// that calls the impls first.
    pub(crate) fn show(
        &mut self,
        value: &Value,
        then: impl FnOnce(&mut Vm, String) -> Result<Step, VmError> + Send + 'static,
    ) -> Result<Step, VmError> {
        value.writable()?;
        if !self.global_slots.any_shown() {
            let text = self.display_value(value);
            return then(self, text);
        }
        // What the formatter writes, with nothing for the parts a
        // written impl shows: complete if there is no such part.
        let parts: RefCell<Vec<(Value, Value)>> = RefCell::new(Vec::new());
        let unset: RefCell<Option<VmError>> = RefCell::new(None);
        let text = Shown(value, &|part, _| {
            let display = match self.written_display(part) {
                Ok(display) => display?,
                Err(error) => {
                    unset.borrow_mut().get_or_insert(error);
                    return None;
                }
            };
            parts.borrow_mut().push((part.clone(), display));
            Some(Ok(()))
        })
        .to_string();
        if let Some(error) = unset.into_inner() {
            return Err(error);
        }
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

    /// The text `value` is shown with, for a host that has a value in
    /// hand after the code that made it has returned (the `Err` a
    /// `main` or a test returned, a REPL entry's value): as `println`
    /// would write it, written `Display` impls included. The impls run
    /// here, on this thread, as a call of the VM's own. If one of them
    /// fails, or cannot run to its end (it waits for something that
    /// never comes), the text is the value's plain structure.
    pub fn show_text(&mut self, value: &Value) -> String {
        let floor = self.frames.len();
        let stack_floor = self.stack.len();
        match self.shown(value) {
            Ok(Step::Done(Value::String(text))) => text.to_string(),
            Ok(Step::Run(showing)) => {
                self.push_native_frame(showing);
                let run = self.run_thread(floor, |vm| vm.run_frames(floor, usize::MAX));
                match self.finish_run(run, floor, stack_floor) {
                    Ok(Value::String(text)) => text.to_string(),
                    _ => value.to_string(),
                }
            }
            _ => value.to_string(),
        }
    }

    /// [`Vm::show`] for what gives the text as its value.
    pub(crate) fn shown(&mut self, value: &Value) -> Result<Step, VmError> {
        self.show(value, |_, text| Ok(Step::Done(Value::String(text.into()))))
    }
}
