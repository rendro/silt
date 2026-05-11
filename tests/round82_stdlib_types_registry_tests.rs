//! Round-82 DX-GAP-1 regression: stdlib record/enum types are
//! registered per-module by `src/typechecker/builtins/*.rs`, but
//! historically there was no central enumeration. As a result they
//! were absent from editor grammars and were silently renameable via
//! LSP — a rename of `Response` → `Foo` would rewrite user
//! references while leaving the builtin type intact.
//!
//! This file locks the new central registry at
//! `silt::module::BUILTIN_STDLIB_TYPE_NAMES` against:
//!
//!   1. The typechecker's own registered record/enum set, so adding a
//!      new stdlib record/enum without updating the registry fails.
//!   2. Both editor grammars (vim / vscode), so the registry stays in
//!      sync with what cross-editor highlighting recognises.
//!   3. The LSP rename gate, so stdlib type names cannot be silently
//!      rewritten by `textDocument/rename`.
//!
//! User-defined records remain renameable (negative test).

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use silt::module::{BUILTIN_STDLIB_TYPE_NAMES, feature_gated_stdlib_type};
use silt::types::builtins::iter_all;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_grammar(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Whole-word membership check matching the helper in
/// `tests/builtin_types_authoritative_parity_tests.rs`.
fn grammar_mentions_name(grammar: &str, name: &str) -> bool {
    let bytes = grammar.as_bytes();
    let nlen = name.len();
    let nbytes = name.as_bytes();
    if bytes.len() < nlen {
        return false;
    }
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i + nlen <= bytes.len() {
        if &bytes[i..i + nlen] == nbytes {
            let left_ok = i == 0 || !is_word(bytes[i - 1]);
            let right_ok = i + nlen == bytes.len() || !is_word(bytes[i + nlen]);
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Filter `BUILTIN_STDLIB_TYPE_NAMES` to the names that should be
/// present under the active cargo-feature set. Names with no feature
/// gate are unconditionally included; gated names are kept only when
/// their feature is on.
fn expected_under_active_features() -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for name in BUILTIN_STDLIB_TYPE_NAMES {
        match feature_gated_stdlib_type(name) {
            None => {
                set.insert((*name).to_string());
            }
            Some("postgres") => {
                #[cfg(feature = "postgres")]
                set.insert((*name).to_string());
            }
            Some("tcp") => {
                #[cfg(feature = "tcp")]
                set.insert((*name).to_string());
            }
            Some(other) => panic!(
                "unknown feature gate `{other}` for stdlib type `{name}` — \
                 update tests/round82_stdlib_types_registry_tests.rs"
            ),
        }
    }
    set
}

// ── (1) Registry parity vs typechecker ─────────────────────────────

/// Every name in `BUILTIN_STDLIB_TYPE_NAMES` (filtered by active
/// features) appears in the typechecker's registered record/enum sets
/// after `register_builtins`. The reverse direction — every nominal
/// record/enum the typechecker registers (excluding prelude enums like
/// `Result`/`Option`) appears in the registry — is locked by
/// `registry_covers_every_user_visible_stdlib_type` below.
#[test]
fn registry_names_are_registered_by_typechecker() {
    let registered: BTreeSet<String> = silt::typechecker::registered_builtin_type_names()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let expected = expected_under_active_features();
    let mut missing: Vec<String> = Vec::new();
    for name in &expected {
        if !registered.contains(name) {
            missing.push(name.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "BUILTIN_STDLIB_TYPE_NAMES lists names not registered by the \
         typechecker. Either remove from `src/module.rs::BUILTIN_STDLIB_TYPE_NAMES` \
         or add the corresponding registration to the matching \
         `src/typechecker/builtins/*.rs::register`. Missing from \
         checker.records/enums: {missing:?}"
    );
}

/// Every nominal record/enum the typechecker registers — minus the
/// always-on prelude/value-shape enums declared inline in
/// `register_builtins` itself (`Result`, `Option`, `Step`,
/// `ChannelResult`, `ChannelOp`) and the primitive descriptors — must
/// appear in `BUILTIN_STDLIB_TYPE_NAMES`. The exclusions are the
/// language's own prelude shapes (not stdlib-module types) and are
/// surfaced via `BUILTIN_TYPES` / `builtin_enum_variants` already.
#[test]
fn registry_covers_every_user_visible_stdlib_type() {
    let prelude_exclusions: BTreeSet<&'static str> = [
        // Prelude enums declared in `builtins.rs::register_builtins`.
        "Result",
        "Option",
        "Step",
        "ChannelResult",
        "ChannelOp",
    ]
    .into_iter()
    .collect();

    let registered: BTreeSet<String> = silt::typechecker::registered_builtin_type_names()
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    let registry: BTreeSet<String> = expected_under_active_features();

    let mut uncovered: Vec<String> = Vec::new();
    for name in &registered {
        if prelude_exclusions.contains(name.as_str()) {
            continue;
        }
        if registry.contains(name) {
            continue;
        }
        uncovered.push(name.clone());
    }
    assert!(
        uncovered.is_empty(),
        "Typechecker registers nominal record/enum names not in the \
         central registry `src/module.rs::BUILTIN_STDLIB_TYPE_NAMES`. \
         Either add to the registry (and propagate to the editor grammars + \
         LSP rename gate) or, if it's a prelude shape, add to the test's \
         exclusion list. Uncovered: {uncovered:?}"
    );
}

// ── (2) Editor-grammar parity ──────────────────────────────────────

/// Every name in `BUILTIN_STDLIB_TYPE_NAMES` appears in both editor
/// grammars as a standalone word. This test fails loudly if the
/// registry gains a type but the grammars don't.
#[test]
fn registry_names_appear_in_both_editor_grammars() {
    let vim = read_grammar("editors/vim/syntax/silt.vim");
    let vscode = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");
    let mut missing: Vec<String> = Vec::new();
    for name in BUILTIN_STDLIB_TYPE_NAMES {
        if !grammar_mentions_name(&vim, name) {
            missing.push(format!("vim missing `{name}`"));
        }
        if !grammar_mentions_name(&vscode, name) {
            missing.push(format!("vscode missing `{name}`"));
        }
    }
    assert!(
        missing.is_empty(),
        "Editor grammars missing stdlib type names from the central \
         registry. Add to:\n  - editors/vim/syntax/silt.vim (siltType \
         keyword line)\n  - editors/vscode/syntaxes/silt.tmLanguage.json \
         (\"primitives\" regex)\n\nMissing:\n  - {}",
        missing.join("\n  - ")
    );
}

// ── (3) LSP rename rejection ───────────────────────────────────────

/// Every name in `BUILTIN_STDLIB_TYPE_NAMES` is rejected by the LSP
/// rename gate. Pre-fix, only `BUILTIN_TYPES` primitives were
/// protected; renaming `FileStat` → `Foo` would silently rewrite user
/// references and leave the builtin record intact.
#[test]
#[cfg(feature = "lsp")]
fn lsp_rename_rejects_every_stdlib_type_name() {
    for name in BUILTIN_STDLIB_TYPE_NAMES {
        assert!(
            !silt::lsp::is_user_renameable(name),
            "LSP rename gate does not protect stdlib type `{name}` — \
             `is_user_renameable` should return false but returned true. \
             Check that `src/lsp/rename.rs::builtin_globals()` includes \
             `module::BUILTIN_STDLIB_TYPE_NAMES`."
        );
    }
}

// ── (4) Negative: user-defined record stays renameable ─────────────

/// Sanity check: the rename gate does NOT over-fire on user-defined
/// names that happen to be uppercase. `MyRecord` is not in any registry
/// and must remain renameable.
#[test]
#[cfg(feature = "lsp")]
fn lsp_rename_accepts_user_defined_record_names() {
    for name in &["MyRecord", "UserType", "FooBar", "Bar"] {
        assert!(
            silt::lsp::is_user_renameable(name),
            "User-defined name `{name}` must remain renameable — \
             `is_user_renameable` returned false. The rename gate \
             must reject only stdlib names, not arbitrary uppercase \
             identifiers."
        );
    }
}

// ── LSP end-to-end rename rejection ────────────────────────────────

#[cfg(feature = "lsp")]
mod lsp_e2e {
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

    pub struct LspClient {
        child: Child,
        stdin: ChildStdin,
        rx: Receiver<Value>,
    }

    impl LspClient {
        pub fn spawn() -> Self {
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
                    Err(RecvTimeoutError::Timeout) => {
                        panic!("timed out waiting for response id={id}");
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        panic!("server disconnected waiting for id={id}");
                    }
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

        pub fn did_open_and_wait(&mut self, uri: &str, text: &str) {
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
                    panic!("timed out waiting for publishDiagnostics for {uri}");
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
                    Err(RecvTimeoutError::Timeout) => {
                        panic!("diagnostic timeout for {uri}");
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        panic!("server disconnected");
                    }
                }
            }
        }

        pub fn request(&mut self, method: &str, params: Value) -> Value {
            let id = next_id();
            self.send_raw(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }));
            self.recv_response_for(id)
        }

        pub fn shutdown(mut self) {
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

    /// End-to-end rename rejection: a silt program that uses `FileStat`
    /// as a stdlib type reference (e.g. via `fs.stat`) — rename of
    /// `FileStat` → `Foo` must NOT produce a non-empty WorkspaceEdit.
    /// Acceptable rejection shapes (mirroring
    /// `lsp_rename_gated_constructor_rejection_tests`): an `error`
    /// response, a null `result`, or `changes` absent/empty.
    #[test]
    fn rename_stdlib_record_type_file_stat_is_rejected() {
        let mut client = LspClient::spawn();
        let uri = "file:///tmp/silt_rn_stdlib_file_stat.silt";
        // Mention `FileStat` in a context the parser accepts so the
        // identifier lands in the AST and reaches the rename guard.
        let source = "import fs\nfn main() {\n  let _x = FileStat\n  0\n}\n";
        client.did_open_and_wait(uri, source);

        // `FileStat` starts at line=2, char=11.
        let resp = client.request(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 2, "character": 11 },
                "newName": "Foo"
            }),
        );

        let has_error = resp.get("error").is_some();
        let result = resp.get("result");
        let result_is_null = result.map(|v| v.is_null()).unwrap_or(true);
        let changes_empty = result
            .and_then(|r| r.get("changes"))
            .and_then(|c| c.as_object())
            .map(|o| o.is_empty())
            .unwrap_or(true);

        assert!(
            has_error || result_is_null || changes_empty,
            "rename on stdlib record type `FileStat` must be rejected \
             (error) or produce no edits (null result / empty changes); \
             got {resp}"
        );
        client.shutdown();
    }

    /// Sibling end-to-end coverage for the http feature: `Response`
    /// is a stdlib record name. Mirrors the `FileStat` test above.
    #[test]
    fn rename_stdlib_record_type_response_is_rejected() {
        let mut client = LspClient::spawn();
        let uri = "file:///tmp/silt_rn_stdlib_response.silt";
        let source = "import http\nfn main() {\n  let _x = Response\n  0\n}\n";
        client.did_open_and_wait(uri, source);

        // `Response` starts at line=2, char=11.
        let resp = client.request(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 2, "character": 11 },
                "newName": "Foo"
            }),
        );

        let has_error = resp.get("error").is_some();
        let result = resp.get("result");
        let result_is_null = result.map(|v| v.is_null()).unwrap_or(true);
        let changes_empty = result
            .and_then(|r| r.get("changes"))
            .and_then(|c| c.as_object())
            .map(|o| o.is_empty())
            .unwrap_or(true);

        assert!(
            has_error || result_is_null || changes_empty,
            "rename on stdlib record type `Response` must be rejected; got {resp}"
        );
        client.shutdown();
    }
}

