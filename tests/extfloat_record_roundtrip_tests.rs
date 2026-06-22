//! Regression lock for Fix 1 (round-trip of `ExtFloat` record fields).
//!
//! Background: `src/compiler/mod.rs` `encode_type_expr` emits a compact
//! string descriptor for each record field's type; this descriptor is
//! consumed at runtime by `src/builtins/data.rs` `decode_field_type`
//! when `json.parse` / `toml.parse` rebuild a typed record.
//!
//! The scalar-passthrough match arm listed `Int | Float | String | Bool |
//! Date | Time | DateTime` but OMITTED `ExtFloat`. Because "ExtFloat"
//! starts uppercase it fell through to the `Record:{s}` arm, encoding an
//! ExtFloat field as `Record:ExtFloat`. On parse the decoder then failed
//! with `unknown record type 'ExtFloat'` — even though the decoder
//! already handled a bare `"ExtFloat"` descriptor. The encoder was the
//! one-sided culprit.
//!
//! Note on test values: assigning a finite Float *literal* to an
//! ExtFloat field type-errors (`expected ExtFloat, got Float`). The
//! typechecker produces `ExtFloat` from float division
//! (`Float / Float -> ExtFloat`), so the test constructs the field value
//! that way. The point of the test is the round-trip, not the literal.
//!
//! These tests exercise the runtime via the `silt` CLI: they fail before
//! the encoder fix and pass after.

use std::process::Command;

/// Run a Silt source program via `silt run`; return (stdout, stderr, ok).
fn run_silt(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_extfloat_rt_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// The canonical repro from the fix brief: an ExtFloat-typed record field
/// must survive `json.stringify` -> `json.parse`. Before the fix the
/// parse arm returned `Err` (descriptor `Record:ExtFloat` =>
/// `unknown record type 'ExtFloat'`); after the fix it returns `Ok` and
/// the value round-trips.
#[test]
fn json_extfloat_record_field_roundtrips() {
    let src = r#"
import json
type Rec { x: ExtFloat }
fn main() {
  let r = Rec { x: 1.0 / 4.0 }
  let s = json.stringify(r)
  match json.parse(s, Rec) {
    Ok(v) -> {
      println("OK")
      println(v.x)
    }
    Err(e) -> println("ERR")
  }
}
"#;
    let (stdout, stderr, ok) = run_silt("json", src);
    assert!(
        ok,
        "silt run should succeed; stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("OK"),
        "ExtFloat field must round-trip through json (expected OK); \
         stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("0.25"),
        "round-tripped ExtFloat value must equal 0.25; stdout={stdout}"
    );
    assert!(
        !stdout.contains("ERR"),
        "parse must not fail with the Record:ExtFloat descriptor bug; stdout={stdout}"
    );
}

/// The fix touches the *shared* encoder, so the toml parser benefits too.
/// This guards against a future regression that re-narrows the fix to
/// json only.
#[test]
fn toml_extfloat_record_field_roundtrips() {
    // `toml.stringify` returns `Result(String, TomlError)`, unlike
    // `json.stringify` which returns a bare String — so the value is
    // unwrapped before parsing back.
    let src = r#"
import toml
type Rec { x: ExtFloat }
fn main() {
  let r = Rec { x: 1.0 / 4.0 }
  match toml.stringify(r) {
    Ok(s) -> {
      match toml.parse(s, Rec) {
        Ok(v) -> {
          println("OK")
          println(v.x)
        }
        Err(e) -> println("ERR_PARSE")
      }
    }
    Err(e) -> println("ERR_STRINGIFY")
  }
}
"#;
    let (stdout, stderr, ok) = run_silt("toml", src);
    assert!(
        ok,
        "silt run should succeed; stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("OK"),
        "ExtFloat field must round-trip through toml (expected OK); \
         stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("0.25"),
        "round-tripped ExtFloat value must equal 0.25; stdout={stdout}"
    );
    assert!(
        !stdout.contains("ERR"),
        "toml parse must not fail with the Record:ExtFloat descriptor bug; stdout={stdout}"
    );
}
