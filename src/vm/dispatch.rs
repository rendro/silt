//! Builtin registration and dispatch.

use super::{Vm, VmError};
use crate::builtins;
use crate::value::Value;

impl Vm {
    /// Register all builtin functions and variant constructors in globals.
    pub(super) fn register_builtins(&mut self) {
        // Variant constructors
        self.globals
            .insert("Ok".into(), Value::VariantConstructor("Ok".into(), 1));
        self.globals
            .insert("Err".into(), Value::VariantConstructor("Err".into(), 1));
        self.globals
            .insert("Some".into(), Value::VariantConstructor("Some".into(), 1));
        self.globals
            .insert("None".into(), Value::Variant("None".into(), Vec::new()));
        self.globals
            .insert("Stop".into(), Value::VariantConstructor("Stop".into(), 1));
        self.globals.insert(
            "Continue".into(),
            Value::VariantConstructor("Continue".into(), 1),
        );
        self.globals.insert(
            "Message".into(),
            Value::VariantConstructor("Message".into(), 1),
        );
        self.globals
            .insert("Closed".into(), Value::Variant("Closed".into(), Vec::new()));
        self.globals
            .insert("Empty".into(), Value::Variant("Empty".into(), Vec::new()));
        for day in [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ] {
            self.globals
                .insert(day.into(), Value::Variant(day.into(), Vec::new()));
        }
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
            self.globals
                .insert(method.into(), Value::Variant(method.into(), Vec::new()));
        }

        // Primitive type descriptors
        self.globals
            .insert("Int".into(), Value::PrimitiveDescriptor("Int".into()));
        self.globals
            .insert("Float".into(), Value::PrimitiveDescriptor("Float".into()));
        self.globals
            .insert("String".into(), Value::PrimitiveDescriptor("String".into()));
        self.globals
            .insert("Bool".into(), Value::PrimitiveDescriptor("Bool".into()));

        // Math constants
        self.globals
            .insert("math.pi".into(), Value::Float(std::f64::consts::PI));
        self.globals
            .insert("math.e".into(), Value::Float(std::f64::consts::E));

