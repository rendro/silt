//! Round-71 regression: LSP rename on top-level `let` bindings and
//! cross-namespace symbol-collision corruption via FieldAccess.
//!
//! - **DX-1** — `Decl::Let.span` was the `let` keyword's span (parser
//!   stored `let span = self.span()` before consuming `let`).
//!   `build_definitions` recorded that span as the binding's
//!   `DefInfo.span`; LSP rename applied the new name as a TextEdit over
//!   `token_len_at(source, span.offset)` bytes, so renaming `counter`
//!   in `let counter = 42` produced `ctr ctr = 42` (clobbering `let`),
//!   and renaming through `pub let counter = 42` clobbered `pub`. The
//!   fix mirrors round-63 B1: add a `name_span` field on `Decl::Let`
//!   that the parser fills in from the bound identifier's span, and
//!   thread it through `definitions.rs` so the binder's `DefInfo.span`
//!   covers the name (not the keyword).
//!
//! - **DX-2** — `collect_references_in_expr` matched on
//!   `ExprKind::FieldAccess(_, field) if *field == name`, but
//!   `FieldAccess.span = receiver.span` (parser.rs:2620/2696). Symbols
//!   are interned, so a top-level `let name` and a record field `name`
//!   share the same `Symbol`. Renaming the let pushed the receiver's
//!   span as a "reference", silently corrupting `r.name` into
//!   `<newname>.name`. Field names live in a separate namespace from
//!   let/fn names; co-renaming by symbol identity is wrong. Fix: drop
//!   the FieldAccess arm.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::support::LspClient;

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
fn unique_uri(tag: &str) -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_r71_letrename_{tag}_{n}.silt")
}

// ── Edit helpers (mirrors lsp_fn_decl_rename_and_handlers_tests) ──────

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

// ── DX-1: rename of top-level `let` does not clobber the `let` keyword ──

#[test]
fn rename_let_top_level_does_not_clobber_keyword() {
    // Pre-fix: `let counter = 42` after rename `counter` -> `ctr`
    // produced `ctr ctr = 42` because the `Decl::Let.span` pointed at
    // the `let` keyword and the rename TextEdit covered three bytes
    // starting at offset 0 (i.e. it overwrote `let`).
    let source = "let counter = 42\nfn main() {\n  println(counter)\n}\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_let_top");
    client.did_open_and_wait(&uri, source);

    // Cursor on `counter`: line 0, char=4 (after `let `).
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 4 },
            "newName": "ctr"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on let-binding name must NOT return null; got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");

    let renamed = apply_all_edits(source, edits);
    assert!(
        renamed.contains("let ctr = 42"),
        "rename should produce `let ctr = 42`; got:\n{renamed}"
    );
    // Pre-fix corruption shape:
    assert!(
        !renamed.starts_with("ctr ctr"),
        "must not corrupt the `let` keyword; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("let counter"),
        "old name must be gone from decl; got:\n{renamed}"
    );
    assert!(
        renamed.contains("println(ctr)"),
        "use site should be renamed; got:\n{renamed}"
    );
    client.shutdown();
}

#[test]
fn rename_pub_let_does_not_clobber_pub_keyword() {
    // Pre-fix: `pub let counter = 42` rename `counter` -> `ctr` produced
    // `ctr let counter = 42` (or worse) — the decl span was placed at
    // the `pub` keyword by the parser's `pub let` arm, so the TextEdit
    // covered chars 0..3 (`pub`).
    let source = "pub let counter = 42\nfn main() {\n  println(counter)\n}\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_pub_let");
    client.did_open_and_wait(&uri, source);

    // Cursor on `counter`: line 0, char=8 (after `pub let `).
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 8 },
            "newName": "ctr"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on pub-let name must NOT return null; got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");

    let renamed = apply_all_edits(source, edits);
    assert!(
        renamed.contains("pub let ctr = 42"),
        "rename should produce `pub let ctr = 42`; got:\n{renamed}"
    );
    assert!(
        renamed.starts_with("pub let "),
        "must not corrupt the `pub` keyword; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("pub let counter"),
        "old name must be gone; got:\n{renamed}"
    );
    assert!(
        renamed.contains("println(ctr)"),
        "use site should be renamed; got:\n{renamed}"
    );
    client.shutdown();
}

