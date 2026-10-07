//! The language server in a workspace of a thousand files that import
//! one module, none of them open: what an editor asks for whenever the
//! cursor rests must not walk the workspace, and what walks it must not
//! walk it twice.
//!
//! The caps are ten times what the server needs (a release build
//! answers a highlight in about 5 ms and a repeated "find references"
//! in under 0.3 s): they catch a query that is back to walking every
//! file, not a slow machine.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::LspClient;

const FILES: usize = 1000;

fn timed(client: &mut LspClient, method: &str, params: Value) -> (Duration, Value) {
    let start = Instant::now();
    let response = client.request(method, params);
    (start.elapsed(), response)
}

#[test]
fn highlight_stays_in_the_document_and_references_are_walked_once() {
    let dir = std::env::temp_dir().join(format!("silt_lsp_perf_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = silt::source::canonical_path(&dir);
    let geo = "pub fn mk(w: Int) -> Int {\n  w + 1\n}\n";
    std::fs::write(dir.join("geo.silt"), geo).unwrap();
    for i in 0..FILES {
        let mut text = String::from("import geo\n\n");
        for j in 0..20 {
            text.push_str(&format!(
                "pub fn f{i}_{j}(x: Int) -> Int {{\n  geo.mk(x) + {j}\n}}\n\n"
            ));
        }
        std::fs::write(dir.join(format!("m{i}.silt")), text).unwrap();
    }
    let uri_of = |path: &std::path::Path| {
        silt::lsp::path_to_file_uri(path)
            .expect("a file URI")
            .as_str()
            .to_string()
    };
    let mut client = LspClient::spawn_with_root(Some(&uri_of(&dir)));
    let uri = uri_of(&dir.join("geo.silt"));
    client.did_open_and_wait(&uri, geo);
    // On `mk` of `pub fn mk`.
    let at = json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 7}});
    let references = json!({
        "textDocument": {"uri": uri},
        "position": {"line": 0, "character": 7},
        "context": {"includeDeclaration": true},
    });

    // A highlight before anything walked the workspace, and five more
    // after: one place, in this document, each time.
    let mut highlights = Vec::new();
    let (first, response) = timed(&mut client, "textDocument/documentHighlight", at.clone());
    assert_eq!(
        response["result"].as_array().map(Vec::len),
        Some(1),
        "{response}"
    );
    highlights.push(first);

    let (_, response) = timed(&mut client, "textDocument/references", references.clone());
    let places = response["result"].as_array().map_or(0, Vec::len);
    assert_eq!(
        places,
        FILES * 20 + 1,
        "every importer's uses and the declaration"
    );
    let (again, response) = timed(&mut client, "textDocument/references", references);
    assert_eq!(response["result"].as_array().map_or(0, Vec::len), places);

    for _ in 0..5 {
        let (elapsed, _) = timed(&mut client, "textDocument/documentHighlight", at.clone());
        highlights.push(elapsed);
    }
    client.shutdown();
    let _ = std::fs::remove_dir_all(&dir);

    highlights.sort();
    let highlight = highlights[highlights.len() / 2];
    assert!(
        highlight < Duration::from_millis(50),
        "a highlight took {highlight:?} (all: {highlights:?}): it reads more than its document"
    );
    assert!(
        again < Duration::from_secs(3),
        "the second `find references` took {again:?}: the workspace was walked again"
    );
}
