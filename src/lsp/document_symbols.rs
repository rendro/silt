//! `textDocument/documentSymbol` handler.

use lsp_types::{DocumentSymbol, DocumentSymbolResponse, Range, SymbolKind};

use crate::ast::*;
use crate::lexer::Span;

use super::Server;
use super::conversions::{offset_to_position, span_to_position, span_to_range};
use super::text_utils::match_closing_brace;

impl Server {
    // ── Document symbols ──────────────────────────────────────────

    #[allow(deprecated)] // DocumentSymbol::deprecated field
    pub(super) fn document_symbols(
        &self,
        params: lsp_types::DocumentSymbolParams,
    ) -> Option<DocumentSymbolResponse> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;

        let mut symbols = Vec::new();
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => {
                    let detail = doc
                        .definitions
                        .get(&f.name)
                        .and_then(|d| d.ty.as_ref())
                        .map(|t| format!("{t}"));
                    symbols.push(DocumentSymbol {
                        name: f.name.to_string(),
                        detail,
                        kind: SymbolKind::FUNCTION,
                        // Round-80 G1: per LSP spec, `range` should
                        // encompass the entire declaration (so the
                        // editor's "go to symbol" navigates to the
                        // whole fn) while `selectionRange` is just the
                        // identifier (so the highlight on selection is
                        // the name, not the keyword).
                        range: decl_full_range(&f.span, &doc.source),
                        selection_range: span_to_range(&f.name_span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Type(t) => {
                    let kind = match &t.body {
                        TypeBody::Enum(_) => SymbolKind::ENUM,
                        TypeBody::Record(_) => SymbolKind::STRUCT,
                        // Phase D: type aliases (`type Bytes = List(Int)`)
                        // surface as a generic TYPE_PARAMETER symbol — they
                        // don't form a new nominal class but the editor
                        // should still see them in the document outline.
                        TypeBody::Alias(_) => SymbolKind::TYPE_PARAMETER,
                    };
                    symbols.push(DocumentSymbol {
                        name: t.name.to_string(),
                        detail: None,
                        kind,
                        // Round-80 G1: `range` covers the whole decl,
                        // `selectionRange` covers only the type name.
                        range: decl_full_range(&t.span, &doc.source),
                        selection_range: span_to_range(&t.name_span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Trait(t) => {
                    symbols.push(DocumentSymbol {
                        name: t.name.to_string(),
                        detail: None,
                        kind: SymbolKind::INTERFACE,
                        // Round-80 G1: `range` covers the whole decl,
                        // `selectionRange` covers only the trait name.
                        range: decl_full_range(&t.span, &doc.source),
                        selection_range: span_to_range(&t.name_span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Let {
                    pattern,
                    span,
                    name_span,
                    value,
                    ..
                } if matches!(pattern.kind, PatternKind::Ident(_)) => {
                    let name = match &pattern.kind {
                        PatternKind::Ident(n) => *n,
                        _ => unreachable!(),
                    };
                    let detail = value.ty.as_ref().map(|t| format!("{t}"));
                    // Round-80 G1: `range` should cover the whole
                    // let-binding declaration. A bare `let x = expr`
                    // typically fits on one line and has no body block,
                    // so `decl_full_range` falls back to a single-token
                    // range at the keyword. We extend it to cover the
                    // full source line containing the let so the range
                    // is semantically distinct from the identifier-only
                    // `selectionRange`. `selectionRange` uses the
                    // parser-recorded `name_span` (round-71 DX-1) when
                    // present, otherwise the pattern span.
                    let range = let_decl_range(span, &doc.source);
                    let sel_span = name_span.unwrap_or(pattern.span);
                    symbols.push(DocumentSymbol {
                        name: name.to_string(),
                        detail,
                        kind: SymbolKind::VARIABLE,
                        range,
                        selection_range: span_to_range(&sel_span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                // Trait implementations surface in the outline as
                // `impl <Trait> for <Target>`. The editor's document-symbol
                // panel would otherwise skip them entirely, making impl
                // blocks invisible when navigating a file. We use the
                // descriptive `impl ... for ...` name (mirroring the
                // idiomatic outline caption other editors use) even though
                // silt's source syntax is `trait X for Y` — the outline
                // caption should reflect what the declaration does, not
                // the keyword it starts with. The range spans the whole
                // impl block so clicking the symbol jumps to it.
                Decl::TraitImpl(ti) => {
                    if ti.is_auto_derived {
                        continue;
                    }
                    // Round-80 G1: an impl block has no single
                    // identifier (the synthesized "impl Trait for
                    // Target" caption spans two identifiers in the
                    // source), so `selectionRange` is the same as
                    // `range`. This is the LSP-spec-compliant fallback
                    // when no single name is available — clients display
                    // the entire decl as the highlight target.
                    let range = decl_full_range(&ti.span, &doc.source);
                    symbols.push(DocumentSymbol {
                        name: format!("impl {} for {}", ti.trait_name, ti.target_type),
                        detail: None,
                        kind: SymbolKind::NAMESPACE,
                        range,
                        selection_range: range,
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                _ => {}
            }
        }

        Some(DocumentSymbolResponse::Nested(symbols))
    }
}

/// Compute an LSP `Range` covering an entire declaration starting at
/// `start_span` (the keyword token). Walks the source forward from
/// `start_span.offset` to the matching `}` of the decl's body block.
/// Falls back to a single-token range at the keyword when the decl has
/// no body block (e.g. a `type` alias or a recovery stub).
///
/// Round-80 G1: `range` (per LSP spec) should encompass the whole
/// symbol so editors can navigate to / highlight the entire decl,
/// distinct from `selectionRange` which is just the identifier.
fn decl_full_range(start_span: &Span, source: &str) -> Range {
    let start = span_to_position(start_span, source);
    if let Some(end_offset) = match_closing_brace(source, start_span.offset) {
        // `match_closing_brace` returns the offset just past the `}`.
        // Convert back to a position; we synthesize a Span with the
        // computed line so `span_to_position` does the UTF-16 column
        // walk correctly.
        let end_pos = offset_to_position(source, end_offset);
        return Range::new(start, end_pos);
    }
    // No body block — fall back to the single-token range we always
    // produced before round 80.
    span_to_range(start_span, source)
}

/// Compute an LSP `Range` covering a top-level `let` declaration.
/// `let` bindings have no body block; the source typically reads
/// `let name = expr` on a single line. We extend the range to the end
/// of the line containing `start_span.offset` so the `range` is
/// semantically distinct from the identifier-only `selectionRange`.
/// Multi-line let RHSs (e.g. `let x = match ... { ... }`) are still
/// covered correctly because the line walk picks up everything between
/// the `let` keyword and the next newline at the SAME nesting depth as
/// the keyword — for the common case (single-line let) that's the rest
/// of the line, which contains the entire RHS expression.
fn let_decl_range(start_span: &Span, source: &str) -> Range {
    let start = span_to_position(start_span, source);
    let bytes = source.as_bytes();
    let start_off = start_span.offset.min(bytes.len());
    // Walk forward until end-of-line at depth 0 (so braces / parens on
    // a single-line let with a record/list literal don't terminate the
    // walk early). For multi-line lets, fall back to the matching `}`
    // if the very first non-whitespace character after the `=` is `{`
    // (block expression body); otherwise use the line end.
    let mut i = start_off;
    let mut depth: i32 = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' && depth == 0 {
            break;
        }
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = (depth - 1).max(0),
            _ => {}
        }
        i += 1;
    }
    let end_pos = offset_to_position(source, i);
    Range::new(start, end_pos)
}
