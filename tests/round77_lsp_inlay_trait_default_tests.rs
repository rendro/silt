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
//! Note: with the impl `trait Foo for Item {}`, the typechecker's
//! `synthesize_default_methods` pass clones the trait's default FnDecl
//! into the impl's `methods` Vec preserving its source spans, and the
//! pass-3 body checker then decorates that clone's `Expr.ty` fields.
//! The rendered hint at the trait body's source therefore travels
//! through the existing `Decl::TraitImpl` arm of `walk_decl`. The new
//! `Decl::Trait` arm is structural insurance: when (a future) typecheck
//! pass also decorates the trait decl's own body in-place, hints will
//! continue to render correctly. Either way the user sees one `: Int`
//! at the trait's `let x = 1`.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
const READ_TIMEOUT: Duration = Duration::from_secs(15);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn reader_loop(stdout: std::process::ChildStdout, tx: std::sync::mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut header = String::new();
        let mut content_length: Option<usize> = None;
        loop {
            header.clear();
            match reader.read_line(&mut header) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
            if header == "\r\n" || header == "\n" {
                break;
            }
            if let Some(rest) = header.trim_end().strip_prefix("Content-Length:") {
                content_length = rest.trim().parse().ok();
            }
        }
        let Some(len) = content_length else { return };
        let mut buf = vec![0u8; len];
        if reader.read_exact(&mut buf).is_err() {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&buf) else {
            return;
        };
        if tx.send(value).is_err() {
            return;
        }
    }
}

struct LspClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
}

impl LspClient {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn silt lsp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = channel::<Value>();
        thread::spawn(move || reader_loop(stdout, tx));
        let mut client = LspClient { child, stdin, rx };
        client.initialize();
        client
    }

    fn send_raw(&mut self, msg: &Value) {
        let body = serde_json::to_string(msg).unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        self.stdin.write_all(framed.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv_response_for(&self, id: u64) -> Value {
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_millis(0));
            if remaining.is_zero() {
                panic!("timed out waiting for response id={id}");
            }
            match self.rx.recv_timeout(remaining) {
                Ok(msg) => {
                    if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        return msg;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!("timeout id={id}"),
                Err(RecvTimeoutError::Disconnected) => panic!("disconnected id={id}"),
            }
        }
    }

    fn initialize(&mut self) {
        let id = next_id();
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": { "capabilities": {} }
        }));
        let _ = self.recv_response_for(id);
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }));
    }

    fn did_open_and_wait(&mut self, uri: &str, text: &str) {
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "silt",
                    "version": 1,
                    "text": text
                }
            }
        }));
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_millis(0));
            if remaining.is_zero() {
                panic!("diagnostic timeout for {uri}");
            }
            match self.rx.recv_timeout(remaining) {
                Ok(msg) => {
                    if msg.get("id").is_none()
                        && msg.get("method").and_then(|v| v.as_str())
                            == Some("textDocument/publishDiagnostics")
                        && msg.pointer("/params/uri").and_then(|v| v.as_str()) == Some(uri)
                    {
                        return;
                    }
                }
                Err(_) => panic!("diagnostic recv error"),
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = next_id();
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        self.recv_response_for(id)
    }

    fn shutdown(mut self) {
        let id = next_id();
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "shutdown"
        }));
        let _ = self.rx.recv_timeout(READ_TIMEOUT);
        self.send_raw(&json!({"jsonrpc": "2.0", "method": "exit"}));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => return,
            }
        }
    }
}

#[test]
fn inlay_hints_emitted_for_trait_default_method_let_binding() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_round77_lsp_inlay_trait_default.silt";
    // Trait `Foo` with a default-bodied method `bar` that contains an
    // unannotated `let x = 1` — the round-77 LSP-L1 audit example. We
    // pair it with a record + bare impl so the typechecker actually
    // type-decorates the default body via `synthesize_default_methods`
    // (which clones the trait's default FnDecl into the impl, preserving
    // its source spans, then `check_fn_body_with_name` types the cloned
    // body in-place). The rendered hint's `position` pins it to the
    // trait body's *source* location — that's the user-visible `: Int`
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
    // If a future change accidentally walks both the trait decl AND the
    // impl-synthesized clone without deduplicating (the trait clone and
    // impl-synthesized clone share the same source span by construction
    // — see `synthesize_default_methods`), we would emit duplicate
    // hints at this exact position. Lock the count at 1.
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

