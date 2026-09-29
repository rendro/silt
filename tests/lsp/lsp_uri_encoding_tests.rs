//! Locks the `file://` URI ↔ `PathBuf` conversion helpers inside the
//! LSP server against percent-encoding regressions.
//!
//! Pre-fix the helpers were ASCII-only and silently broke any workspace
//! whose path contained a space or non-ASCII byte:
//!
//! 1. `file_uri_to_path` fed the raw post-`file://` slice into
//!    `PathBuf::from`, so `file:///home/klaus/My%20Project` became the
//!    literal directory `My%20Project` — `fs::read_dir` then returned
//!    `NotFound` and the workspace preload dropped every file.
//!
//! 2. `path_to_file_uri` formatted the raw path into `file://...`
//!    without encoding, so spaces (an invalid URI character per RFC
//!    3986) made `Uri::from_str` return `Err(...)` and the file was
//!    silently skipped from preload.
//!
//! Every test here is designed to fail pre-fix and pass post-fix.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use lsp_types::Uri;

use silt::lsp::{file_uri_to_path, path_to_file_uri};

/// VSCode sends `rootUri` with spaces percent-encoded. Pre-fix we
/// returned `PathBuf::from("/home/klaus/My%20Project")` — a directory
/// that virtually never exists. Post-fix we percent-decode first.
#[test]
fn file_uri_decodes_percent_encoded_spaces() {
    let got = file_uri_to_path("file:///home/klaus/My%20Project")
        .expect("file:// URI with a single path component must parse");
    assert_eq!(got, PathBuf::from("/home/klaus/My Project"));
}

/// Non-ASCII (here: Cyrillic `тест` = `test` in Russian) is percent-
/// encoded as UTF-8 byte sequences. Pre-fix the decoded path kept the
/// literal `%D1%82...` bytes; post-fix we get the actual characters.
#[test]
fn file_uri_decodes_utf8_percent_encoded() {
    let got = file_uri_to_path("file:///tmp/%D1%82%D0%B5%D1%81%D1%82")
        .expect("UTF-8 percent-encoded URI must decode");
    assert_eq!(got, PathBuf::from("/tmp/\u{0442}\u{0435}\u{0441}\u{0442}"));
    // Sanity: the literal form we're decoding away must NOT survive.
    assert!(!got.to_string_lossy().contains('%'));
}

/// `Uri::from_str` rejects bare spaces. Pre-fix we fed the raw path to
/// `Uri::from_str`, which returned `Err`, which bubbled up to an
/// `Option::None` and silently skipped the file from preload. Post-fix
/// we percent-encode path bytes, so the `Uri` parses and the URI
/// string carries `My%20Project` so downstream clients can round-trip
/// it.
#[test]
fn path_to_file_uri_encodes_spaces() {
    // Use an obviously-bogus path so `canonicalize` fails and
    // `path_to_file_uri` falls back to the raw path (otherwise the
    // test would depend on the filesystem state of the machine).
    let uri = path_to_file_uri(Path::new("/nonexistent-sentinel/My Project/foo.silt"))
        .expect("encoded URI with a space must parse via Uri::from_str");

    let rendered = uri.as_str();
    assert!(
        rendered.contains("My%20Project"),
        "URI should carry %20 for the space, got {rendered:?}"
    );
    // And the round-trip through `Uri::from_str` from the rendered
    // form must also succeed — this is the load-bearing check, since
    // it's exactly what LSP clients do on the other end.
    assert!(Uri::from_str(rendered).is_ok());
}

/// Full round-trip: path → URI → path should be identity for a path
/// containing both spaces and non-ASCII. Pre-fix, step 1 would return
/// `None` for the space, so this never got a chance to run; post-fix
/// the round-trip is clean.
#[test]
fn round_trip_uri_path_with_spaces_and_unicode() {
    let original =
        PathBuf::from("/nonexistent-sentinel/My Project/\u{0442}\u{0435}\u{0441}\u{0442}.silt");
    let uri = path_to_file_uri(&original).expect("encode must succeed");
    let rendered = uri.as_str();
    let decoded = file_uri_to_path(rendered).expect("decode must succeed");
    assert_eq!(decoded, original);
}

/// The Windows leading-slash drop (turns `file:///C:/foo` into
/// `C:/foo`) happens *before* percent-decoding. This test locks that
/// the percent-decode step does not interfere: on Windows we still
/// drop the leading `/` and then decode `foo%20bar`; on Unix we keep
/// the leading `/` (`PathBuf::from("/C:/foo bar")` — which is a valid
/// Unix path that just happens to contain a colon).
///
/// The key property we want to guarantee is that the encoded
/// `foo%20bar` segment becomes a real `foo bar` segment regardless of
/// platform — pre-fix, even on Windows, the decode didn't happen and
/// the directory name stayed `foo%20bar`.
#[test]
fn leading_slash_drop_on_windows_style_uri_still_works() {
    let got = file_uri_to_path("file:///C:/foo%20bar")
        .expect("Windows-style URI must parse on every platform");

    #[cfg(windows)]
    {
        assert_eq!(got, PathBuf::from("C:/foo bar"));
    }
    #[cfg(not(windows))]
    {
        // On Unix the leading `/` is preserved (the path isn't a
        // "Windows drive" on this host). What we care about is that
        // the `%20` got decoded to a space — which pre-fix it did not.
        assert_eq!(got, PathBuf::from("/C:/foo bar"));
    }

    // Cross-platform invariant: the literal `%20` must be gone.
    assert!(
        !got.to_string_lossy().contains("%20"),
        "percent-decoding must have run; got {got:?}"
    );
}
