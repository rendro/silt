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

use super::common::{err, nibble_to_hex, ok};
use super::typed::{Bytes, builtins};
use crate::value::Value;
use crate::vm::VmError;

/// Upper bound on `crypto.random_bytes(n)`. Chosen to match the 1 MiB
/// cap documented in `docs/stdlib/crypto.md`; this is a sanity guard against accidental giant allocations, not a
/// security boundary.
const RANDOM_BYTES_CAP: i64 = 1_048_576;

/// Lower-case hex encoding, with the digit table every hex-emitting
/// builtin shares (`nibble_to_hex`).
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(nibble_to_hex(b >> 4, false));
        out.push(nibble_to_hex(b & 0x0f, false));
    }
    out
}

builtins! {
    // ── Hashes ─────────────────────────────────────────────────────────

    fn sha256(data: Bytes) -> Vec<u8> {
        Sha256::digest(data.as_slice()).to_vec()
    }

    fn sha512(data: Bytes) -> Vec<u8> {
        Sha512::digest(data.as_slice()).to_vec()
    }

    // MD5 is cryptographically broken for collision-resistance (well
    // under 2^64 work). It lives here for interop with legacy content
    // stores, Git-style hashing, and cache keys where an adversary
    // isn't in play. Do NOT use it for signatures, certs, or any
    // security decision — use `sha256` / `blake2b` instead.
    fn md5(data: Bytes) -> Vec<u8> {
        Md5::digest(data.as_slice()).to_vec()
    }

    fn md5_hex(data: Bytes) -> String {
        hex_encode(&Md5::digest(data.as_slice()))
    }

    // Blake2b512 = BLAKE2b at the full 512-bit (64-byte) output width,
    // per RFC 7693. Faster than SHA-512 on 64-bit hardware and with a
    // cleaner design than SHA-2; preferred for new protocols unless
    // there's a specific reason to match a SHA-family spec.
    fn blake2b(data: Bytes) -> Vec<u8> {
        Blake2b512::digest(data.as_slice()).to_vec()
    }

    fn blake2b_hex(data: Bytes) -> String {
        hex_encode(&Blake2b512::digest(data.as_slice()))
    }

    // ── HMAC ───────────────────────────────────────────────────────────

    fn hmac_sha256(key: Bytes, msg: Bytes) -> Result<Vec<u8>, VmError> {
        // `new_from_slice` on `Hmac<Sha256>` accepts any key length — it
        // never errors in practice for SHA-256, but we handle the Result
        // defensively rather than unwrapping.
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_slice())
            .map_err(|e| VmError::new(format!("crypto.hmac_sha256 key error: {e}")))?;
        mac.update(msg.as_slice());
        Ok(mac.finalize().into_bytes().to_vec())
    }

    fn hmac_sha512(key: Bytes, msg: Bytes) -> Result<Vec<u8>, VmError> {
        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(key.as_slice())
            .map_err(|e| VmError::new(format!("crypto.hmac_sha512 key error: {e}")))?;
        mac.update(msg.as_slice());
        Ok(mac.finalize().into_bytes().to_vec())
    }

    // ── CSPRNG ─────────────────────────────────────────────────────────

    fn random_bytes(n: i64) -> Value {
        if n < 0 {
            return err("n must be non-negative");
        }
        if n > RANDOM_BYTES_CAP {
            return err("n exceeds 1 MiB cap");
        }
        let mut buf = vec![0u8; n as usize];
        if let Err(e) = getrandom::getrandom(&mut buf) {
            return err(format!("CSPRNG failure: {e}"));
        }
        ok(Value::Bytes(Arc::new(buf)))
    }

    // ── Timing-safe comparison ─────────────────────────────────────────

    fn constant_time_eq(a: Bytes, b: Bytes) -> bool {
        // Length mismatch short-circuits to false. Standard crypto-library
        // practice (cf. Python's hmac.compare_digest, Rust's subtle crate
        // for equal-length slices): the *lengths* leak via timing, but the
        // *contents* of equal-length buffers do not. Callers that need
        // length privacy should pad their inputs to a common fixed size
        // before calling. See the module-level comment.
        if a.len() != b.len() {
            return false;
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
        diff == 0
    }
}
