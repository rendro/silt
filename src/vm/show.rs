//! Showing a value: `println`, `print`, interpolation, `string.from`, a
//! panic's message.
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
use std::fmt::Write as _;

use std::sync::Arc;

use super::runtime::{Native, Step};
use super::{Vm, VmError};
use crate::builtins::common::Bounded;
use crate::bytecode::Globals;
use crate::value::{Shown, Value};

/// How long the text of a shown value may be.
#[derive(Clone, Copy)]
pub(crate) enum Limit {
    /// No longer than a string may be: a longer one is an error of the
    /// builtin named (interpolation, `string.from`).
    Refuse(&'static str),
    /// The first that many bytes, and how many more there were: a
    /// message that must be given whatever its length (a panic's, the
    /// text of `main`'s `Err`).
    Cut(usize),
    /// None kept: only the parts written impls show are wanted (a print,
    /// which writes the text out itself).
    Discard,
    /// All of it (a REPL entry's value, as the released REPL shows it).
    Whole,
}

/// The most of a message a panic, or the `Err` of `main` or of a test,
/// gives; the rest is said by its length.
pub(crate) const MESSAGE_MOST: usize = 100_000;

/// The most of a print's text that is made whole: a longer one is
/// written out in pieces as it is made ([`HostIo::print_streamed`]).
///
/// [`HostIo::print_streamed`]: super::HostIo::print_streamed
const PRINTED_WHOLE: usize = 1_000_000;

/// The text `write` writes, as `limit` lets it be.
fn written(
    limit: Limit,
    write: impl FnOnce(&mut dyn std::fmt::Write) -> std::fmt::Result,
) -> Result<String, VmError> {
    match limit {
        Limit::Refuse(name) => {
            let mut text = Bounded::default();
            let _ = write(&mut text);
            text.text(name, None)
        }
        Limit::Cut(most) => {
            let mut text = Cut::new(most);
            let _ = write(&mut text);
            Ok(text.text())
        }
        Limit::Whole => {
            let mut text = String::new();
            let _ = write(&mut text);
            Ok(text)
        }
        Limit::Discard => {
            struct Nothing;
            impl std::fmt::Write for Nothing {
                fn write_str(&mut self, _: &str) -> std::fmt::Result {
                    Ok(())
                }
            }
            let _ = write(&mut Nothing);
            Ok(String::new())
        }
    }
}

/// A text kept to its first `most` bytes (at a character's end), with
/// how many more were written.
pub(crate) struct Cut {
    kept: String,
    most: usize,
    more: usize,
}

impl Cut {
    pub(crate) fn new(most: usize) -> Cut {
        Cut {
            kept: String::new(),
            most,
            more: 0,
        }
    }

    /// The text: whole, or its first part and `… (N more bytes)`.
    pub(crate) fn text(self) -> String {
        match self.more {
            0 => self.kept,
            more => format!("{}… ({more} more bytes)", self.kept),
        }
    }
}

impl std::fmt::Write for Cut {
    fn write_str(&mut self, part: &str) -> std::fmt::Result {
        if self.more == 0 {
            let room = self.most - self.kept.len();
            if part.len() <= room {
                self.kept.push_str(part);
                return Ok(());
            }
            let mut end = room;
            while !part.is_char_boundary(end) {
                end -= 1;
            }
            self.kept.push_str(&part[..end]);
            self.more = part.len() - end;
            return Ok(());
        }
        self.more += part.len();
        Ok(())
    }
}

/// A text kept whole as long as it is no longer than `most`: a longer
/// one stops the writing.
struct Whole {
    text: String,
    most: usize,
    over: bool,
}

impl std::fmt::Write for Whole {
    fn write_str(&mut self, part: &str) -> std::fmt::Result {
        if self.text.len() + part.len() > self.most {
            self.over = true;
            return Err(std::fmt::Error);
        }
        self.text.push_str(part);
        Ok(())
    }
}

/// What is done with a shown value: its text given to `then`, as long
/// as `Limit` lets it be; or printed to `stream` (with a line end when
/// `newline`), by the builtin `call`, whatever its length.
pub(crate) enum Deliver {
    Text(Limit, Then),
    Print {
        stream: super::Stream,
        call: &'static str,
        newline: bool,
    },
}

impl Deliver {
    /// The limit of the text made before it is delivered.
    fn limit(&self) -> Limit {
        match self {
            Deliver::Text(limit, _) => *limit,
            Deliver::Print { .. } => Limit::Discard,
        }
    }

