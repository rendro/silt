//! Phase B locks for the LSP hover renderer's effect-set output.
//!
//! See `docs/proposals/effect-rows.md` (Part 7). On hover over a fn
//! decl, the LSP must emit the effect annotation between the type
//! signature and the doc-comment separator. Three render variants:
//!   - declared `!{io, fs}` → `effects: !{io, fs}`
//!   - no annotation → `effects: !*` (loud, signals gradual rollout)
//!   - declared narrower than body, or body narrower than declared →
//!     two lines (`effects: ... (declared)` / `inferred: ... (body)`).
//!
//! Uses the shared LSP client in `support.rs`, which
//! drives the real silt-lsp binary over stdio so the rendering is
//! locked end-to-end.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use crate::support::LspClient;

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_effect_hover_{n}.silt")
}

fn hover_value(client: &mut LspClient, uri: &str, line: u32, character: u32) -> String {
    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    let result = resp.get("result").expect("hover has result");
    assert!(!result.is_null(), "hover must not be null; got {resp}");
    result
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .expect("hover.contents.value is a string")
}

// ── 1. Hover renders declared effects ──────────────────────────────

#[test]
fn hover_renders_declared_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    // Pure body so inferred == EMPTY; declared == {io, fs}; the two
    // differ, so this ALSO exercises the dual-render path. Test 3
    // locks the dual-render content; here we just need the declared
    // line to be present.
    client.did_open_and_wait(&uri, "fn read() -> Int !{io, fs} = 0\n");
    let value = hover_value(&mut client, &uri, 0, 3); // cursor on `read`
    assert!(
        value.contains("effects:"),
        "hover must include an effects: line; got {value:?}"
    );
    assert!(
        value.contains("!{fs, io}"),
        "hover must render declared effects in alphabetic order; got {value:?}"
    );
    client.shutdown();
}

// ── 2. Hover renders TOP for unannotated fn ────────────────────────

#[test]
fn hover_renders_top_for_unannotated() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    // No annotation → declared defaults to TOP. Hover should loudly
    // surface `!*` so users see the gradual-rollout state.
    client.did_open_and_wait(&uri, "fn legacy() -> Int = 0\n");
    let value = hover_value(&mut client, &uri, 0, 3); // cursor on `legacy`
    assert!(
        value.contains("effects: !*"),
        "unannotated fn must render `effects: !*`; got {value:?}"
    );
    client.shutdown();
}

// ── 3. Dual-render when declared and inferred differ ───────────────

#[test]
fn hover_renders_inferred_when_narrower_than_declared() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    // Declared `!{io}` but body is fully pure (inferred EMPTY).
    // The two differ → both lines appear in the hover render.
    client.did_open_and_wait(&uri, "fn pretend() -> Int !{io} = 0\n");
    let value = hover_value(&mut client, &uri, 0, 3); // cursor on `pretend`
    assert!(
        value.contains("effects: !{io}"),
        "hover must show declared `!{{io}}`; got {value:?}"
    );
    assert!(
        value.contains("(declared)"),
        "hover must label the declared line; got {value:?}"
    );
    assert!(
        value.contains("inferred: !{}"),
        "hover must show inferred `!{{}}` when body is pure; got {value:?}"
    );
    assert!(
        value.contains("(body)"),
        "hover must label the inferred line; got {value:?}"
    );
    client.shutdown();
}
