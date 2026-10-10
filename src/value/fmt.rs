use std::fmt;

use super::Value;
use crate::typeinfo::ty;

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "\"{s}\""),
            Value::List(xs) => f.debug_list().entries(xs.iter()).finish(),
            Value::Range(lo, hi) => write!(f, "{lo}..{hi}"),
            Value::Map(m) => f.debug_map().entries(m.iter()).finish(),
            Value::Set(s) => {
                write!(f, "#[")?;
                for (i, v) in s.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v:?}")?;
                }
                write!(f, "]")
            }
            Value::Tuple(vs) => {
                let mut t = f.debug_tuple("");
                for v in vs {
                    t.field(v);
                }
                t.finish()
            }
            Value::Record(ty, fields) => {
                write!(f, "{} {{", ty.name)?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v:?}")?;
                }
                write!(f, "}}")
            }
            Value::Variant(name, fields) => {
                if fields.is_empty() {
                    write!(f, "{name}")
                } else {
                    write!(f, "{name}(")?;
                    for (i, v) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{v:?}")?;
                    }
                    write!(f, ")")
                }
            }
            Value::VmClosure(c) => write!(f, "<fn:{}>", c.function.name()),
            Value::BuiltinFn(id) => write!(f, "<builtin:{id}>"),
            Value::HostFn(h) => write!(f, "<host:{}>", h.name),
            Value::VariantConstructor(tag) => write!(f, "<constructor:{tag}>"),
            Value::TypeDescriptor(ty) => write!(f, "<type:{}>", ty.name),
            Value::PrimitiveDescriptor(name) => write!(f, "<type:{name}>"),
            Value::Channel(ch) => write!(f, "<channel:{}>", ch.id()),
            Value::Handle(h) => write!(f, "<handle:{}>", h.id),
            Value::Bytes(b) => write!(f, "{}", format_bytes_preview(b)),
            Value::TcpListener(t) => write!(f, "<tcp-listener:{}>", t.id),
            Value::TcpStream(t) => write!(f, "<tcp-stream:{}>", t.id),
            Value::Unit => write!(f, "()"),
        }
    }
}

impl Value {
    /// Format a value in silt syntax, suitable for `io.inspect`.
    ///
    /// Unlike `Display` (which prints bare strings for user output) or `Debug`
    /// (which leaks Rust internals), this produces the silt-source representation:
    /// strings are quoted, collections use silt syntax, etc.
    pub fn format_silt(&self) -> String {
        match self {
            Value::Int(n) => format!("{n}"),
            Value::Float(n) => format!("{n}"),
            Value::Bool(b) => format!("{b}"),
            Value::String(s) => format!("\"{s}\""),
            Value::List(xs) => {
                let items: Vec<String> = xs.iter().map(|v| v.format_silt()).collect();
                format!("[{}]", items.join(", "))
            }
            Value::Range(lo, hi) => format!("{lo}..{hi}"),
            Value::Map(m) => {
                let items: Vec<String> = m
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.format_silt(), v.format_silt()))
                    .collect();
                format!("#{{{}}}", items.join(", "))
            }
            Value::Set(s) => {
                let items: Vec<String> = s.iter().map(|v| v.format_silt()).collect();
                format!("#[{}]", items.join(", "))
            }
            Value::Tuple(vs) => {
                let items: Vec<String> = vs.iter().map(|v| v.format_silt()).collect();
                format!("({})", items.join(", "))
            }
            Value::Record(ty, fields) => {
                let items: Vec<String> = record_fields(ty, fields)
                    .map(|(k, v)| format!("{k}: {}", v.format_silt()))
                    .collect();
                // (An anonymous record is written without a name.)
                match ty.is_anon() {
                    true => format!("{{{}}}", items.join(", ")),
                    false => format!("{} {{{}}}", ty.name, items.join(", ")),
                }
            }
            Value::Variant(name, fields) => {
                if fields.is_empty() {
                    name.name().to_string()
                } else {
                    let items: Vec<String> = fields.iter().map(|v| v.format_silt()).collect();
                    format!("{name}({})", items.join(", "))
                }
            }
            Value::VmClosure(_) => "<fn>".to_string(),
            Value::BuiltinFn(_) | Value::HostFn(_) => "<fn>".to_string(),
            Value::VariantConstructor(tag) => format!("<constructor:{tag}>"),
            Value::TypeDescriptor(ty) => format!("<type:{}>", ty.name),
            Value::PrimitiveDescriptor(name) => format!("<type:{name}>"),
            Value::Channel(ch) => format!("<channel:{}>", ch.id()),
            Value::Handle(h) => format!("<handle:{}>", h.id),
            Value::Bytes(b) => format_bytes_preview(b),
            Value::TcpListener(t) => format!("<tcp-listener:{}>", t.id),
            Value::TcpStream(t) => format!("<tcp-stream:{}>", t.id),
            Value::Unit => "()".to_string(),
        }
    }
}

