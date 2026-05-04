//! Shared message-text predicates used to filter typechecker
//! diagnostics that the compiler will resolve later (or that another
//! pass will re-emit). Centralised here so the LSP's `TypeError`-shaped
//! callers and the CLI pipeline's `SourceError`-shaped callers stay in
//! lock-step instead of drifting through cut-and-paste maintenance.
//!
//! Both call sites already render their error to a string at some
//! point (see `TypeError::message` in `src/types/mod.rs:305` and
//! `SourceError::message` in `src/errors.rs:35`); these helpers take
//! that message slice and return whether it matches one of the two
//! filterable shapes. Severity / kind gating stays at each call site
//! because those types differ structurally.

/// Returns true iff `message` is the typechecker's "unknown module"
/// warning that the compiler will later resolve. Callers must also
/// check that the diagnostic is a *warning* — we only match on the
/// message body so a future real type error that happens to mention
/// those words isn't swallowed.
pub fn is_unknown_module_warning_message(message: &str) -> bool {
    message.contains("unknown module")
}

/// Returns true iff `message` is one of the typechecker error shapes
/// that the compiler is likely to resolve at link time when the name
/// comes from a user-module import the type checker can't see into.
///
/// Deliberately omits a `starts_with("type ")` clause: the CLI filter
/// dropped that pattern because it swallowed real type-mismatch errors
/// alongside user-module follow-ons (GAP #7). Trait-impl cascades flow
/// through the narrow `"does not implement"` substring instead. See
/// `src/cli/pipeline.rs::is_user_import_resolvable_error` for the long
/// rationale.
pub fn is_user_import_resolvable_error_message(message: &str) -> bool {
    message.starts_with("undefined variable")
        || message.starts_with("undefined constructor")
        || message.starts_with("undefined type")
        || message.starts_with("unknown field")
        || message.contains("does not implement")
}
