//! Round-73 BLOAT-1 + BLOAT-3 parity locks.
//!
//! Two BLOAT findings collapsed in this round:
//!
//!   * **BLOAT-1** (HIGH semantic risk): the stdlib typed-error enum
//!     name set was hand-rolled at three independent sites
//!     (`src/vm/dispatch.rs::register_builtins` for the
//!     `<EnumName>.message` BuiltinFn registration,
//!     `src/typechecker/builtins/errors.rs::register` for the
//!     `trait_impl_set` / `method_table` registration, and
//!     `src/typechecker/mod.rs` for the synth pass — sibling-owned).
//!     This round derives both VM-side and (newly) typechecker-side
//!     `enum_names` lists from the authoritative registry at
//!     `module::builtin_error_enum_variants_with_arity`.
//!
//!   * **BLOAT-3** (MED semantic risk): the trait-method dispatch in
//!     `src/vm/dispatch.rs` had 11 byte-near-identical match arms of
//!     the form `"<Enum>" => catch_builtin_panic("<Enum>",
//!     AssertUnwindSafe(|| <module>::call_<x>_error_trait(...)))`. All
//!     11 are now collapsed onto a single static
//!     `ERROR_TRAIT_DISPATCH: &[(&str, fn(...))]` table lookup. The
//!     uniform `fn(&str, &[Value]) -> Result<Value,VmError>` signature
//!     across all `call_*_error_trait` helpers is what makes this
//!     possible (verified by table compilation; if any helper changes
//!     signature the table won't compile).
//!
//! Source-grep style: per the audit prompt, source-grep locks beat
//! tautological format-the-error tests. These tests pin that the
//! deletions/refactors were semantic no-ops.

use silt::module::builtin_error_enum_variants_with_arity;

const DISPATCH_SRC: &str = include_str!("../src/vm/dispatch.rs");
const ERRORS_SRC: &str = include_str!("../src/typechecker/builtins/errors.rs");

// ── BLOAT-1: dispatch.rs `<Enum>.message` BuiltinFn loop is registry-driven ──

/// SOURCE-LEVEL: `src/vm/dispatch.rs::register_builtins` must NOT
/// contain the literal hand-rolled list of error-enum names that was
/// there pre-round-73. The previous shape was a `for enum_name in
/// &[...]` array literal containing all 11 names. Replacement is a
/// loop over `module::builtin_error_enum_variants_with_arity()`.
#[test]
fn bloat1_dispatch_message_loop_is_registry_driven() {
    // The pre-fix shape contained ALL of these consecutively:
    //   "IoError",
    //   "JsonError",
    //   "TomlError",
    // ...as items in a single `&[ ... ]` array literal feeding a
    // `for enum_name in &[...] { let key = format!("{enum_name}.message"); }`
    // loop. The unique fingerprint we lock on is: the literal
    // sequence of `"IoError",` followed shortly after by `"JsonError",`
    // and then `"TomlError",` — but ONLY in a context that's NOT a
    // comment. After the fix, the only such occurrence in the file is
    // inside this very test's source string (which is fine — the lock
    // checks the SOURCE OF THE DISPATCH CODE).
    //
    // Stronger: assert that the registry call is present AND that no
    // `for enum_name in &[` appears (which is what the old block had).
    assert!(
        DISPATCH_SRC.contains("builtin_error_enum_variants_with_arity"),
        "src/vm/dispatch.rs must call \
         `module::builtin_error_enum_variants_with_arity` to derive the \
         `<EnumName>.message` BuiltinFn registrations. Round-73 BLOAT-1 \
         requires this call to drive the loop."
    );
    // The pre-fix array-literal driver shape is gone. We look for any
    // non-comment line containing `for enum_name in &[` — pre-fix this
    // appeared exactly once at the top of the message-registration
    // block.
    let bad: Vec<(usize, &str)> = DISPATCH_SRC
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with("//") && l.contains("for enum_name in &[")
        })
        .map(|(i, l)| (i + 1, l))
        .collect();
    assert!(
        bad.is_empty(),
        "src/vm/dispatch.rs still contains a hand-rolled \
         `for enum_name in &[...]` driver for the error-enum message \
         registration. Round-73 BLOAT-1 collapsed this onto a registry \
         iteration. Found: {bad:?}"
    );
}

// ── BLOAT-1: errors.rs `enum_names` Vec literal is registry-driven ──

