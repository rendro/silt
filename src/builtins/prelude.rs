//! The functions of the prelude (`print`, `println`, `panic`).

use super::typed::builtins;
use crate::value::Value;
use crate::vm::{Step, Vm, VmError};

/// Write `text` to the host's stdout. A failure is a runtime error.
fn write_stdout(vm: &Vm, text: &str) -> Result<(), VmError> {
    vm.runtime.io.out(text).map_err(|e| {
        VmError::new(format!(
            "cannot write to stdout: {}",
            crate::diagnostic::io_error_text(&e)
        ))
    })
}

builtins! {
    fn print(vm, value: &Value) -> Result<Step, VmError> {
        vm.show(value, |vm, text| {
            write_stdout(vm, &text)?;
            Ok(Step::Done(Value::Unit))
        })
    }

    fn println(vm, value: &Value) -> Result<Step, VmError> {
        vm.show(value, |vm, mut text| {
            text.push('\n');
            write_stdout(vm, &text)?;
            Ok(Step::Done(Value::Unit))
        })
    }

    fn panic(vm, message: &Value) -> Result<Step, VmError> {
        vm.show(message, |_, text| Err(VmError::new(format!("panic: {text}"))))
    }
}
