//! Round-75 regression tests for LSP trait rename / references.
//!
//! - **DX-2** — `TraitDecl` lacked `name_span`; `definitions.rs`
//!   recorded `t.span` (the `trait` keyword) as the trait's
//!   `DefInfo.span`. LSP rename then replaced the `trait` keyword with
//!   the new name — exactly the same bug round-71 fixed for `let`,
//!   round-63 fixed for `fn`/`type`, but missed for `trait`.
//!
//! - **DX-4** — `TraitImpl::trait_name` and `TraitImpl::target_type`
//!   were never visited by the LSP ident walker, so `find_ident` /
//!   `references` / `rename` skipped both references inside the impl
//!   header. Where-clause trait references and supertrait references
//!   on `TraitDecl` were also unvisited.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::support::LspClient;

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
fn unique_uri(tag: &str) -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_r75_trait_{tag}_{n}.silt")
}

// ── Edit helpers ──────────────────────────────────────────────────

fn apply_edit(source: &str, edit: &Value) -> String {
    let new_text = edit
        .get("newText")
        .and_then(|v| v.as_str())
        .expect("edit has newText");
    let range = edit.get("range").expect("edit has range");
    let sl = range
        .pointer("/start/line")
        .and_then(|v| v.as_u64())
        .unwrap() as usize;
    let sc = range
        .pointer("/start/character")
        .and_then(|v| v.as_u64())
        .unwrap() as usize;
    let el = range.pointer("/end/line").and_then(|v| v.as_u64()).unwrap() as usize;
    let ec = range
        .pointer("/end/character")
        .and_then(|v| v.as_u64())
        .unwrap() as usize;

    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut start_off = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if i == sl {
            start_off += sc.min(line.len());
            break;
        }
        start_off += line.len();
    }
    let mut end_off = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if i == el {
            end_off += ec.min(line.len());
            break;
        }
        end_off += line.len();
    }

    let mut buf = String::with_capacity(source.len() + new_text.len());
    buf.push_str(&source[..start_off]);
    buf.push_str(new_text);
    buf.push_str(&source[end_off..]);
    buf
}

fn apply_all_edits(source: &str, edits: &[Value]) -> String {
    let mut sorted: Vec<Value> = edits.to_vec();
    sorted.sort_by(|a, b| {
        let key = |e: &Value| -> (u64, u64) {
            (
                e.pointer("/range/start/line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                e.pointer("/range/start/character")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            )
        };
        key(b).cmp(&key(a))
    });
    let mut out = source.to_string();
    for e in &sorted {
        out = apply_edit(&out, e);
    }
    out
}

// ── DX-2: rename of trait declaration name does not clobber `trait`
//          keyword. ────────────────────────────────────────────────

#[test]
fn rename_trait_decl_does_not_clobber_trait_keyword() {
    // Pre-fix: `trait Foo { fn bar(self) -> Int { 0 } }` after rename
    // `Foo` -> `Bar` produced `Bar Foo { ... }` because the trait
    // decl's `DefInfo.span` was the `trait` keyword.
    let source = "trait Foo {\n  fn bar(self) -> Int { 0 }\n}\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_trait_decl");
    client.did_open_and_wait(&uri, source);

    // Cursor on `Foo`: line 0, char 6 (after `trait `).
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 6 },
            "newName": "Bar"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on trait-decl name must NOT return null; got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    let renamed = apply_all_edits(source, edits);

    assert!(
        renamed.contains("trait Bar {"),
        "rename should produce `trait Bar {{`; got:\n{renamed}"
    );
    // Pre-fix corruption shape:
    assert!(
        !renamed.starts_with("Bar Foo"),
        "must not corrupt the `trait` keyword; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("trait Foo"),
        "old trait name must be gone from decl; got:\n{renamed}"
    );

    client.shutdown();
}

// ── DX-4: trait-impl trait_name + target_type + where-clause refs are
//          renamed when the trait is renamed. ──────────────────────

#[test]
fn rename_trait_updates_impl_header_and_where_clause() {
    // Source uses `trait Greet { ... }` (the declaration), one impl
    // `trait Greet for Int { ... }`, and a where-clause reference
    // `where n: Greet`. Renaming the trait `Greet -> Wave` from the
    // declaration site must update:
    //   1. the trait declaration itself,
    //   2. the impl-header `trait Greet for Int`,
    //   3. the where-clause's `Greet` reference.
    //
    // Pre-fix: ast_walk + workspace did not visit the impl's
    // `trait_name` or any where-clause `trait_name`, so the rename
    // only edited the declaration and left both impl and where-clause
    // references stale.
    let source = "trait Greet {\n  fn hello(self) -> String\n}\n\
                  trait Greet for Int {\n  fn hello(self) -> String { \"hi\" }\n}\n\
                  fn x(n: a) -> String where a: Greet {\n  n.hello()\n}\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_trait_xref");
    client.did_open_and_wait(&uri, source);

    // Cursor on `Greet` in the trait decl: line 0, char 6.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 6 },
            "newName": "Wave"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on trait-decl name must NOT return null; got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    let renamed = apply_all_edits(source, edits);

    assert!(
        renamed.contains("trait Wave {"),
        "trait declaration must be renamed; got:\n{renamed}"
    );
    assert!(
        renamed.contains("trait Wave for Int"),
        "impl-header trait_name must be renamed; got:\n{renamed}"
    );
    assert!(
        renamed.contains("where a: Wave"),
        "where-clause trait-ref must be renamed; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("Greet"),
        "no `Greet` remnant should remain; got:\n{renamed}"
    );

    client.shutdown();
}
