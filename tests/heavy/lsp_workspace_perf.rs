//! The language server in a workspace of files that import one module,
//! none of them open: what an editor asks for whenever the cursor rests
//! must not walk the workspace, and what walks it must not walk it
//! twice.
//!
//! No time is asked of the first walk: it checks every file, and how
//! long that takes is the machine's business (a debug build needs 2.6 s
//! for a thousand files on a quiet machine; a loaded runner several
//! times that). What is asked is what the machine's speed does not
//! decide: a highlight is answered as for a document alone, and the
//! second "find references" in a fraction of the time of the first.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::LspClient;

/// The importers.
const FILES: usize = 1000;

/// How long an answer is waited for. The times are judged by the
/// assertions at the end, which say what is wrong; this only ends the
/// wait for a server that will not answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(600);

fn timed(client: &mut LspClient, method: &str, params: Value) -> (Duration, Value) {
    let start = Instant::now();
    let response = client.request_within(ANSWER_TIMEOUT, method, params);
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

    let (walk, response) = timed(&mut client, "textDocument/references", references.clone());
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

    // (Shown when the test fails, and with `--no-capture`.)
    eprintln!(
        "{FILES} files: the first `find references` took {walk:?}, the second {again:?}, \
         the highlights {highlights:?}"
    );
    highlights.sort();
    let highlight = highlights[highlights.len() / 2];
    // A debug build answers in 1 to 3 ms here; a highlight that asks the
    // workspace takes at least what the second `find references` does.
    assert!(
        highlight < Duration::from_millis(100),
        "a highlight took {highlight:?} (all: {highlights:?}): it reads more than its document"
    );
    // The first answer checked every importer; the second reads what
    // the first learned, in a fifth of the time here. Half of it is a
    // server that checked them again, on a machine of any speed.
    assert!(
        again * 2 < walk,
        "the second `find references` took {again:?}, the first {walk:?}: \
         the workspace was walked again"
    );
}
