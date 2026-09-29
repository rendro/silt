//! Phase C: hover on a stdlib call (`io.read_file`, `tcp.connect`,
//! `list.map`, …) must surface the builtin's effect set in the same
//! `effects: !{...}` block the user-fn hover uses. The lookup path
//! goes through `Server::builtin_effects`, populated from
//! `typechecker::builtin_effects()`.
//!
//! See `docs/proposals/effect-rows.md` Part 7 for the rollout plan.
//! Uses the shared LSP client in `support.rs`.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use crate::support::LspClient;

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_effect_stdlib_hover_{n}.silt")
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

#[test]
fn hover_on_io_read_file_shows_io_fs_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    // Place the call inside a body so the source has a clean cursor
    // anchor. The `read_file` token starts at column 19 on line 2.
    let src = "import io\nfn main() {\n  io.read_file(\"x\")\n}\n";
    client.did_open_and_wait(&uri, src);
    // Cursor on `read_file` (column ~6 in `  io.read_file(`).
    let value = hover_value(&mut client, &uri, 2, 8);
    assert!(
        value.contains("effects: !{fs, io}"),
        "hover on io.read_file must render `effects: !{{fs, io}}`; got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_list_map_shows_pure_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let src = "import list\nfn main() {\n  list.map([1, 2], fn(x) { x })\n}\n";
    client.did_open_and_wait(&uri, src);
    // Cursor on `map`.
    let value = hover_value(&mut client, &uri, 2, 8);
    assert!(
        value.contains("effects: !{}"),
        "hover on list.map must render `effects: !{{}}` (pure); got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_println_shows_io_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let src = "fn main() {\n  println(\"hi\")\n}\n";
    client.did_open_and_wait(&uri, src);
    // Cursor on `println` (the global, not module-qualified).
    let value = hover_value(&mut client, &uri, 1, 4);
    assert!(
        value.contains("effects: !{io}"),
        "hover on println must render `effects: !{{io}}`; got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_uuid_v4_shows_io_random_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let src = "import uuid\nfn main() {\n  uuid.v4()\n}\n";
    client.did_open_and_wait(&uri, src);
    let value = hover_value(&mut client, &uri, 2, 8);
    assert!(
        value.contains("effects: !{io, random}"),
        "hover on uuid.v4 must render `effects: !{{io, random}}`; got {value:?}"
    );
    client.shutdown();
}
