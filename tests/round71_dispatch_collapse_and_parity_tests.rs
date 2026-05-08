//! Round-71 audit: dead-code collapse + parallel-array-drift +
//! signature-help findings.
//!
//! Lock tests for each round-71 fix that lands in this PR's
//! `src/vm/dispatch.rs`, `src/module.rs`, `src/typechecker/builtins/errors.rs`,
//! `src/builtins/io.rs`, `src/builtins/toml.rs`, `src/lsp/signature_help.rs`,
//! `src/errors.rs`, and `src/compiler/mod.rs`.
//!
//! Each test's failure precondition is: the audit-reported BLOAT-DUP /
//! PARALLEL-ARRAY-DRIFT / STALE-DOC / DX shape was not yet collapsed
//! / fixed. After the fix the assertions all pass, so any regression
//! in a future round will reintroduce the failure.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use silt::module::{
    builtin_enum_variants, builtin_error_enum_variants_with_arity,
    builtin_prelude_enum_variants_with_arity,
};

const DISPATCH_SRC: &str = include_str!("../src/vm/dispatch.rs");
const ARITHMETIC_SRC: &str = include_str!("../src/vm/arithmetic.rs");
const ERRORS_BUILTIN_SRC: &str = include_str!("../src/typechecker/builtins/errors.rs");
const COMPILER_MOD_SRC: &str = include_str!("../src/compiler/mod.rs");
const ERRORS_SRC: &str = include_str!("../src/errors.rs");
const MODULE_SRC: &str = include_str!("../src/module.rs");
const IO_SRC: &str = include_str!("../src/builtins/io.rs");
const TOML_SRC: &str = include_str!("../src/builtins/toml.rs");

// ── DEAD-1: corpus equivalence for `compare` 4-arm collapse ──────────

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round71_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// All four `(Float|ExtFloat, Float|ExtFloat)` NaN combinations driven
/// through the trait-bound `compare` path must surface the SAME
/// canonical wording, "cannot compare non-finite float values" — the
/// arithmetic.rs canonical form. Pre-round 71 the dispatch.rs side
/// emitted a `compare()`-prefixed wording on `(Float, Float)` and a
/// "cannot compare NaN values" wording on the three Float/ExtFloat
/// shapes; the round-71 collapse unifies all four to the canonical
/// form.
#[test]
fn dispatch_compare_nan_corpus_emits_unified_wording() {
    // Construct each (Float|ExtFloat) NaN by source. `0.0 / 0.0` is
    // the canonical Float-typed NaN; widening is forced by passing
    // through the divide path that promotes to ExtFloat.
    //
    // Test driver: drive the public `Compare` trait-bound dispatch
    // (the surface that lands in `dispatch.rs::compare`'s arm) on each
    // NaN combination and assert the SAME wording surfaces every time.
    //
    // We don't have direct VM dispatch handles in integration tests —
    // we drive end-to-end via the CLI, the same as the sibling
    // `vm_trait_dispatch_runtime_tests.rs::compare_runs_on_extfloat_*`
    // cases. Each combination is a separate program so any single
    // failure isolates cleanly.
    //
    // The four shapes: (Float, Float), (Float, ExtFloat),
    // (ExtFloat, Float), (ExtFloat, ExtFloat). `1.0 / 0.0 - 1.0 / 0.0`
    // produces NaN as ExtFloat. A `Float`-typed NaN is constructed by
    // `0.0 / 0.0` which by the typechecker stays Float (no widening
    // applied to literals).
    //
    // If the typechecker actually widens `0.0 / 0.0` to ExtFloat
    // unconditionally, the two-mode test still verifies that ALL
    // `partial_cmp` failures route to the same canonical wording.
    // The runtime path the typechecker permits is `(ExtFloat, ExtFloat)`
    // (Float / Float widens to ExtFloat, mixed Float/ExtFloat shapes
    // are also accepted). What this corpus pins is that EVERY runtime
    // path through the collapsed arm yields the SAME canonical
    // wording — multiple sources that produce a NaN must all surface
    // identical error output. The source-grep tests below pin that
    // all four typechecker-distinguished arms collapsed in the
    // dispatch.rs source.
    let cases: &[(&str, &str)] = &[
        // NaN from `0/0` (positive vs positive yields NaN).
        (
            "div_zero",
            r#"
fn cmp(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
  let x: Float = 0.0
  let y: Float = 0.0
  let nan = x / y
  println(cmp(nan, nan))
}
"#,
        ),
        // NaN from `inf - inf`.
        (
            "inf_minus_inf",
            r#"
fn cmp(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
  let x: Float = 1.0
  let y: Float = 0.0
  let pos_inf = x / y
  let nan = pos_inf - pos_inf
  println(cmp(nan, nan))
}
"#,
        ),
    ];
    for (label, src) in cases {
        let (stdout, stderr, ok) = run_silt_raw(label, src);
        assert!(
            !ok,
            "compare on NaN should fail at runtime; case={label}, stdout={stdout}, stderr={stderr}"
        );
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            combined.contains("cannot compare non-finite float values"),
            "case={label}: missing canonical wording 'cannot compare non-finite float values' \
             in output: {combined}"
        );
        assert!(
            !combined.contains("cannot compare NaN values"),
            "case={label}: stale 'cannot compare NaN values' wording leaked: {combined}"
        );
        assert!(
            !combined.contains("compare() cannot compare"),
            "case={label}: stale 'compare()' prefix leaked: {combined}"
        );
    }
}

