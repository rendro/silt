//! Round 60 G8: every entry in `BUILTIN_MODULES` (the authoritative
//! list at `src/module.rs:4`) must have a corresponding match arm in
//! `Vm::dispatch_builtin` (`src/vm/dispatch.rs:515`). Without this
//! lock, adding a module to `BUILTIN_MODULES` without wiring it into
//! the dispatcher would silently produce
//!   `unknown module: <name>`
//! at runtime — even though the rest of the toolchain (typechecker,
//! manifest validator, REPL completion, editor grammars, compiler
//! prelude registration) treats the module as live.
//!
//! Behavioural-call form is unreachable from `tests/`: the
//! `dispatch_builtin` entry point is `pub(super)` (only callable from
//! within `src/vm/`), and the typechecker rejects user-written
//! `module.__unknown__()` calls before they can reach the dispatcher.
//! We therefore lock the parity at the source level: read
//! `src/vm/dispatch.rs` at compile time and assert each module name
//! appears as a `"<module>" =>` match arm.
//!
//! If this test fails after adding a new module to `BUILTIN_MODULES`,
//! add a corresponding arm in `src/vm/dispatch.rs` (within the
//! `dispatch_builtin` `match module { ... }` block) that routes to
//! the module's `call_*` function via `catch_builtin_panic`.

use silt::module::BUILTIN_MODULES;

/// Source of `src/vm/dispatch.rs` captured at compile time. Embedding
/// at compile time (rather than reading at runtime) makes the test
/// deterministic regardless of the working directory `cargo test` is
/// invoked from.
const DISPATCH_SRC: &str = include_str!("../src/vm/dispatch.rs");

/// Returns `true` iff `src` contains a match arm of the exact form
/// `"<module>" =>` — the canonical shape used by every arm in
/// `dispatch_builtin`'s `match module { ... }` block.
fn has_dispatch_arm(src: &str, module: &str) -> bool {
    let needle = format!("\"{module}\" =>");
    src.contains(&needle)
}

#[test]
fn every_builtin_module_has_a_dispatch_arm() {
    let mut missing: Vec<&str> = Vec::new();
    for &module in BUILTIN_MODULES {
        if !has_dispatch_arm(DISPATCH_SRC, module) {
            missing.push(module);
        }
    }
    assert!(
        missing.is_empty(),
        "BUILTIN_MODULES lists module(s) with no `\"<name>\" =>` arm in \
         src/vm/dispatch.rs::dispatch_builtin: {missing:?}.\n\
         Without the arm, calls into the module fall through to \
         `unknown module: <name>` at runtime even though the typechecker, \
         compiler prelude, REPL, and editor grammars treat the module as live.\n\
         Authoritative list: src/module.rs (BUILTIN_MODULES).\n\
         Dispatcher: src/vm/dispatch.rs (Vm::dispatch_builtin).",
    );
}

/// Belt-and-braces: also assert the *negative* — every `"<name>" =>`
/// arm in `dispatch_builtin` must correspond to either a
/// `BUILTIN_MODULES` entry, a builtin trait Error impl name (e.g.
/// `IoError`, `JsonError`), or the test-only `__test_panic_builtin`
/// arm. Catches the inverse drift: a stale arm left over for a module
/// that has since been removed from `BUILTIN_MODULES`.
#[test]
fn no_orphan_dispatch_arms() {
    // Names allowed beyond `BUILTIN_MODULES`: built-in trait Error
    // impls (Phase 1 of the stdlib error redesign — see comment at
    // `src/vm/dispatch.rs:617`) and the `#[cfg(test)]`-only synthetic
    // panic arm used by `test_builtin_panic_converted_to_vm_error`.
    let allow_extra = [
        "IoError",
        "JsonError",
        "TomlError",
        "ParseError",
        "HttpError",
        "RegexError",
        "PgError",
        "TcpError",
        "TimeError",
        "BytesError",
        "ChannelError",
        "__test_panic_builtin",
    ];

    // Collect every `"<ident>" =>` substring inside the `match
    // module { ... }` block of `dispatch_builtin`. Bare-name builtins
    // (`println`, `print`, `panic`) live in the sibling `match name`
    // arm and must NOT be scanned — they have no module prefix.
    let dispatch_start = DISPATCH_SRC
        .find("pub(super) fn dispatch_builtin(")
        .expect("dispatch_builtin signature missing");
    // The qualified-name dispatch sits inside `match module { ... }`.
    // Scan from the `match module {` token through the matching close
    // brace. The block contains nested braces (closures inside each
    // `catch_builtin_panic` call), so track depth.
    let after = &DISPATCH_SRC[dispatch_start..];
    let match_rel = after
        .find("match module {")
        .expect("`match module {` block missing from dispatch_builtin");
    let block_start = dispatch_start + match_rel + "match module {".len();
    let block_bytes = DISPATCH_SRC.as_bytes();
    let mut depth: i32 = 1;
    let mut j = block_start;
    while j < block_bytes.len() && depth > 0 {
        match block_bytes[j] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        j += 1;
    }
    assert!(depth == 0, "unbalanced braces in dispatch_builtin scan");
    // Exclude the trailing `}` itself.
    let body = &DISPATCH_SRC[block_start..j - 1];

    let mut found: Vec<String> = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // Find the matching close quote on the same line. The
            // module-name arms never contain backslashes or interior
            // quotes, so a naive scan suffices.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' && bytes[j] != b'\n' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                // Look for ` =>` immediately after the closing quote.
                let after = &body[j + 1..];
                if after.starts_with(" =>") {
                    let name = &body[i + 1..j];
                    // Only accept names that look like identifiers —
                    // skip stray match arms like `"a"` etc. that would
                    // never be a module.
                    if !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        found.push(name.to_string());
                    }
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }

    let mut orphans: Vec<String> = Vec::new();
    for name in &found {
        let in_modules = BUILTIN_MODULES.contains(&name.as_str());
        let in_extra = allow_extra.contains(&name.as_str());
        if !in_modules && !in_extra {
            orphans.push(name.clone());
        }
    }

    assert!(
        orphans.is_empty(),
        "src/vm/dispatch.rs contains `\"<name>\" =>` arms with no matching \
         BUILTIN_MODULES entry or allow-listed Error trait name: {orphans:?}.\n\
         Either add `<name>` to BUILTIN_MODULES (src/module.rs) or to the \
         `allow_extra` list in this test (if it is a builtin trait Error impl).",
    );
}