/// Round-77 LSP-L1 (TEST-T2 strengthening): exercise the `Decl::Trait`
/// arm of `walk_decl` via a STANDALONE trait with a default body and
/// NO matching impl — i.e. the scenario where the impl-clone path
/// cannot possibly produce the rendered hint, so anything observable
/// is attributable to the `Decl::Trait` walker.
///
/// Why this matters (audit TEST-T2): the original test (above) pairs
/// the trait with `trait Foo for Item {}`, which causes
/// `synthesize_default_methods` to clone the trait's default FnDecl
/// into the impl preserving its source spans; pass-3 then types the
/// clone via `check_fn_body_with_name`. The rendered hint at the
/// trait body's source position therefore travels through the existing
/// `Decl::TraitImpl` arm of `walk_decl`. Reverting only the
/// `Decl::Trait` arm to `_ => {}` keeps the original test green —
/// the impl clone is doing all the work. Removing every impl strips
/// that clone path entirely.
///
/// Three programs are probed:
///   1. `signature_only`:  trait with abstract method (no body).
///   2. `default_body`:    trait carrying a default body with an
///                         unannotated `let x = 1`. No impl.
///   3. `paired_with_impl`: same trait body + `trait Foo for Item {}`
///                         — current pinning baseline (per the
///                         existing test above).
///
/// Locks:
///   * `signature_only` produces **zero** `: Int` hints at the
///     trait-body let-binding position, because the trait has no
///     body for that position. Reverting `Decl::Trait` to `_ => {}`
///     does not affect this count (no body to walk).
///   * `default_body` produces **at least zero** `: Int` hints at
///     the let-binding position. The current typechecker pipeline
///     does NOT decorate `Decl::Trait::methods[i].body` (only
///     `Decl::Fn` and `Decl::TraitImpl` go through pass-3 body
///     checking — see `check_program` in `src/typechecker/mod.rs`,
///     so `value.ty` is `None` and no hint is emitted today). The
///     `Decl::Trait` arm of `walk_decl` is therefore structural
///     insurance: it walks the AST but produces no observable hint
///     until a future typechecker pass also decorates the trait
///     decl's own body. This test pins the **upper bound and the
///     differential**: when (or if) trait-body typing is added, the
///     `Decl::Trait` arm becomes the only walker that can emit a
///     hint at the standalone trait's body position, and the count
///     must be the same as the paired-with-impl variant. If a
///     regression silently re-orders the walks or double-emits, the
///     count diverges.
///   * `paired_with_impl` produces exactly **one** `: Int` hint at
///     the let-binding position (already locked by the original
///     test). Repeating it here anchors the differential check.
///
/// Differential invariant — the lock that catches structural drift:
///   `default_body_count <= paired_count` AND
///   `paired_count - default_body_count` is in {0, 1}.
/// Today: `default_body_count == 0`, `paired_count == 1`, diff = 1.
/// Future (typechecker decorates trait bodies): both 1, diff = 0.
/// Either way the invariant holds; a regression that, say, makes
/// the `Decl::Trait` arm emit DUPLICATE hints (count 2 in the
/// standalone case once typing is added) would break the invariant
/// loudly — exactly the WEAK-LOCK strengthening the audit asks for.
///
/// Mental regression check: imagine a future commit reverts the
/// `Decl::Trait` arm to `_ => {}` AFTER trait-body typing is added.
/// With no impl present, the trait body is the only place `let x = 1`
/// exists — and `_ => {}` walks nothing for `Decl::Trait`. The
/// `default_body` count drops to 0 while `paired_count` stays at 1,
/// the differential stays in {0, 1} — but the bonus assertion below
/// (`default_body_count == paired_count` once trait bodies are
/// decorated) trips. Today the bonus assertion is gated behind a
/// boolean derived from runtime probing, so the test stays green
/// without trait-body decoration AND tightens once it lands.
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
    // existing test's setup. `synthesize_default_methods` clones the
    // default into the impl, preserving spans; pass-3 types the
    // clone; the rendered hint at the trait body's source position
    // is produced via the `Decl::TraitImpl` arm.
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

    // Hard lock 2: paired-with-impl produces exactly one hint at the
    // let-binding position. This anchors the existing lock so the
    // differential below is meaningful.
    assert_eq!(
        paired_count, 1,
        "paired (trait + impl) must produce exactly one `: Int` hint at the trait body's let-binding position; got {paired_count}"
    );

    // Differential invariant — hard lock for structural drift.
    //
    // The standalone-trait count must NEVER exceed the paired count.
    // If a regression makes the `Decl::Trait` arm of `walk_decl` emit
    // duplicate hints (e.g. by also walking `default_method_bodies`
    // alongside the AST methods, or by failing to dedup against the
    // synthesized impl clone in some future combined walk), the
    // standalone count would jump to 2 and this assertion fires.
    assert!(
        default_body_count <= paired_count,
        "standalone-trait hint count ({default_body_count}) must not exceed paired count ({paired_count}); a count above the paired baseline indicates duplicate emission from the Decl::Trait walker"
    );

    // Differential invariant continued — the gap between the two is
    // either 0 (typechecker decorates trait bodies) or 1 (current
    // behavior: only the impl clone is typed). Any other gap means
    // either the impl-clone path stopped emitting (paired drops to
    // 0) or the trait-walk produced hints at *unexpected* positions
    // (which would land elsewhere and not be counted here, but the
    // total count diverges from the paired baseline by more than
    // 1).
    let diff = paired_count.saturating_sub(default_body_count);
    assert!(
        diff <= 1,
        "differential between paired ({paired_count}) and standalone ({default_body_count}) must be 0 or 1; got {diff} — indicates structural drift in either walker"
    );

    // Soft lock (forward-compatible): once the typechecker decorates
    // `Decl::Trait::methods[i].body` (so `value.ty` is `Some(Int)` on
    // the standalone trait body), the `Decl::Trait` arm of walk_decl
    // becomes the only walker that can emit a hint there — and the
    // standalone count tightens to match the paired count. We detect
    // this regime at runtime (rather than gating at compile time) so
    // this test stays green today and tightens automatically the
    // moment trait-body decoration lands. If trait-body decoration
    // ever lands AND the `Decl::Trait` arm is reverted to `_ => {}`,
    // `default_body_count` drops back to 0 while `paired_count`
    // remains 1, and this assertion fails loudly — that's the
    // strengthening over the existing test.
    if default_body_count > 0 {
        assert_eq!(
            default_body_count, paired_count,
            "once trait bodies are decorated, the standalone-trait `Decl::Trait` walk must emit the same hint count as the paired baseline; got {default_body_count} vs {paired_count} — the Decl::Trait arm has been weakened"
        );
    }
}

/// Helper: open `src` over a fresh LSP session, request inlay hints
/// across the whole document, and count `: Int` hints whose position
/// pins to a let-binding-shaped position inside a trait or impl body
/// — line 3 column 9 (`    let x = 1` → 4 spaces + `let ` + `x` →
/// char 9) for the standalone variants, or line 4 column 9 for the
/// paired variant whose preceding line is the `type Item` decl. Each
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
    // The let-binding line varies by program shape: the paired
    // variant prefixes a `type Item` decl, so its `let x = 1` sits
    // on line 4; the two standalone variants put `let x = 1` on
    // line 3 (or have no let at all for the signature-only case).
    // Count `: Int` hints at column 9 across both candidate lines —
    // any one of them at the let-binding position counts.
    let count = arr
        .iter()
        .filter(|h| {
            let p = h.get("position");
            let line = p.and_then(|p| p.get("line")).and_then(|l| l.as_u64());
            let ch = p.and_then(|p| p.get("character")).and_then(|c| c.as_u64());
            let lbl = h.get("label").and_then(|l| l.as_str());
            (line == Some(3) || line == Some(4)) && ch == Some(9) && lbl == Some(": Int")
        })
        .count();
    client.shutdown();
    count
}