// ── DX-2: rename does not corrupt receiver via field-name symbol collision ──

#[test]
fn rename_let_does_not_corrupt_record_field_via_symbol_collision() {
    // Pre-fix: top-level `let name` shares a `Symbol` with the record
    // field `name` in `Person { name: String, ... }`. The FieldAccess
    // arm in `collect_references_in_expr` fired on `r.name` and pushed
    // the receiver's span (the `r`), so renaming the let mangled
    // `r.name` into `<newname>.name`.
    let source = "type Person { name: String, age: Int }\nlet name = \"Alice\"\nfn main() {\n  let r = Person { name: \"Bob\", age: 30 }\n  println(name)\n  println(r.name)\n}\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_let_field_collision");
    client.did_open_and_wait(&uri, source);

    // Cursor on `name` (the let): line 1, char=4 (after `let `).
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 4 },
            "newName": "alice"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on let `name` must NOT return null; got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");

    let renamed = apply_all_edits(source, edits);
    // The let and its bare-Ident use site should be renamed.
    assert!(
        renamed.contains("let alice = \"Alice\""),
        "rename should produce `let alice = \"Alice\"`; got:\n{renamed}"
    );
    assert!(
        renamed.contains("println(alice)"),
        "bare ident use should be renamed; got:\n{renamed}"
    );
    // The record field declaration must NOT be touched.
    assert!(
        renamed.contains("name: String"),
        "record field declaration must be untouched; got:\n{renamed}"
    );
    // The record literal field must NOT be touched.
    assert!(
        renamed.contains("name: \"Bob\""),
        "record literal field must be untouched; got:\n{renamed}"
    );
    // The receiver `r.name` must NOT be mangled — `r` stays as `r` and
    // `.name` stays as `.name`. Specifically, pre-fix the receiver `r`
    // got replaced with `alice`, producing `alice.name`.
    assert!(
        renamed.contains("r.name"),
        "field access receiver must be untouched; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("alice.name"),
        "must not have mangled `r.name` into `alice.name`; got:\n{renamed}"
    );
    client.shutdown();
}

// ── Destructuring let bail-out ─────────────────────────────────────

#[test]
fn rename_destructuring_let_is_handled_safely() {
    // `let (a, b) = (1, 2)` — destructuring patterns at top-level have
    // no single name span. Either the rename bails (returns null/empty)
    // or it emits a correct edit that does NOT clobber the `let`
    // keyword. Both shapes are acceptable; the regression we're
    // guarding is "must not clobber `let`".
    let source = "let (a, b) = (1, 2)\nfn main() {\n  println(a)\n  println(b)\n}\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_destructure_let");
    client.did_open_and_wait(&uri, source);

    // Cursor on `a` in the destructuring pattern: line 0, char=5.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 5 },
            "newName": "first"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    if result.is_null() {
        // Acceptable: destructuring rename may bail. Done.
        client.shutdown();
        return;
    }
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");

    let renamed = apply_all_edits(source, edits);
    // The `let` keyword must be preserved regardless of whether or not
    // the destructured leaf was renamed.
    assert!(
        renamed.starts_with("let "),
        "must not corrupt the `let` keyword on destructuring let; got:\n{renamed}"
    );
    // If `a` was renamed, the LHS must contain `first` and the use
    // sites must too. Not asserting strict equality so a "no-op rename"
    // also passes.
    if !renamed.contains("(a, b)") {
        assert!(
            renamed.contains("(first, b)"),
            "if rename happened, leaf should be `first`; got:\n{renamed}"
        );
    }
    client.shutdown();
}
