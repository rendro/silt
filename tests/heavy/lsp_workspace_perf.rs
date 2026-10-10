//! The language server in a workspace of files that import one module,
//! none of them open: what an editor asks for whenever the cursor rests
//! must not walk the workspace, and what walks it must not walk it
//! twice.
//!
//! No time is asked of the first walk: it checks every file, and how
//! long that takes is the machine's business (a debug build on a loaded
//! runner needs a quarter of a minute for a thousand files). What is
//! asked is what the machine's speed does not decide: a highlight is
//! answered as for a document alone, and the second "find references"
//! in a fraction of the time of the first.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::LspClient;

/// The importers: a thousand for an optimised server; a quarter of that
/// for a debug build, where the first walk of a thousand takes ten
/// times as long as that of 250.
const FILES: usize = if cfg!(debug_assertions) { 250 } else { 1000 };

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
    // the first learned, in a ninth of the time here. Half of it is a
    // server that checked them again, on a machine of any speed.
    assert!(
        again * 2 < walk,
        "the second `find references` took {again:?}, the first {walk:?}: \
         the workspace was walked again"
    );
}

/// Signature help reads the document's tokens, which are made once for
/// each text: the first request of a megabyte of text lexes it (170 ms
/// in a debug build here), and the requests behind it read what is
/// kept (6 ms). A server that lexes for each request answers every one
/// in the time of the first.
#[test]
fn signature_help_lexes_a_document_once() {
    let mut text = String::new();
    let mut n = 0;
    while text.len() < 1 << 20 {
        text.push_str(&format!(
            "fn f{n}(a: Int, b: Int) -> Int {{\n  a + b + {n}\n}}\n\n"
        ));
        n += 1;
    }
    text.push_str("fn main() {\n  f1(1, 2)\n}\n");
    let line = text.lines().count() as u32 - 2;
    let mut client = LspClient::spawn();
    let uri = "file:///silt_lsp_perf/signature.silt";
    client.did_open(uri, &text);
    client.recv_until_within(ANSWER_TIMEOUT, "the diagnostics", |msg| {
        msg.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
    });
    // Behind the comma of `f1(1, 2)`.
    let at = json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": 8}});
    let mut times = Vec::new();
    for _ in 0..7 {
        let (elapsed, response) = timed(&mut client, "textDocument/signatureHelp", at.clone());
        assert_eq!(
            response["result"]["signatures"][0]["label"], "fn f1(a: Int, b: Int) -> Int",
            "{response}"
        );
        assert_eq!(response["result"]["activeParameter"], 1, "{response}");
        times.push(elapsed);
    }
    client.shutdown();
    let first = times.remove(0);
    times.sort();
    let later = times[times.len() / 2];
    eprintln!(
        "signature help in {} bytes: the first {first:?}, later {later:?}",
        text.len()
    );
    assert!(
        later * 3 < first,
        "signature help took {later:?} (the first request {first:?}, all later: {times:?}): \
         the document is lexed for each request"
    );
}
