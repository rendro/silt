//! Builtin function modules for the Silt VM.
//!
//! Each submodule implements a family of builtin functions (e.g. `string.*`,
//! `list.*`) and exposes a single `call` entry point that the main VM dispatch
//! delegates to.

pub mod bytes;
pub mod collections;
mod common;
pub mod concurrency;
pub mod core;
pub mod crypto;
pub(crate) mod data;
pub mod encoding;
pub mod http;
pub mod io;
pub mod json;
pub mod numeric;
#[cfg(feature = "postgres")]
pub mod postgres;
pub(crate) mod prelude;
pub mod regex;
pub mod registry;
pub mod stream;
pub mod string;
#[cfg(feature = "tcp")]
pub mod tcp;
pub mod time;
pub mod toml;
pub(crate) mod typed;
pub mod uuid;

use crate::value::Value;
use crate::vm::VmError;

/// The text of a variant of a builtin error enum, as its `message`
/// gives it and as it is shown (`"{e}"`): one text for both. `None` if
/// the variant is of no builtin error enum that is built (a program's
/// own enum is shown as any variant is).
///
/// The enum is found by its type's id (`Registry::error_module`), and
/// its module says what each variant reads as (`error:` of `module!`).
pub fn error_text(error: &crate::value::Variant) -> Option<String> {
    let text = registry::registry().error_module(error.type_id())?.text?;
    text(error.name(), error.fields())
}

typed::builtins! {
    /// The body of every error enum's `message` row.
    fn error_message(error: &Value) -> Result<String, VmError> {
        let Value::Variant(variant) = error else {
            return Err(typed::unsound("message", "error"));
        };
        error_text(variant)
            .ok_or_else(|| typed::unsound(&format!("{}.message", variant.ty().name), "error"))
    }
}
