//! Round-62 audit fix regressions:
//!
//! - **B8** — `find_ident_in_pattern` recognises record-shorthand
//!   binders (`let Point { x, y } = p` → cursor on `x` resolves to a
//!   symbol; previously returned `None`).
//! - **B9** — `PatternKind::AnonRecord` (row-polymorphic destructure
//!   `let { x, y } = p`) wired through every LSP walker so binders are
//!   visible to hover / prepareRename / rename / find references.
//! - **B10** — `visit_expr_children` recurses into
//!   `ExprKind::AnonRecord` field values so identifier searches reach
//!   them (find references on an ident inside an anon-record literal).
//! - **B11** — `Decl::Trait` walks default-method param patterns and
//!   bodies (cursor on `x` in
//!   `trait T { fn foo(x: Int) -> Int = x + 1 }` returns a symbol).
//! - **G6** — completion + REPL `builtin_names` surface primitive /
//!   container type names from `BUILTIN_TYPES` (so `Int`, `Bool`,
//!   `List`, etc. appear in identifier completion).

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::support::LspClient;

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
fn unique_uri(tag: &str) -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_r62_{tag}_{n}.silt")
}

fn extract_completion_labels(result: &Value) -> Vec<String> {
    if let Some(arr) = result.as_array() {
        arr.iter()
            .filter_map(|it| {
                it.get("label")
                    .and_then(|l| l.as_str())
                    .map(|s| s.to_string())
            })
            .collect()
    } else if let Some(arr) = result.pointer("/items").and_then(|v| v.as_array()) {
        arr.iter()
            .filter_map(|it| {
                it.get("label")
                    .and_then(|l| l.as_str())
                    .map(|s| s.to_string())
            })
            .collect()
    } else {
        Vec::new()
    }
}

// ── B8: record-shorthand binder ─────────────────────────────────────

#[test]
fn prepare_rename_on_record_shorthand_binder_returns_edit() {
    // Source layout:
    //   line 0: `type Point { x: Int, y: Int }`
    //   line 1: `fn main() {`
    //   line 2: `  let p = Point { x: 1, y: 2 }`
    //   line 3: `  let Point { x, y } = p`
    //   line 4: `  println("{x} {y}")`
    //   line 5: `}`
    // Cursor on shorthand binder `x` at line 3, char=15
    // (`  let Point { x, y } = p` -> two spaces + "let Point { " = 14, x at 14)
    let mut client = LspClient::spawn();
    let uri = unique_uri("rec_shorthand_pr");
    let text = "type Point { x: Int, y: Int }\nfn main() {\n  let p = Point { x: 1, y: 2 }\n  let Point { x, y } = p\n  println(\"{x} {y}\")\n}\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3, "character": 14 }
        }),
    );
    let result = resp.get("result").expect("prepareRename has result");
    assert!(
        !result.is_null(),
        "prepareRename on record-shorthand binder must NOT return null (round-62 B8); got {resp}"
    );
    client.shutdown();
}

#[test]
fn hover_on_record_shorthand_binder_returns_signature() {
    let mut client = LspClient::spawn();
    let uri = unique_uri("rec_shorthand_hover");
    let text = "type Point { x: Int, y: Int }\nfn main() {\n  let p = Point { x: 1, y: 2 }\n  let Point { x, y } = p\n  println(\"{x} {y}\")\n}\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3, "character": 14 }
        }),
    );
    let result = resp.get("result").expect("hover has result");
    assert!(
        !result.is_null(),
        "hover on record-shorthand binder must NOT return null (round-62 B8); got {resp}"
    );
    client.shutdown();
}

// ── B9: anon-record destructure binder ──────────────────────────────

#[test]
fn prepare_rename_on_anon_record_destructure_binder_returns_edit() {
    // `  let { x, y } = p` -> two spaces + "let { " = 8, `x` at 8
    let mut client = LspClient::spawn();
    let uri = unique_uri("anon_rec_pr");
    let text =
        "fn main() {\n  let p = { x: 10, y: 20 }\n  let { x, y } = p\n  println(\"{x} {y}\")\n}\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 8 }
        }),
    );
    let result = resp.get("result").expect("prepareRename has result");
    assert!(
        !result.is_null(),
        "prepareRename on anon-record-destructure binder must NOT return null (round-62 B9); got {resp}"
    );
    client.shutdown();
}

// ── B10: anon-record literal field-value walking ────────────────────