/// SOURCE-LEVEL: `src/typechecker/builtins/errors.rs::register` must
/// derive its `enum_names: Vec<&'static str>` list from the registry,
/// not from a hand-rolled `vec![...]` literal. The cfg-aware
/// `PgError`/`TcpError` filtering is preserved by skipping those names
/// in the iterator chain when their feature is off.
#[test]
fn bloat1_errors_rs_enum_names_is_registry_driven() {
    assert!(
        ERRORS_SRC.contains("builtin_error_enum_variants_with_arity"),
        "src/typechecker/builtins/errors.rs must call \
         `module::builtin_error_enum_variants_with_arity` to derive the \
         `enum_names` list for the trait-impl registration loop. \
         Round-73 BLOAT-1 requires this call."
    );
    // The pre-fix shape was a `let mut enum_names: Vec<&'static str> = vec![`
    // immediately followed by ~9 string-literal lines. Lock that the
    // `vec![` literal driver is gone (allowing comment mentions).
    let bad: Vec<(usize, &str)> = ERRORS_SRC
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with("//") && l.contains("let mut enum_names") && l.contains("vec![")
        })
        .map(|(i, l)| (i + 1, l))
        .collect();
    assert!(
        bad.is_empty(),
        "src/typechecker/builtins/errors.rs still contains a hand-rolled \
         `let mut enum_names ... vec![...]` literal driver. Round-73 \
         BLOAT-1 collapsed this onto a registry iteration. Found: {bad:?}"
    );
}

// ── BLOAT-1: cfg-aware filter preserved ──

/// The pre-round-73 errors.rs site applied
///   `#[cfg(feature = "postgres")] enum_names.push("PgError")`
///   `#[cfg(feature = "tcp")]      enum_names.push("TcpError")`
/// after the bare `vec![...]` literal. The post-round-73 shape must
/// preserve that filter — i.e. when a feature is off, the corresponding
/// enum must NOT appear in the registered set.
#[cfg(not(feature = "postgres"))]
#[test]
fn bloat1_pg_error_skipped_when_feature_off_at_typecheck() {
    use silt::types::Severity;
    // If PgError were still in the typechecker's trait_impl_set when
    // the postgres feature is off, calling `.message()` on it would
    // typecheck but then crash at runtime (trait_impl_set advertises a
    // trait the runtime can't fulfill). The fix's filter prevents
    // this — but the actual proof is at the construction site (audit
    // gate from Round 64): with the feature off, PgError should not
    // even be a registered enum so the constructor reference fails.
    let src = r#"
fn main() {
    let _ = PgError.PgConnect("nope")
}
"#;
    let tokens = silt::lexer::Lexer::new(src).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    let errors = silt::typechecker::check(&mut program);
    let messages: Vec<String> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();
    assert!(
        !messages.is_empty(),
        "with `postgres` feature OFF, PgError must remain unregistered. \
         The Round-73 BLOAT-1 collapse must preserve the cfg-aware \
         filter that the round-64 GAP fix introduced."
    );
}

// ── BLOAT-1: registry/dispatch source-grep parity ──

/// SOURCE parity: every enum in the registry must appear as a
/// well-formed registry-driven entry that resolves at the dispatch
/// site. Since we can't access `Vm::globals` from an integration test
/// (it's `pub(crate)`), we lock the SOURCE-side derivation: the
/// registry call is the ONLY thing feeding the message-registration
/// loop, so its size determines the registered set. The runtime-side
/// proof of correctness is `bloat3_io_error_message_dispatch_works_end_to_end`
/// below — `IoNotFound(...).message()` only succeeds if `IoError.message`
/// was registered as a BuiltinFn.
#[test]
fn bloat1_registry_size_is_load_bearing() {
    // Sanity: the registry currently has 11 entries (matching the
    // pre-fix hand-rolled list). If a new error enum is added the
    // registry grows — and the dispatch loop, being registry-driven,
    // automatically picks it up. This is the round-73 fix's payoff.
    let n = builtin_error_enum_variants_with_arity().len();
    assert!(
        n >= 11,
        "registry must contain all stdlib error enums; got {n}"
    );
}

// ── BLOAT-3: 11 hand-rolled trait-dispatch arms collapsed onto a table ──