        // All builtin function names
        let builtin_names = [
            "print",
            "println",
            "io.inspect",
            "panic",
            "list.map",
            "list.filter",
            "list.each",
            "list.fold",
            "list.find",
            "list.zip",
            "list.flatten",
            "list.sort_by",
            "list.flat_map",
            "list.filter_map",
            "list.any",
            "list.all",
            "list.fold_until",
            "list.unfold",
            "list.head",
            "list.tail",
            "list.last",
            "list.reverse",
            "list.sort",
            "list.unique",
            "list.contains",
            "list.length",
            "list.append",
            "list.prepend",
            "list.concat",
            "list.get",
            "list.set",
            "list.take",
            "list.drop",
            "list.enumerate",
            "list.group_by",
            "result.unwrap_or",
            "result.map_ok",
            "result.map_err",
            "result.flatten",
            "result.flat_map",
            "result.is_ok",
            "result.is_err",
            "option.map",
            "option.unwrap_or",
            "option.to_result",
            "option.is_some",
            "option.is_none",
            "option.flat_map",
            "string.from",
            "string.split",
            "string.trim",
            "string.trim_start",
            "string.trim_end",
            "string.char_code",
            "string.from_char_code",
            "string.contains",
            "string.replace",
            "string.join",
            "string.length",
            "string.byte_length",
            "string.to_upper",
            "string.to_lower",
            "string.starts_with",
            "string.ends_with",
            "string.chars",
            "string.repeat",
            "string.index_of",
            "string.slice",
            "string.pad_left",
            "string.pad_right",
            "string.is_empty",
            "string.is_alpha",
            "string.is_digit",
            "string.is_upper",
            "string.is_lower",
            "string.is_alnum",
            "string.is_whitespace",
            "int.parse",
            "int.abs",
            "int.min",
            "int.max",
            "int.to_float",
            "int.to_string",
            "float.parse",
            "float.round",
            "float.ceil",
            "float.floor",
            "float.abs",
            "float.to_string",
            "float.to_int",
            "float.min",
            "float.max",
            "map.get",
            "map.set",
            "map.delete",
            "map.contains",
            "map.keys",
            "map.values",
            "map.length",
            "map.merge",
            "map.filter",
            "map.map",
            "map.entries",
            "map.from_entries",
            "map.each",
            "map.update",
            "set.new",
            "set.from_list",
            "set.to_list",
            "set.contains",
            "set.insert",
            "set.remove",
            "set.length",
            "set.union",
            "set.intersection",
            "set.difference",
            "set.is_subset",
            "set.map",
            "set.filter",
            "set.each",
            "set.fold",
            "io.read_file",
            "io.write_file",
            "io.read_line",
            "io.args",
            "fs.exists",
            "test.assert",
            "test.assert_eq",
            "test.assert_ne",
            "math.sqrt",
            "math.pow",
            "math.log",
            "math.log10",
            "math.sin",
            "math.cos",
            "math.tan",
            "math.asin",
            "math.acos",
            "math.atan",
            "math.atan2",
            "math.exp",
            "regex.is_match",
            "regex.find",
            "regex.find_all",
            "regex.split",
            "regex.replace",
            "regex.replace_all",
            "regex.replace_all_with",
            "regex.captures",
            "regex.captures_all",
            "json.parse",
            "json.parse_list",
            "json.parse_map",
            "json.stringify",
            "json.pretty",
            "channel.new",
            "channel.send",
            "channel.receive",
            "channel.close",
            "channel.try_send",
            "channel.try_receive",
            "channel.select",
            "channel.each",
            "task.spawn",
            "task.join",
            "task.cancel",
            "time.now",
            "time.today",
            "time.date",
            "time.time",
            "time.datetime",
            "time.to_datetime",
            "time.to_instant",
            "time.to_utc",
            "time.from_utc",
            "time.format",
            "time.format_date",
            "time.parse",
            "time.parse_date",
            "time.add_days",
            "time.add_months",
            "time.add",
            "time.since",
            "time.hours",
            "time.minutes",
            "time.seconds",
            "time.ms",
            "time.weekday",
            "time.days_between",
            "time.days_in_month",
            "time.is_leap_year",
            "time.sleep",
            "http.get",
            "http.request",
            "http.serve",
            "http.segments",
        ];

        for name in builtin_names {
            self.globals
                .insert(name.into(), Value::BuiltinFn(name.into()));
        }

