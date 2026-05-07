//! Round-75 G5 — parity lock between `GLOBAL_FLAGS` and `run_main` arms.
//!
//! `src/cli/help.rs:101-102` declares the recognized top-level flag
//! tokens as `GLOBAL_FLAGS` and the doc-comment (lines 97-100) cites
//! "Keep in sync with the match arms in `run_main`". The arms live in
//! `src/main.rs:63-100` (specifically the `--version | -V | -v`,
//! `--help | -h`, and `help` arms before subcommand routing kicks in).
//!
//! Pre-round-75, no test enforced that the two sets stayed in sync.
//! Adding a new short alias to one match arm without updating the
//! const would silently break the typo-suggestion path
//! (`silt --hlpe` would not surface `--help` as a suggestion if `-h`
//! was the only entry in `GLOBAL_FLAGS`). Adding a new entry to the
//! const without a match arm would advertise a token the binary then
//! routes through the unknown-command arm — equally surprising.
//!
//! Lock approach: source-include both files via `include_str!`,
//! regex-extract the flag-shaped tokens from each, assert set
//! equality. This pattern mirrors the sibling parity tests under
//! `tests/*_parity_tests.rs` (e.g. `keyword_list_parity_tests.rs`).

use std::collections::BTreeSet;

const HELP_RS: &str = include_str!("../src/cli/help.rs");
const MAIN_RS: &str = include_str!("../src/main.rs");

/// Extract the entries of the `GLOBAL_FLAGS` const literal in
/// `src/cli/help.rs`. The const is declared as
/// `pub(crate) const GLOBAL_FLAGS: &[&str] = &["...", "...", ...];`
/// over one or more lines. We find the literal opener and read until
/// the matching `];`, then pull all double-quoted tokens from that
/// slice.
fn extract_global_flags_const() -> BTreeSet<String> {
    // Anchor the search on the const declaration. rustfmt may wrap the
    // body across lines, so we don't require the `&[` to be glued to
    // the same line — we find the `&[` opener AFTER the `GLOBAL_FLAGS`
    // marker and read until the matching `];`.
    let anchor = "GLOBAL_FLAGS: &[&str]";
    let start = HELP_RS
        .find(anchor)
        .expect("src/cli/help.rs missing GLOBAL_FLAGS const");
    let after_anchor = &HELP_RS[start + anchor.len()..];
    let open = after_anchor
        .find("&[")
        .expect("GLOBAL_FLAGS const missing `&[` opener");
    let body_start = open + "&[".len();
    let after = &after_anchor[body_start..];
    let end = after
        .find("];")
        .expect("GLOBAL_FLAGS const missing closing `];`");
    let body = &after[..end];
    extract_quoted_tokens(body)
}

/// Pull every `"..."` literal out of a slice of source text. The
/// const body is comma-separated quoted strings, possibly with
/// embedded whitespace and line breaks; `quoted_tokens` is a
/// minimal handwritten parser that handles only the unescaped
/// case (which is all we need — none of the flag tokens contain
/// quotes or backslashes).
fn extract_quoted_tokens(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                j += 1;
            }
            if j < bytes.len() {
                out.insert(std::str::from_utf8(&bytes[start..j]).unwrap().to_string());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Extract the global-flag arm tokens from the `run_main` match in
/// `src/main.rs`. The recognized arms are:
///
///   "--version" | "-V" | "-v" => { ... }
///   "--help"    | "-h"        => { ... }
///   "help"                    => { ... }
///
/// All three appear before the subcommand arms (`"run"`, `"disasm"`,
/// etc) in the same `match args[1].as_str()` block. We bound the
/// extraction to the `match args[1].as_str() {` opener and stop at
/// the first subcommand arm marker (`"run" =>`) so we don't pick
/// up any quoted strings from later subcommand handling.
fn extract_run_main_global_arms() -> BTreeSet<String> {
    let match_start = MAIN_RS
        .find("match args[1].as_str() {")
        .expect("src/main.rs missing run_main match on args[1]");
    let after = &MAIN_RS[match_start..];
    // The first subcommand arm is `"run" => cli::run::dispatch(&args)`.
    // Everything before that line is global-flag territory.
    let stop = after
        .find("\"run\" =>")
        .expect("src/main.rs missing `\"run\" =>` arm marker");
    let region = &after[..stop];
    // The region also contains incidental `"silt {}"`-style format
    // strings used inside the arm bodies (e.g. `println!("silt {}",
    // env!("CARGO_PKG_VERSION"))`). Filter to only flag-shaped
    // tokens — anything starting with `-` is a flag, plus the literal
    // `"help"` which is the lone non-`-` global token.
    extract_quoted_tokens(region)
        .into_iter()
        .filter(|tok| tok.starts_with('-') || tok == "help")
        .collect()
}

#[test]
fn global_flags_const_matches_run_main_arms() {
    // Round-75 G5 parity lock: the GLOBAL_FLAGS const and the
    // run_main match arms must declare exactly the same set of
    // flag tokens. A drift on either side (adding an alias to the
    // match without updating the const, or vice versa) silently
    // breaks the unknown-flag suggestion path.
    let const_set = extract_global_flags_const();
    let arm_set = extract_run_main_global_arms();

    assert!(
        !const_set.is_empty(),
        "extract_global_flags_const returned empty — extractor regressed?"
    );
    assert!(
        !arm_set.is_empty(),
        "extract_run_main_global_arms returned empty — extractor regressed?"
    );

    let only_in_const: Vec<&String> = const_set.difference(&arm_set).collect();
    let only_in_arms: Vec<&String> = arm_set.difference(&const_set).collect();

    assert!(
        only_in_const.is_empty() && only_in_arms.is_empty(),
        "GLOBAL_FLAGS (src/cli/help.rs:101-102) and run_main match arms \
         (src/main.rs:63-100) disagree.\n\
         \n  Tokens only in GLOBAL_FLAGS: {only_in_const:?}\
         \n  Tokens only in run_main arms: {only_in_arms:?}\
         \n\n\
         Keep both sides in sync: every recognized flag must appear in \
         GLOBAL_FLAGS (so the typo-suggestion path can pool against it) \
         AND in a match arm (so the binary actually handles it). \
         The const's doc-comment cites `run_main` explicitly as the \
         authority to mirror."
    );
}

#[test]
fn global_flags_const_includes_known_minimum_set() {
    // Belt-and-braces: the const must always include the documented
    // minimum surface — `--version`, `-V`, `-v`, `--help`, `-h`, `help`.
    // This catches a degenerate case where both sides are emptied
    // simultaneously (the parity check above would still pass on
    // {} == {}). Listing them inline so a regression names the exact
    // missing entry in the failure message.
    let expected = ["--version", "-V", "-v", "--help", "-h", "help"];
    let const_set = extract_global_flags_const();
    for token in expected {
        assert!(
            const_set.contains(token),
            "GLOBAL_FLAGS const missing required token {token:?}. \
             Current set: {const_set:?}"
        );
    }
}
