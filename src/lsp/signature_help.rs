//! `textDocument/signatureHelp` handler: the call the cursor is in is
//! read from the tokens in front of the cursor.

use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};

use crate::intern::{intern, resolve};
use crate::lexer::{Lexer, Tok, Token};
use crate::source::FileId;
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

        // The tokens of the text in front of the cursor say which call
        // it is in: the lexer goes on behind an error, so a call that is
        // being typed (an argument that is no token yet, a string that
        // is not closed) has its tokens all the same.
        let cursor = position_to_offset(&doc.source, &pos);
        let lexed = Lexer::new(FileId::default(), &doc.source.text[..cursor]).tokenize();
        let (fn_name, active_param) = call_site(&lexed.tokens)?;

        // Look up in definitions first, then builtins.
        let fn_sym = intern(&fn_name);
        // A member of an imported module: `m.f(`, or `f(` with `f` from
        // `import m.{ f }`.
        let imported = match fn_name.split_once('.') {
            Some((module, member)) => self
                .imported_module(doc, intern(module))
                .and_then(|mut view| view.definitions.remove(&intern(member))),
            None => self
                .item_module(doc, fn_sym)
                .and_then(|mut view| view.definitions.remove(&fn_sym)),
        };
        let (label, params_info, doc_text) =
            if let Some(def) = doc.definitions.get(&fn_sym).or(imported.as_ref()) {
                let (label, params_info) = build_signature_from_def(&fn_name, def);
                (label, params_info, def.doc.clone())
            } else {
                // A builtin: the signature of its row in the builtin
                // registry, with each parameter where the signature
                // writes it, and the stdlib markdown alongside.
                let (module, function) = fn_name.split_once('.')?;
                let (label, ranges) = crate::builtins::registry::registry()
                    .row(module, function)?
                    .qualified_signature(module)?;
                let doc_text = self.builtin_docs.get(&fn_name).cloned();
                let params_info = ranges
                    .into_iter()
                    .map(|range| ParameterInformation {
                        label: ParameterLabel::LabelOffsets(range),
                        documentation: None,
                    })
                    .collect::<Vec<_>>();
                (label, params_info, doc_text)
            };

        // A client that does not read label offsets gets each
        // parameter as its text, a substring of the label.
        let params_info = if self.label_offsets {
            params_info
        } else {
            params_info
                .into_iter()
                .map(|p| match p.label {
                    ParameterLabel::LabelOffsets([start, end]) => ParameterInformation {
                        label: ParameterLabel::Simple(
                            label[start as usize..end as usize].to_string(),
                        ),
                        documentation: p.documentation,
                    },
                    ParameterLabel::Simple(_) => p,
                })
                .collect()
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
                    if sym.is_builtin(crate::defs::TYPE_OF) && args.len() == 1 =>
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

/// The call that the end of `tokens` is in, as the name written in
/// front of its `(` (`f`, or `m.f`) and the number of its arguments
/// that are complete (the index of the one being written): the
/// innermost `(` that is not closed. A list, a record, a block or a
/// string interpolation that is open inside it is part of the argument
/// being written, and its commas are its own.
fn call_site(tokens: &[Tok]) -> Option<(String, u32)> {
    #[derive(PartialEq)]
    enum Open {
        Paren,
        Bracket,
        Brace,
        Interpolation,
    }
    // Each delimiter that is open: its kind, its token, the commas
    // directly inside it.
    let mut open: Vec<(Open, usize, u32)> = Vec::new();
    for (index, tok) in tokens.iter().enumerate() {
        // A closer closes the innermost delimiter if it is of its kind;
        // one that is not (text that is being typed) closes nothing.
        let mut close = |kind: Open| {
            if open.last().is_some_and(|(top, ..)| *top == kind) {
                open.pop();
            }
        };
        match tok.kind {
            Token::RParen => close(Open::Paren),
            Token::RBracket => close(Open::Bracket),
            Token::RBrace => close(Open::Brace),
            Token::StringEnd(_) => close(Open::Interpolation),
            Token::LParen => open.push((Open::Paren, index, 0)),
            Token::LBracket | Token::HashBracket => open.push((Open::Bracket, index, 0)),
            Token::LBrace | Token::HashBrace => open.push((Open::Brace, index, 0)),
            Token::StringStart(_) => open.push((Open::Interpolation, index, 0)),
            Token::Comma => {
                if let Some((.., commas)) = open.last_mut() {
                    *commas += 1;
                }
            }
            _ => {}
        }
    }
    let (_, paren, commas) = open.iter().rev().find(|(kind, ..)| *kind == Open::Paren)?;
    let name_at = |index: Option<usize>| match index.and_then(|i| tokens.get(i)) {
        Some(Tok {
            kind: Token::Ident(name),
            ..
        }) => Some(resolve(*name)),
        _ => None,
    };
    let is_dot = |index: Option<usize>| {
        index
            .and_then(|i| tokens.get(i))
            .is_some_and(|tok| tok.kind == Token::Dot)
    };
    let name = name_at(paren.checked_sub(1))?;
    // `m.f(`: a member of a module. A longer path (`a.b.f(`) is a method
    // or a field of a value, which has no signature here.
    if is_dot(paren.checked_sub(2)) {
        let module = name_at(paren.checked_sub(3))?;
        if is_dot(paren.checked_sub(4)) {
            return None;
        }
        return Some((format!("{module}.{name}"), *commas));
    }
    Some((name, *commas))
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_signature_from_def ─────────────────────────────────

    #[test]
    fn test_build_signature_simple() {
        let def = DefInfo {
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
            ty: None,
            params: vec!["x".into(), "y".into()],
            doc: None,
        };
        let (label, params) = build_signature_from_def("foo", &def);
        assert_eq!(label, "fn foo(x, y)");
        assert_eq!(params.len(), 2);
    }
}
