//! LSP regression tests for GAP #8: `import <user_module>` must NOT
//! produce the "unknown module" warning (nor follow-on "undefined X"
//! errors for names imported through it) in the editor. The LSP once
//! checked each document without its imports, so every legitimate
//! user-module import was flagged; it now publishes the session's
//! analysis, which loads the imported modules.
//!
//! Communicates with the compiled `silt lsp` subprocess end-to-end so
//! we exercise the real pipeline.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::support::LspClient;

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_user_import_filter_{n}.silt")
}

fn diagnostic_messages(notif: &Value) -> Vec<String> {
    notif
        .pointer("/params/diagnostics")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|d| {
                    d.get("message")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── Tests ──────────────────────────────────────────────────────────

/// A silt file that imports a user module and uses a name from it must
/// not surface the typechecker's "unknown module" warning or the
/// follow-on "undefined variable" error through LSP diagnostics.
#[test]
fn test_lsp_filters_unknown_module_warning_for_user_import() {
    let mut client = LspClient::spawn();

    let uri = unique_uri();
    let source = "import my_user_module\nfn main() { println(my_user_module.something()) }\n";
    let notif = client.did_open_and_wait(&uri, source);
    let messages = diagnostic_messages(&notif);

    for m in &messages {
        assert!(
            !m.contains("unknown module"),
            "LSP must filter the 'unknown module' warning for user imports; got: {m:?}"
        );
        assert!(
            !m.starts_with("undefined variable"),
            "LSP must filter 'undefined variable' follow-ons for user imports; got: {m:?}"
        );
    }

    client.shutdown();
}

/// The filter must not hide real errors that happen alongside a
/// user-module import. Here we intentionally call `add` with the wrong
/// arity — an error the typechecker reports as
/// `` `add` expects 2 arguments, got 1 ``, which is outside the
/// user-import filter's pattern list. The filter must NOT swallow it.
#[test]
fn test_lsp_still_reports_real_type_errors_with_user_import() {
    let mut client = LspClient::spawn();

    let uri = unique_uri();
    // Contains:
    //   (a) a user-module import that triggers the filtered warning
    //   (b) an arity error on a local fn the typechecker catches; the
    //       arity-mismatch message doesn't match any user-import filter
    //       pattern, so it must still surface.
    let source = "import my_user_module\nfn add(a, b) { a + b }\nfn main() { add(1) }\n";
    let notif = client.did_open_and_wait(&uri, source);
    let messages = diagnostic_messages(&notif);

    // No filtered noise.
    for m in &messages {
        assert!(
            !m.contains("unknown module"),
            "LSP must filter 'unknown module'; got: {m:?}"
        );
    }

    // The real arity error must still be present.
    let has_arity_error = messages
        .iter()
        .any(|m| m.contains("expects 2 arguments, got 1"));
    assert!(
        has_arity_error,
        "LSP must still surface real type errors alongside a user import; got diagnostics: {messages:?}"
    );

    client.shutdown();
}
