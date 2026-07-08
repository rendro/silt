//! Citation-resolve lock for `src/typechecker/mod.rs:<N>` references
//! that live in *other* files' doc-comments.
//!
//! Doc-comments in `src/typechecker/inference.rs` and `src/vm/dispatch.rs`
//! point at specific lines in `src/typechecker/mod.rs` to explain why a
//! dispatch arm is reached / how a canonical name is produced. Those line
//! numbers drift silently whenever `mod.rs` is edited, leaving a comment
//! that cites a function that does not exist (round-95-follow-up finding:
//! the comment named `type_name_for_method_dispatch`, which never existed
//! — the real mapper is `type_name_for_impl`) or that lands on unrelated
//! code (the `register_auto_derived_impls_for` cite pointed at an
//! arity-check block; the List/Unit Compare cites pointed at `EffectSet`
//! comment lines).
//!
//! This test pins each citation to the *named item* the prose claims lives
//! there. If `mod.rs` is reorganised so a cited line no longer contains the
//! expected item, this test fails and the comment must be re-synced.
//!
//! Mirrors the spirit of the round-94 dead-arm prose grep-lock
//! (`tests/auto_derive_dead_arm_proof_tests.rs::dead_arm_prose_does_not_regress`),
//! but instead of forbidding a stale phrase it *resolves* each line cite.

use std::collections::BTreeSet;

const MOD_RS: &str = include_str!("../src/typechecker/mod.rs");
const INFERENCE_RS: &str = include_str!("../src/typechecker/inference.rs");
const DISPATCH_RS: &str = include_str!("../src/vm/dispatch.rs");

/// Return the 1-based line `n`'s text from `src`, or panic with context.
fn line(src: &str, n: usize) -> &str {
    src.lines()
        .nth(n - 1)
        .unwrap_or_else(|| panic!("src/typechecker/mod.rs has no line {n}"))
}

/// Collect every `src/typechecker/mod.rs:<N>` line number cited in `src`.
fn cited_mod_lines(src: &str) -> BTreeSet<usize> {
    const NEEDLE: &str = "src/typechecker/mod.rs:";
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(pos) = rest.find(NEEDLE) {
        let after = &rest[pos + NEEDLE.len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<usize>() {
            out.insert(n);
        }
        rest = &after[digits.len().max(1)..];
    }
    out
}

/// Assert the line cited as `n` in `mod.rs` contains `expected`, the named
/// item the citing prose claims lives there.
fn assert_cite(n: usize, expected: &str, citing_file: &str) {
    let text = line(MOD_RS, n);
    assert!(
        text.contains(expected),
        "stale citation: a comment in {citing_file} cites \
         `src/typechecker/mod.rs:{n}` for `{expected}`, but that line reads:\n  {text}\n\
         The comment's line number has drifted — re-sync it (find the line \
         actually containing `{expected}`)."
    );
}

/// Every `src/typechecker/mod.rs:<N>` cite in `inference.rs`/`dispatch.rs`
/// must (a) be one of the cites we have an expectation for, and (b) land on
/// a line containing the named item the prose attributes to it.
///
/// The expectation set is closed: if a NEW `mod.rs:<N>` cite is added to
/// either file and not registered here, the "unexpected citation" guard
/// below fails, forcing the author to anchor it to a named item too.
#[test]
fn mod_rs_citations_in_inference_and_dispatch_resolve() {
    // (line, named-item-on-that-line) the prose claims. The negative
    // function `type_name_for_method_dispatch` does not appear here on
    // purpose — it never existed; the real mapper is `type_name_for_impl`.
    let expectations: &[(usize, &str)] = &[
        // inference.rs primitive-dispatch comment block.
        (8112, "fn register_auto_derived_impls_for"),
        (2081, r#"Type::Channel(_) => Some(intern("Channel"))"#),
        (2092, r#"Type::Fun(_, _) => Some(intern("Fn"))"#),
        // dispatch.rs compare-arm comment block.
        (
            8012,
            r#"register_auto_derived_impls_for(checker, &["List"]"#,
        ),
        (8009, r#""Unit""#),
    ];

    let mut expected_lines: BTreeSet<usize> = BTreeSet::new();
    for &(n, item) in expectations {
        expected_lines.insert(n);
        // Pick the citing file for a precise failure message.
        let citing = if cited_mod_lines(INFERENCE_RS).contains(&n) {
            "src/typechecker/inference.rs"
        } else {
            "src/vm/dispatch.rs"
        };
        assert_cite(n, item, citing);
    }

    // Closed-world guard: no un-anchored cite may slip in.
    let mut actual: BTreeSet<usize> = cited_mod_lines(INFERENCE_RS);
    actual.extend(cited_mod_lines(DISPATCH_RS));
    let unexpected: Vec<usize> = actual.difference(&expected_lines).copied().collect();
    assert!(
        unexpected.is_empty(),
        "new `src/typechecker/mod.rs:<N>` citation(s) {unexpected:?} added to \
         inference.rs/dispatch.rs without registering the named item they \
         point at. Add an (line, item) entry to `expectations` so the cite \
         is pinned and cannot silently drift."
    );

    // Sanity: the dead reference must stay dead. If anyone reintroduces the
    // phantom `type_name_for_method_dispatch`, fail loudly.
    assert!(
        !INFERENCE_RS.contains("type_name_for_method_dispatch"),
        "the phantom function `type_name_for_method_dispatch` reappeared in \
         src/typechecker/inference.rs — it does not exist; the real canonical \
         name mapper is `type_name_for_impl` (src/typechecker/mod.rs:2063)."
    );
    assert!(
        !DISPATCH_RS.contains("type_name_for_method_dispatch"),
        "the phantom function `type_name_for_method_dispatch` reappeared in \
         src/vm/dispatch.rs."
    );
    // And it must actually exist under its real name.
    assert!(
        MOD_RS.contains("fn type_name_for_impl"),
        "expected `fn type_name_for_impl` in src/typechecker/mod.rs"
    );
}
