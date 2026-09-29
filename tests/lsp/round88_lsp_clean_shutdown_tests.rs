//! Round 88 — LSP graceful shutdown regression.
//!
//! Reproduces the latent bug where `silt lsp` hangs after a clean
//! `shutdown`/`exit` handshake. Root cause was that `Server::run`
//! returning did not drop the writer-side channel `Sender`
//! (`connection.sender` lives inside `Server`), so `io_threads.join()`
//! parked forever on the writer thread. The fix drops `server` before
//! calling `join`. This test catches the regression by spawning the
//! real `silt lsp` binary, driving the full init / shutdown / exit
//! sequence over stdio, and asserting the process exits cleanly within
//! 3 s — i.e. without SIGKILL.
//!
//! The pattern is intentionally minimal vs. other lsp_*_tests.rs files:
//! no diagnostics, no didOpen, no semantic-tokens — only the shutdown
//! handshake itself, so the test stays small and fast.
//!
//! NB: the test fails outright on timeout. It does NOT fall back to
//! `child.kill()` — doing so would mask the very hang we want to catch.

use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::support::LspClient;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Drive the full init / shutdown / exit handshake and assert the
/// child exits on its own within 3 s. The pid is incorporated into a
/// unique workspace `rootUri` so parallel test runs do not collide on
/// the same path string.
#[test]
fn lsp_exits_cleanly_on_shutdown_exit() {
    let mut client = LspClient::spawn_uninitialized();

    // 1) initialize → initialized
    let pid = std::process::id();
    let workspace = std::env::temp_dir().join(format!("silt_round88_lsp_{pid}"));
    let root_uri = format!("file://{}", workspace.display());
    client.send_raw(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {},
            "rootUri": root_uri,
        }
    }));
    let _ = client.recv_response_for(1);
    client.send_raw(&json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    }));

    // 2) shutdown → wait for response → exit
    client.send_raw(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "shutdown"
    }));
    let _ = client.recv_response_for(2);
    client.send_raw(&json!({"jsonrpc": "2.0", "method": "exit"}));

    // 3) Poll for exit within 3 s. The test deliberately does NOT call
    // child.kill() on timeout — that would mask the very hang we want
    // to catch.
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    let status = loop {
        match client.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Best-effort: kill the leaked child so we don't
                    // leave zombies behind when the assertion below
                    // panics. The kill happens AFTER we've decided the
                    // test fails, so it does not mask the bug.
                    let _ = client.child.kill();
                    let _ = client.child.wait();
                    panic!(
                        "silt lsp did not exit within {:?} after shutdown/exit handshake — \
                         regression of the LSP shutdown hang (writer thread parked because \
                         `connection.sender` was not dropped before `io_threads.join()`)",
                        SHUTDOWN_TIMEOUT
                    );
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("child.try_wait failed: {e}"),
        }
    };

    // 4) Clean exit. `silt lsp` does not currently call process::exit
    // with a specific code on graceful shutdown — Rust's default
    // termination yields success. Accept either success or any
    // signal-less exit code: the load-bearing assertion above is the
    // "did it exit at all" check.
    assert!(
        status.code().is_some(),
        "child exited via signal, not graceful exit: {status:?}"
    );
}