    /// Deliver the text `write` writes.
    fn deliver(
        self,
        vm: &mut Vm,
        write: &dyn Fn(&mut dyn std::fmt::Write) -> std::fmt::Result,
    ) -> Result<Step, VmError> {
        match self {
            Deliver::Text(limit, then) => {
                let text = written(limit, |out| write(out))?;
                then(vm, text)
            }
            Deliver::Print {
                stream,
                call,
                newline,
            } => print_shown(vm, stream, call, newline, write),
        }
    }
}

/// Print the text `write` writes to `stream`: made whole and printed as
/// any print is, when it is no longer than [`PRINTED_WHOLE`]; else
/// written out in pieces as it is made, so that a text of any length
/// takes no more memory than a piece.
fn print_shown(
    vm: &mut Vm,
    stream: super::Stream,
    call: &'static str,
    newline: bool,
    write: &dyn Fn(&mut dyn std::fmt::Write) -> std::fmt::Result,
) -> Result<Step, VmError> {
    let mut whole = Whole {
        text: String::new(),
        most: PRINTED_WHOLE,
        over: false,
    };
    let _ = write(&mut whole);
    if !whole.over {
        let mut text = whole.text;
        if newline {
            text.push('\n');
        }
        return crate::builtins::prelude::print_to(vm, stream, text, call);
    }
    drop(whole);
    let failed = vm.runtime.io.print_streamed(stream, &|out| {
        write(out)?;
        match newline {
            true => out.write_str("\n"),
            false => Ok(()),
        }
    });
    match (failed, stream) {
        (Some(why), super::Stream::Stdout) => {
            Err(VmError::new(format!("cannot write to stdout: {why}")))
        }
        _ => Ok(Step::Done(Value::Unit)),
    }
}

/// Write the text of `value` to `out` if it is a String, an Int, a
/// Float or a Bool: values no written `Display` impl shows, and whose
/// text needs no walk. `false`, and nothing written, for any other.
///
/// The one place these four are written: [`Vm::display_value`] and
/// `Interpolate` (src/vm/run.rs), which writes them straight into the
/// string it builds, so that `DisplayValue` leaves them as they are.
pub(crate) fn write_plain(out: &mut String, value: &Value) -> bool {
    match value {
        Value::String(s, _) => out.push_str(s),
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Float(f) => {
            let _ = write!(out, "{f}");
        }
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        _ => return false,
    }
    true
}

/// The parts of a value that written `Display` impls show, each with
/// its type's `display` and the global slot of it, in the order the
/// formatter comes to them.
type Parts = Vec<(Value, u16, Value)>;

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
    /// What is done with the text.
    deliver: Option<Deliver>,
}