#[test]
fn find_references_inside_anon_record_literal_field_value() {
    // `let arg = 99\nlet r = { name: arg, age: 30 }`
    // Cursor on `arg` at the use-site inside the anon-record literal.
    // Without the B10 fix, visit_expr_children skipped ExprKind::AnonRecord
    // so the find_references walk never visited the `arg` use site.
    //
    // Layout:
    //   line 0: `fn main() {`
    //   line 1: `  let arg = 99`
    //   line 2: `  let r = { name: arg, age: 30 }`
    //   line 3: `  println(\"{r.name}\")`
    //   line 4: `}`
    // `arg` use-site is on line 2 at char=18: `  let r = { name: arg`
    //                                          0         1
    //                                          012345678901234567890
    let mut client = LspClient::spawn();
    let uri = unique_uri("anon_rec_field_val");
    let text = "fn main() {\n  let arg = 99\n  let r = { name: arg, age: 30 }\n  println(\"{r.name}\")\n}\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 18 },
            "context": { "includeDeclaration": true }
        }),
    );
    let result = resp.get("result").expect("references has result");
    assert!(
        !result.is_null(),
        "find references inside anon-record literal field value must NOT return null (round-62 B10); got {resp}"
    );
    let arr = result.as_array().expect("references result is an array");
    assert!(
        !arr.is_empty(),
        "find references must return at least one location (round-62 B10); got {resp}"
    );
    client.shutdown();
}

// ── B11: trait default-method body / param ─────────────────────────

#[test]
fn prepare_rename_on_trait_default_method_param_returns_edit() {
    // `trait T { fn foo(x: Int) -> Int = x + 1 }`
    // cursor on `x` parameter binder at line 0.
    //  0         1         2
    //  012345678901234567890
    //  trait T { fn foo(x:
    //                   ^ char=17
    let mut client = LspClient::spawn();
    let uri = unique_uri("trait_default_method");
    let text = "trait T { fn foo(x: Int) -> Int = x + 1 }\nfn main() { 0 }\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 17 }
        }),
    );
    let result = resp.get("result").expect("prepareRename has result");
    assert!(
        !result.is_null(),
        "prepareRename on trait default-method param binder must NOT return null (round-62 B11); got {resp}"
    );
    client.shutdown();
}

// ── G6: primitive-type completion + REPL parity ─────────────────────

#[test]
fn completion_offers_primitive_types_in_type_annotation_position() {
    // Source:
    //   line 0: `fn main() {`
    //   line 1: `  let x: B`
    //   line 2: `}`
    // Cursor at end of `B` on line 1, char=11. The completion handler
    // doesn't filter by prefix server-side (the editor handles
    // filtering), but the response MUST include `Bool`, `Int`, `List`,
    // etc. so the editor has anything to filter from.
    let mut client = LspClient::spawn();
    let uri = unique_uri("type_annotation_complete");
    let text = "fn main() {\n  let x: B\n}\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 11 }
        }),
    );
    let result = resp.get("result").expect("completion has result");
    let labels = extract_completion_labels(result);
    for expected in &["Int", "Bool", "Float", "String", "List", "Map"] {
        assert!(
            labels.contains(&expected.to_string()),
            "completion in type-annotation position must include `{expected}` (round-62 G6); \
             got {} labels including: {:?}",
            labels.len(),
            labels
                .iter()
                .filter(|l| !l.contains('.'))
                .take(20)
                .collect::<Vec<_>>()
        );
    }
    client.shutdown();
}

#[test]
fn repl_builtin_names_includes_primitive_types() {
    // `repl::builtin_names` must surface every BUILTIN_TYPES entry so
    // <Tab> completion in the REPL offers them.
    let names = silt::repl::builtin_names();

    // Sourced from the authoritative constant.
    let expected: Vec<&str> = silt::types::builtins::iter_all().map(|b| b.name).collect();

    for entry in &expected {
        assert!(
            names.iter().any(|n| n == entry),
            "repl::builtin_names must include `{entry}` (round-62 G6); \
             got {} entries; missing this BUILTIN_TYPES name",
            names.len()
        );
    }

    // Sanity: at least the 15 user-typeable type names mentioned in the
    // round-62 audit (the `()` surface alias makes 16 total — the audit
    // text refers to the 15 user-facing names).
    assert!(
        expected.len() >= 15,
        "BUILTIN_TYPES is expected to have at least 15 entries; got {} — \
         the authoritative list shrank, update this test.",
        expected.len()
    );

    // Spot-check every primitive (audit's "primitive type names") and a
    // representative slice of containers.
    for required in &[
        "Int", "Float", "ExtFloat", "Bool", "String", "Unit", "List", "Range", "Map", "Set",
        "Channel", "Tuple", "Fn", "Fun", "Handle",
    ] {
        assert!(
            names.iter().any(|n| n == required),
            "repl::builtin_names missing core BUILTIN_TYPES entry `{required}`"
        );
    }
}