/// The fields of a record in the order they are written in, in every
/// text of it: the order the type declares them in; an anonymous
/// record's, which has no declaration, in name order.
fn record_fields<'a>(
    ty: &'a crate::typeinfo::TypeInfo,
    fields: &'a std::collections::BTreeMap<String, Value>,
) -> RecordFields<'a> {
    match &ty.shape {
        crate::typeinfo::Shape::Record(declared) if !declared.is_empty() => {
            RecordFields::Declared {
                declared: declared.iter(),
                fields,
                in_step: Some(fields.iter()),
            }
        }
        _ => RecordFields::Named(fields.iter()),
    }
}

/// See [`record_fields`].
enum RecordFields<'a> {
    Declared {
        declared: std::slice::Iter<'a, (String, crate::typeinfo::FieldType)>,
        fields: &'a std::collections::BTreeMap<String, Value>,
        /// The fields in name order, for as long as the declaration
        /// has gone in that order too: the next declared field is then
        /// the next of these, and is not looked up.
        in_step: Option<std::collections::btree_map::Iter<'a, String, Value>>,
    },
    Named(std::collections::btree_map::Iter<'a, String, Value>),
}

impl<'a> Iterator for RecordFields<'a> {
    type Item = (&'a str, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            RecordFields::Declared {
                declared,
                fields,
                in_step,
            } => loop {
                let (name, _) = declared.next()?;
                if let Some(by_name) = in_step {
                    match by_name.next() {
                        Some((key, value)) if key == name => return Some((name.as_str(), value)),
                        _ => *in_step = None,
                    }
                }
                if let Some(value) = fields.get(name.as_str()) {
                    return Some((name.as_str(), value));
                }
            },
            RecordFields::Named(fields) => fields.next().map(|(name, v)| (name.as_str(), v)),
        }
    }
}

/// Shared rendering for `Bytes` values: short hex preview + length.
/// Truncates to the first 32 bytes with an ellipsis to keep output
/// readable for large buffers (e.g. tcp.read(conn, 4096)).
fn format_bytes_preview(b: &[u8]) -> String {
    const PREVIEW: usize = 32;
    if b.len() <= PREVIEW {
        let hex: Vec<String> = b.iter().map(|byte| format!("{byte:02x}")).collect();
        format!("bytes({}, length: {})", hex.join(" "), b.len())
    } else {
        let hex: Vec<String> = b[..PREVIEW]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("bytes({} …, length: {})", hex.join(" "), b.len())
    }
}

/// Extract an i64 from an optional Value reference.
fn val_i64(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Int(n)) => *n,
        _ => 0,
    }
}

/// Format a duration in nanoseconds as a human-readable string.
fn fmt_duration(f: &mut fmt::Formatter<'_>, total_ns: i64) -> fmt::Result {
    if total_ns < 0 {
        write!(f, "-")?;
    }
    let ns = total_ns.unsigned_abs();
    if ns == 0 {
        write!(f, "0s")
    } else if ns >= 3_600_000_000_000 {
        let h = ns / 3_600_000_000_000;
        let m = (ns % 3_600_000_000_000) / 60_000_000_000;
        let s = (ns % 60_000_000_000) / 1_000_000_000;
        if m > 0 && s > 0 {
            write!(f, "{h}h{m}m{s}s")
        } else if m > 0 {
            write!(f, "{h}h{m}m")
        } else {
            write!(f, "{h}h")
        }
    } else if ns >= 60_000_000_000 {
        let m = ns / 60_000_000_000;
        let s = (ns % 60_000_000_000) / 1_000_000_000;
        if s > 0 {
            write!(f, "{m}m{s}s")
        } else {
            write!(f, "{m}m")
        }
    } else if ns >= 1_000_000_000 {
        let s = ns / 1_000_000_000;
        let ms = (ns % 1_000_000_000) / 1_000_000;
        if ms > 0 {
            write!(f, "{s}.{ms:03}s")
        } else {
            write!(f, "{s}s")
        }
    } else if ns >= 1_000_000 {
        write!(f, "{}ms", ns / 1_000_000)
    } else if ns >= 1_000 {
        write!(f, "{}us", ns / 1_000)
    } else {
        write!(f, "{ns}ns")
    }
}

