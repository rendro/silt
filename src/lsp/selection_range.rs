//! `textDocument/selectionRange` — smart expand/shrink selection.
//!
//! For each cursor position the client sends, we return a chain of
//! expanding ranges: innermost enclosing expression outward to the
//! enclosing declaration. Editors cycle through the chain on
//! Shift+Alt+→ / Shift+Alt+←.
//!
//! Implementation walks the AST collecting every expression whose
//! extent covers the cursor offset, then chains them from innermost
//! to outermost.

use lsp_types::SelectionRange;

use crate::ast::*;
use crate::lexer::Span;

use super::Server;
use super::conversions::{position_to_offset, span_to_range};
use super::text_utils::{expr_extent, match_closing_brace};

impl Server {
    pub(super) fn selection_range(
        &self,
        params: lsp_types::SelectionRangeParams,
    ) -> Option<Vec<SelectionRange>> {
        let uri = &params.text_document.uri;
        let doc = self.documents.get(uri)?;
        let program = doc.program.as_ref()?;
        let source = &doc.source;

        let mut results = Vec::new();
        for pos in &params.positions {
            let cursor = position_to_offset(source, pos);
            let mut ranges: Vec<Span> = Vec::new();
            for decl in &program.decls {
                collect_decl_ranges(decl, source, cursor, &mut ranges);
            }
            if ranges.is_empty() {
                // Fall back to a degenerate range at the cursor.
                results.push(SelectionRange {
                    range: lsp_types::Range {
                        start: *pos,
                        end: *pos,
                    },
                    parent: None,
                });
                continue;
            }
            // Sort innermost-first by extent width (smaller first).
            ranges.sort_by_key(|sp| span_extent(sp, source));
            let chain = build_chain(&ranges, source);
            results.push(chain);
        }
        Some(results)
    }
}

fn span_extent(span: &Span, source: &str) -> usize {
    let (end, _) = (
        // Minimal: use offset + crude forward scan via source length clamp.
        source
            .len()
            .min(span.offset + span_width_guess(span, source)),
        span.offset,
    );
    end.saturating_sub(span.offset)
}

/// Very coarse span-width guess used only for sort ordering. The
/// authoritative extent is computed via expr_extent where possible; for
/// decl-level spans we fall back to the rest-of-source length.
fn span_width_guess(span: &Span, source: &str) -> usize {
    source.len().saturating_sub(span.offset)
}

fn build_chain(ranges: &[Span], source: &str) -> SelectionRange {
    let mut parent: Option<Box<SelectionRange>> = None;
    // Walk outermost → innermost, building parents as we go.
    for span in ranges.iter().rev() {
        let range = span_to_range(span, source);
        parent = Some(Box::new(SelectionRange { range, parent }));
    }
    match parent {
        Some(inner) => *inner,
        None => SelectionRange {
            range: lsp_types::Range::default(),
            parent: None,
        },
    }
}

// ── Decl walkers ───────────────────────────────────────────────────

fn collect_decl_ranges(decl: &Decl, source: &str, cursor: usize, out: &mut Vec<Span>) {
    match decl {
        Decl::Fn(f) => {
            let (end, _) = expr_extent(&f.body, source);
            if cursor >= f.span.offset && cursor <= end {
                out.push(f.span);
                collect_expr_ranges(&f.body, source, cursor, out);
            }
        }
        Decl::Let { value, span, .. } => {
            let (end, _) = expr_extent(value, source);
            if cursor >= span.offset && cursor <= end {
                out.push(*span);
                collect_expr_ranges(value, source, cursor, out);
            }
        }
        Decl::TraitImpl(ti) => {
            if ti.is_auto_derived {
                return;
            }
            for method in &ti.methods {
                let (end, _) = expr_extent(&method.body, source);
                if cursor >= method.span.offset && cursor <= end {
                    out.push(method.span);
                    collect_expr_ranges(&method.body, source, cursor, out);
                }
            }
        }
        Decl::Type(td) => {
            // Round-83 GAP fix: bound the cursor on BOTH sides with the
            // decl's actual extent. `td.span` is just the `type` keyword
            // span (4 bytes); without an upper bound we'd push this
            // unrelated keyword span into the chain whenever the cursor
            // appears AFTER the decl in source order, making it the
            // outermost element of an unrelated chain (chain parent does
            // not enclose its child — Shift+Alt+→ semantics broken).
            let end = type_decl_extent(td, source);
            if cursor >= td.span.offset && cursor <= end {
                out.push(td.span);
            }
        }
        Decl::Trait(t) => {
            // Round-83 GAP fix: same as Decl::Type — `t.span` is just the
            // `trait` keyword (5 bytes). The trait body is always
            // brace-delimited, so scan for the matching `}`.
            let end = match_closing_brace(source, t.span.offset).unwrap_or(source.len());
            if cursor >= t.span.offset && cursor <= end {
                out.push(t.span);
            }
        }
        _ => {}
    }
}

/// End offset (exclusive) of a `type` declaration in source.
///
/// For brace-bodied types (`type Foo { ... }` — records and enums) this
/// scans forward for the matching `}`. For aliases (`type Foo = ...`)
/// there is no brace body in source; we scan to end-of-line, which is
/// the canonical decl terminator for single-line aliases. Multi-line
/// aliases are rare; falling back to end-of-line on those still gives a
/// non-zero extent that correctly excludes the rest of the file.
fn type_decl_extent(td: &TypeDecl, source: &str) -> usize {
    match &td.body {
        TypeBody::Record(_) | TypeBody::Enum(_) => {
            match_closing_brace(source, td.span.offset).unwrap_or(source.len())
        }
        TypeBody::Alias(_) => {
            // Alias: scan from the `type` keyword to the next newline.
            let bytes = source.as_bytes();
            let start = td.span.offset.min(bytes.len());
            let mut i = start;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            i
        }
    }
}

fn collect_expr_ranges(expr: &Expr, source: &str, cursor: usize, out: &mut Vec<Span>) {
    let (end, _) = expr_extent(expr, source);
    if cursor < expr.span.offset || cursor > end {
        return;
    }
    out.push(expr.span);
    super::ast_walk::visit_expr_children(expr, |child| {
        collect_expr_ranges(child, source, cursor, out);
    });
}
