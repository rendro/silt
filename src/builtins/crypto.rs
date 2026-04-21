//! `crypto.*` builtin functions: hashing, HMAC, CSPRNG, and timing-safe
//! comparison. All functions operate on `Value::Bytes(Arc<Vec<u8>>)` as
//! the payload type (see `src/builtins/bytes.rs`).
//!
//! The hash + HMAC primitives are backed by the `sha2` / `hmac`
//! RustCrypto crates. `random_bytes` pulls from the OS CSPRNG via
//! `getrandom`. `constant_time_eq` performs a bitwise-OR accumulation
//! over the full contents of both buffers so its running time does not
//! leak the position of the first differing byte.
//!
//! Length-leak note: `constant_time_eq` short-circuits on a length
//! mismatch and returns `false`. This matches the common crypto-library
//! convention (e.g. Rust's `subtle::ConstantTimeEq` for equal-length
//! slices, and Python's `hmac.compare_digest` for bytes): the *lengths*
//! can leak via timing, but the *contents* cannot. Callers that need
//! length to be private should pad their inputs to a fixed size before
//! comparing.

use std::sync::Arc;

use blake2::Blake2b512;
use hmac::{Hmac, Mac};
use md5::Md5;
use sha2::{Digest, Sha256, Sha512};

use crate::value::Value;
use crate::vm::{Vm, VmError};

/// Upper bound on `crypto.random_bytes(n)`. Chosen to match the 1 MiB
/// cap documented at `docs/stdlib/crypto.md`; this is a sanity guard
/// against accidental giant allocations, not a security boundary.
const RANDOM_BYTES_CAP: i64 = 1_048_576;

/// Dispatch `crypto.<name>(args)`.
pub fn call(_vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "sha256" => sha256(args),
        "sha512" => sha512(args),
        "md5" => md5(args),
        "md5_hex" => md5_hex(args),
        "blake2b" => blake2b(args),
        "blake2b_hex" => blake2b_hex(args),
        "hmac_sha256" => hmac_sha256(args),
        "hmac_sha512" => hmac_sha512(args),
        "random_bytes" => random_bytes(args),
        "constant_time_eq" => constant_time_eq(args),
        _ => Err(VmError::new(format!("unknown crypto function: {name}"))),
    }
}

// ── Helpers (mirror src/builtins/bytes.rs) ─────────────────────────────

fn ok(v: Value) -> Value {
    Value::Variant("Ok".into(), vec![v])
}

fn err(s: impl Into<String>) -> Value {
    Value::Variant("Err".into(), vec![Value::String(s.into())])
}

fn require_bytes(arg: &Value, fn_label: &str) -> Result<Arc<Vec<u8>>, VmError> {
    match arg {
        Value::Bytes(b) => Ok(b.clone()),
        other => Err(VmError::new(format!(
            "{fn_label} requires Bytes, got {}",
            value_kind(other)
        ))),
    }
}

fn require_int(arg: &Value, fn_label: &str) -> Result<i64, VmError> {
    match arg {
        Value::Int(n) => Ok(*n),
        other => Err(VmError::new(format!(
            "{fn_label} requires Int, got {}",
            value_kind(other)
        ))),
    }
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::ExtFloat(_) => "ExtFloat",
        Value::Bool(_) => "Bool",
        Value::String(_) => "String",
        Value::List(_) => "List",
        Value::Bytes(_) => "Bytes",
        Value::Tuple(_) => "Tuple",
        _ => "value",
    }
}

// ── Hashes ─────────────────────────────────────────────────────────────

