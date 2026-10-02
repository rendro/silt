//! End-to-end LSP test for `textDocument/semanticTokens/full`.
//!
//! Uses the shared LSP client in `support.rs`, which
//! spawns `silt lsp` and speaks LSP JSON-RPC over stdio.

use serde_json::json;

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

/// Indexes into `TOKEN_LEGEND` — must match
/// `src/lsp/semantic_tokens.rs::TOKEN_LEGEND`.
const TT_FUNCTION: u64 = 0;
const TT_TYPE: u64 = 1;
const TT_ENUM: u64 = 2;
const TT_INTERFACE: u64 = 4;

#[test]
fn semantic_tokens_full_returns_classified_tokens() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_sem_tokens_a.silt";
    // Has: fn decl (foo), let-binding (x), type decl (Color), variant (Red),
    // trait decl (Show), method inside trait (show).
    let src =
        "fn foo() { let x = 42 }\ntype Color { Red }\ntrait Show { fn show(self) -> String }\n";
    client.did_open_and_wait(uri, src);

    let resp = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );

    let data = resp
        .pointer("/result/data")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected /result/data array; got: {resp}"));
    assert!(
        !data.is_empty(),
        "expected non-empty semantic tokens data; got: {resp}"
    );
    assert_eq!(
        data.len() % 5,
        0,
        "semantic tokens data length must be divisible by 5; got {}",
        data.len()
    );

    // Decode tokens (delta-encoded). Each is [deltaLine, deltaStart, length,
    // tokenType, tokenModifiers]. Reconstruct absolute positions so we can
    // verify the encoding is syntactically valid, and collect token types.
    let mut abs_line = 0i64;
    let mut abs_start = 0i64;
    let mut types: Vec<u64> = Vec::new();
    for chunk in data.as_chunks::<5>().0 {
        let dl = chunk[0].as_i64().expect("deltaLine u32");
        let ds = chunk[1].as_i64().expect("deltaStart u32");
        let len = chunk[2].as_i64().expect("length u32");
        let tt = chunk[3].as_u64().expect("tokenType u32");
        let _mods = chunk[4].as_i64().expect("tokenModifiers u32");
        assert!(dl >= 0, "deltaLine must be non-negative");
        assert!(ds >= 0, "deltaStart must be non-negative");
        assert!(len > 0, "token length must be positive");
        if dl == 0 {
            abs_start += ds;
        } else {
            abs_line += dl;
            abs_start = ds;
        }
        assert!(abs_line >= 0 && abs_start >= 0);
        types.push(tt);
    }

    assert!(
        types.contains(&TT_FUNCTION),
        "expected a FUNCTION token for `foo`; got types: {types:?}"
    );
    assert!(
        types.contains(&TT_ENUM) || types.contains(&TT_TYPE),
        "expected a TYPE/ENUM token for `Color`; got types: {types:?}"
    );
    assert!(
        types.contains(&TT_INTERFACE),
        "expected an INTERFACE token for `Show`; got types: {types:?}"
    );

    client.shutdown();
}
