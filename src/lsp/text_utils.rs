//! Text-level scanning helper: the head name of a module-qualified
//! record or constructor, which has no span of its own in the AST.

/// Resolve the byte offset of the head NAME token in a module-qualified
/// head (`util.Pt { .. }`, `shapes.Circle(r)`). `head_offset` sits on the
/// module qualifier's first byte (the AST span of a qualified record
/// create or qualified constructor/record pattern starts at the
/// qualifier; the name has no span of its own); the name token follows
/// the first `.`. Returns `None` when the token cannot
/// be located — callers treat that as "no reference here" (conservative:
/// better to miss an edit than to corrupt the qualifier).
pub(super) fn qualified_head_name_offset(
    source: &str,
    head_offset: usize,
    name: &str,
) -> Option<usize> {
    if name.is_empty() || head_offset >= source.len() {
        return None;
    }
    // `get` instead of indexing: a span offset inside a multi-byte
    // character means "cannot be located", not a panic.
    let dot = source.get(head_offset..)?.find('.')? + head_offset;
    let bytes = source.as_bytes();
    let mut off = dot + 1;
    while off < bytes.len() && bytes[off].is_ascii_whitespace() {
        off += 1;
    }
    if !source.get(off..)?.starts_with(name) {
        return None;
    }
    // Whole-token check: the match must not continue as a longer ident.
    let end = off + name.len();
    if let Some(&b) = bytes.get(end)
        && (b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    Some(off)
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qualified_head_name_offset_inside_a_character_is_none() {
        // `é` occupies bytes 0..2.
        let source = "é.Pt";
        assert_eq!(qualified_head_name_offset(source, 1, "Pt"), None);
        assert_eq!(qualified_head_name_offset(source, 0, "Pt"), Some(3));
    }

    #[test]
    fn test_qualified_head_name_offset_finds_the_name_after_the_dot() {
        assert_eq!(
            qualified_head_name_offset("util.Pt { x: 1 }", 0, "Pt"),
            Some(5)
        );
        assert_eq!(qualified_head_name_offset("util. Pt", 0, "Pt"), Some(6));
        assert_eq!(qualified_head_name_offset("util.Ptx", 0, "Pt"), None);
    }
}
