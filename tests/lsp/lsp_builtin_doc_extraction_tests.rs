//! Integration tests for round 62 phase-2 LSP doc extraction:
//! every `docs/stdlib/*.md` file's prose has been inlined into
//! `src/typechecker/builtins/docs.rs` as `*_MD` raw-string constants
//! and attached to the corresponding `env.bindings` entries via
//! `attach_module_docs` / `attach_module_overview` /
//! `attach_module_docs_filtered`. The LSP `Server` ingests these
//! through `typechecker::builtin_docs()` and surfaces them via
//! hover, completion, and signature-help.
//!
//! These tests spawn the compiled `silt lsp` binary as a subprocess
//! and exercise the LSP request handlers end-to-end. Helpers are
//! local to this file (kept identical to `tests/lsp/lsp.rs` so this
//! file is independently buildable).

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::support::LspClient;

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_builtin_doc_test_{n}.silt")
}

/// Extract the Markdown hover content text from a hover response.
/// Returns `None` if the response had no hover (server returned
/// `null`) or the contents shape is unexpected.
fn hover_markdown(resp: &Value) -> Option<String> {
    let contents = resp.pointer("/result/contents")?;
    // MarkupContent { kind: "markdown", value: "..." } shape.
    let value = contents.get("value")?.as_str()?;
    Some(value.to_string())
}

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn hover_on_list_map_returns_markdown_doc() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let source =
        "import list\n\nfn main() {\n    let xs = list.map([1, 2, 3], { x -> x + 1 })\n    xs\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor on `map` in `list.map`. The line is
    // `    let xs = list.map([1, 2, 3], { x -> x + 1 })`
    // and `map` spans columns 18..21 (0-indexed). Use 19 to be
    // squarely inside.
    let resp = client.hover(&uri, 3, 19);
    let md = hover_markdown(&resp).expect("expected hover markdown for list.map");
    // The list.map section in list.md begins with the signature
    // line — accept any of the per-name body markers.
    assert!(
        md.contains("list.map") || md.contains("map") || md.contains("List"),
        "hover on list.map should surface its inlined doc; got:\n{md}"
    );
    // Look for prose unique to the list.map section.
    assert!(
        md.contains("Apply") || md.contains("transform") || md.contains("each"),
        "hover on list.map should mention the function's purpose; got:\n{md}"
    );
    client.shutdown();
}

#[test]
fn completion_for_list_module_includes_docs() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let source = "import list\n\nfn main() {\n    list.\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor immediately after `list.` on line 3 (0-indexed).
    let resp = client.completion(&uri, 3, 9);
    let items = resp
        .pointer("/result/items")
        .or_else(|| resp.pointer("/result"))
        .and_then(|v| v.as_array())
        .expect("expected completion items array");
    assert!(!items.is_empty(), "list.<dot> should produce completions");

    // At least one of the function items must carry `documentation`
    // populated from the inlined builtin docs.
    let mut with_doc = 0usize;
    for it in items {
        if it.get("documentation").is_some() {
            with_doc += 1;
        }
    }
    assert!(
        with_doc > 0,
        "expected at least one list.* completion to carry documentation \
         (round 62 phase-2 builtin doc inlining); items: {items:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_math_cos_includes_signature_and_doc() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let source = "import math\n\nfn main() {\n    math.cos(0.0)\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor on `cos` in `math.cos`, line 3 col 9-ish.
    let resp = client.hover(&uri, 3, 10);
    let md = hover_markdown(&resp).expect("expected hover markdown for math.cos");
    // The math.cos doc section's body starts with the signature
    // block and then the prose `Returns the cosine of \`x\``.
    assert!(
        md.contains("cosine"),
        "hover on math.cos should include the prose 'cosine'; got:\n{md}"
    );
    client.shutdown();
}

#[test]
fn hover_on_println_returns_globals_doc() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let source = "fn main() {\n    println(\"hi\")\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor on `println` line 1 col 6.
    let resp = client.hover(&uri, 1, 6);
    let md = hover_markdown(&resp).expect("expected hover markdown for println");
    // The globals.md `## \`println\`` section talks about printing
    // a value followed by a newline.
    assert!(
        md.contains("newline") || md.contains("Display"),
        "hover on println should include the globals.md prose; got:\n{md}"
    );
    client.shutdown();
}

#[test]
fn hover_on_io_error_variant_returns_errors_doc() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let source = "import io\n\nfn main() {\n    let e = IoNotFound(\"x\")\n    e\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor on `IoNotFound` line 3 col 14.
    let resp = client.hover(&uri, 3, 14);
    let md = hover_markdown(&resp).expect("expected hover markdown for IoNotFound");
    // The IoError section in errors.md mentions the variant table.
    assert!(
        md.contains("IoNotFound") || md.contains("path") || md.contains("Variant"),
        "hover on IoNotFound should include the IoError section; got:\n{md}"
    );
    client.shutdown();
}

/// Coverage smoke test (round 62 phase-2 lock). Every authoritative
/// qualified builtin name must have a non-empty doc string. Adding
/// a new builtin without a `## \`<name>\`` section in the matching
/// `*_MD` blob fails this test.
#[test]
fn every_authoritative_builtin_has_a_non_empty_doc_via_lsp_pipeline() {
    let docs = silt::typechecker::builtin_docs();
    let sigs = silt::typechecker::builtin_type_signatures();

    let mut missing: Vec<String> = Vec::new();
    for name in sigs.keys() {
        match docs.get(name) {
            None => missing.push(name.clone()),
            Some(d) if d.trim().is_empty() => missing.push(name.clone()),
            _ => {}
        }
    }
    if !missing.is_empty() {
        missing.sort();
        panic!(
            "{} authoritative builtin name(s) lack a non-empty doc \
             string: {:?}\n\nThe LSP `Server` populates its \
             `builtin_docs` cache from `typechecker::builtin_docs()` \
             at startup. Each entry feeds hover / completion / \
             signature-help. Adding a new builtin without inlining \
             its prose into the corresponding `super::docs::*_MD` \
             blob in `src/typechecker/builtins/docs.rs` (per round \
             62 phase-2) fails this test.",
            missing.len(),
            missing,
        );
    }
}
