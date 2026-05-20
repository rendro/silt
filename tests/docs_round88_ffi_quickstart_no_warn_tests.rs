//! Round 88 LATENT lock: the FFI Quick Start snippet in docs/ffi.md
//! must not declare imports it does not use.
//!
//! Pre-fix: the Quick Start `use silt::{Vm, Value, VmError};` line
//! imported `VmError` even though the snippet body never referenced
//! it. Any first-time embedder who pasted the snippet verbatim into
//! `main.rs` would see `unused_imports` warnings on a fresh build —
//! a noisy first impression for a "Quick Start" section.
//!
//! Post-fix: the Quick Start snippet imports only `Vm` and `Value`.
//! `VmError` still appears later in the file (the raw-registration
//! example and the error-handling section), but those snippets
//! import / use it locally.
//!
//! Lock shape: extract the first ```rust fenced block from
//! `docs/ffi.md` (which is the Quick Start) and assert that every
//! named import in its `use` lines is actually referenced in the
//! snippet body. Sourced via `include_str!` so the test runs
//! offline and stays cheap.

use std::collections::HashSet;

const FFI_DOC: &str = include_str!("../docs/ffi.md");

/// Extract the body of the first fenced ```rust block in `src`.
/// Returns `None` if no fenced rust block is found. Trims the trailing
/// newline of the closing fence.
fn first_rust_block(src: &str) -> Option<&str> {
    let open_tag = "```rust";
    let open = src.find(open_tag)?;
    let after_open = &src[open + open_tag.len()..];
    // Skip past the newline that follows the opening fence's language tag.
    let body_start = after_open.find('\n')? + 1;
    let body_and_rest = &after_open[body_start..];
    let close = body_and_rest.find("\n```")?;
    Some(&body_and_rest[..close])
}

/// Parse a `use silt::{A, B, C};` line into the set of imported names.
/// Returns `None` for any line that does not match a brace-grouped
/// `use` statement (i.e. single-name forms like `use silt::Vm;` or
/// pathy forms like `use silt::compiler::Compiler;` are skipped here —
/// they each import exactly one name, so we extract that single name
/// instead via `single_use_name`).
fn brace_group_names(line: &str) -> Option<HashSet<String>> {
    let line = line.trim();
    let open = line.find('{')?;
    let close = line.find('}')?;
    if close <= open {
        return None;
    }
    let inside = &line[open + 1..close];
    let names: HashSet<String> = inside
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some(names)
}

/// For a `use foo::bar::Baz;` (no braces), return `Baz` — the single
/// name brought into scope. Returns `None` for non-use lines or
/// glob imports.
fn single_use_name(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = line.strip_prefix("use ")?;
    let rest = rest.strip_suffix(';')?;
    if rest.contains('{') || rest.contains('*') {
        return None;
    }
    let last = rest.rsplit("::").next()?;
    // `as` rename: `Foo as Bar` → the binding is `Bar`. We iterate
    // forward and keep the last segment; `rsplit` on `" as "` is not
    // double-ended.
    let last = last.split(" as ").last()?.trim();
    if last.is_empty() {
        return None;
    }
    Some(last.to_string())
}

#[test]
fn ffi_quickstart_imports_are_all_referenced() {
    let body = first_rust_block(FFI_DOC).expect(
        "docs/ffi.md must contain a fenced ```rust block (the Quick Start). \
         If the doc layout changed, update this test.",
    );

    // Collect every name imported by `use ...` lines in the snippet.
    let mut imported: HashSet<String> = HashSet::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("use ") {
            continue;
        }
        if let Some(names) = brace_group_names(trimmed) {
            imported.extend(names);
        } else if let Some(name) = single_use_name(trimmed) {
            imported.insert(name);
        }
    }

    assert!(
        !imported.is_empty(),
        "expected the Quick Start snippet to declare at least one `use` import; \
         got none. The block-extractor may have grabbed the wrong fenced block."
    );

    // For each imported name, assert it appears somewhere in the
    // snippet body OUTSIDE the `use` lines themselves. We do the
    // exclusion by stripping `use` lines first.
    let body_minus_use_lines: String = body
        .lines()
        .filter(|l| !l.trim().starts_with("use "))
        .collect::<Vec<_>>()
        .join("\n");

    let mut unused: Vec<String> = Vec::new();
    for name in &imported {
        // Word-boundary-ish check: surround with non-identifier chars
        // OR start/end-of-string. Cheap implementation: check that the
        // name appears AND is followed by a non-identifier char (or
        // EOS) AND preceded by a non-identifier char (or BOS).
        if !contains_as_identifier(&body_minus_use_lines, name) {
            unused.push(name.clone());
        }
    }

    assert!(
        unused.is_empty(),
        "docs/ffi.md Quick Start snippet declares imports that are never \
         referenced in the snippet body: {unused:?}. \
         Pasting this snippet verbatim would produce an `unused_imports` \
         cargo warning. Remove the unused import(s) from the `use silt::{{...}}` \
         line so first-time embedders get a clean build."
    );
}

/// True if `needle` appears in `hay` as a standalone identifier
/// (preceded and followed by a non-identifier character or
/// string boundary).
fn contains_as_identifier(hay: &str, needle: &str) -> bool {
    let bytes = hay.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.is_empty() {
        return false;
    }
    let mut i = 0;
    while i + needle_bytes.len() <= bytes.len() {
        if &bytes[i..i + needle_bytes.len()] == needle_bytes {
            let before_ok = if i == 0 {
                true
            } else {
                !is_ident_byte(bytes[i - 1])
            };
            let after_idx = i + needle_bytes.len();
            let after_ok = if after_idx == bytes.len() {
                true
            } else {
                !is_ident_byte(bytes[after_idx])
            };
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
