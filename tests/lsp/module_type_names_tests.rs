//! A program's own `type Regex` beside `regex.Regex`: where the file gives
//! the bare name to its own type, the editor writes the builtin type with
//! its module, in inlay hints and completion details as in hover (owner
//! card TYP-1).

use serde_json::{Value, json};

use crate::support::LspClient;

const SRC: &str = "\
import regex

type Regex { n: Int }

fn both(mine: Regex, compiled: regex.Regex) -> Int { mine.n }

fn main() -> Result((), regex.RegexError) {
  let c = regex.compile(\"a\")?
  println(both(Regex { n: 1 }, c))

  Ok(())
}
";

#[test]
fn inlay_hints_and_completion_write_the_builtin_regex_with_its_module() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_module_type_names_regex.silt";
    client.did_open_and_wait(file, SRC);

    let hints = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": file },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 20, "character": 0 }
            }
        }),
    );
    let labels: Vec<String> = (hints.get("result").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(|h| h.get("label").and_then(Value::as_str).map(String::from))
        .collect();
    assert!(
        labels.iter().any(|l| l == ": regex.Regex"),
        "the hint of `let c` names the regex module's type: {labels:?}"
    );

    // The blank line in `main`, where `c` and `both` are in scope.
    let completion = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 9, "character": 2 }
        }),
    );
    let items = completion
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let detail = |label: &str| {
        items
            .iter()
            .find(|item| item["label"] == label)
            .map(|item| item["detail"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(detail("c"), json!("regex.Regex"), "{completion}");
    assert_eq!(
        detail("both"),
        json!("Fn(Regex, regex.Regex) -> Int"),
        "{completion}"
    );
    client.shutdown();
}
