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
use super::text_utils::expr_extent;

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
        Decl::Type(td) if cursor >= td.span.offset => {
            out.push(td.span);
        }
        Decl::Trait(t) if cursor >= t.span.offset => {
            out.push(t.span);
        }
        _ => {}
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
