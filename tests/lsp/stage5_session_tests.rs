//! The server keeps one compilation session per project (stage 5,
//! step 4): every open document is an overlay of it, and the features
//! read the session's checked modules.
//!
//! - An edit to an imported file, open in the editor, is seen by the
//!   file that imports it: its diagnostics are published again, and they
//!   stay right after the importer is edited in turn. Each analysis used
//!   to build a fresh session that read every other file from disk.
//! - Hover, definition and signature help on a member of an imported
//!   module (`helper.twice`, or `twice` from `import helper.{ twice }`)
//!   reach the imported module's declaration in its own file.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::support::LspClient;

/// A fresh directory with `files` (name, text) in it.
fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let n = crate::support::next_id();
    let dir = std::env::temp_dir().join(format!(
        "silt_stage5_session_{tag}_{}_{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    for (name, text) in files {
        fs::write(dir.join(name), text).expect("write");
    }
    fs::canonicalize(&dir).expect("canonicalize")
}

fn uri(path: &Path) -> String {
    let s = path.to_str().expect("utf8 path").replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// The messages of the diagnostics in a `publishDiagnostics`.
fn messages(publish: &Value) -> Vec<String> {
    publish
        .pointer("/params/diagnostics")
        .and_then(Value::as_array)
        .map(|diags| {
            diags
                .iter()
                .filter_map(|d| d["message"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn did_change(client: &mut LspClient, uri: &str, version: i32, text: &str) {
    client.send_notification(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }],
        }),
    );
}

const HELPER: &str = "-- Doubles its argument.\npub fn twice(x: Int) -> Int {\n  x * 2\n}\n";
const HELPER_WITH_THRICE: &str = "-- Doubles its argument.\npub fn twice(x: Int) -> Int {\n  x * 2\n}\n\npub fn thrice(x: Int) -> Int {\n  x * 3\n}\n";
const MAIN: &str =
    "import helper\n\nfn main() {\n  println(\"{helper.twice(1)} {helper.thrice(1)}\")\n}\n";

#[test]
fn editing_an_imported_file_republishes_its_importer() {
    let dir = project("edit", &[("helper.silt", HELPER), ("main.silt", MAIN)]);
    let main_uri = uri(&dir.join("main.silt"));
    let helper_uri = uri(&dir.join("helper.silt"));
    let mut client = LspClient::spawn_with_root(Some(&uri(&dir)));

    let first = client.did_open_and_wait(&main_uri, MAIN);
    assert!(
        messages(&first).iter().any(|m| m.contains("thrice")),
        "main.silt uses `helper.thrice`, which helper.silt lacks: {first}"
    );
    client.did_open_and_wait(&helper_uri, HELPER);

    // The editor adds `thrice` to helper.silt; nothing is saved.
    did_change(&mut client, &helper_uri, 2, HELPER_WITH_THRICE);
    let republished = client.wait_for_diagnostics(&main_uri);
    assert_eq!(
        messages(&republished),
        Vec::<String>::new(),
        "main.silt is published again without the error: {republished}"
    );

    // An edit to main.silt still sees the unsaved helper.silt.
    let edited = MAIN.replace("helper.twice(1)", "helper.twice(2)");
    did_change(&mut client, &main_uri, 2, &edited);
    let after = client.wait_for_diagnostics(&main_uri);
    assert_eq!(
        messages(&after),
        Vec::<String>::new(),
        "main.silt keeps seeing `thrice`: {after}"
    );

    // Closing helper.silt makes the disk text its text again.
    client.send_notification(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": helper_uri } }),
    );
    let reverted = client.wait_for_diagnostics(&main_uri);
    assert!(
        messages(&reverted).iter().any(|m| m.contains("thrice")),
        "with helper.silt closed, its disk text has no `thrice`: {reverted}"
    );
    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

fn position_of(text: &str, needle: &str, offset: usize) -> (u32, u32) {
    let at = text.find(needle).expect("needle in text") + offset;
    let line = text[..at].matches('\n').count() as u32;
    let col = (at - text[..at].rfind('\n').map_or(0, |i| i + 1)) as u32;
    (line, col)
}

fn request_at(client: &mut LspClient, method: &str, uri: &str, at: (u32, u32)) -> Value {
    client.request_result(
        method,
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": at.0, "character": at.1 },
        }),
    )
}

#[test]
fn hover_definition_and_signature_help_reach_an_imported_module() {
    let main = "import helper\nimport helper.{ twice }\n\nfn main() {\n  let a = helper.twice(1)\n  let b = twice(a)\n  println(\"{b}\")\n}\n";
    let dir = project("cross", &[("helper.silt", HELPER), ("main.silt", main)]);
    let main_uri = uri(&dir.join("main.silt"));
    let helper_uri = uri(&dir.join("helper.silt"));
    let mut client = LspClient::spawn_with_root(Some(&uri(&dir)));
    let diags = client.did_open_and_collect_diagnostics(&main_uri, main);
    assert!(diags.is_empty(), "the program is clean: {diags:?}");

    // `twice` in `helper.twice(1)`.
    let qualified = position_of(main, "helper.twice(1)", "helper.".len() + 1);
    let hover = request_at(&mut client, "textDocument/hover", &main_uri, qualified);
    let text = hover.pointer("/contents/value").and_then(Value::as_str);
    let text = text.unwrap_or_default();
    assert!(text.contains("Int) -> Int"), "the type: {hover}");
    assert!(
        text.contains("Doubles its argument."),
        "helper's doc: {hover}"
    );

    let def = request_at(&mut client, "textDocument/definition", &main_uri, qualified);
    assert_eq!(def["uri"], json!(helper_uri), "into helper.silt: {def}");
    assert_eq!(def.pointer("/range/start/line"), Some(&json!(1)), "{def}");

    // `twice` imported by name.
    let bare = position_of(main, "twice(a)", 1);
    let def = request_at(&mut client, "textDocument/definition", &main_uri, bare);
    assert_eq!(def["uri"], json!(helper_uri), "into helper.silt: {def}");
    assert_eq!(def.pointer("/range/start/line"), Some(&json!(1)), "{def}");

    // Signature help inside `helper.twice(`.
    let inside = position_of(main, "helper.twice(1)", "helper.twice(".len());
    let help = request_at(&mut client, "textDocument/signatureHelp", &main_uri, inside);
    let label = help.pointer("/signatures/0/label").and_then(Value::as_str);
    assert_eq!(label, Some("fn helper.twice(x: Int) -> Int"), "{help}");
    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// A type alias changed in an open imported file changes what the
/// importer's annotations mean (the alias is the imported module's, and
/// the importer is checked again with it).
#[test]
fn changing_an_imported_alias_rechecks_the_importer() {
    let units = "pub type Meters = Int\n";
    let main = "import units.{ Meters }\n\nfn grow(m: Meters) -> Int {\n  m + 1\n}\n\nfn main() {\n  println(\"{grow(1)}\")\n}\n";
    let dir = project("alias", &[("units.silt", units), ("main.silt", main)]);
    let main_uri = uri(&dir.join("main.silt"));
    let units_uri = uri(&dir.join("units.silt"));
    let mut client = LspClient::spawn_with_root(Some(&uri(&dir)));
    let first = client.did_open_and_wait(&main_uri, main);
    assert_eq!(messages(&first), Vec::<String>::new(), "{first}");
    client.did_open_and_wait(&units_uri, units);

    did_change(&mut client, &units_uri, 2, "pub type Meters = String\n");
    let after = client.wait_for_diagnostics(&main_uri);
    assert!(
        messages(&after).iter().any(|m| m.contains("type mismatch")),
        "`m + 1` and `grow(1)` no longer check with Meters = String: {after}"
    );

    did_change(&mut client, &units_uri, 3, units);
    let back = client.wait_for_diagnostics(&main_uri);
    assert_eq!(messages(&back), Vec::<String>::new(), "{back}");
    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// A broken silt.toml: its error is published on silt.toml and the
/// document gets one note; once silt.toml is fixed, the next analysis
/// resolves the project again.
#[test]
fn fixing_silt_toml_resolves_the_project_again() {
    let broken = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nnope = { path = \"../does-not-exist\" }\n";
    let fixed = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";
    let main = "import helper\n\nfn main() {\n  println(\"{helper.twice(2)}\")\n}\n";
    let dir = project("manifest", &[("silt.toml", broken)]);
    fs::create_dir_all(dir.join("src")).expect("mkdir src");
    fs::write(dir.join("src/helper.silt"), HELPER).expect("write");
    fs::write(dir.join("src/main.silt"), main).expect("write");
    let main_uri = uri(&dir.join("src/main.silt"));
    let manifest_uri = uri(&dir.join("silt.toml"));
    let mut client = LspClient::spawn_with_root(Some(&uri(&dir)));

    client.did_open(&main_uri, main);
    let manifest = client.wait_for_diagnostics(&manifest_uri);
    assert!(
        messages(&manifest)
            .iter()
            .any(|m| m.contains("does not exist")),
        "the manifest error is on silt.toml: {manifest}"
    );
    let note = client.wait_for_diagnostics(&main_uri);
    let diags = note
        .pointer("/params/diagnostics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(diags.len(), 1, "one note, no import cascade: {note}");
    assert_eq!(diags[0]["severity"], json!(3), "{note}");

    fs::write(dir.join("silt.toml"), fixed).expect("write");
    did_change(&mut client, &main_uri, 2, main);
    let cleared = client.wait_for_diagnostics(&manifest_uri);
    assert_eq!(messages(&cleared), Vec::<String>::new(), "{cleared}");
    let clean = client.wait_for_diagnostics(&main_uri);
    assert_eq!(messages(&clean), Vec::<String>::new(), "{clean}");
    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

const MAIN_TWICE: &str = "import helper\n\nfn main() {\n  println(\"{helper.twice(1)}\")\n}\n";

/// A client that does not watch files: a closed imported file that
/// changes or goes away on disk is read again at the next analysis
/// (its stamp changed), and a deleted import is reported as not found.
#[test]
fn a_closed_import_changed_on_disk_is_read_again() {
    let dir = project(
        "disk",
        &[("helper.silt", HELPER), ("main.silt", MAIN_TWICE)],
    );
    let main_uri = uri(&dir.join("main.silt"));
    let mut client = LspClient::spawn_with_root(Some(&uri(&dir)));
    let first = client.did_open_and_wait(&main_uri, MAIN_TWICE);
    assert_eq!(messages(&first), Vec::<String>::new(), "{first}");

    // Another program rewrites helper.silt without `twice`; the size
    // changes too, so the stamp differs whatever the clock resolution.
    fs::write(dir.join("helper.silt"), "pub fn other() -> Int {\n  0\n}\n").expect("write");
    did_change(&mut client, &main_uri, 2, MAIN_TWICE);
    let changed = client.wait_for_diagnostics(&main_uri);
    assert!(
        messages(&changed).iter().any(|m| m.contains("twice")),
        "helper.silt is read again: {changed}"
    );

    fs::remove_file(dir.join("helper.silt")).expect("remove");
    did_change(&mut client, &main_uri, 3, MAIN_TWICE);
    let deleted = client.wait_for_diagnostics(&main_uri);
    assert!(
        messages(&deleted)
            .iter()
            .any(|m| m.contains("cannot load module 'helper'")),
        "the deleted import is not found: {deleted}"
    );
    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// A client that watches files: the server registers for
/// `workspace/didChangeWatchedFiles`, and a reported deletion of a closed
/// import republishes the importer, with no edit of it.
#[test]
fn a_reported_deletion_republishes_the_importer() {
    let dir = project(
        "watch",
        &[("helper.silt", HELPER), ("main.silt", MAIN_TWICE)],
    );
    let main_uri = uri(&dir.join("main.silt"));
    let helper_uri = uri(&dir.join("helper.silt"));
    let mut client = LspClient::spawn_uninitialized();
    let id = crate::support::next_id();
    client.send_request(
        id,
        "initialize",
        json!({
            "rootUri": uri(&dir),
            "capabilities": {
                "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } }
            },
        }),
    );
    client.recv_response_for(id);
    client.send_notification("initialized", json!({}));
    let registration = client.recv_until("client/registerCapability", |msg| {
        msg["method"] == "client/registerCapability"
    });
    let watched = &registration["params"]["registrations"][0];
    assert_eq!(
        watched["method"], "workspace/didChangeWatchedFiles",
        "{registration}"
    );
    client.send_raw(&json!({ "jsonrpc": "2.0", "id": registration["id"], "result": null }));

    let first = client.did_open_and_wait(&main_uri, MAIN_TWICE);
    assert_eq!(messages(&first), Vec::<String>::new(), "{first}");

    fs::remove_file(dir.join("helper.silt")).expect("remove");
    client.send_notification(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": helper_uri, "type": 3 }] }),
    );
    let deleted = client.wait_for_diagnostics(&main_uri);
    assert!(
        messages(&deleted)
            .iter()
            .any(|m| m.contains("cannot load module 'helper'")),
        "the deleted import is not found: {deleted}"
    );
    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// Two open files that import one missing module: each gets the error at
/// its own import, and fixing one import clears its error only.
#[test]
fn each_importer_of_a_missing_module_gets_its_own_error() {
    let a = "import nope\n\nfn main() {\n  println(\"a\")\n}\n";
    let b = "import nope\n\npub fn b() -> Int {\n  1\n}\n";
    let dir = project("missing", &[("a.silt", a), ("b.silt", b)]);
    let a_uri = uri(&dir.join("a.silt"));
    let b_uri = uri(&dir.join("b.silt"));
    let mut client = LspClient::spawn_with_root(Some(&uri(&dir)));
    let missing = |publish: &Value| {
        messages(publish)
            .iter()
            .filter(|m| m.contains("cannot load module 'nope'"))
            .count()
    };
    let first = client.did_open_and_wait(&a_uri, a);
    assert_eq!(missing(&first), 1, "{first}");
    let second = client.did_open_and_wait(&b_uri, b);
    assert_eq!(missing(&second), 1, "{second}");

    let fixed = a.replace("import nope\n", "");
    did_change(&mut client, &a_uri, 2, &fixed);
    let after = client.wait_for_diagnostics(&a_uri);
    assert_eq!(missing(&after), 0, "a.silt no longer imports nope: {after}");
    let pull = client.request_result(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": b_uri } }),
    );
    let still = pull["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|d| {
                    d["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("cannot load module 'nope'"))
                })
                .count()
        })
        .unwrap_or(0);
    assert_eq!(still, 1, "b.silt still imports nope: {pull}");
    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// A workspace opened through a symbolic link: the diagnostics go to the
/// open document's URI (the link's spelling), not to the indexed
/// canonical one. Several servers, since the bug depended on hash order.
#[cfg(unix)]
#[test]
fn a_symlinked_workspace_publishes_to_the_open_uri() {
    let main = "import helper\n\nfn main() {\n  println(\"{helper.twice(missing)}\")\n}\n";
    let real = project("real", &[("helper.silt", HELPER), ("main.silt", main)]);
    let link = real.with_file_name(format!(
        "{}_link",
        real.file_name().and_then(|n| n.to_str()).expect("name")
    ));
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let main_uri = uri(&link.join("main.silt"));
    for _ in 0..4 {
        let mut client = LspClient::spawn_with_root(Some(&uri(&link)));
        let publish = client.did_open_and_wait(&main_uri, main);
        assert!(
            messages(&publish).iter().any(|m| m.contains("missing")),
            "the open document gets its error: {publish}"
        );
        client.shutdown();
    }
    let _ = fs::remove_file(&link);
    let _ = fs::remove_dir_all(&real);
}