impl Native for Showing {
    fn name(&self) -> &str {
        "display"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if self.calling {
            match input {
                Value::String(text, _) => self.texts.push(text.to_string()),
                other => {
                    return Err(VmError::type_confusion(format!(
                        "display returned {}, not a String",
                        vm.user_facing_type_name(&other)
                    )));
                }
            }
        }
        if let Some((part, slot, display)) = self.parts.get(self.texts.len()) {
            self.calling = true;
            // (The VM's own copy of the impl's closure, as a call of
            // the impl's method would take.)
            let display = vm.own.value(*slot, display.clone());
            return Ok(vm.call(display, [part.clone()]));
        }
        let next = Cell::new(0);
        let texts = std::mem::take(&mut self.texts);
        let table = self.table.clone();
        let value = self.value.clone();
        let deliver = self.deliver.take().ok_or_else(|| {
            VmError::new("internal VM error: a shown value was finished twice".into())
        })?;
        // (Written again from the start each time: a print may write it
        // twice.)
        deliver.deliver(vm, &|out| {
            next.set(0);
            let shown = Shown(&value, &|part, f| {
                written_slot(&table, part)?;
                let text = texts.get(next.get())?;
                next.set(next.get() + 1);
                Some(f.write_str(text))
            });
            write!(out, "{shown}")
        })
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
    /// The `display` the program wrote for the type of `value`, and its
    /// global slot.
    ///
    /// An impl of `Display` is written in the module that declares its
    /// type, and a module's functions are set before any of its code
    /// runs, so a value of the type is never there before the impl is:
    /// an impl that is not set yet is a bug of silt, and said so, not
    /// taken for a type without one.
    fn written_display(
        &self,
        table: &Globals,
        value: &Value,
    ) -> Result<Option<(u16, Value)>, VmError> {
        let Some(slot) = written_slot(table, value) else {
            return Ok(None);
        };
        match self.global(slot).cloned() {
            Some(display) => Ok(Some((slot, display))),
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
        limit: Limit,
    ) -> Result<(String, Parts, bool), VmError> {
        let parts: RefCell<Parts> = RefCell::new(Vec::new());
        let unset: RefCell<Option<VmError>> = RefCell::new(None);
        let unwritten = Cell::new(false);
        let shown = Shown(value, &|part, _| {
            let (slot, display) = match self.written_display(table, part) {
                Ok(Some(written)) => written,
                Ok(None) => {
                    unwritten.set(true);
                    return None;
                }
                Err(error) => {
                    unset.borrow_mut().get_or_insert(error);
                    return None;
                }
            };
            parts.borrow_mut().push((part.clone(), slot, display));
            Some(Ok(()))
        });
        let text = written(limit, |out| write!(out, "{shown}"))?;
        match unset.into_inner() {
            Some(error) => Err(error),
            None => Ok((text, parts.into_inner(), unwritten.get())),
        }
    }

    /// A record or variant has no written impl (of `Display`, of
    /// `json.Encode`) in this VM's table: the table that may have one,
    /// if there is one and `any` says it has an impl of the trait. That
    /// is the program's newest, with what the REPL entry being run adds
    /// (see [`Vm::look_again`]): a later entry may have written the
    /// impl, for the VM of a task of an earlier one.
    ///
    /// A program that is no REPL session asks nothing: its VMs have
    /// all of it in their table (`table` is `entry`, one table), and
    /// nothing newer ever comes. So does the VM that runs a session's
    /// entry. Only the VM of a session's task goes on from here.
    pub(super) fn wider_table(&mut self, any: fn(&Globals) -> bool) -> Option<Arc<Globals>> {
        match Arc::ptr_eq(&self.view.table, &self.view.entry) {
            true => None,
            false => self.entry_table(any),
        }
    }

    /// [`Vm::wider_table`], for a VM that may not have all of the
    /// program in its table.
    #[cold]
    #[inline(never)]
    fn entry_table(&mut self, any: fn(&Globals) -> bool) -> Option<Arc<Globals>> {
        self.look_again();
        any(&self.view.entry).then(|| self.view.entry.clone())
    }

    /// Show `value` as a message (a panic's): cut at [`MESSAGE_MOST`]
    /// bytes, and go on with `then`, which gets its text.
    pub(crate) fn show(
        &mut self,
        value: &Value,
        then: impl FnOnce(&mut Vm, String) -> Result<Step, VmError> + Send + 'static,
    ) -> Result<Step, VmError> {
        self.show_to(
            value,
            Deliver::Text(Limit::Cut(MESSAGE_MOST), Box::new(then)),
        )
    }

    /// Print `value` to `stream` for the builtin `call` (`println`,
    /// `io.eprint`), with a line end when `newline`: its whole text,
    /// whatever its length.
    pub(crate) fn print_value(
        &mut self,
        value: &Value,
        stream: super::Stream,
        call: &'static str,
        newline: bool,
    ) -> Result<Step, VmError> {
        self.show_to(
            value,
            Deliver::Print {
                stream,
                call,
                newline,
            },
        )
    }

    /// [`Vm::show`], with a text no longer than a string may hold: an
    /// error of the builtin `limit` names (`string.from`, string
    /// interpolation) if it would be.
    pub(crate) fn show_within(
        &mut self,
        value: &Value,
        limit: &'static str,
        then: impl FnOnce(&mut Vm, String) -> Result<Step, VmError> + Send + 'static,
    ) -> Result<Step, VmError> {
        self.show_to(value, Deliver::Text(Limit::Refuse(limit), Box::new(then)))
    }

    /// Show `value` and deliver its text: at once when no written
    /// `Display` impl has a part in it, else as a frame that calls the
    /// impls first.
    fn show_to(&mut self, value: &Value, deliver: Deliver) -> Result<Step, VmError> {
        value.writable()?;
        let limit = deliver.limit();
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
            wider = holds_parts
                .then(|| self.wider_table(Globals::any_shown))
                .flatten();
            if wider.is_none() {
                return deliver.deliver(self, &|out| match value {
                    Value::String(s, _) => out.write_str(s),
                    Value::Int(n) => write!(out, "{n}"),
                    Value::Float(f) => write!(out, "{f}"),
                    Value::Bool(b) => out.write_str(if *b { "true" } else { "false" }),
                    _ => write!(out, "{value}"),
                });
            }
        }
        // What the formatter writes, with nothing for the parts a
        // written impl shows: complete if there is no such part.
        let table = wider.as_deref().unwrap_or(&self.view.table);
        let (mut text, mut parts, unwritten) = self.parts_shown(table, value, limit)?;
        if unwritten
            && wider.is_none()
            && let Some(table) = self.wider_table(Globals::any_shown)
        {
            (text, parts, _) = self.parts_shown(&table, value, limit)?;
            wider = Some(table);
        }
        if parts.is_empty() {
            return match deliver {
                Deliver::Text(_, then) => then(self, text),
                print => print.deliver(self, &|out| write!(out, "{value}")),
            };
        }
        Ok(Step::Run(Box::new(Showing {
            value: value.clone(),
            table: wider.unwrap_or_else(|| self.view.table.clone()),
            parts,
            texts: Vec::new(),
            calling: false,
            deliver: Some(deliver),
        })))
    }

    /// The text `value` is shown with, for a host that has a value in
    /// hand after the code that made it has returned (a REPL entry's
    /// value): as `println` would write it, written `Display` impls
    /// included. The impls run here, on this thread, as a call of the
    /// VM's own. If one of them fails, or cannot run to its end (it
    /// waits for something that never comes), the text is the value's
    /// plain structure.
    pub fn show_text(&mut self, value: &Value) -> String {
        self.show_or_fail(value, Limit::Whole)
            .unwrap_or_else(|_| value.to_string())
    }

    /// [`Vm::show_text`] for the `Err` that `main` or a test returned,
    /// but what ends the run in a `Display` impl (a `process.exit`, the
    /// step budget used up) is the result: that impl is code of the
    /// program. Any other failure of it shows the value's structure,
    /// as [`Vm::show_text`] does.
    pub(crate) fn show_text_of_err(&mut self, value: &Value) -> Result<String, VmError> {
        match self.show_or_fail(value, Limit::Cut(MESSAGE_MOST)) {
            Err(e) if e.exit.is_some() || e.exit_in_a_test || e.out_of_steps => Err(e),
            Err(_) => Ok(message_of(value)),
            Ok(text) => Ok(text),
        }
    }

    fn show_or_fail(&mut self, value: &Value, limit: Limit) -> Result<String, VmError> {
        let floor = self.frames.len();
        let stack_floor = self.stack.len();
        let shown = self.show_to(
            value,
            Deliver::Text(
                limit,
                Box::new(|_, text| Ok(Step::Done(Value::string(text)))),
            ),
        )?;
        match shown {
            Step::Done(Value::String(text, _)) => Ok(text.to_string()),
            Step::Run(showing) => {
                self.push_native_frame(showing);
                let run = self.run_thread(floor, |vm| vm.run_frames(floor, vm.own_slice()));
                match self.finish_run(run, floor, stack_floor)? {
                    Value::String(text, _) => Ok(text.to_string()),
                    _ => Ok(message_of(value)),
                }
            }
            _ => Ok(message_of(value)),
        }
    }

    /// [`Vm::shown`] for the builtin `name`, whose text is a string a
    /// program holds: no longer than a string may be.
    pub(crate) fn shown_within(
        &mut self,
        value: &Value,
        name: &'static str,
    ) -> Result<Step, VmError> {
        self.show_within(value, name, |_, text| Ok(Step::Done(Value::string(text))))
    }
}

/// The structure of `value` as a message: cut at [`MESSAGE_MOST`] bytes.
fn message_of(value: &Value) -> String {
    let mut text = Cut::new(MESSAGE_MOST);
    let _ = write!(text, "{value}");
    text.text()
}
