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

use std::sync::Arc;

use super::runtime::{Native, Step};
use super::{Vm, VmError};
use crate::bytecode::Globals;
use crate::value::{Shown, Value};

/// The parts of a value that written `Display` impls show, each with
/// its type's `display`, in the order the formatter comes to them.
type Parts = Vec<(Value, Value)>;

/// What is done with the text of a value once it is shown.
type Then = Box<dyn FnOnce(&mut Vm, String) -> Result<Step, VmError> + Send>;

/// The frame that shows a value with parts a written `Display` impl
/// shows.
struct Showing {
    value: Value,
    /// The table the parts were found with: the VM may have a newer
    /// one by the time the impls have run.
    table: Arc<Globals>,
    parts: Parts,
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
            written_slot(&self.table, part)?;
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

/// Where the `display` the program wrote for the type of `value`, a
/// record or a variant, is kept, by the table `table`.
fn written_slot(table: &Globals, value: &Value) -> Option<u16> {
    let ty = match value {
        Value::Record(record) => record.type_id(),
        Value::Variant(variant) => variant.type_id(),
        _ => return None,
    };
    table.shown(ty)
}

impl Vm {
    /// The `display` the program wrote for the type of `value`.
    ///
    /// An impl of `Display` is written in the module that declares its
    /// type, and a module's functions are set before any of its code
    /// runs, so a value of the type is never there before the impl is:
    /// an impl that is not set yet is a bug of silt, and said so, not
    /// taken for a type without one.
    fn written_display(&self, table: &Globals, value: &Value) -> Result<Option<Value>, VmError> {
        let Some(slot) = written_slot(table, value) else {
            return Ok(None);
        };
        match self.global(slot).cloned() {
            Some(display) => Ok(Some(display)),
            None => Err(VmError::type_confusion(format!(
                "the `display` written for '{}' is called before it is set",
                self.user_facing_type_name(value)
            ))),
        }
    }

    /// What the formatter writes for `value`, with nothing for the
    /// parts a written impl of the table `table` shows; those parts,
    /// each with its type's `display`, in the order the formatter comes
    /// to them; and whether a record or variant in the value has no
    /// written impl in the table.
    fn parts_shown(
        &self,
        table: &Globals,
        value: &Value,
    ) -> Result<(String, Parts, bool), VmError> {
        let parts: RefCell<Parts> = RefCell::new(Vec::new());
        let unset: RefCell<Option<VmError>> = RefCell::new(None);
        let unwritten = Cell::new(false);
        let text = Shown(value, &|part, _| {
            let display = match self.written_display(table, part) {
                Ok(Some(display)) => display,
                Ok(None) => {
                    unwritten.set(true);
                    return None;
                }
                Err(error) => {
                    unset.borrow_mut().get_or_insert(error);
                    return None;
                }
            };
            parts.borrow_mut().push((part.clone(), display));
            Some(Ok(()))
        })
        .to_string();
        match unset.into_inner() {
            Some(error) => Err(error),
            None => Ok((text, parts.into_inner(), unwritten.get())),
        }
    }

    /// A record or variant has no written `Display` impl in this VM's
    /// table: the table that may have one, if there is one. That is
    /// the program's newest, with what the REPL entry being run adds
    /// (see [`Vm::look_again`]): a later entry may have written the
    /// impl, for the VM of a task of an earlier one.
    ///
    /// A program that is no REPL session asks nothing: its VMs have
    /// all of it in their table (`table` is `entry`, one table), and
    /// nothing newer ever comes. So does the VM that runs a session's
    /// entry. Only the VM of a session's task goes on from here.
    fn wider_table(&mut self) -> Option<Arc<Globals>> {
        match Arc::ptr_eq(&self.view.table, &self.view.entry) {
            true => None,
            false => self.entry_table_shown(),
        }
    }

    /// [`Vm::wider_table`], for a VM that may not have all of the
    /// program in its table.
    #[cold]
    #[inline(never)]
    fn entry_table_shown(&mut self) -> Option<Arc<Globals>> {
        self.look_again();
        (self.view.entry.any_shown()).then(|| self.view.entry.clone())
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
        // The table the value is shown by, where it is not this VM's.
        let mut wider = None;
        if !self.view.table.any_shown() {
            // No impl in this VM's table. Only a value that can hold a
            // record or a variant is asked about in a wider one.
            let holds_parts = matches!(
                value,
                Value::Record(_)
                    | Value::Variant(_)
                    | Value::List(_)
                    | Value::Tuple(_)
                    | Value::Map(_)
                    | Value::Set(_)
            );
            wider = holds_parts.then(|| self.wider_table()).flatten();
            if wider.is_none() {
                let text = self.display_value(value);
                return then(self, text);
            }
        }
        // What the formatter writes, with nothing for the parts a
        // written impl shows: complete if there is no such part.
        let table = wider.as_deref().unwrap_or(&self.view.table);
        let (mut text, mut parts, unwritten) = self.parts_shown(table, value)?;
        if unwritten
            && wider.is_none()
            && let Some(table) = self.wider_table()
        {
            (text, parts, _) = self.parts_shown(&table, value)?;
            wider = Some(table);
        }
        if parts.is_empty() {
            return then(self, text);
        }
        Ok(Step::Run(Box::new(Showing {
            value: value.clone(),
            table: wider.unwrap_or_else(|| self.view.table.clone()),
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
                let run = self.run_thread(floor, |vm| vm.run_frames(floor, vm.own_slice()));
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
