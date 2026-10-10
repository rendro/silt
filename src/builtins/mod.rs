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

/// Re-export the canonical `Ok(v)` variant builder so an integration
/// test can lock the round-83 dedup: three sibling builtin modules
/// (`tcp`, `stream`, `postgres`) had byte-identical local `fn ok`
/// clones that were collapsed to call `common::ok` instead. The lock
/// in `tests/meta/round83_dead_code_dedup_lock_tests.rs` proves the deletion
/// was a semantic no-op by comparing this builder's output against a
/// hand-rolled `Value::variant(bv::OK, vec![v])`. A thin wrapper: only
/// this one helper is widened, not the rest of `common.rs`.
pub fn ok(v: Value) -> Value {
    common::ok(v)
}

/// The text of the variant `tag` with `fields` of a builtin error enum,
/// as its `message` gives it and as it is shown (`"{e}"`): one text for
/// both. `None` if the variant is of no builtin error enum that is
/// built (a program's own enum is shown as any variant is).
///
/// The enum is found by its type's id (`Registry::error_module`), and
/// its module says what each variant reads as (`error:` of `module!`).
pub fn error_text(tag: &crate::typeinfo::Tag, fields: &[Value]) -> Option<String> {
    let text = registry::registry().error_module(tag.ty().id)?.text?;
    text(tag.name(), fields)
}

typed::builtins! {
    /// The body of every error enum's `message` row.
    fn error_message(error: &Value) -> Result<String, VmError> {
        let Value::Variant(tag, fields) = error else {
            return Err(typed::unsound("message", "error"));
        };
        error_text(tag, fields)
            .ok_or_else(|| typed::unsound(&format!("{}.message", tag.ty().name), "error"))
    }
}
