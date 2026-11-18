//! Builtin function modules for the Silt VM.
//!
//! Each submodule implements a family of builtin functions (e.g. `string.*`,
//! `list.*`) and exposes a single `call` entry point that the main VM dispatch
//! delegates to.

pub(crate) mod applicative;
pub(crate) mod bytes;
pub(crate) mod collections;
pub(crate) mod common;
pub(crate) mod concurrency;
pub(crate) mod core;
pub(crate) mod crypto;
pub(crate) mod csv;
pub(crate) mod data;
pub(crate) mod decode;
pub(crate) mod encoding;
pub mod http;
pub(crate) mod io;
pub(crate) mod json;
pub(crate) mod numeric;
#[cfg(feature = "postgres")]
pub mod postgres;
pub(crate) mod prelude;
pub(crate) mod process;
pub(crate) mod regex;
pub mod registry;
pub(crate) mod stream;
pub(crate) mod string;
#[cfg(feature = "tcp")]
pub(crate) mod tcp;
pub(crate) mod time;
pub(crate) mod toml;
pub(crate) mod typed;
pub(crate) mod url;
pub(crate) mod uuid;

/// The text of a variant of a builtin error enum, as it is shown
/// (`"{e}"`, the `error:` line of a `main` that returns it). `None` if
/// the variant is of no builtin error enum that is built (a program's
/// own enum is shown as any variant is).
///
/// The enum is found by its type's id (`Registry::error_module`), and
/// its module says what each variant reads as (`error:` of `module!`).
pub(crate) fn error_text(error: &crate::value::Variant) -> Option<String> {
    let text = registry::registry().error_module(error.type_id())?.text?;
    text(error.name(), error.fields())
}
