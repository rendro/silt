//! IO and filesystem builtin functions (`io.*`, `fs.*`).

use std::sync::Arc;

use crate::value::Value;
use crate::vm::{BlockReason, Vm, VmError};

/// Dispatch `io.<name>(args)`.
pub fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "inspect" => {
            if args.len() != 1 {
                return Err(VmError::new("io.inspect takes 1 argument".into()));
            }
            Ok(Value::String(args[0].format_silt()))
        }
        "read_file" => {
            if args.len() != 1 {
                return Err(VmError::new("io.read_file takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new("io.read_file requires a string path".into()));
            };

            if vm.is_scheduled_task {
                // Check for pending completion from a previous yield
                if let Some(completion) = vm.pending_io.take() {
                    if let Some(result) = completion.try_get() {
                        return Ok(result);
                    }
                    // Not ready yet — re-park
                    vm.pending_io = Some(completion.clone());
                    vm.block_reason = Some(BlockReason::Io(completion));
                    for arg in args {
                        vm.push(arg.clone());
                    }
                    return Err(VmError::yield_signal());
                }
                // First call — submit to I/O pool
                let path = path.clone();
                let completion =
                    vm.runtime
                        .io_pool
                        .submit(move || match std::fs::read_to_string(&path) {
                            Ok(content) => {
                                Value::Variant("Ok".into(), vec![Value::String(content)])
                            }
                            Err(e) => {
                                Value::Variant("Err".into(), vec![Value::String(e.to_string())])
                            }
                        });
                vm.pending_io = Some(completion.clone());
                vm.block_reason = Some(BlockReason::Io(completion));
                for arg in args {
                    vm.push(arg.clone());
                }
                return Err(VmError::yield_signal());
            }
            // Main thread: synchronous fallback
            match std::fs::read_to_string(path) {
                Ok(content) => Ok(Value::Variant("Ok".into(), vec![Value::String(content)])),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(e.to_string())],
                )),
            }
        }
        "write_file" => {
            if args.len() != 2 {
                return Err(VmError::new("io.write_file takes 2 arguments".into()));
            }
            let (Value::String(path), Value::String(content)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "io.write_file requires string arguments".into(),
                ));
            };

            if vm.is_scheduled_task {
                // Check for pending completion from a previous yield
                if let Some(completion) = vm.pending_io.take() {
                    if let Some(result) = completion.try_get() {
                        return Ok(result);
                    }
                    // Not ready yet — re-park
                    vm.pending_io = Some(completion.clone());
                    vm.block_reason = Some(BlockReason::Io(completion));
                    for arg in args {
                        vm.push(arg.clone());
                    }
                    return Err(VmError::yield_signal());
                }
                // First call — submit to I/O pool
                let path = path.clone();
                let content = content.clone();
                let completion =
                    vm.runtime
                        .io_pool
                        .submit(move || match std::fs::write(&path, &content) {
                            Ok(()) => Value::Variant("Ok".into(), vec![Value::Unit]),
                            Err(e) => {
                                Value::Variant("Err".into(), vec![Value::String(e.to_string())])
                            }
                        });
                vm.pending_io = Some(completion.clone());
                vm.block_reason = Some(BlockReason::Io(completion));
                for arg in args {
                    vm.push(arg.clone());
                }
                return Err(VmError::yield_signal());
            }
            // Main thread: synchronous fallback
            match std::fs::write(path, content) {
                Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(e.to_string())],
                )),
            }
        }
        "read_line" => {
            if vm.is_scheduled_task {
                // Check for pending completion from a previous yield
                if let Some(completion) = vm.pending_io.take() {
                    if let Some(result) = completion.try_get() {
                        return Ok(result);
                    }
                    // Not ready yet — re-park
                    vm.pending_io = Some(completion.clone());
                    vm.block_reason = Some(BlockReason::Io(completion));
                    for arg in args {
                        vm.push(arg.clone());
                    }
                    return Err(VmError::yield_signal());
                }
                // First call — submit to I/O pool
                let completion = vm.runtime.io_pool.submit(move || {
                    let mut line = String::new();
                    match std::io::stdin().read_line(&mut line) {
                        Ok(_) => Value::Variant(
                            "Ok".into(),
                            vec![Value::String(line.trim_end().to_string())],
                        ),
                        Err(e) => Value::Variant("Err".into(), vec![Value::String(e.to_string())]),
                    }
                });
                vm.pending_io = Some(completion.clone());
                vm.block_reason = Some(BlockReason::Io(completion));
                for arg in args {
                    vm.push(arg.clone());
                }
                return Err(VmError::yield_signal());
            }
            // Main thread: synchronous fallback
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(_) => Ok(Value::Variant(
                    "Ok".into(),
                    vec![Value::String(line.trim_end().to_string())],
                )),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(e.to_string())],
                )),
            }
        }
        "args" => {
            let args_list: Vec<Value> = std::env::args().map(Value::String).collect();
            Ok(Value::List(Arc::new(args_list)))
        }
        _ => Err(VmError::new(format!("unknown io function: {name}"))),
    }
}

/// Dispatch `fs.<name>(args)`.
pub fn call_fs(_vm: &Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "exists" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.exists takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new("fs.exists requires a string path".into()));
            };
            Ok(Value::Bool(std::path::Path::new(path).exists()))
        }
        "is_file" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.is_file takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new("fs.is_file requires a string path".into()));
            };
            Ok(Value::Bool(std::path::Path::new(path).is_file()))
        }
        "is_dir" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.is_dir takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new("fs.is_dir requires a string path".into()));
            };
            Ok(Value::Bool(std::path::Path::new(path).is_dir()))
        }
        "list_dir" => {
            if args.len() != 1 {
                return Err(VmError::new("fs.list_dir takes 1 argument".into()));
            }
            let Value::String(path) = &args[0] else {
                return Err(VmError::new("fs.list_dir requires a string path".into()));
            };
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut items = Vec::new();
                    for entry in entries {
                        match entry {
                            Ok(e) => {
                                items.push(Value::String(
                                    e.file_name().to_string_lossy().into_owned(),
                                ));
                            }
                            Err(e) => {
                                return Err(VmError::new(format!(
                                    "fs.list_dir: error reading entry: {e}"
                                )));
                            }
                        }
                    }
                    Ok(Value::List(Arc::new(items)))
                }
                Err(e) => Err(VmError::new(format!("fs.list_dir: {e}"))),
            }
        }
        _ => Err(VmError::new(format!("unknown fs function: {name}"))),
    }
}