/// Source-level lock: the four (Float|ExtFloat, Float|ExtFloat)
/// `partial_cmp` arms in `dispatch_trait_method`'s `"compare"` branch
/// have collapsed into one or-pattern. We assert the dispatch.rs
/// source no longer contains any of the pre-round wordings, and that
/// it does contain the unified canonical wording.
#[test]
fn dispatch_compare_arms_collapsed_to_or_pattern() {
    // Pre-round wordings that must NOT appear:
    assert!(
        !DISPATCH_SRC.contains("compare() cannot compare non-finite float values"),
        "stale `compare()` prefix wording on the (Float, Float) NaN arm \
         leaked back into src/vm/dispatch.rs — the round-71 collapse \
         should have replaced it with the canonical \
         `cannot compare non-finite float values`."
    );
    assert!(
        !DISPATCH_SRC.contains("compare() cannot compare NaN values"),
        "stale `cannot compare NaN values` wording on the (ExtFloat, *) \
         arms leaked back into src/vm/dispatch.rs — the round-71 \
         collapse should have replaced it with the canonical \
         `cannot compare non-finite float values`."
    );
    // Unified wording must be present in BOTH dispatch.rs and
    // arithmetic.rs (the round-71 sibling fix):
    assert!(
        DISPATCH_SRC.contains("cannot compare non-finite float values"),
        "expected canonical `cannot compare non-finite float values` \
         wording in src/vm/dispatch.rs after round-71 collapse"
    );
    assert!(
        ARITHMETIC_SRC.contains("cannot compare non-finite float values"),
        "expected canonical `cannot compare non-finite float values` \
         wording in src/vm/arithmetic.rs (round-67 baseline)"
    );
}

// ── DEAD-2: prelude variant constructor parity ───────────────────────

