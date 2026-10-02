//! `textDocument/documentSymbol` handler.

use lsp_types::{DocumentSymbol, DocumentSymbolResponse, SymbolKind};

use crate::ast::*;

use super::Server;
use super::conversions::span_to_range;

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
                        range: span_to_range(&f.span, &doc.source),
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
                        range: span_to_range(&t.span, &doc.source),
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
                        range: span_to_range(&t.span, &doc.source),
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
                    // Round-80 G1: `range` covers the whole let-binding
                    // declaration, its value included; `selectionRange`
                    // uses the parser-recorded `name_span` (round-71
                    // DX-1) when present, otherwise the pattern span.
                    let range = span_to_range(span, &doc.source);
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
                    let range = span_to_range(&ti.span, &doc.source);
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
