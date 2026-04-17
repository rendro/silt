//! Feature-flag detection for the `silt --help` footer.
//!
//! The list is built at compile time from `cfg!(feature = "...")` so it
//! reflects exactly what was linked into this binary.

/// Comma-separated list of Cargo features compiled into this binary.
/// Shown in `silt --help` so users can tell at a glance which optional
/// subcommands (repl, lsp) and capabilities (watch, local-clock, http,
/// tcp, tcp-tls, postgres, postgres-tls) are available.
pub(crate) fn enabled_features() -> String {
    let mut feats: Vec<&'static str> = Vec::new();
    if cfg!(feature = "repl") {
        feats.push("repl");
    }
    if cfg!(feature = "lsp") {
        feats.push("lsp");
    }
    if cfg!(feature = "watch") {
        feats.push("watch");
    }
    if cfg!(feature = "local-clock") {
        feats.push("local-clock");
    }
    if cfg!(feature = "http") {
        feats.push("http");
    }
    if cfg!(feature = "tcp") {
        feats.push("tcp");
    }
    if cfg!(feature = "tcp-tls") {
        feats.push("tcp-tls");
    }
    if cfg!(feature = "postgres") {
        feats.push("postgres");
    }
    if cfg!(feature = "postgres-tls") {
        feats.push("postgres-tls");
    }
    if feats.is_empty() {
        "(none)".to_string()
    } else {
        feats.join(", ")
    }
}
