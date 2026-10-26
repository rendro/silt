//! The running process (`process.*`): its exit.

use super::typed::builtins;
use crate::value::Value;
use crate::vm::{Exit, VmError};

builtins! {
    // What the exit does is the host's to say (`Vm::set_exit`).
    fn exit(vm, code: i64) -> Result<Value, VmError> {
        let Ok(code) = u8::try_from(code) else {
            return Err(VmError::new(format!(
                "process.exit: exit code {code} is out of range 0 to 255"
            )));
        };
        match vm.exit() {
            Exit::EndsProcess => crate::scheduler::exit_from_the_program(code),
            Exit::FailsTest => Err(VmError::new(format!(
                "process.exit({code}) was called in a test: a test ends by returning, or by \
                 failing an assertion"
            ))),
            Exit::EndsRun => {
                vm.scheduler().exit(code);
                Err(VmError::exited(code))
            }
        }
    }
}
