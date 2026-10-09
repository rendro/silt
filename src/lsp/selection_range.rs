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
use crate::source::{SourceFile, Span};

use super::Server;
use super::conversions::{offsets_to_range, position_to_offset};

/// A byte-offset extent `[start, end)` collected during the AST walk:
/// the extent of a node's span. A decl-level element of the chain
/// encloses the cursor (round-84 lock; tier-2
/// `selection_range_returns_nested_chain`).
#[derive(Copy, Clone)]
struct Extent {
    start: usize,
    end: usize,
}

impl Extent {
    fn of(span: Span) -> Self {
        Extent {
            start: span.start as usize,
            end: span.end as usize,
        }
    }

    fn width(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the extent holds `cursor` (its end included, so a cursor
    /// right after the last character still selects the node).
    fn holds(&self, cursor: usize) -> bool {
        cursor >= self.start && cursor <= self.end
    }
}

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
            let mut ranges: Vec<Extent> = Vec::new();
            for decl in &program.decls {
                collect_decl_ranges(decl, cursor, &mut ranges);
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
            ranges.sort_by_key(|e| e.width());
            let chain = build_chain(&ranges, source);
            results.push(chain);
        }
        Some(results)
    }
}

fn build_chain(ranges: &[Extent], source: &SourceFile) -> SelectionRange {
    let mut parent: Option<Box<SelectionRange>> = None;
    // Walk outermost → innermost, building parents as we go.
    for ext in ranges.iter().rev() {
        let range = offsets_to_range(source, ext.start, ext.end);
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

fn collect_decl_ranges(decl: &Decl, cursor: usize, out: &mut Vec<Extent>) {
    match decl {
        Decl::Fn(f) => try_push_body_extent(f.span, &f.body, cursor, out),
        Decl::Let { value, span, .. } => try_push_body_extent(*span, value, cursor, out),
        Decl::TraitImpl(ti) => {
            for method in &ti.methods {
                try_push_body_extent(method.span, &method.body, cursor, out);
            }
        }
        // Round-83 GAP fix: the decl is pushed only when it holds the
        // cursor, so an unrelated decl never becomes the outermost
        // element of a chain (a chain parent encloses its child —
        // Shift+Alt+→ semantics).
        Decl::Type(TypeDecl { span, .. }) | Decl::Trait(TraitDecl { span, .. }) => {
            let extent = Extent::of(*span);
            if extent.holds(cursor) {
                out.push(extent);
            }
        }
        _ => {}
    }
}

fn collect_expr_ranges(expr: &Expr, cursor: usize, out: &mut Vec<Extent>) {
    let extent = Extent::of(expr.span);
    if !extent.holds(cursor) {
        return;
    }
    out.push(extent);
    super::ast_walk::visit_expr_children(expr, |child| {
        collect_expr_ranges(child, cursor, out);
    });
}

/// Shared boilerplate used by `Decl::Fn`, `Decl::Let`, and each
/// `Decl::TraitImpl` method arm: push the decl's extent when it holds
/// the cursor, and recurse into the body for nested expression ranges.
/// Round-85 BLOAT-DUP fix.
fn try_push_body_extent(decl_span: Span, body: &Expr, cursor: usize, out: &mut Vec<Extent>) {
    let extent = Extent::of(decl_span);
    if extent.holds(cursor) {
        out.push(extent);
        collect_expr_ranges(body, cursor, out);
    }
}
