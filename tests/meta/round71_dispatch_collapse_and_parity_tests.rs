//! Round-71 audit: dead-code collapse + parallel-array-drift +
//! signature-help findings.
//!
//! Kept here: the prelude variant registry parity (DEAD-2) and the
//! LSP signature-help parameters (DX-4). The NaN `compare` corpus
//! (DEAD-1) is golden cases under `tests/golden/meta/runtime/`.

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
