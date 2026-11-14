//! The functions of the prelude (`print`, `println`, `panic`).

use super::typed::builtins;
use crate::runtime::sync::{Arm, Wait};
use crate::value::Value;
use crate::vm::{Printed, Step, Stream, Vm, VmError};

/// Print `text` to `stream` of the host: `call` is the builtin that
/// prints. The text goes to the output's writer; where the writer holds
/// as much as it may, the print waits for room, as a wait of a task
/// does (a `task.deadline` around it can end it, and the text is not
/// written then). A failure of an earlier write to stdout is a runtime
/// error; one to stderr is ignored, as there is nowhere to report it.
pub(crate) fn print_to(
    vm: &mut Vm,
    stream: Stream,
    text: String,
    call: &'static str,
) -> Result<Step, VmError> {
    let scheduler = vm.scheduler().clone();
    match vm.runtime.io.print(stream, text, &scheduler) {
        Printed::Queued => Ok(Step::Done(Value::Unit)),
        Printed::Waits(room) => {
            let wait = Wait::new(vec![Arm::Cell(room)]);
            Ok(vm.park(call, wait, |_, _| Ok(Step::Done(Value::Unit))))
        }
        Printed::Failed(why) => match stream {
            Stream::Stdout => Err(VmError::new(format!("cannot write to stdout: {why}"))),
            Stream::Stderr => Ok(Step::Done(Value::Unit)),
        },
    }
}

/// Print the text `write` writes to `stream` in pieces, for `call`: a
/// thread of its own makes the text, a piece at a time, and the print
/// gives each piece to the output's writer as a print gives its text,
/// parked while the writer holds as much as it may. The pieces go out
/// whole and in order, and no other print's text comes between them
/// (those wait behind the print). The print is a wait of its task as
/// any print is: a `task.deadline` around it, or a cancel of its task,
/// ends it between two pieces; what is written then stays written (a
/// part of the text), and the prints behind it go on.
pub(crate) fn print_in_pieces(
    vm: &mut Vm,
    stream: Stream,
    call: &'static str,
    newline: bool,
    write: crate::vm::show::Writer,
) -> Result<Step, VmError> {
    /// The pieces of a text, each sent when it is full.
    struct Pieces {
        piece: String,
        send: std::sync::mpsc::SyncSender<String>,
    }
    impl Pieces {
        fn flush(&mut self) -> std::fmt::Result {
            if self.piece.is_empty() {
                return Ok(());
            }
            let piece = std::mem::take(&mut self.piece);
            self.send.send(piece).map_err(|_| std::fmt::Error)
        }
    }
    impl std::fmt::Write for Pieces {
        fn write_str(&mut self, mut part: &str) -> std::fmt::Result {
            while !part.is_empty() {
                let room = crate::vm::PIECE.saturating_sub(self.piece.len()).max(1);
                let mut cut = part.len().min(room);
                while !part.is_char_boundary(cut) {
                    cut += 1;
                }
                self.piece.push_str(&part[..cut]);
                part = &part[cut..];
                if self.piece.len() >= crate::vm::PIECE {
                    self.flush()?;
                }
            }
            Ok(())
        }
    }
    let (send, receive) = std::sync::mpsc::sync_channel::<String>(2);
    let made = std::thread::Builder::new()
        .name("silt-print".into())
        .spawn(move || {
            let mut pieces = Pieces {
                piece: String::new(),
                send,
            };
            let _ = write(&mut pieces)
                .and_then(|()| match newline {
                    true => std::fmt::Write::write_str(&mut pieces, "\n"),
                    false => Ok(()),
                })
                .and_then(|()| pieces.flush());
        });
    if made.is_err() {
        return Err(VmError::new(format!(
            "{call}: cannot start the thread that writes a long text"
        )));
    }
    let printing = Printing {
        pieces: receive,
        end: vm.runtime.io.begin_stream(),
        stream,
        call,
    };
    next_piece(vm, printing)
}

/// A print in pieces under way ([`print_in_pieces`]).
struct Printing {
    pieces: std::sync::mpsc::Receiver<String>,
    /// Lets the prints behind it go on when it is dropped.
    end: crate::vm::StreamEnd,
    stream: Stream,
    call: &'static str,
}

/// Give the next pieces of `printing` to the writer, parking when it
/// holds as much as it may.
fn next_piece(vm: &mut Vm, printing: Printing) -> Result<Step, VmError> {
    let scheduler = vm.scheduler().clone();
    loop {
        let Ok(piece) = printing.pieces.recv() else {
            return Ok(Step::Done(Value::Unit));
        };
        match (vm.runtime.io).print_piece(&printing.end, printing.stream, piece, &scheduler) {
            Printed::Queued => {}
            Printed::Waits(room) => {
                let wait = Wait::new(vec![Arm::Cell(room)]);
                let call = printing.call;
                let mut printing = Some(printing);
                return Ok(vm.park(call, wait, move |vm, _| match printing.take() {
                    Some(printing) => next_piece(vm, printing),
                    None => Ok(Step::Done(Value::Unit)),
                }));
            }
            Printed::Failed(why) => {
                return match printing.stream {
                    Stream::Stdout => Err(VmError::new(format!("cannot write to stdout: {why}"))),
                    Stream::Stderr => Ok(Step::Done(Value::Unit)),
                };
            }
        }
    }
}

builtins! {
    fn print(vm, value: &Value) -> Result<Step, VmError> {
        vm.print_value(value, Stream::Stdout, "print", false)
    }

    fn println(vm, value: &Value) -> Result<Step, VmError> {
        vm.print_value(value, Stream::Stdout, "println", true)
    }

    fn panic(vm, message: &Value) -> Result<Step, VmError> {
        vm.show(message, |_, text| Err(VmError::new(format!("panic: {text}"))))
    }
}
