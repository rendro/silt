//! Round-76 V2 lock: the `regex_cache` field doc on `Vm` must
//! describe FIFO eviction, not "LRU-like".
//!
//! Pre-fix (src/vm/mod.rs:146): the field-level doc claimed
//! "LRU-like eviction", but `RegexCache::get` (src/vm/runtime.rs:382-419)
//! is pure FIFO — `self.order` is only updated on miss
//! (line 415, `self.order.push_back(pattern.to_string())`); a hit
//! does NOT re-insert the pattern at the back of the deque. So the
//! cache is FIFO, not LRU. The struct-level doc on `RegexCache`
//! itself was already correct ("Tracks insertion order with a
//! `VecDeque`...") — only the field-level doc was wrong.
//!
//! Post-fix: the field doc says "FIFO" / "first-in first-out" and
//! omits the misleading "LRU-like" wording.
//!
//! Lock strategy: source-grep. The doc is stable string content with
//! no other observable signal, so a grep over `src/vm/mod.rs` is the
//! right shape of lock for this round-76 doc-currency fix.

use std::fs;

const VM_MOD_PATH: &str = "src/vm/mod.rs";

/// Read the source file. Tests run with `cargo test`, whose cwd is the
/// crate root, so a relative path is correct here.
fn read_vm_mod() -> String {
    fs::read_to_string(VM_MOD_PATH).unwrap_or_else(|e| panic!("failed to read {VM_MOD_PATH}: {e}"))
}

/// Find the field-doc block immediately preceding `pub(crate) regex_cache:`
/// in src/vm/mod.rs. Returns the joined doc lines so subsequent
/// assertions can check wording without snapping to one specific phrasing.
fn extract_regex_cache_field_doc(src: &str) -> String {
    // Walk lines, collect the run of `///` doc lines that precede the
    // `regex_cache:` field declaration. We tolerate any indentation.
    let mut doc_lines: Vec<&str> = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut field_idx: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.contains("regex_cache:") && line.contains("RegexCache") {
            field_idx = Some(i);
            break;
        }
    }
    let field_idx = field_idx.expect(
        "expected to find `regex_cache: ... RegexCache` field declaration in src/vm/mod.rs",
    );

    // Walk backwards collecting contiguous `///` doc lines.
    let mut i = field_idx;
    while i > 0 {
        i -= 1;
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("///") {
            doc_lines.push(lines[i]);
        } else if trimmed.is_empty() {
            // Blank line ends the doc block.
            break;
        } else {
            // Non-doc, non-blank line ends the block.
            break;
        }
    }
    doc_lines.reverse();
    doc_lines.join("\n")
}

#[test]
fn regex_cache_field_doc_no_longer_says_lru() {
    let src = read_vm_mod();
    let doc = extract_regex_cache_field_doc(&src);
    assert!(
        !doc.to_ascii_lowercase().contains("lru"),
        "REGRESSION: regex_cache field doc still mentions 'LRU' — but \
         the impl in src/vm/runtime.rs is pure FIFO (RegexCache::get \
         only pushes to `order` on miss; hits do not re-order). \
         Doc currently:\n{doc}"
    );
}

#[test]
fn regex_cache_field_doc_says_fifo() {
    let src = read_vm_mod();
    let doc = extract_regex_cache_field_doc(&src);
    let lower = doc.to_ascii_lowercase();
    assert!(
        lower.contains("fifo") || lower.contains("first-in first-out"),
        "regex_cache field doc must describe the eviction order as FIFO \
         (or 'first-in first-out') so it matches the impl. Doc \
         currently:\n{doc}"
    );
}
