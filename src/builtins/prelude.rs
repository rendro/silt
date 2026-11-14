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

builtins! {
    fn print(vm, value: &Value) -> Result<Step, VmError> {
        vm.show(value, "print", |vm, text| print_to(vm, Stream::Stdout, text, "print"))
    }

    fn println(vm, value: &Value) -> Result<Step, VmError> {
        vm.show(value, "println", |vm, mut text| {
            text.push('\n');
            print_to(vm, Stream::Stdout, text, "println")
        })
    }

    fn panic(vm, message: &Value) -> Result<Step, VmError> {
        vm.show(message, "panic", |_, text| Err(VmError::new(format!("panic: {text}"))))
    }
}
