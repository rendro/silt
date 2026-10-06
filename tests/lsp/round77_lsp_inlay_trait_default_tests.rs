//! Round-77 LSP-L1: inlay hints inside trait default-method bodies.
//!
//! Background — `src/lsp/inlay_hints.rs::walk_decl` previously matched
//! `Decl::Fn`, `Decl::Let`, `Decl::TraitImpl`, then `_ => {}` for the
//! `Decl::Trait` arm. Default-method bodies authored inside a trait
//! declaration therefore got no inlay hints from that walk path, even
//! when typechecking succeeded — the walker never descended into them.
//!
//! The fix mirrors the `Decl::TraitImpl` arm: walk every method on the
//! trait via `collect_fn_hints`, which in turn descends into the body
//! and emits a `: <type>` hint for every `let x = expr` whose author
//! omitted the type ascription.
//!
//! This test drives the actual LSP server end-to-end: it spawns the
//! `silt lsp` subprocess, opens a document containing a trait with a
//! default-bodied method whose body has an unannotated let-binding,
//! requests `textDocument/inlayHint`, and asserts the rendered hint
//! label is `: Int` and its position pins to the trait body's source.
//! Asserting on the rendered label/position (not just a non-empty list
//! from `walk_decl`) avoids the weak-gate pattern flagged in the audit
//! guide.
//!
//! The typechecker checks a default body once, in its trait, so the
//! trait declaration's own body carries the types and the `Decl::Trait`
//! arm renders the hint. An impl that leaves the method out gets a copy
//! of the body for the compiler; the copy carries no types, so the user
//! sees one `: Int` at the trait's `let x = 1`, with or without an impl.

use serde_json::json;

use crate::support::LspClient;

#[test]
fn inlay_hints_emitted_for_trait_default_method_let_binding() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_round77_lsp_inlay_trait_default.silt";
    // Trait `Foo` with a default-bodied method `bar` that contains an
    // unannotated `let x = 1` — the round-77 LSP-L1 audit example,
    // paired with a record and a bare impl, which gets a copy of the
    // default body. The rendered hint's `position` pins it to the trait
    // body's *source* location — that's the user-visible `: Int`
    // annotation the audit calls out.
    let src = "\
type Item { name: String }
trait Foo {
  fn bar(self) -> Int {
    let x = 1
    x + 1
  }
}
trait Foo for Item {}
fn main() { 0 }
";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": file },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 20, "character": 0 }
            }
        }),
    );

    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let labels: Vec<String> = arr
        .iter()
        .filter_map(|h| h.get("label").and_then(|l| l.as_str()).map(String::from))
        .collect();

    // The `let x = 1` inside the trait's default method body must
    // render a `: Int` hint. Pre-fix, `walk_decl` skipped `Decl::Trait`
    // entirely so this hint never appeared.
    assert!(
        labels.iter().any(|l| l == ": Int"),
        "expected `: Int` hint for `let x = 1` inside trait default method body; got {labels:?}"
    );

    // Pinpoint the hint's position so a future regression that swaps
    // the trait-walk for a sibling decl's hint (e.g. accidentally
    // hinting somewhere outside the trait) is caught. The trait body's
    // `let x = 1` sits on line 3 (0-indexed); the hint is rendered
    // immediately after the `x` ident at character 9 (`    let x` =
    // 4 spaces + `let ` + `x` ⇒ char 9).
    let int_hint = arr
        .iter()
        .find(|h| h.get("label").and_then(|l| l.as_str()) == Some(": Int"))
        .expect("`: Int` hint must exist");
    let pos = int_hint.get("position").expect("hint has a position");
    // The `let x = 1` sits on line 3 of `src` (zero-indexed): line 0
    // is the type decl, 1 is `trait Foo {`, 2 is `  fn bar(self) ->
    // Int {`, 3 is `    let x = 1`. The rendered hint sits right after
    // the `x` ident at column 9 (`    let x` ⇒ 4 spaces + `let ` + `x`
    // = 9 chars; UTF-8 only, so UTF-16 column matches).
    assert_eq!(
        pos.get("line").and_then(|l| l.as_u64()),
        Some(3),
        "hint must render at the trait body's source line (line 3); got {int_hint:?}"
    );
    assert_eq!(
        pos.get("character").and_then(|l| l.as_u64()),
        Some(9),
        "hint must render right after `x` ident at column 9; got {int_hint:?}"
    );

    // Lock dedup: there is exactly one `: Int` hint at this position.
    // The impl's copy of the default body shares the trait body's
    // spans. Were it typed too, both walks would emit a hint at this
    // exact position. Lock the count at 1.
    let dup_count = arr
        .iter()
        .filter(|h| {
            let p = h.get("position");
            let line = p.and_then(|p| p.get("line")).and_then(|l| l.as_u64());
            let ch = p.and_then(|p| p.get("character")).and_then(|c| c.as_u64());
            let lbl = h.get("label").and_then(|l| l.as_str());
            line == Some(3) && ch == Some(9) && lbl == Some(": Int")
        })
        .count();
    assert_eq!(
        dup_count, 1,
        "expected exactly one `: Int` hint at the let-binding position; got {dup_count} — full hints: {arr:?}"
    );

    client.shutdown();
}