// ── Source-grep guard ──────────────────────────────────────────────

/// Lock the rename gate's reliance on the central registry at the
/// source level: `builtin_globals()` must consult
/// `BUILTIN_STDLIB_TYPE_NAMES`, not hand-roll its own list of stdlib
/// type names. If a future refactor reverts to a parallel array this
/// test fires.
#[test]
fn rename_source_consults_central_stdlib_registry() {
    let path = repo_root().join("src/lsp/rename.rs");
    let src = fs::read_to_string(&path).expect("read src/lsp/rename.rs");
    assert!(
        src.contains("BUILTIN_STDLIB_TYPE_NAMES"),
        "src/lsp/rename.rs must consult \
         `module::BUILTIN_STDLIB_TYPE_NAMES` so adding a new stdlib \
         record/enum name automatically protects it from LSP rename. \
         The hand-rolled list would drift."
    );
}

/// Sibling lock: the central registry must not accidentally collide
/// with `BUILTIN_TYPES`. Primitives + generic containers are owned by
/// `src/types/builtins.rs::BUILTIN_TYPES`; this registry is strictly
/// for per-module nominal records/enums.
#[test]
fn registry_does_not_overlap_with_builtin_types() {
    let primitives: BTreeSet<&'static str> = iter_all().map(|b| b.name).collect();
    for name in BUILTIN_STDLIB_TYPE_NAMES {
        assert!(
            !primitives.contains(name),
            "Name `{name}` appears in both `BUILTIN_TYPES` and \
             `BUILTIN_STDLIB_TYPE_NAMES`. The two registries must be \
             disjoint — primitives + generic containers belong in the \
             former, per-module record/enum types in the latter."
        );
    }
}
