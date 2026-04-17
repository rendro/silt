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
                        range: span_to_range(&f.span, &doc.source),
                        selection_range: span_to_range(&f.span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Type(t) => {
                    let kind = match &t.body {
                        TypeBody::Enum(_) => SymbolKind::ENUM,
                        TypeBody::Record(_) => SymbolKind::STRUCT,
                    };
                    symbols.push(DocumentSymbol {
                        name: t.name.to_string(),
                        detail: None,
                        kind,
                        range: span_to_range(&t.span, &doc.source),
                        selection_range: span_to_range(&t.span, &doc.source),
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
                        range: span_to_range(&t.span, &doc.source),
                        selection_range: span_to_range(&t.span, &doc.source),
                        tags: None,
                        deprecated: None,
                        children: None,
                    });
                }
                Decl::Let {
                    pattern,
                    span,
                    value,
                    ..
                } if matches!(pattern.kind, PatternKind::Ident(_)) => {
                    let name = match &pattern.kind {
                        PatternKind::Ident(n) => *n,
                        _ => unreachable!(),
                    };
                    let detail = value.ty.as_ref().map(|t| format!("{t}"));
                    symbols.push(DocumentSymbol {
                        name: name.to_string(),
                        detail,
                        kind: SymbolKind::VARIABLE,
                        range: span_to_range(span, &doc.source),
                        selection_range: span_to_range(span, &doc.source),
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
