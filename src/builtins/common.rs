//! Shared helper functions used by several builtin modules.

use crate::typeinfo::bv;
use crate::value::{MAX_TEXT_LEN, Value};
use crate::vm::VmError;

/// The most bytes one read of a connection takes (`tcp.read`, a chunk
/// of `stream.tcp_chunks`), and one read of a file of no known length
/// (`stream.file_chunks`): a caller may ask for more, and gets what one
/// read of this size gives. The buffer of a read is for what can
/// arrive, never for the number asked for.
pub(crate) const READ_AT_ONCE: usize = 64 * 1024;

/// A count, width or position cut to the range there is: a negative
/// one is 0. (One past the end the caller cuts to the end.)
pub(crate) fn cut(n: i64) -> usize {
    usize::try_from(n.max(0)).unwrap_or(usize::MAX)
}

/// A text that a builtin writes, which holds at most
/// [`MAX_TEXT_LEN`] bytes: a write that would make it longer
/// fails, and the text says how long it would have been at least. As
/// [`std::fmt::Write`] for what a formatter writes, as
/// [`std::io::Write`] for an encoder that writes bytes (which must be
/// UTF-8).
#[derive(Default)]
pub(crate) struct Bounded {
    bytes: Vec<u8>,
    over: Option<usize>,
}

impl Bounded {
    fn take(&mut self, part: &[u8]) -> bool {
        let len = self.bytes.len().saturating_add(part.len());
        if len > MAX_TEXT_LEN {
            self.over = Some(len);
            return false;
        }
        self.bytes.extend_from_slice(part);
        true
    }

    /// The text, or the error `name` gives for one too long: what was
    /// written, if no write failed, or the failure `failed` is (the
    /// writer's own, when it was not the length).
    pub(crate) fn text(self, name: &str, failed: Option<String>) -> Result<String, VmError> {
        if let Some(len) = self.over {
            return Err(too_long_text(name, len));
        }
        if let Some(failure) = failed {
            return Err(VmError::new(failure));
        }
        String::from_utf8(self.bytes).map_err(|_| {
            VmError::new(format!(
                "{name}: internal error: the text written is not UTF-8"
            ))
        })
    }
}

impl std::fmt::Write for Bounded {
    fn write_str(&mut self, part: &str) -> std::fmt::Result {
        match self.take(part.as_bytes()) {
            true => Ok(()),
            false => Err(std::fmt::Error),
        }
    }
}