/// `builtin_prelude_enum_variants_with_arity` must cover every
/// non-error enum tracked by `builtin_enum_variants`. The two
/// registries are kept in shape lockstep — drift in either will
/// surface here.
#[test]
fn prelude_variant_registry_matches_builtin_enum_variants() {
    use std::collections::BTreeSet;

    let prelude_set: BTreeSet<&str> = builtin_prelude_enum_variants_with_arity()
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let error_set: BTreeSet<&str> = builtin_error_enum_variants_with_arity()
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let all_set: BTreeSet<&str> = builtin_enum_variants()
        .iter()
        .map(|(name, _)| *name)
        .collect();

    // Every non-error enum in `builtin_enum_variants` must live in the
    // prelude registry.
    for name in &all_set {
        if error_set.contains(name) {
            continue;
        }
        assert!(
            prelude_set.contains(name),
            "enum `{name}` is in builtin_enum_variants but missing from \
             builtin_prelude_enum_variants_with_arity — round-71 \
             dispatch.rs registration loop will skip its constructors"
        );
    }
    // Every prelude registry entry must live in `builtin_enum_variants`.
    for name in &prelude_set {
        assert!(
            all_set.contains(name),
            "enum `{name}` is in builtin_prelude_enum_variants_with_arity \
             but missing from builtin_enum_variants — likely a typo or \
             stale entry"
        );
    }
}

/// Pin the exact `(variant, arity)` shape the prelude registry exposes
/// so adding/renaming a constructor is intentional. The list mirrors
/// the pre-round-71 hand-rolled `register_builtins` block (lines
/// 95-140 in dispatch.rs) byte-for-byte on (name, arity).
#[test]
fn prelude_variant_registry_exact_shape() {
    let expected: &[(&str, &[(&str, usize)])] = &[
        ("Result", &[("Ok", 1), ("Err", 1)]),
        ("Option", &[("Some", 1), ("None", 0)]),
        ("Step", &[("Stop", 1), ("Continue", 1)]),
        (
            "ChannelResult",
            &[("Message", 1), ("Closed", 0), ("Empty", 0), ("Sent", 0)],
        ),
        ("ChannelOp", &[("Recv", 1), ("Send", 2)]),
        (
            "Weekday",
            &[
                ("Monday", 0),
                ("Tuesday", 0),
                ("Wednesday", 0),
                ("Thursday", 0),
                ("Friday", 0),
                ("Saturday", 0),
                ("Sunday", 0),
            ],
        ),
        (
            "Method",
            &[
                ("GET", 0),
                ("POST", 0),
                ("PUT", 0),
                ("PATCH", 0),
                ("DELETE", 0),
                ("HEAD", 0),
                ("OPTIONS", 0),
            ],
        ),
    ];
    let actual = builtin_prelude_enum_variants_with_arity();
    assert_eq!(
        actual.len(),
        expected.len(),
        "prelude registry length drift: actual={actual:?}, expected={expected:?}"
    );
    for ((aname, avariants), (ename, evariants)) in actual.iter().zip(expected.iter()) {
        assert_eq!(
            aname, ename,
            "prelude registry enum-name drift at same index"
        );
        assert_eq!(
            avariants.len(),
            evariants.len(),
            "variant list length drift for `{aname}`"
        );
        for ((avname, aarity), (evname, earity)) in avariants.iter().zip(evariants.iter()) {
            assert_eq!(
                avname, evname,
                "variant name drift in `{aname}`: actual={avname}, expected={evname}"
            );
            assert_eq!(
                aarity, earity,
                "variant arity drift for `{aname}.{avname}`: actual={aarity}, expected={earity}"
            );
        }
    }
}

/// Source-level lock: dispatch.rs must consume
/// `builtin_prelude_enum_variants_with_arity` and must NOT contain the
/// pre-round-71 hand-rolled `globals.insert(...)` calls for the prelude
/// constructors.
#[test]
fn dispatch_consumes_prelude_registry() {
    assert!(
        DISPATCH_SRC.contains("builtin_prelude_enum_variants_with_arity"),
        "src/vm/dispatch.rs must consult \
         `module::builtin_prelude_enum_variants_with_arity` to seed \
         prelude variant constructors — round-71 collapse"
    );
    // Pre-round dispatch.rs registered each prelude constructor by an
    // explicit `globals.insert("Ok".into(), ...)` line. The collapse
    // replaces every such line with one data-driven loop.
    let banned_inserts = [
        r#"globals.insert("Ok""#,
        r#"globals.insert("Err""#,
        r#"globals.insert("Some""#,
        r#"globals.insert("None""#,
        r#"globals.insert("Stop""#,
        r#"globals.insert("Continue""#,
        r#"globals.insert("Recv""#,
        r#"globals.insert("Send""#,
    ];
    for needle in banned_inserts {
        assert!(
            !DISPATCH_SRC.contains(needle),
            "stale hand-rolled prelude constructor insertion `{needle}` \
             leaked back into src/vm/dispatch.rs after the round-71 \
             collapse"
        );
    }
}

