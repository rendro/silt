//! `textDocument/completion` handler and its dot-completion helpers.

use lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse, Position};

use crate::ast::*;
use crate::intern::intern;
use crate::module;
use crate::types::Type;

use super::Server;
use super::ast_walk::find_ident_type_by_name;
use super::conversions::position_to_offset;
use super::fields::{record_fields_from_type, type_expr_to_type};
use super::locals::locals_at_offset;
use super::state::Document;

impl Server {
    // ── Completion ─────────────────────────────────────────────────

    pub(super) fn completion(
        &self,
        params: lsp_types::CompletionParams,
    ) -> Option<CompletionResponse> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let doc = self.documents.get(uri);

        // Detect dot-completion context: extract the identifier before the `.`
        if let Some(doc) = doc
            && let Some(prefix) = extract_dot_prefix(&doc.source, &pos)
        {
            let cursor = position_to_offset(&doc.source, &pos);
            let items = self.dot_completions(doc, &prefix, cursor);
            return Some(CompletionResponse::Array(items));
        }

        let mut items: Vec<CompletionItem> = Vec::new();

        // Keywords
        for kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..CompletionItem::default()
            });
        }

        // Builtins (globals + stdlib)
        for (name, kind) in builtins() {
            let detail = self.builtin_sigs.get(&name).cloned();
            items.push(CompletionItem {
                label: name,
                kind: Some(kind),
                detail,
                ..CompletionItem::default()
            });
        }

        // User-defined names from the current document
        if let Some(doc) = doc {
            for (name, def) in &doc.definitions {
                let kind = match &def.ty {
                    Some(Type::Fun(..)) => CompletionItemKind::FUNCTION,
                    _ => CompletionItemKind::VARIABLE,
                };
                let detail = def.ty.as_ref().map(|t| format!("{t}"));
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(kind),
                    detail,
                    ..CompletionItem::default()
                });
            }

            // Local variables in scope at the cursor position
            if let Some(program) = &doc.program {
                let cursor = position_to_offset(&doc.source, &pos);
                for local in locals_at_offset(program, cursor) {
                    let detail = local.ty.as_ref().map(|t| format!("{t}"));
                    items.push(CompletionItem {
                        label: local.name,
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail,
                        ..CompletionItem::default()
                    });
                }
            }
        }

        Some(CompletionResponse::Array(items))
    }

    /// Produce completions after a `.` — either module functions or record fields.
    fn dot_completions(&self, doc: &Document, prefix: &str, cursor: usize) -> Vec<CompletionItem> {
        let mut items = Vec::new();

        // 1. Builtin module → return its functions and constants with type signatures.
        //    Module constants (e.g. `math.pi`, `float.infinity`) are distinct from
        //    functions and must be surfaced here so editor autocompletion after
        //    `math.` / `float.` offers them alongside `sin`, `cos`, `parse`, etc.
        if module::is_builtin_module(prefix) {
            for func in module::builtin_module_functions(prefix) {
                let qualified = format!("{prefix}.{func}");
                let detail = self.builtin_sigs.get(&qualified).cloned();
                items.push(CompletionItem {
                    label: func.to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail,
                    ..CompletionItem::default()
                });
            }
            for constant in module::builtin_module_constants(prefix) {
                let qualified = format!("{prefix}.{constant}");
                let detail = self.builtin_sigs.get(&qualified).cloned();
                items.push(CompletionItem {
                    label: constant.to_string(),
                    kind: Some(CompletionItemKind::CONSTANT),
                    detail,
                    ..CompletionItem::default()
                });
            }
            // Gated enum constructors that belong to this module
            // (e.g. `io.IoNotFound`, `http.GET`, `channel.Recv`,
            // `time.Monday`, `postgres.PgConnect`). Emitted as
            // CONSTRUCTOR entries so editors distinguish them from
            // module functions / constants.
            //
            // Silt does not expose gated constructors via `module.Name`
            // at the runtime binding level — they bind as bare globals
            // once the module is imported — but surfacing them after a
            // `.` is the most intuitive discovery affordance for users
            // exploring the API.
            for (_enum_name, variants) in module::builtin_enum_variants() {
                for &variant in *variants {
                    if module::gated_constructor_module(variant) == Some(prefix) {
                        items.push(CompletionItem {
                            label: variant.to_string(),
                            kind: Some(CompletionItemKind::CONSTRUCTOR),
                            ..CompletionItem::default()
                        });
                    }
                }
            }
            // Deterministic ordering so clients/tests see a stable list, and
            // dedupe in case a name was declared as both function and constant.
            items.sort_by(|a, b| a.label.cmp(&b.label));
            items.dedup_by(|a, b| a.label == b.label);
            return items;
        }

        let program = match &doc.program {
            Some(p) => p,
            None => return items,
        };

        // 2. Check local variables in scope at cursor for the prefix
        let locals = locals_at_offset(program, cursor);
        if let Some(local) = locals.iter().rev().find(|l| l.name == prefix)
            && let Some(ref ty) = local.ty
            && let Some(fields) = record_fields_from_type(ty, program)
        {
            for (name, field_ty) in &fields {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("{field_ty}")),
                    ..CompletionItem::default()
                });
            }
            return items;
        }

        // 3. Try to resolve the identifier's type from the typed AST
        if let Some(ty) = find_ident_type_by_name(program, prefix)
            && let Some(fields) = record_fields_from_type(&ty, program)
        {
            for (name, field_ty) in &fields {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(format!("{field_ty}")),
                    ..CompletionItem::default()
                });
            }
            return items;
        }

        // 3. Fallback: if the prefix matches a type name, offer its fields
        let prefix_sym = intern(prefix);
        for decl in &program.decls {
            if let Decl::Type(td) = decl
                && td.name == prefix_sym
                && let TypeBody::Record(fields) = &td.body
            {
                for field in fields {
                    items.push(CompletionItem {
                        label: field.name.to_string(),
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(format!("{}", type_expr_to_type(&field.ty))),
                        ..CompletionItem::default()
                    });
                }
            }
        }

        items
    }
}