/// Round-77 LSP-L1 (TEST-T2 strengthening): the hint in a default body
/// comes from the `Decl::Trait` arm of `walk_decl`, whether or not an
/// impl copies the body.
///
/// Three programs are probed:
///   1. `signature_only`:  trait with abstract method (no body): no
///                         hint.
///   2. `default_body`:    trait carrying a default body with an
///                         unannotated `let x = 1`, and no impl: one
///                         hint, which only the `Decl::Trait` arm can
///                         emit.
///   3. `paired_with_impl`: same trait body + `trait Foo for Item {}`:
///                         still one hint, not one for the trait and
///                         one for the impl's copy.
#[test]
fn inlay_hints_standalone_trait_default_body_attribution() {
    // Program 1: signature-only trait. No body, no hint.
    let signature_only_src = "\
trait Foo {
  fn bar(self) -> Int
}
fn main() -> Int { 0 }
";
    // Program 2: standalone trait with a default body. NO impl.
    // Any `: Int` hint at line 3 col 9 must come from the
    // `Decl::Trait` arm of `walk_decl` (the impl-clone path is
    // unreachable — there is no impl to clone into).
    let default_body_src = "\
trait Foo {
  fn bar(self) -> Int {
    let x = 1
    x + 1
  }
}
fn main() -> Int { 0 }
";
    // Program 3: trait body paired with a bare impl, matching the
    // existing test's setup.
    let paired_src = "\
type Item { name: String }
trait Foo {
  fn bar(self) -> Int {
    let x = 1
    x + 1
  }
}
trait Foo for Item {}
fn main() { 0 }
";

    let signature_only_count = collect_int_hints_at_let_x(
        "file:///tmp/silt_round77_lsp_inlay_trait_default_sigonly.silt",
        signature_only_src,
    );
    let default_body_count = collect_int_hints_at_let_x(
        "file:///tmp/silt_round77_lsp_inlay_trait_default_only.silt",
        default_body_src,
    );
    let paired_count = collect_int_hints_at_let_x(
        "file:///tmp/silt_round77_lsp_inlay_trait_default_paired.silt",
        paired_src,
    );

    // Hard lock 1: signature-only produces zero hints. There is no
    // `let x = 1` to hint anywhere in this program. Reverting the
    // `Decl::Trait` arm cannot alter this — there is no body to walk.
    assert_eq!(
        signature_only_count, 0,
        "signature-only trait must not produce a `: Int` hint at the (nonexistent) let-binding position; got {signature_only_count}"
    );

    // The default body is typed where it is written: exactly one hint,
    // with an impl (whose copy of the body adds none) and without one.
    assert_eq!(
        default_body_count, 1,
        "a standalone trait's default body must produce exactly one `: Int` hint at its let-binding position; got {default_body_count}"
    );
    assert_eq!(
        paired_count, 1,
        "paired (trait + impl) must produce exactly one `: Int` hint at the trait body's let-binding position; got {paired_count}"
    );
}

/// Helper: open `src` over a fresh LSP session, request inlay hints
/// across the whole document, and count `: Int` hints whose position
/// pins to a let-binding-shaped position inside a trait or impl body
/// — line 2 column 9 (`    let x = 1` → 4 spaces + `let ` + `x` →
/// char 9) for the standalone variant, or line 3 column 9 for the
/// paired variant whose first line is the `type Item` decl. Each
/// invocation spawns and shuts down its own LSP client so the probes
/// in `inlay_hints_standalone_trait_default_body_attribution` are
/// fully independent.
fn collect_int_hints_at_let_x(uri: &str, src: &str) -> usize {
    let mut client = LspClient::spawn();
    client.did_open_and_wait(uri, src);
    let resp = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 20, "character": 0 }
            }
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    // The let-binding line varies by program shape (lines count from
    // 0): the paired variant prefixes a `type Item` decl, so its
    // `let x = 1` sits on line 3; the standalone variant puts it on
    // line 2 (the signature-only case has no let at all).
    // Count `: Int` hints at column 9 across both candidate lines —
    // any one of them at the let-binding position counts.
    let count = arr
        .iter()
        .filter(|h| {
            let p = h.get("position");
            let line = p.and_then(|p| p.get("line")).and_then(|l| l.as_u64());
            let ch = p.and_then(|p| p.get("character")).and_then(|c| c.as_u64());
            let lbl = h.get("label").and_then(|l| l.as_str());
            (line == Some(2) || line == Some(3)) && ch == Some(9) && lbl == Some(": Int")
        })
        .count();
    client.shutdown();
    count
}

/// Hover inside a default method body answers from the trait's own
/// body, which is where the body is typed: with an impl that leaves the
/// method out (and gets an untyped copy of it), and without one.
#[test]
fn hover_inside_trait_default_method_body() {
    let body = "\
trait Loud {
  fn loud(self) -> String
  fn twice(self) -> String {
    let me = self
    \"{me.loud()}{me.loud()}\"
  }
}
";
    let with_impl = format!(
        "{body}trait Loud for Int {{ fn loud(self) -> String {{ \"i\" }} }}\nfn main() {{ println(1.twice()) }}\n"
    );
    let without_impl = format!("{body}fn main() {{ println(1) }}\n");
    for (name, src) in [("with_impl", with_impl), ("without_impl", without_impl)] {
        let mut client = LspClient::spawn();
        let uri = format!("file:///tmp/silt_hover_trait_default_{name}.silt");
        client.did_open_and_wait(&uri, &src);
        // `me` in the interpolation, and `self` in the `let`.
        for (line, character) in [(4, 6), (3, 13)] {
            let hover = client.hover(&uri, line, character);
            let shown = hover.to_string();
            assert!(
                shown.contains("Self"),
                "{name}: hover at {line}:{character} inside the default body shows the type \
                 `Self`; got {shown}"
            );
        }
        client.shutdown();
    }
}