/// The pre-round-73 file contained 11 distinct match arms of the
/// shape `"<Enum>" => catch_builtin_panic("<Enum>",
/// AssertUnwindSafe(|| <module>::call_<x>_error_trait(func, args)))`.
/// After the fix there must be at most ONE non-comment occurrence of
/// each `call_<x>_error_trait` symbol (the entry inside the new
/// `ERROR_TRAIT_DISPATCH` table).
#[test]
fn bloat3_no_per_arm_call_error_trait_dispatch() {
    let names = [
        "call_io_error_trait",
        "call_json_error_trait",
        "call_toml_error_trait",
        "call_parse_error_trait",
        "call_http_error_trait",
        "call_regex_error_trait",
        "call_pg_error_trait",
        "call_tcp_error_trait",
        "call_time_error_trait",
        "call_bytes_error_trait",
        "call_channel_error_trait",
    ];
    for fn_name in names {
        let occurrences: Vec<(usize, &str)> = DISPATCH_SRC
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim_start();
                !t.starts_with("//") && l.contains(fn_name)
            })
            .map(|(i, l)| (i + 1, l))
            .collect();
        assert!(
            occurrences.len() <= 1,
            "src/vm/dispatch.rs contains {} non-comment occurrences of \
             `{fn_name}` — the round-73 BLOAT-3 collapse expects at \
             most one (inside the `ERROR_TRAIT_DISPATCH` table). \
             Found:\n  - {}",
            occurrences.len(),
            occurrences
                .iter()
                .map(|(n, l)| format!("line {n}: {l}"))
                .collect::<Vec<_>>()
                .join("\n  - ")
        );
    }
}

/// The dispatch table itself must exist by name. Pinning the symbol
/// lets future grep audits track the load-bearing structure.
#[test]
fn bloat3_dispatch_table_exists() {
    assert!(
        DISPATCH_SRC.contains("ERROR_TRAIT_DISPATCH"),
        "src/vm/dispatch.rs must define the `ERROR_TRAIT_DISPATCH` \
         table that replaces the 11 hand-rolled trait-dispatch arms."
    );
    assert!(
        DISPATCH_SRC.contains("fn error_trait_dispatch("),
        "src/vm/dispatch.rs must define the `error_trait_dispatch` \
         lookup helper that consults `ERROR_TRAIT_DISPATCH`."
    );
}

/// Pre-round-73, the match arms looked like
/// `"IoError" => catch_builtin_panic("IoError", ...)` — a literal
/// `"IoError"` string-arm at the top level of the dispatch match.
/// After the collapse, no such per-enum string-arm should exist for
/// the error enums (they all flow through the table lookup).
#[test]
fn bloat3_no_per_enum_string_arm_in_match() {
    // Each of these would appear as a line ending in `=> catch_builtin_panic(`
    // pre-round-73. We grep for the precise old shape.
    let pre_fix_shapes = [
        "\"IoError\" => catch_builtin_panic(",
        "\"JsonError\" => catch_builtin_panic(",
        "\"TomlError\" => catch_builtin_panic(",
        "\"ParseError\" => catch_builtin_panic(",
        "\"HttpError\" => catch_builtin_panic(",
        "\"RegexError\" => catch_builtin_panic(",
        "\"PgError\" => catch_builtin_panic(",
        "\"TcpError\" => catch_builtin_panic(",
        "\"TimeError\" => catch_builtin_panic(",
        "\"BytesError\" => catch_builtin_panic(",
        "\"ChannelError\" => catch_builtin_panic(",
    ];
    let mut bad: Vec<&str> = Vec::new();
    for shape in pre_fix_shapes {
        if DISPATCH_SRC.contains(shape) {
            bad.push(shape);
        }
    }
    assert!(
        bad.is_empty(),
        "src/vm/dispatch.rs still contains pre-round-73 hand-rolled \
         per-enum dispatch arms: {bad:?}. The round-73 BLOAT-3 collapse \
         routes all 11 through `ERROR_TRAIT_DISPATCH`."
    );
}

// ── BLOAT-3: end-to-end runtime check ──

/// Construct an IoError, dispatch `.message()` through the VM, and
/// assert a non-empty string comes back. This exercises the table
/// lookup -> `catch_builtin_panic` -> `call_io_error_trait` end-to-end.
/// If the table accidentally drops IoError (or routes the call
/// elsewhere), this test fails.
#[test]
fn bloat3_io_error_message_dispatch_works_end_to_end() {
    let src = r#"
import io

fn main() {
    let e = IoNotFound("missing.txt")
    println(e.message())
}
"#;
    let tmp = std::env::temp_dir().join("silt_round73_bloat3_io_error.silt");
    std::fs::write(&tmp, src).expect("write temp");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "silt run failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let trimmed = stdout.trim();
    assert!(
        !trimmed.is_empty(),
        "IoNotFound(...).message() produced empty output. The \
         round-73 BLOAT-3 dispatch table must route IoError through \
         `call_io_error_trait`, which produces a non-empty message."
    );
    // The message body should mention the file name.
    assert!(
        trimmed.contains("missing.txt"),
        "IoNotFound(\"missing.txt\").message() should mention the path. \
         Got: {trimmed:?}"
    );
}