        // Float constants — registered after builtin function names so that
        // `float.max` and `float.min` resolve to the constant values (f64::MAX,
        // f64::MIN) when used as bare expressions, overriding the BuiltinFn
        // entries.  Function calls like `float.max(a, b)` still work because
        // they compile to Op::CallBuiltin, which bypasses the global lookup.
        self.globals
            .insert("float.max_value".into(), Value::Float(f64::MAX));
        self.globals
            .insert("float.min_value".into(), Value::Float(f64::MIN));
        self.globals
            .insert("float.epsilon".into(), Value::Float(f64::EPSILON));
        self.globals
            .insert("float.min_positive".into(), Value::Float(f64::MIN_POSITIVE));
        self.globals
            .insert("float.infinity".into(), Value::ExtFloat(f64::INFINITY));
        self.globals.insert(
            "float.neg_infinity".into(),
            Value::ExtFloat(f64::NEG_INFINITY),
        );
        self.globals
            .insert("float.nan".into(), Value::ExtFloat(f64::NAN));
    }

    // ── Built-in trait methods on primitive types ──────────────────

    /// Handle built-in trait methods like .display(), .equal(), .compare()
    /// on primitive types. Returns Some(result) if handled, None otherwise.
    pub(super) fn dispatch_trait_method(
        &self,
        receiver: &Value,
        method: &str,
        extra_args: &[Value],
    ) -> Option<Result<Value, VmError>> {
        match method {
            "display" => {
                if !extra_args.is_empty() {
                    return Some(Err(VmError::new("display() takes no arguments".into())));
                }
                Some(Ok(Value::String(self.display_value(receiver))))
            }
            "equal" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("equal() takes 1 argument".into())));
                }
                Some(Ok(Value::Bool(*receiver == extra_args[0])))
            }
            "compare" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("compare() takes 1 argument".into())));
                }
                let other = &extra_args[0];
                let ord = match (receiver, other) {
                    (Value::Int(a), Value::Int(b)) => a.cmp(b),
                    (Value::Float(a), Value::Float(b)) => {
                        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (Value::String(a), Value::String(b)) => a.cmp(b),
                    (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
                    _ => {
                        return Some(Err(VmError::new(format!(
                            "compare() not supported between {} and {}",
                            self.type_name(receiver),
                            self.type_name(other)
                        ))));
                    }
                };
                let result = match ord {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Some(Ok(Value::Int(result)))
            }
            _ => None,
        }
    }

    // ── Builtin dispatch ──────────────────────────────────────────

    pub(super) fn dispatch_builtin(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Result<Value, VmError> {
        // Foreign functions take priority -- lets embedders override builtins.
        if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
            return f(args);
        }
        if let Some((module, func)) = name.split_once('.') {
            match module {
                "list" => builtins::collections::call_list(self, func, args),
                "string" => builtins::string::call(self, func, args),
                "int" => builtins::numeric::call_int(func, args),
                "float" => builtins::numeric::call_float(func, args),
                "map" => builtins::collections::call_map(self, func, args),
                "set" => builtins::collections::call_set(self, func, args),
                "result" => builtins::core::call_result(self, func, args),
                "option" => builtins::core::call_option(self, func, args),
                "io" => builtins::io::call(self, func, args),
                "fs" => builtins::io::call_fs(self, func, args),
                "test" => builtins::core::call_test(self, func, args),
                "math" => builtins::numeric::call_math(func, args),
                "regex" => builtins::data::call_regex(self, func, args),
                "json" => builtins::data::call_json(self, func, args),
                "channel" => builtins::concurrency::call_channel(self, func, args),
                "task" => builtins::concurrency::call_task(self, func, args),
                "time" => builtins::data::call_time(self, func, args),
                "http" => builtins::data::call_http(self, func, args),
                _ => {
                    if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
                        f(args)
                    } else {
                        Err(VmError::new(format!("unknown module: {module}")))
                    }
                }
            }
        } else {
            match name {
                "println" => {
                    match args.len() {
                        0 => println!(),
                        1 => println!("{}", self.display_value(&args[0])),
                        _ => {
                            let parts: Vec<String> =
                                args.iter().map(|v| self.display_value(v)).collect();
                            println!("{}", parts.join(" "));
                        }
                    }
                    Ok(Value::Unit)
                }
                "print" => {
                    match args.len() {
                        0 => {}
                        1 => print!("{}", self.display_value(&args[0])),
                        _ => {
                            let parts: Vec<String> =
                                args.iter().map(|v| self.display_value(v)).collect();
                            print!("{}", parts.join(" "));
                        }
                    }
                    Ok(Value::Unit)
                }
                "panic" => {
                    let msg = args.first().map(|v| v.to_string()).unwrap_or_default();
                    Err(VmError::new(format!("panic: {msg}")))
                }
                _ => {
                    if let Some(f) = self.runtime.foreign_fns.get(name).cloned() {
                        f(args)
                    } else {
                        Err(VmError::new(format!("unknown builtin: {name}")))
                    }
                }
            }
        }
    }

    /// Get current epoch milliseconds. Uses `__wasm_epoch_ms` foreign function
    /// if registered (WASM), otherwise falls back to `SystemTime`.
    pub(crate) fn epoch_ms(&self) -> Result<i64, VmError> {
        if let Some(f) = self.runtime.foreign_fns.get("__wasm_epoch_ms") {
            match f(&[])? {
                Value::Int(ms) => Ok(ms),
                _ => Err(VmError::new("__wasm_epoch_ms returned non-Int".into())),
            }
        } else {
            use std::time::{SystemTime, UNIX_EPOCH};
            let dur = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| VmError::new(format!("clock failed: {e}")))?;
            Ok(dur.as_millis() as i64)
        }
    }
}
