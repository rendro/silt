//! Source-grep regression locks for round-80 visibility fixes.
//!
//! Finding (round 80): four items in `src/parser.rs` and `src/lockfile.rs`
//! were declared `pub` despite having no callers outside the crate and no
//! re-exports through `src/lib.rs`. The audit demoted them to `pub(crate)`
//! so the public surface stays minimal — accidental future re-promotion
//! would silently grow the API.
//!
//! Verification at the time of the demotion:
//!   * `src/lib.rs` does not re-export `DocIndex`,
//!     `DocIndex::from_source`, `DocIndex::doc_for_decl_at_line`, or
//!     `checksum_path_source`.
//!   * `rg` over `tests/`, `benches/`, and `examples/` showed no usages
//!     outside the crate.
//!
//! These tests are pure source greps: each asserts the exact
//! `pub(crate) ...` wording is present and the bare `pub ...` form is
//! absent for each item. If a future patch re-promotes any of the four,
//! these locks fail loudly.

const PARSER_RS: &str = include_str!("../src/parser.rs");
const LOCKFILE_RS: &str = include_str!("../src/lockfile.rs");

#[test]
fn doc_index_struct_is_pub_crate() {
    assert!(
        PARSER_RS.contains("pub(crate) struct DocIndex {"),
        "expected `pub(crate) struct DocIndex {{` in src/parser.rs; \
         visibility lock fired — was the struct re-promoted to bare `pub`?"
    );
    assert!(
        !PARSER_RS.contains("pub struct DocIndex {"),
        "src/parser.rs declares `pub struct DocIndex`; round-80 audit demoted \
         this to `pub(crate)` because no caller outside the crate uses it"
    );
}

#[test]
fn doc_index_from_source_is_pub_crate() {
    assert!(
        PARSER_RS.contains("pub(crate) fn from_source(source: &str) -> Self {"),
        "expected `pub(crate) fn from_source(source: &str) -> Self` in \
         src/parser.rs (DocIndex::from_source); visibility lock fired"
    );
    assert!(
        !PARSER_RS.contains("pub fn from_source(source: &str) -> Self {"),
        "src/parser.rs declares `pub fn from_source` on DocIndex; round-80 \
         audit demoted this to `pub(crate)`"
    );
}

#[test]
fn doc_for_decl_at_line_is_pub_crate() {
    assert!(
        PARSER_RS.contains(
            "pub(crate) fn doc_for_decl_at_line(&self, decl_line: usize) -> Option<String> {"
        ),
        "expected `pub(crate) fn doc_for_decl_at_line` in src/parser.rs; \
         visibility lock fired"
    );
    assert!(
        !PARSER_RS
            .contains("pub fn doc_for_decl_at_line(&self, decl_line: usize) -> Option<String> {"),
        "src/parser.rs declares `pub fn doc_for_decl_at_line`; round-80 \
         audit demoted this to `pub(crate)`"
    );
}

#[test]
fn checksum_path_source_is_pub_crate() {
    assert!(
        LOCKFILE_RS.contains(
            "pub(crate) fn checksum_path_source(pkg_root: &Path) -> Result<String, std::io::Error> {"
        ),
        "expected `pub(crate) fn checksum_path_source` in src/lockfile.rs; \
         visibility lock fired"
    );
    assert!(
        !LOCKFILE_RS.contains(
            "pub fn checksum_path_source(pkg_root: &Path) -> Result<String, std::io::Error> {"
        ),
        "src/lockfile.rs declares `pub fn checksum_path_source`; round-80 \
         audit demoted this to `pub(crate)`"
    );
}