/// What writes a value in place of the formatter: for a value of a type
/// with a `Display` impl a program wrote, what the impl gave. `None`
/// for any other value, which the formatter writes itself.
pub type Written<'a> = &'a dyn Fn(&Value, &mut fmt::Formatter<'_>) -> Option<fmt::Result>;

/// A value as it is shown (`println`, interpolation), with `written`
/// asked first at each record and variant inside it.
pub struct Shown<'a>(pub &'a Value, pub Written<'a>);

impl fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.show(f, self.1)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.show(f, &|_, _| None)
    }
}

impl Value {
    fn show(&self, f: &mut fmt::Formatter<'_>, written: Written<'_>) -> fmt::Result {
        if matches!(self, Value::Record(..) | Value::Variant(..))
            && let Some(done) = written(self, f)
        {
            return done;
        }
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "{s}"),
            Value::List(xs) => {
                write!(f, "[")?;
                for (i, v) in xs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    v.show(f, written)?;
                }
                write!(f, "]")
            }
            Value::Range(lo, hi) => write!(f, "{lo}..{hi}"),
            Value::Map(m) => {
                write!(f, "#{{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if let Value::String(s) = k {
                        write!(f, "\"{s}\": ")?;
                    } else {
                        k.show(f, written)?;
                        write!(f, ": ")?;
                    }
                    v.show(f, written)?;
                }
                write!(f, "}}")
            }
            Value::Set(s) => {
                write!(f, "#[")?;
                for (i, v) in s.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    v.show(f, written)?;
                }
                write!(f, "]")
            }
            Value::Tuple(vs) => {
                write!(f, "(")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    v.show(f, written)?;
                }
                write!(f, ")")
            }
            Value::Record(ty, fields) => match ty.id {
                ty::DATE => {
                    let y = val_i64(fields.get("year"));
                    let m = val_i64(fields.get("month"));
                    let d = val_i64(fields.get("day"));
                    write!(f, "{y:04}-{m:02}-{d:02}")
                }
                ty::TIME => {
                    let h = val_i64(fields.get("hour"));
                    let m = val_i64(fields.get("minute"));
                    let s = val_i64(fields.get("second"));
                    let ns = val_i64(fields.get("ns"));
                    if ns > 0 {
                        write!(f, "{h:02}:{m:02}:{s:02}.{ns:09}")
                    } else {
                        write!(f, "{h:02}:{m:02}:{s:02}")
                    }
                }
                ty::DATE_TIME => {
                    if let (Some(date), Some(time)) = (fields.get("date"), fields.get("time")) {
                        write!(f, "{date}T{time}")
                    } else {
                        write!(f, "DateTime {{}}")
                    }
                }
                ty::DURATION => fmt_duration(f, val_i64(fields.get("ns"))),
                _ => {
                    // (An anonymous record is written without a name.)
                    if !ty.is_anon() {
                        f.write_str(&ty.name)?;
                        f.write_str(" ")?;
                    }
                    f.write_str("{")?;
                    for (i, (k, v)) in record_fields(ty, fields).enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        f.write_str(k)?;
                        f.write_str(": ")?;
                        v.show(f, written)?;
                    }
                    f.write_str("}")
                }
            },
            Value::Variant(name, fields) => {
                // Stdlib error variants render via
                // their `Error::message()` implementation so that
                // `format!("{e}")` and `e.message()` produce the same
                // text — the "one way" principle. User enums are
                // unaffected (the registry only covers stdlib errors).
                if let Some(msg) = crate::builtins::error_text(name, fields.as_slice()) {
                    return write!(f, "{msg}");
                }
                if fields.is_empty() {
                    write!(f, "{name}")
                } else {
                    write!(f, "{name}(")?;
                    for (i, v) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        v.show(f, written)?;
                    }
                    write!(f, ")")
                }
            }
            Value::VmClosure(c) => write!(f, "<fn:{}>", c.function.name()),
            Value::BuiltinFn(id) => write!(f, "<builtin:{id}>"),
            Value::HostFn(h) => write!(f, "<host:{}>", h.name),
            Value::VariantConstructor(tag) => write!(f, "<constructor:{tag}>"),
            Value::TypeDescriptor(ty) => write!(f, "<type:{}>", ty.name),
            Value::PrimitiveDescriptor(name) => write!(f, "<type:{name}>"),
            Value::Channel(ch) => write!(f, "<channel:{}>", ch.id()),
            Value::Handle(h) => write!(f, "<handle:{}>", h.id),
            Value::Bytes(b) => write!(f, "{}", format_bytes_preview(b)),
            Value::TcpListener(t) => write!(f, "<tcp-listener:{}>", t.id),
            Value::TcpStream(t) => write!(f, "<tcp-stream:{}>", t.id),
            Value::Unit => write!(f, "()"),
        }
    }
}
