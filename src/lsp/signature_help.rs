//! `textDocument/signatureHelp` handler and its call-site scanner.

use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};

use crate::intern::intern;
use crate::types::Type;

use super::Server;
use super::conversions::position_to_offset;
use super::state::DefInfo;

impl Server {
    // ── Signature help ────────────────────────────────────────────

    pub(super) fn signature_help(
        &self,
        params: lsp_types::SignatureHelpParams,
    ) -> Option<SignatureHelp> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;

        // Walk backwards from cursor to find the function name before `(`.
        let cursor = position_to_offset(&doc.source, &pos);
        let before = &doc.source[..cursor];

        // Forward-scan `before` to find the active call site: the last `(`
        // at nesting depth 0, and count commas at depth 1 from there.
        // Skips string literals and silt comments so that commas/parens
        // inside them are not miscounted.
        let (active_param, paren_pos) = scan_call_site_forward(before.as_bytes())?;
        let before_paren = before[..paren_pos].trim_end();
        let fn_name: String = before_paren
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect::<String>()
            .chars()
            .rev()
            .collect();

        if fn_name.is_empty() {
            return None;
        }

        // Look up in definitions first, then builtins.
        let fn_sym = intern(&fn_name);
        let (label, params_info, doc_text) = if let Some(def) = doc.definitions.get(&fn_sym) {
            let (label, params_info) = build_signature_from_def(&fn_name, def);
            (label, params_info, def.doc.clone())
        } else if let Some(sig) = self.builtin_sigs.get(&fn_name) {
            // Show builtin type signature (no individual param info).
            // Phase-2 builtin docs: surface stdlib markdown
            // alongside the signature so signature-help is a real
            // documentation surface for builtins, not just a type.
            let doc_text = self.builtin_docs.get(&fn_name).cloned();
            (format!("{fn_name}: {sig}"), vec![], doc_text)
        } else {
            return None;
        };

        let documentation = doc_text.map(|d| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: d,
            })
        });

        Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation,
                parameters: Some(params_info),
                active_parameter: Some(active_param),
            }],
            active_signature: Some(0),
            active_parameter: Some(active_param),
        })
    }
}

// ── Signature help helpers ─────────────────────────────────────────

pub(super) fn build_signature_from_def(
    name: &str,
    def: &DefInfo,
) -> (String, Vec<ParameterInformation>) {
    let mut label = format!("fn {name}(");
    let mut params_info = Vec::new();

    if let Some(Type::Fun(param_types, ret)) = &def.ty {
        for (i, pty) in param_types.iter().enumerate() {
            let pname = def.params.get(i).map(|s| s.as_str()).unwrap_or("_");
            // `type a` parameters carry compile-time type `TypeOf(a)` in the
            // scheme. Render them as `type a` rather than leaking the
            // internal descriptor name.
            let param_label = match pty {
                Type::Generic(sym, args)
                    if crate::intern::resolve(*sym) == "TypeOf" && args.len() == 1 =>
                {
                    format!("type {pname}")
                }
                _ => format!("{pname}: {pty}"),
            };
            let start = label.len() as u32;
            label.push_str(&param_label);
            let end = label.len() as u32;
            if i + 1 < param_types.len() {
                label.push_str(", ");
            }
            params_info.push(ParameterInformation {
                label: ParameterLabel::LabelOffsets([start, end]),
                documentation: None,
            });
        }
        label.push_str(&format!(") -> {ret}"));
    } else {
        for (i, pname) in def.params.iter().enumerate() {
            let start = label.len() as u32;
            label.push_str(pname);
            let end = label.len() as u32;
            if i + 1 < def.params.len() {
                label.push_str(", ");
            }
            params_info.push(ParameterInformation {
                label: ParameterLabel::LabelOffsets([start, end]),
                documentation: None,
            });
        }
        label.push(')');
    }

    (label, params_info)
}

