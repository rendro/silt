//! What the server answers does not depend on the order its documents
//! were opened in, or on the order a hash map walks them in: a list
//! over several documents is in the order of their URIs, a document's
//! own names are completed in the order of the names, and a name the
//! document does not bind is no record of another module.

use serde_json::{Value, json};

use crate::support::LspClient;

/// The names of the documents, in the order they are opened.
const OPENED: [&str; 8] = ["h", "c", "f", "a", "e", "b", "g", "d"];

fn labels(response: &Value) -> Vec<String> {
    let result = response.get("result").expect("a completion result");
    let items = match result {
        Value::Array(items) => items,
        other => other
            .get("items")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("unexpected completion result: {result}")),
    };
    items
        .iter()
        .filter_map(|item| item.get("label").and_then(|v| v.as_str()))
        .map(String::from)
        .collect()
}

/// The impls of a trait in eight documents come back in the order of
/// the documents' URIs.
#[test]
fn implementations_are_listed_in_the_order_of_their_uris() {
    let mut client = LspClient::spawn();
    let uri = |name: &str| format!("file:///tmp/silt_fixed_order_impl/{name}.silt");
    client.did_open_and_wait(&uri("shown"), "pub trait Shown { fn m(self) -> Int }\n");
    for name in OPENED {
        let ty = name.to_uppercase();
        client.did_open_and_wait(
            &uri(name),
            &format!(
                "import shown.{{ Shown }}\n\
                 type {ty} {{ n: Int }}\n\
                 trait Shown for {ty} {{ fn m(self) -> Int {{ self.n }} }}\n"
            ),
        );
    }

    // On `Shown` of `trait Shown for D`.
    let resp = client.request(
        "textDocument/implementation",
        json!({
            "textDocument": { "uri": uri("d") },
            "position": { "line": 2, "character": 8 }
        }),
    );
    let found: Vec<String> = resp
        .get("result")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| panic!("expected a list of implementations; got {resp}"))
        .iter()
        .filter_map(|loc| loc.get("uri").and_then(|v| v.as_str()))
        .map(String::from)
        .collect();
    let mut sorted: Vec<String> = OPENED.iter().map(|name| uri(name)).collect();
    sorted.sort();
    assert_eq!(found, sorted);
    client.shutdown();
}

/// A document's own definitions are offered in the order of their
/// names.
#[test]
fn own_definitions_are_completed_in_the_order_of_their_names() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_fixed_order_own.silt";
    let mut text = String::new();
    for name in OPENED {
        text.push_str(&format!("fn {name}_own() {{ 1 }}\n"));
    }
    text.push_str("fn main() {\n  \n}\n");
    client.did_open_and_wait(uri, &text);

    let resp = client.completion(uri, OPENED.len() as u32 + 1, 2);
    let own: Vec<String> = labels(&resp)
        .into_iter()
        .filter(|label| label.ends_with("_own"))
        .collect();
    let mut sorted: Vec<String> = OPENED.iter().map(|name| format!("{name}_own")).collect();
    sorted.sort();
    assert_eq!(own, sorted);
    client.shutdown();
}

/// A name before the dot that the document does not bind offers no
/// field: not those of a record of that name in another module of the
/// session, whichever a walk over the session's records met first.
#[test]
fn a_name_the_document_does_not_bind_offers_no_fields() {
    let mut client = LspClient::spawn();
    let uri = |name: &str| format!("file:///tmp/silt_fixed_order_record/{name}.silt");
    for name in OPENED {
        client.did_open_and_wait(
            &uri(name),
            &format!("pub type Point {{ {name}_field: Int }}\n"),
        );
    }
    client.did_open_and_wait(&uri("user"), "fn use_it() {\n  Point.\n}\n");

    let resp = client.completion(&uri("user"), 1, 8);
    let fields: Vec<String> = labels(&resp)
        .into_iter()
        .filter(|label| label.ends_with("_field"))
        .collect();
    assert_eq!(fields, Vec::<String>::new());
    client.shutdown();
}
