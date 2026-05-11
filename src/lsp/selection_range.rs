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

use super::Server;
use super::conversions::{offset_to_position, position_to_offset};
use super::text_utils::{expr_extent, match_closing_brace};

/// A byte-offset extent `[start, end)` collected during the AST walk.
/// Replaces the previous `Vec<Span>` representation, which only carried
/// the start offset and forced downstream `span_to_range` to derive an
/// end via token-length lookup — that gave a 2-byte range for `fn`
/// decls regardless of body extent. Carrying the explicit end here
/// produces decl-level chain elements that actually ENCLOSE the cursor
/// (round-84 lock; tier-2 `selection_range_returns_nested_chain`).
#[derive(Copy, Clone)]
struct Extent {
    start: usize,
    end: usize,
}

impl Extent {
    fn width(&self) -> usize {
        self.end.saturating_sub(self.start)
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
            ranges.sort_by_key(|e| e.width());
            let chain = build_chain(&ranges, source);
            results.push(chain);
        }
        Some(results)
    }
}

fn build_chain(ranges: &[Extent], source: &str) -> SelectionRange {
    let mut parent: Option<Box<SelectionRange>> = None;
    // Walk outermost → innermost, building parents as we go.
    for ext in ranges.iter().rev() {
        let range = lsp_types::Range::new(
            offset_to_position(source, ext.start),
            offset_to_position(source, ext.end),
        );
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

fn collect_decl_ranges(decl: &Decl, source: &str, cursor: usize, out: &mut Vec<Extent>) {
    match decl {
        Decl::Fn(f) => {
            try_push_body_extent(&f.body, f.span.offset, cursor, out, source);
        }
        Decl::Let { value, span, .. } => {
            try_push_body_extent(value, span.offset, cursor, out, source);
        }
        Decl::TraitImpl(ti) => {
            if ti.is_auto_derived {
                return;
            }
            for method in &ti.methods {
                try_push_body_extent(&method.body, method.span.offset, cursor, out, source);
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
                out.push(Extent {
                    start: td.span.offset,
                    end,
                });
            }
        }
        Decl::Trait(t) => {
            // Round-83 GAP fix: same as Decl::Type — `t.span` is just the
            // `trait` keyword (5 bytes). The trait body is always
            // brace-delimited, so scan for the matching `}`.
            let end = match_closing_brace(source, t.span.offset).unwrap_or(source.len());
            if cursor >= t.span.offset && cursor <= end {
                out.push(Extent {
                    start: t.span.offset,
                    end,
                });
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

fn collect_expr_ranges(expr: &Expr, source: &str, cursor: usize, out: &mut Vec<Extent>) {
    let end = expr_extent(expr, source);
    if cursor < expr.span.offset || cursor > end {
        return;
    }
    out.push(Extent {
        start: expr.span.offset,
        end,
    });
    super::ast_walk::visit_expr_children(expr, |child| {
        collect_expr_ranges(child, source, cursor, out);
    });
}

/// Shared body-extent boilerplate used by `Decl::Fn`, `Decl::Let`, and each
/// `Decl::TraitImpl` method arm: compute the body's end offset, push the
/// `[decl_start, end]` extent when the cursor is enclosed, and recurse into
/// the body for nested expression ranges. Round-85 BLOAT-DUP fix.
fn try_push_body_extent(
    body: &Expr,
    decl_start: usize,
    cursor: usize,
    out: &mut Vec<Extent>,
    source: &str,
) {
    let end = expr_extent(body, source);
    if cursor >= decl_start && cursor <= end {
        out.push(Extent {
            start: decl_start,
            end,
        });
        collect_expr_ranges(body, source, cursor, out);
    }
}