impl std::io::Write for Bounded {
    fn write(&mut self, part: &[u8]) -> std::io::Result<usize> {
        match self.take(part) {
            true => Ok(part.len()),
            false => Err(std::io::Error::other("the text is too long")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What `name` says of a string of `len` bytes, more than a string may
/// hold: `<name>: result would exceed maximum string size (N bytes >
/// 10000000 limit)` (1000000 in WebAssembly). (Where the text was stopped as it was written,
/// `len` is as far as it had come: it would have been longer.)
pub(crate) fn too_long_text(name: &str, len: usize) -> VmError {
    VmError::new(format!(
        "{name}: result would exceed maximum string size ({len} bytes > {MAX_TEXT_LEN} limit)"
    ))
}

pub(crate) fn ok(v: Value) -> Value {
    Value::variant(bv::OK, [v])
}

/// Convert a 4-bit nibble (0..=15) to its ASCII hex character.
///
/// It replaces two near-identical helpers — `bytes::hex_char`
/// (lowercase) and `encoding::upper_hex` (uppercase) — that differed
/// only in the alphabetic case of the `'a'..='f'` digits. The two old
/// bodies are both implemented here so byte-identical output is
/// preserved at every existing call site.
///
/// `pub(crate)` so internal callers (and any future in-crate test) can
/// reference the helper. The goldens
/// `tests/golden/lang/builtins/builtin_nibble_to_hex_helper__*` prove the deletion was
/// a semantic no-op by driving both call paths through the public
/// builtin API (`bytes.to_hex` and `encoding.form_encode`).
pub(crate) fn nibble_to_hex(n: u8, uppercase: bool) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => {
            let base = if uppercase { b'A' } else { b'a' };
            (base + (n - 10)) as char
        }
        _ => unreachable!("nibble_to_hex called with n > 15"),
    }
}

/// The reason of a failed TLS handshake in the program's words, from the
/// text of the TLS library's error (`raw`), which writes the library's
/// own names (`invalid peer certificate: Other(OtherError(
/// CaUsedAsEndEntity))`). `peer` is who sent what was refused: "the
/// server" or "the client". A text this does not know is kept.
#[cfg(any(feature = "http", feature = "tcp-tls"))]
pub(crate) fn tls_reason(raw: &str, peer: &str) -> String {
    if let Some(cert) = raw.strip_prefix("invalid peer certificate: ") {
        // (The library writes some of its reasons in words, `certificate
        // expired: ...`, and the others by name, `Expired`.)
        let is = |name: &str, words: &str| cert.starts_with(name) || cert.starts_with(words);
        let why = if cert.contains("CaUsedAsEndEntity") {
            "is self-signed, not signed by an authority this program trusts".to_string()
        } else if is("UnknownIssuer", "certificate issuer unknown") {
            "is not signed by an authority this program trusts".to_string()
        } else if is("Expired", "certificate expired") {
            "has expired".to_string()
        } else if is("NotValidYet", "certificate not valid yet") {
            "is not valid yet".to_string()
        } else if is("NotValidForName", "certificate not valid for name") {
            "is not for this host name".to_string()
        } else if is("Revoked", "certificate revoked") {
            "is revoked".to_string()
        } else if is("BadSignature", "certificate bad signature") {
            "has a signature that does not verify".to_string()
        } else if is("BadEncoding", "certificate bad encoding") {
            "cannot be read".to_string()
        } else if is("InvalidPurpose", "certificate does not allow") {
            "is not for this use".to_string()
        } else if cert.starts_with("certificate ") {
            format!("is refused: {cert}")
        } else {
            let name = cert.trim_start_matches("Other(OtherError(");
            format!("is refused: {}", library_name_in_words(name))
        };
        return format!("{peer}'s certificate {why}");
    }
    if raw.starts_with("received corrupt message") {
        return format!("what {peer} sent is not TLS");
    }
    for (prefix, said) in [
        ("received fatal alert: ", "ended the handshake"),
        ("peer is incompatible: ", "is incompatible"),
        ("peer misbehaved: ", "broke the protocol"),
    ] {
        if let Some(name) = raw.strip_prefix(prefix) {
            return format!("{peer} {said}: {}", library_name_in_words(name));
        }
    }
    raw.to_string()
}

/// A name of the TLS library's (`HandshakeFailure`) in words
/// (`handshake failure`); any part after the name is dropped.
#[cfg(any(feature = "http", feature = "tcp-tls"))]
fn library_name_in_words(name: &str) -> String {
    let name = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or(name);
    let mut words = String::new();
    for (i, c) in name.char_indices() {
        if c.is_ascii_uppercase() && i > 0 {
            words.push(' ');
        }
        words.push(c.to_ascii_lowercase());
    }
    words
}

#[cfg(all(test, any(feature = "http", feature = "tcp-tls")))]
mod tls_reason_tests {
    use super::tls_reason;

    #[test]
    fn the_library_s_names_are_said_in_words() {
        let cases = [
            (
                "invalid peer certificate: Other(OtherError(CaUsedAsEndEntity))",
                "the server's certificate is self-signed, not signed by an authority this \
                 program trusts",
            ),
            (
                "invalid peer certificate: UnknownIssuer",
                "the server's certificate is not signed by an authority this program trusts",
            ),
            (
                "invalid peer certificate: certificate expired: verification time 1 (UNIX), but certificate is not valid after 0 (1 seconds ago)",
                "the server's certificate has expired",
            ),
            (
                "invalid peer certificate: certificate not valid for name \"a\"; certificate is not valid for any names (according to its subjectAltName extension)",
                "the server's certificate is not for this host name",
            ),
            (
                "invalid peer certificate: Other(OtherError(SomethingElse))",
                "the server's certificate is refused: something else",
            ),
            (
                "received corrupt message of type InvalidContentType",
                "what the server sent is not TLS",
            ),
            (
                "received fatal alert: HandshakeFailure",
                "the server ended the handshake: handshake failure",
            ),
            (
                "peer is incompatible: NoCipherSuitesInCommon",
                "the server is incompatible: no cipher suites in common",
            ),
            ("connection reset", "connection reset"),
        ];
        for (raw, want) in cases {
            assert_eq!(tls_reason(raw, "the server"), want, "{raw}");
        }
    }
}