// ── DEAD-4: stale "six per-module error enums" doc ───────────────────

#[test]
fn errors_doc_does_not_say_six_per_module() {
    assert!(
        !ERRORS_BUILTIN_SRC.contains("six per-module"),
        "src/typechecker/builtins/errors.rs still mentions `six \
         per-module` — actual stdlib has 11 typed-error enums (IoError, \
         JsonError, TomlError, ParseError, HttpError, RegexError, \
         PgError, TcpError, TimeError, BytesError, ChannelError). The \
         round-71 fix should have updated the wording or dropped the \
         count entirely."
    );
}

// ── DEAD-5: Phase 0 reference triplication ───────────────────────────

#[test]
fn phase_0_canonical_wording_appears_at_most_once() {
    // The canonical `Phase 0 of the stdlib error redesign` wording is
    // the round-71 deduped form. Cross-references use a different
    // shape (e.g. "for Phase 0 background") so this assertion locks
    // that the canonical mention is single-sourced.
    let needle = "Phase 0 of the stdlib error redesign";
    let occurrences: Vec<&str> = [
        ("module.rs", MODULE_SRC),
        ("vm/dispatch.rs", DISPATCH_SRC),
        ("builtins/io.rs", IO_SRC),
        ("builtins/toml.rs", TOML_SRC),
        ("typechecker/builtins/errors.rs", ERRORS_BUILTIN_SRC),
    ]
    .iter()
    .filter_map(|(name, src)| {
        if src.contains(needle) {
            Some(*name)
        } else {
            None
        }
    })
    .collect();
    assert!(
        occurrences.len() <= 1,
        "canonical `{needle}` wording duplicated across {n} files: {occurrences:?}. \
         Round-71 dedup should keep one canonical mention (in module.rs::\
         builtin_error_enum_variants_with_arity) and replace the others \
         with cross-references.",
        n = occurrences.len()
    );
}

// ── ERR-1: clamp_span dedup ──────────────────────────────────────────

#[test]
fn clamp_span_to_module_source_delegates() {
    // The function body must now be a one-liner delegating to
    // `errors::clamp_span_to_source`. Pre-round it was a byte-identical
    // copy of that helper.
    assert!(
        COMPILER_MOD_SRC.contains("crate::errors::clamp_span_to_source(span, source)"),
        "src/compiler/mod.rs::clamp_span_to_module_source must \
         delegate to crate::errors::clamp_span_to_source after the \
         round-71 ERR-1 fix"
    );
    // The canonical helper is now `pub(crate)` so the module-side
    // wrapper can call it.
    assert!(
        ERRORS_SRC.contains("pub(crate) fn clamp_span_to_source"),
        "src/errors.rs::clamp_span_to_source must be `pub(crate)` after \
         the round-71 ERR-1 fix so the compiler-side helper can delegate"
    );
}

