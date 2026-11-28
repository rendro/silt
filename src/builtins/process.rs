//! The running process (`process.*`): its exit, and the signals that
//! ask it to stop.

use super::typed::builtins;
use crate::runtime::sync::Channel;
use crate::value::Value;
use crate::vm::{Exit, VmError};

mod stop;

pub(crate) use stop::when_untaken;

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
            Exit::FailsTest => Err(VmError::exit_in_a_test(code)),
            Exit::FailsTestFile => Err(VmError::exit_in_a_test_file(code)),
            Exit::EndsRun => {
                vm.scheduler().exit(code);
                Err(VmError::exited(code))
            }
        }
    }

    // The first stop signal from now on is a message on the channel,
    // which is then closed, instead of ending the process; the one after
    // it ends the process as it would have (`stop`).
    fn stop_signals(vm) -> Result<Value, VmError> {
        let ch = Channel::new(vm.next_channel_id(), 1);
        stop::send_the_next_into(&ch, vm.scheduler())
            .map_err(|why| VmError::new(format!("process.stop_signals: {why}")))?;
        Ok(Value::Channel(ch))
    }
}
