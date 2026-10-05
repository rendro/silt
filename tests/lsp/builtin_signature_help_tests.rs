//! Signature help for a builtin function shows its row of the builtin
//! registry: the signature with the parameter names, and where each
//! parameter is in it, so the editor can mark the active one.
//!
//! Before the registry, parameter names came from a table of their own
//! that covered `list`, `string`, `map`, `set` and `io`: a `time.*` call
//! showed `time.add: Fn(Instant, Duration) -> Instant` with no parameter
//! to mark.

use serde_json::{Value, json};

use crate::support::LspClient;

/// The signature help at `line`:`character` of `source`.
fn signature_help(tag: &str, source: &str, line: u32, character: u32) -> Value {
    let mut client = LspClient::spawn();
    let uri = format!(
        "file:///tmp/silt_builtin_sig_help_{}_{tag}.silt",
        std::process::id()
    );
    client.did_open_and_wait(&uri, source);
    let resp = client.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    client.shutdown();
    resp.get("result")
        .cloned()
        .expect("signatureHelp response has a result")
}

/// The text of each parameter of the signature: the parameter's label,
/// or the label's slice at the parameter's offsets for a client that
/// asked for offsets.
fn parameters(signature: &Value) -> Vec<String> {
    let label = signature["label"].as_str().expect("a label");
    signature["parameters"]
        .as_array()
        .expect("a parameters array")
        .iter()
        .map(|p| match &p["label"] {
            Value::String(text) => {
                assert!(label.contains(text.as_str()), "{text} is part of {label}");
                text.clone()
            }
            range => {
                let range = range.as_array().expect("label offsets");
                let (start, end) = (
                    range[0].as_u64().expect("a start") as usize,
                    range[1].as_u64().expect("an end") as usize,
                );
                label[start..end].to_string()
            }
        })
        .collect()
}

/// A client that declares `labelOffsetSupport` gets offsets; one that
/// does not (the other tests' client) gets the parameters' text.
#[test]
fn parameters_are_offsets_only_for_a_client_that_reads_them() {
    let source = "import time\nfn main() {\n  time.add(time.now(), time.seconds(1))\n}\n";
    let mut client = LspClient::spawn_uninitialized();
    let _ = client.request(
        "initialize",
        json!({ "capabilities": { "textDocument": { "signatureHelp": { "signatureInformation": {
            "parameterInformation": { "labelOffsetSupport": true }
        } } } } }),
    );
    client.send_notification("initialized", json!({}));
    let uri = format!(
        "file:///tmp/silt_builtin_sig_offsets_{}.silt",
        std::process::id()
    );
    client.did_open_and_wait(&uri, source);
    let resp = client.request(
        "textDocument/signatureHelp",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 2, "character": 23 } }),
    );
    client.shutdown();
    let signature = &resp["result"]["signatures"][0];
    assert_eq!(signature["parameters"][0]["label"], json!([12, 28]));
    assert_eq!(
        parameters(signature),
        ["instant: Instant", "duration: Duration"]
    );

    let plain = signature_help("time_add_plain", source, 2, 23);
    assert_eq!(
        plain["signatures"][0]["parameters"][0]["label"],
        "instant: Instant"
    );
}

#[test]
fn a_time_function_shows_its_parameter_names() {
    // line 2: `  time.add(time.now(), time.seconds(1))`; the cursor is
    // on the second argument, after `time.now(), `.
    let source = "import time\nfn main() {\n  time.add(time.now(), time.seconds(1))\n}\n";
    let help = signature_help("time_add", source, 2, 23);
    let signature = &help["signatures"][0];
    assert_eq!(
        signature["label"],
        "fn time.add(instant: Instant, duration: Duration) -> Instant"
    );
    assert_eq!(
        parameters(signature),
        ["instant: Instant", "duration: Duration"]
    );
    assert_eq!(signature["activeParameter"], 1);
    assert!(
        signature["documentation"]["value"]
            .as_str()
            .is_some_and(|doc| doc.contains("time.add")),
        "the function's section of docs/stdlib/time.md comes with it: {signature}"
    );
}

#[test]
fn a_function_of_no_parameters_has_none_to_mark() {
    let source = "import time\nfn main() {\n  time.now()\n}\n";
    let help = signature_help("time_now", source, 2, 11);
    let signature = &help["signatures"][0];
    assert_eq!(signature["label"], "fn time.now() -> Instant");
    assert_eq!(parameters(signature), Vec::<String>::new());
}

#[test]
fn a_type_parameter_and_a_where_bound_are_shown_as_written() {
    let source =
        "import json\nimport map\nfn main() {\n  json.parse(\"1\", Int)\n  map.get(#{}, 1)\n}\n";
    let help = signature_help("json_parse", source, 3, 13);
    let signature = &help["signatures"][0];
    assert_eq!(
        signature["label"],
        "fn json.parse(s: String, type a) -> Result(a, JsonError)"
    );
    assert_eq!(parameters(signature), ["s: String", "type a"]);
    let help = signature_help("map_get", source, 4, 10);
    let signature = &help["signatures"][0];
    assert_eq!(
        signature["label"],
        "fn map.get(m: Map(a, b), k: a) -> Option(b) where a: Hash"
    );
    assert_eq!(parameters(signature), ["m: Map(a, b)", "k: a"]);
}

/// Completion after `time.` shows each function's type; a builtin record
/// type is named there as a signature names it, `Duration`, not spelled
/// out with its fields (`Duration {ns: Int}`).
#[test]
fn completion_detail_names_a_builtin_record_by_its_name() {
    let source = "import time\nfn main() {\n  time.\n}\n";
    let mut client = LspClient::spawn();
    let uri = format!(
        "file:///tmp/silt_builtin_completion_{}.silt",
        std::process::id()
    );
    client.did_open_and_wait(&uri, source);
    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 7 }
        }),
    );
    client.shutdown();
    let result = &resp["result"];
    let items = result
        .as_array()
        .or_else(|| result["items"].as_array())
        .expect("completion items");
    let detail = |label: &str| {
        items
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("`{label}` is offered after `time.`"))["detail"]
            .clone()
    };
    assert_eq!(detail("sleep"), "Fn(Duration) -> ()");
    assert_eq!(detail("datetime"), "Fn(Date, Time) -> DateTime");
}

/// `postgres.` on a build without the postgres feature offers nothing:
/// the module's import is the error, and the methods of every type are
/// not its members.
#[cfg(not(feature = "postgres"))]
#[test]
fn completion_after_a_module_that_is_not_built_offers_nothing() {
    let source = "import postgres\nfn main() {\n  postgres.\n}\n";
    let mut client = LspClient::spawn();
    let uri = format!(
        "file:///tmp/silt_builtin_completion_off_{}.silt",
        std::process::id()
    );
    client.did_open_and_wait(&uri, source);
    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 11 }
        }),
    );
    client.shutdown();
    let result = &resp["result"];
    let items = result
        .as_array()
        .or_else(|| result["items"].as_array())
        .expect("completion items");
    assert!(items.is_empty(), "{items:?}");
}
