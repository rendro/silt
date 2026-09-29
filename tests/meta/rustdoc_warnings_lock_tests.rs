//! Rustdoc zero-warning lock: run `cargo doc --no-deps --release` and
//! assert it prints no `warning:` line (intra-doc links to private items,
//! redundant explicit link targets, HTML-like sequences in doc comments).
//!
//! Run with: `cargo test --test meta rustdoc_zero_warnings_via_subprocess`
//! or normally — it is not `#[ignore]`d. CI executes the full suite.

#[test]
fn rustdoc_zero_warnings_via_subprocess() {
    use std::process::Command;

    // Honour the `CARGO` env var if cargo set it for us (it does when
    // running `cargo test`). Fall back to the bare `cargo` binary in
    // PATH otherwise.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let output = match Command::new(&cargo)
        .args([
            "doc",
            "--no-deps",
            "--release",
            "--quiet",
            "--manifest-path",
            "Cargo.toml",
        ])
        // Force colour off so any tty-detection in cargo can't sneak
        // ANSI bytes into the output we grep.
        .env("CARGO_TERM_COLOR", "never")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            // We do NOT pass silently — surface the failure so a broken
            // toolchain can't hide a regression. The only case this
            // legitimately fires is if `cargo` isn't on PATH, which
            // shouldn't happen in any sane test environment.
            panic!(
                "failed to spawn `{} doc --no-deps --release --quiet`: {e}. \
                 The rustdoc-zero-warnings lock requires cargo to be available.",
                cargo.to_string_lossy()
            );
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        panic!(
            "`cargo doc --no-deps --release --quiet` failed (exit {:?}). \
             This lock requires `cargo doc` to succeed so we can count its warnings. \
             ── stdout ──\n{stdout}\n── stderr ──\n{stderr}",
            output.status.code()
        );
    }

    // Rustdoc writes warnings to stderr. Combine both streams to be
    // robust against future changes in cargo's plumbing.
    let combined = format!("{stdout}\n{stderr}");
    let warning_lines: Vec<&str> = combined
        .lines()
        .filter(|l| l.starts_with("warning:"))
        .collect();

    assert!(
        warning_lines.is_empty(),
        "`cargo doc --no-deps --release` produced {} `^warning:` line(s); \
         expected 0. This lock asserts the zero-rustdoc-warning invariant \
         introduced in round 75 L5. Fix the underlying warnings — do not \
         weaken this test.\n── warning lines ──\n{}\n── full stderr ──\n{stderr}",
        warning_lines.len(),
        warning_lines.join("\n")
    );
}
