//! docs/language/error-codes.md lists every diagnostic code once, the
//! errors' and the warnings', and no code that silt does not have.

use std::collections::BTreeMap;

use silt::diagnostic::Code;

#[test]
fn the_error_codes_page_lists_every_code_once() {
    let page = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("language")
            .join("error-codes.md"),
    )
    .expect("docs/language/error-codes.md is read");
    // The rows of the tables: "| `E0301` | ... |", "| `W0201` | ... |".
    let mut listed: BTreeMap<String, usize> = BTreeMap::new();
    for line in page.lines() {
        if let Some(rest) = line.strip_prefix("| `")
            && rest.starts_with(['E', 'W'])
            && let Some((id, _)) = rest.split_once('`')
        {
            *listed.entry(id.to_string()).or_default() += 1;
        }
    }
    let codes: Vec<&str> = Code::ALL.iter().map(|code| code.id()).collect();
    let missing: Vec<&&str> = codes
        .iter()
        .filter(|id| !listed.contains_key(**id))
        .collect();
    let unknown: Vec<&String> = listed
        .keys()
        .filter(|id| !codes.contains(&id.as_str()))
        .collect();
    let twice: Vec<&String> = listed
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(id, _)| id)
        .collect();
    assert!(
        missing.is_empty() && unknown.is_empty() && twice.is_empty(),
        "docs/language/error-codes.md: missing {missing:?}, not codes of silt {unknown:?}, \
         listed twice {twice:?}"
    );
}