// ── Dot-completion helpers ─────────────────────────────────────────

/// Extract the identifier (or postfix-call / index-expression receiver)
/// before the `.` at the cursor position.
///
/// Returns `None` if the cursor is not in a dot-completion context.
///
/// The walk handles chained method calls and index expressions — `xs.first().`
/// and `arr[0].` must trigger completion on the receiver, not bail because
/// `)` / `]` terminate the identifier scan. We scan char-by-char from right
/// to left, skipping over matched `()` / `[]` spans, then greedily collect
/// identifier characters (plus `.` for qualified names like `mod.inner.`).
/// Anything else terminates the walk — keeps multi-statement lines from
/// greedily swallowing the previous expression.
fn extract_dot_prefix(source: &str, pos: &Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = pos.character as usize;
    if col == 0 {
        return None;
    }
    // Convert UTF-16 offset to byte offset
    let mut utf16_offset = 0usize;
    let mut byte_offset = line.len();
    for (byte_idx, ch) in line.char_indices() {
        if utf16_offset >= col {
            byte_offset = byte_idx;
            break;
        }
        utf16_offset += ch.len_utf16();
    }
    let before = &line[..byte_offset];
    // The last character should be '.' (cursor is right after it)
    if !before.ends_with('.') {
        return None;
    }
    let before_dot = &before[..before.len() - 1];
    // Walk backwards, skipping balanced `()` / `[]` groups so method
    // chains like `xs.first().` and index expressions like `arr[0].`
    // resolve to a sensible receiver instead of bailing on the closer.
    let chars: Vec<char> = before_dot.chars().collect();
    let mut end = chars.len(); // exclusive upper bound of the prefix
    loop {
        if end == 0 {
            break;
        }
        let last = chars[end - 1];
        match last {
            ')' | ']' => {
                let (open, close) = if last == ')' { ('(', ')') } else { ('[', ']') };
                // Scan back to the matching opener, handling nesting.
                let mut depth = 1i32;
                let mut i = end - 1; // index of the closer
                while i > 0 {
                    i -= 1;
                    let c = chars[i];
                    if c == close {
                        depth += 1;
                    } else if c == open {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                if depth != 0 {
                    // Unbalanced — stop here.
                    break;
                }
                // `i` now points at the matching opener; consume through it.
                end = i;
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' => {
                // Greedily consume the identifier (possibly qualified).
                while end > 0 {
                    let ch = chars[end - 1];
                    if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                        end -= 1;
                    } else {
                        break;
                    }
                }
                break;
            }
            _ => break,
        }
    }
    let prefix: String = chars[end..].iter().collect();
    // Strip a trailing `.` — shouldn't happen in practice but keeps the
    // contract "returned prefix never ends with `.`" trivially true.
    let prefix = prefix.trim_end_matches('.').to_string();
    if prefix.is_empty() {
        None
    } else {
        Some(prefix)
    }
}

// ── Completion data ────────────────────────────────────────────────

const KEYWORDS: &[&str] = &[
    "as", "else", "fn", "import", "let", "loop", "match", "mod", "pub", "return", "trait", "type",
    "when", "where",
];

/// Build the builtins completion list dynamically from the module registry
/// so it never falls out of sync with `module.rs`.
///
/// `pub` so integration tests (see `tests/builtin_constructor_parity_tests.rs`)
/// can assert every gated constructor from
/// `module::all_builtin_constructor_names` is emitted here.
pub fn builtins() -> Vec<(String, CompletionItemKind)> {
    let mut items = vec![
        // Globals (not part of any module)
        ("print".to_string(), CompletionItemKind::FUNCTION),
        ("println".to_string(), CompletionItemKind::FUNCTION),
        ("panic".to_string(), CompletionItemKind::FUNCTION),
        ("true".to_string(), CompletionItemKind::CONSTANT),
        ("false".to_string(), CompletionItemKind::CONSTANT),
    ];

    // Every builtin enum constructor — prelude (Ok/Err/Some/None) plus
    // every gated variant (Recv/Send, IoNotFound, PgConnect, Monday,
    // GET/POST/…, etc.). Sourced from the authoritative module helper
    // so new variants flow through without editing this list.
    for name in module::all_builtin_constructor_names() {
        items.push((name.to_string(), CompletionItemKind::CONSTRUCTOR));
    }

    for &m in module::BUILTIN_MODULES {
        for func in module::builtin_module_functions(m) {
            items.push((format!("{m}.{func}"), CompletionItemKind::FUNCTION));
        }
        for constant in module::builtin_module_constants(m) {
            items.push((format!("{m}.{constant}"), CompletionItemKind::CONSTANT));
        }
    }

    items
}