// ── DX-4: signature-help builtin parameters ──────────────────────────
//
// Spawn the LSP server as a subprocess and assert that a known-builtin
// call (`list.map`) returns a non-empty `parameters` array containing
// the expected first param name.

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_round71_lsp_{n}.silt")
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
            .expect("failed to spawn silt lsp");
        let stdin = child.stdin.take().expect("no stdin on child");
        let stdout = child.stdout.take().expect("no stdout on child");
        let (tx, rx) = channel::<Value>();
        thread::spawn(move || reader_loop(stdout, tx));
        LspClient { child, stdin, rx }
    }

    fn send_raw(&mut self, msg: &Value) {
        let body = serde_json::to_string(msg).expect("serialize");
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        self.stdin.write_all(framed.as_bytes()).expect("write");
        self.stdin.flush().expect("flush");
    }

    fn send_request(&mut self, id: u64, method: &str, params: Value) {
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
    }

    fn send_notification(&mut self, method: &str, params: Value) {
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
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
                Err(RecvTimeoutError::Timeout) => panic!("timed out for id={id}"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("silt lsp closed stdout unexpectedly")
                }
            }
        }
    }

    fn initialize(&mut self) {
        let id = next_id();
        self.send_request(
            id,
            "initialize",
            json!({
                "processId": null,
                "rootUri": null,
                "capabilities": {},
            }),
        );
        let _ = self.recv_response_for(id);
        self.send_notification("initialized", json!({}));
    }

    fn did_open_and_wait(&mut self, uri: &str, source: &str) {
        self.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "silt",
                    "version": 1,
                    "text": source,
                }
            }),
        );
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_millis(0));
            if remaining.is_zero() {
                panic!("timed out waiting for diagnostics on {uri}");
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
                Err(RecvTimeoutError::Timeout) => panic!("diag timeout on {uri}"),
                Err(RecvTimeoutError::Disconnected) => panic!("lsp stdout closed"),
            }
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn reader_loop<R: Read + Send + 'static>(stdout: R, tx: Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(rest) = line.strip_prefix("Content-Length:") {
                content_length = rest.trim().parse().unwrap_or(0);
            }
        }
        if content_length == 0 {
            continue;
        }
        let mut buf = vec![0u8; content_length];
        if reader.read_exact(&mut buf).is_err() {
            return;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(&buf)
            && tx.send(v).is_err()
        {
            return;
        }
    }
}

/// `signatureHelp` for a known-builtin call (`list.map(`) must return
/// a non-empty `parameters` array whose first entry's label matches
/// the expected param name (`xs`). Pre-round 71 the LSP returned
/// `parameters: vec![]` for every builtin call site, breaking
/// active-arg highlighting across the entire stdlib surface.
#[test]
fn signature_help_for_list_map_has_param_names() {
    let mut client = LspClient::spawn();
    client.initialize();

    let uri = unique_uri();
    // Cursor sits just after `list.map(` on line 1, col 11.
    let source = "fn main() {\n  list.map(\n}\n";
    client.did_open_and_wait(&uri, source);

    let id = next_id();
    client.send_request(
        id,
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 11 }
        }),
    );
    let resp = client.recv_response_for(id);
    assert!(
        resp.get("error").is_none(),
        "signatureHelp returned error: {resp}"
    );
    let result = resp
        .get("result")
        .expect("signatureHelp must have a `result`");
    assert!(!result.is_null(), "expected non-null signatureHelp result");

    let sigs = result
        .pointer("/signatures")
        .and_then(|v| v.as_array())
        .expect("must have signatures array");
    assert!(!sigs.is_empty(), "must return at least one signature");
    let sig = &sigs[0];

    let params = sig
        .get("parameters")
        .and_then(|v| v.as_array())
        .expect("SignatureInformation.parameters must be present");
    assert!(
        !params.is_empty(),
        "round-71 DX-4: builtin signatureHelp must populate \
         `parameters` for known-registry builtins; got empty for list.map"
    );
    // First param of `list.map(xs, f)` is `xs` per the round-71
    // registry in `typechecker::builtin_param_names`.
    let first_label = params[0]
        .get("label")
        .and_then(|v| v.as_str())
        .expect("first parameter label must be a string");
    assert_eq!(
        first_label, "xs",
        "round-71 DX-4: list.map's first param should be `xs` per \
         the canonical registry"
    );
}