/// Forward-scan `before` (the source slice up to the cursor) to find the
/// innermost active call site. Returns `(active_param, paren_byte_offset)`
/// where `paren_byte_offset` is the position of the opening `(` of the call
/// and `active_param` is the 0-based comma count between that `(` and the end.
///
/// Skips string literals (`"..."`, `""" ... """`), line comments (`--`), and
/// block comments (`{- ... -}`) so commas and parens inside them are ignored.
pub(super) fn scan_call_site_forward(bytes: &[u8]) -> Option<(u32, usize)> {
    // Stack of (paren_byte_offset, comma_count) for each nesting depth.
    let mut stack: Vec<(usize, u32)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // ── Strings ──────────────────────────────────────────
            b'"' => {
                if i + 2 < bytes.len() && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
                    i += 3;
                    while i + 2 < bytes.len()
                        && !(bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"')
                    {
                        i += 1;
                    }
                    i = (i + 3).min(bytes.len());
                } else {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' && i + 1 < bytes.len() {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
            }
            // ── Block comments {- ... -} (with nesting) ─────────
            b'{' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                i += 2;
                let mut cd = 1u32;
                while i < bytes.len() && cd > 0 {
                    if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'-' {
                        cd += 1;
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'}' {
                        cd -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            // ── Line comments -- ... ────────────────────────────
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            // ── Brackets ────────────────────────────────────────
            b'(' => {
                stack.push((i, 0));
                i += 1;
            }
            b')' => {
                stack.pop();
                i += 1;
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    // The top of the stack is the innermost unclosed `(` — that's our call site.
    let (paren_pos, comma_count) = stack.last()?;
    Some((*comma_count, *paren_pos))
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Span;

    // ── build_signature_from_def ─────────────────────────────────

    #[test]
    fn test_build_signature_simple() {
        let def = DefInfo {
            span: Span {
                line: 1,
                col: 1,
                offset: 0,
            },
            ty: Some(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
            params: vec!["a".into(), "b".into()],
            doc: None,
        };
        let (label, params) = build_signature_from_def("add", &def);
        assert!(label.starts_with("fn add("));
        assert!(label.contains("-> Int"));
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_build_signature_no_type() {
        let def = DefInfo {
            span: Span {
                line: 1,
                col: 1,
                offset: 0,
            },
            ty: None,
            params: vec!["x".into(), "y".into()],
            doc: None,
        };
        let (label, params) = build_signature_from_def("foo", &def);
        assert_eq!(label, "fn foo(x, y)");
        assert_eq!(params.len(), 2);
    }

    // ── scan_call_site_forward tests ─────────────────────────────

    #[test]
    fn test_sig_help_comma_in_string_not_counted() {
        // foo("hello, world", 42)  — cursor after 42
        // The comma inside the string must not be counted.
        let before = r#"foo("hello, world", 42"#;
        let (param, paren) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "should be param 1 (y), not 2");
        assert_eq!(paren, 3, "paren at index 3");
    }

    #[test]
    fn test_sig_help_comma_in_line_comment_not_counted() {
        // foo(1,\n-- a, b, c\n2)  — cursor after 2
        let before = "foo(1,\n-- a, b, c\n2";
        let (param, _) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "commas inside -- comment should be ignored");
    }

    #[test]
    fn test_sig_help_comma_in_block_comment_not_counted() {
        // foo(1, {- a, b -} 2)  — cursor after 2
        let before = "foo(1, {- a, b -} 2";
        let (param, _) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "commas inside block comment should be ignored");
    }

    #[test]
    fn test_sig_help_nested_call_finds_outer_function() {
        // add(mul(1, 2), 3)  — cursor after 3
        let before = "add(mul(1, 2), 3";
        let (param, paren) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "should be param 1 of add, not param of mul");
        assert_eq!(paren, 3, "paren should be add's ( at index 3");
    }

    #[test]
    fn test_sig_help_cursor_inside_inner_call() {
        // add(mul(1, | — cursor between 1 and closing
        let before = "add(mul(1, ";
        let (param, paren) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "should be param 1 of mul");
        assert_eq!(paren, 7, "paren should be mul's ( at index 7");
    }

    #[test]
    fn test_sig_help_no_open_paren_returns_none() {
        let before = "let x = 42";
        assert!(scan_call_site_forward(before.as_bytes()).is_none());
    }

    #[test]
    fn test_sig_help_triple_quoted_string_skipped() {
        let before = r#"foo("""hello, world""", 42"#;
        let (param, _) = scan_call_site_forward(before.as_bytes()).unwrap();
        assert_eq!(param, 1, "comma inside triple-quoted string ignored");
    }
}
