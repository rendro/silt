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

/// Single shared decision — operating on the already-rendered
/// (severity, message) pair of a `Type`-kind diagnostic — for whether a
/// user-module import diagnostic should be suppressed from CLI output.
///
/// Lives here (the library's `pub` filter module) rather than in the
/// binary-only `cli` module so the `silt run`/`silt check` path
/// (`reportable_type_errors`) and the `silt test` loop can BOTH route
/// through the identical logic without drifting, and so it is reachable
/// from integration tests. The CLI's `should_suppress_import_cascade`
/// is a thin `SourceError`-shaped adapter over this.
///
/// `is_warning` is the diagnostic's own severity; `has_user_import_warning`
/// is whether ANY diagnostic in the same compile produced the "unknown
/// module" warning (the caller computes this across the whole set).
///
/// Suppress when the diagnostic IS the unknown-module warning, OR — only
/// when that warning is present in the set — when it is one of the
/// follow-on undefined-name / trait-cascade errors the compiler resolves
/// at link time.
pub fn should_suppress_import_cascade_message(
    message: &str,
    is_warning: bool,
    has_user_import_warning: bool,
) -> bool {
    (is_warning && is_unknown_module_warning_message(message))
        || (has_user_import_warning
            && !is_warning
            && is_user_import_resolvable_error_message(message))
}