fn sha256(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("crypto.sha256 takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "crypto.sha256")?;
    let digest = Sha256::digest(b.as_slice());
    Ok(Value::Bytes(Arc::new(digest.to_vec())))
}

fn sha512(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("crypto.sha512 takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "crypto.sha512")?;
    let digest = Sha512::digest(b.as_slice());
    Ok(Value::Bytes(Arc::new(digest.to_vec())))
}

/// Lower-case hex encoding. Kept local (rather than a round-trip
/// through `bytes::to_hex`) so the hex-variant helpers don't grow a
/// dependency on the `bytes` module's public API surface.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn md5(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("crypto.md5 takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "crypto.md5")?;
    // MD5 is cryptographically broken for collision-resistance (well
    // under 2^64 work). It lives here for interop with legacy content
    // stores, Git-style hashing, and cache keys where an adversary
    // isn't in play. Do NOT use it for signatures, certs, or any
    // security decision — use `sha256` / `blake2b` instead.
    let digest = Md5::digest(b.as_slice());
    Ok(Value::Bytes(Arc::new(digest.to_vec())))
}

fn md5_hex(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("crypto.md5_hex takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "crypto.md5_hex")?;
    let digest = Md5::digest(b.as_slice());
    Ok(Value::String(hex_encode(&digest)))
}

fn blake2b(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("crypto.blake2b takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "crypto.blake2b")?;
    // Blake2b512 = BLAKE2b at the full 512-bit (64-byte) output width,
    // per RFC 7693. Faster than SHA-512 on 64-bit hardware and with a
    // cleaner design than SHA-2; preferred for new protocols unless
    // there's a specific reason to match a SHA-family spec.
    let digest = Blake2b512::digest(b.as_slice());
    Ok(Value::Bytes(Arc::new(digest.to_vec())))
}

fn blake2b_hex(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("crypto.blake2b_hex takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "crypto.blake2b_hex")?;
    let digest = Blake2b512::digest(b.as_slice());
    Ok(Value::String(hex_encode(&digest)))
}

// ── HMAC ───────────────────────────────────────────────────────────────

fn hmac_sha256(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "crypto.hmac_sha256 takes 2 arguments (key, msg)".into(),
        ));
    }
    let key = require_bytes(&args[0], "crypto.hmac_sha256")?;
    let msg = require_bytes(&args[1], "crypto.hmac_sha256")?;
    // `new_from_slice` on `Hmac<Sha256>` accepts any key length — it
    // never errors in practice for SHA-256, but we handle the Result
    // defensively rather than unwrapping.
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_slice())
        .map_err(|e| VmError::new(format!("crypto.hmac_sha256 key error: {e}")))?;
    mac.update(msg.as_slice());
    let tag = mac.finalize().into_bytes();
    Ok(Value::Bytes(Arc::new(tag.to_vec())))
}

fn hmac_sha512(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "crypto.hmac_sha512 takes 2 arguments (key, msg)".into(),
        ));
    }
    let key = require_bytes(&args[0], "crypto.hmac_sha512")?;
    let msg = require_bytes(&args[1], "crypto.hmac_sha512")?;
    let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(key.as_slice())
        .map_err(|e| VmError::new(format!("crypto.hmac_sha512 key error: {e}")))?;
    mac.update(msg.as_slice());
    let tag = mac.finalize().into_bytes();
    Ok(Value::Bytes(Arc::new(tag.to_vec())))
}

// ── CSPRNG ─────────────────────────────────────────────────────────────

fn random_bytes(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("crypto.random_bytes takes 1 argument".into()));
    }
    let n = require_int(&args[0], "crypto.random_bytes")?;
    if n < 0 {
        return Ok(err("n must be non-negative"));
    }
    if n > RANDOM_BYTES_CAP {
        return Ok(err("n exceeds 1 MiB cap"));
    }
    let n = n as usize;
    if n == 0 {
        return Ok(ok(Value::Bytes(Arc::new(Vec::new()))));
    }
    let mut buf = vec![0u8; n];
    match getrandom::getrandom(&mut buf) {
        Ok(()) => Ok(ok(Value::Bytes(Arc::new(buf)))),
        Err(e) => Ok(err(format!("CSPRNG failure: {e}"))),
    }
}

// ── Timing-safe comparison ─────────────────────────────────────────────

fn constant_time_eq(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new(
            "crypto.constant_time_eq takes 2 arguments".into(),
        ));
    }
    let a = require_bytes(&args[0], "crypto.constant_time_eq")?;
    let b = require_bytes(&args[1], "crypto.constant_time_eq")?;
    // Length mismatch short-circuits to false. Standard crypto-library
    // practice (cf. Python's hmac.compare_digest, Rust's subtle crate
    // for equal-length slices): the *lengths* leak via timing, but the
    // *contents* of equal-length buffers do not. Callers that need
    // length privacy should pad their inputs to a common fixed size
    // before calling. See the module-level comment.
    if a.len() != b.len() {
        return Ok(Value::Bool(false));
    }
    // OR-accumulate byte differences across the full buffer so the
    // running time is independent of where (or whether) a mismatch
    // occurs. The compiler has no strong reason to short-circuit a
    // straight-line `|=` chain, but we also avoid `==` / boolean
    // shortcut operators which might introduce a data-dependent branch.
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    Ok(Value::Bool(diff == 0))
}
