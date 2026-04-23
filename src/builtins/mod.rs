//! Builtin function modules for the Silt VM.
//!
//! Each submodule implements a family of builtin functions (e.g. `string.*`,
//! `list.*`) and exposes a single `call` entry point that the main VM dispatch
//! delegates to.

pub mod bytes;
pub mod collections;
pub mod concurrency;
pub mod core;
pub mod crypto;
pub mod data;
pub mod encoding;
pub mod io;
pub mod numeric;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod stream;
pub mod string;
#[cfg(feature = "tcp")]
pub mod tcp;
pub mod toml;
pub mod uuid;

use crate::value::Value;
use crate::vm::VmError;

/// Shared scaffolding for every `trait Error for <FooError>` builtin
/// dispatch helper (e.g. `call_io_error_trait`,
/// `call_json_error_trait`, …). Round-36 collapsed eleven copy-pasted
/// copies of this control flow — arity check → receiver-shape check →
/// render → `Value::String` — into a single helper. Each caller now
/// only supplies its enum name and a `render_message` closure that
/// pattern-matches the receiver variant and returns the user-facing
/// string.
///
/// ## Error-string contract (drift resolution)
///
/// Before the collapse, every call site hand-rolled the same three
/// error strings. We inspected all 11 sites — they were already in
/// lock-step — so the helper pins the canonical phrasings verbatim:
///
///   * wrong arity      → `"{enum}.message takes 1 argument (self), got {n}"`
///     where `n = args.len()` (the receiver counts as arg 0, so a
///     well-formed call has `args.len() == 1`). We report the full
///     `args.len()` — INCLUDING the receiver — so a user calling
///     `e.message(extra)` sees `got 2`, which matches how they wrote it.
///   * receiver-shape   → `"{enum}.message: expected {enum} variant, got {other}"`
///   * unknown method   → `"unknown {enum} trait method: {name}"`
///
/// If a future trait method with 2+ args is added (e.g. `.with_context(ctx)`),
/// extend this helper rather than re-forking 11 arms.
pub(crate) fn dispatch_error_trait<F>(
    enum_name: &str,
    method_name: &str,
    args: &[Value],
    render_message: F,
) -> Result<Value, VmError>
where
    F: FnOnce(&str, &[Value]) -> Option<String>,
{
    match method_name {
        "message" => {
            if args.len() != 1 {
                return Err(VmError::new(format!(
                    "{enum_name}.message takes 1 argument (self), got {}",
                    args.len()
                )));
            }
            let rendered = match &args[0] {
                Value::Variant(tag, fields) => render_message(tag.as_str(), fields.as_slice())
                    .unwrap_or_else(|| format!("{enum_name}: unrecognized variant shape `{tag}`")),
                other => {
                    return Err(VmError::new(format!(
                        "{enum_name}.message: expected {enum_name} variant, got {other}"
                    )));
                }
            };
            Ok(Value::String(rendered))
        }
        _ => Err(VmError::new(format!(
            "unknown {enum_name} trait method: {method_name}"
        ))),
    }
}
