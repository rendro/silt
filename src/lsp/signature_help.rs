//! `textDocument/signatureHelp` handler: the call the cursor is in is
//! read from the tokens in front of the cursor.

use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};

use crate::intern::{intern, resolve};
use crate::lexer::{Tok, Token};
use crate::types::Type;

use super::Server;
use super::conversions::position_to_offset;
use super::local_bindings::nearest_local_binding_for;
use super::state::{DefInfo, Document, LocalBinding};
use super::workspace::{Named, Target};

impl Server {
    // ── Signature help ────────────────────────────────────────────

    pub(super) fn signature_help(
        &self,
        params: lsp_types::SignatureHelpParams,
    ) -> Option<SignatureHelp> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = self.documents.get(uri)?;

        // The document's tokens that start in front of the cursor say
        // which call it is in: the lexer goes on behind an error, so a
        // call that is being typed (an argument that is no token yet, a
        // string that is not closed) has its tokens all the same. (A
        // token is what it is whatever follows it, so these are the
        // tokens of the text in front of the cursor, but for the one the
        // cursor is in, which opens and closes nothing: a closer is one
        // character, and the end of an interpolation starts behind its
        // brace.)
        let cursor = position_to_offset(&doc.source, &pos);
        let tokens = doc.tokens();
        let before = tokens.partition_point(|tok| (tok.span.start as usize) < cursor);
        let (fn_name, active_param, callee_at) = call_site(&tokens[..before])?;

        // `Tr.m(`: the method of the trait, as the trait declares it.
        if let Some((tr, method)) = fn_name.rsplit_once('.')
            && let Some(id) = self.trait_written(doc, tr)
        {
            let m = (self.trait_call_methods(doc, id).into_iter())
                .find(|m| resolve(m.name) == method)?;
            let params_info = (m.params.iter())
                .map(|range| ParameterInformation {
                    label: ParameterLabel::LabelOffsets(*range),
                    documentation: None,
                })
                .collect();
            return Some(self.signature(m.label, params_info, m.doc, active_param));
        }

        // What the name called means where it is written, as hover finds
        // it: a local (or a parameter) that is spelled like a function is
        // not that function, and a definition is the one the name reaches.
        let (label, params_info, doc_text) = match self.callee(uri, doc, &fn_name, callee_at) {
            Callee::Local(ty) => {
                let (label, params_info) = local_signature(&ty?)?;
                (label, params_info, None)
            }
            Callee::Own(def) => {
                let (label, params_info) = build_signature_from_def(&fn_name, def);
                (label, params_info, def.doc.clone())
            }
            Callee::Definition(def) => {
                let (label, params_info) = build_signature_from_def(&fn_name, &def);
                (label, params_info, def.doc)
            }
            Callee::Builtin(name) => self.builtin_signature(&name)?,
            Callee::Unknown => {
                // No analysis says it: by the name, in definitions first,
                // then builtins.
                let fn_sym = intern(&fn_name);
                // A member of an imported module: `m.f(`, or `f(` with `f`
                // from `import m.{ f }`.
                let imported = match fn_name.split_once('.') {
                    Some((module, member)) => self
                        .imported_module(doc, intern(module))
                        .and_then(|mut view| view.definitions.remove(&intern(member))),
                    None => self
                        .item_module(doc, fn_sym)
                        .and_then(|mut view| view.definitions.remove(&fn_sym)),
                };
                match doc.definitions.get(&fn_sym).or(imported.as_ref()) {
                    Some(def) => {
                        let (label, params_info) = build_signature_from_def(&fn_name, def);
                        (label, params_info, def.doc.clone())
                    }
                    None => self.builtin_signature(&fn_name)?,
                }
            }
        };

        Some(self.signature(label, params_info, doc_text, active_param))
    }

    /// What the callee `fn_name`, whose last name starts at byte `at` of
    /// the open document `uri`, is.
    fn callee<'a>(
        &self,
        uri: &lsp_types::Uri,
        doc: &'a Document,
        fn_name: &str,
        at: usize,
    ) -> Callee<'a> {
        let local =
            |binding: Option<&LocalBinding>| Callee::Local(binding.and_then(|b| b.ty.clone()));
        // First what the document keeps: a local of the name in scope,
        // then one of its own definitions (which no import shadows). Only
        // a name that is neither (silt's own, an import's, a method) is
        // looked up in the analysis, whose walk of the whole document a
        // request would pay each time.
        if !fn_name.contains('.') {
            let sym = intern(fn_name);
            if let Some(binding) = nearest_local_binding_for(&doc.locals, sym, at) {
                return local(Some(binding));
            }
            if let Some(def) = doc.definitions.get(&sym) {
                return Callee::Own(def);
            }
        }
        match self.target_at(uri, at) {
            Some((_, Target::Local(binding))) => {
                local(doc.locals.iter().find(|b| b.binding_offset == binding))
            }
            // A method of a value (`x.m(`): no signature here.
            Some((_, Target::Method { .. })) => Callee::Local(None),
            // A field that holds a function (`p.f(`): its name has a
            // dot, so it is no local either.
            // (A module's alias is no callee: the name behind its dot is.)
            Some((_, Target::Field { .. } | Target::Alias { .. })) => Callee::Unknown,
            Some((_, Target::Defs(_))) => match self.builtin_at(uri, at) {
                Some(name) => Callee::Builtin(name),
                None => match self.named_at(uri, at) {
                    Named::Definition(def) => Callee::Definition(def),
                    Named::Local | Named::Other => Callee::Unknown,
                },
            },
            // (A text that has no analysis: a local of the name in scope
            // there is still the local.)
            None => match (!fn_name.contains('.'))
                .then(|| nearest_local_binding_for(&doc.locals, intern(fn_name), at))
                .flatten()
            {
                Some(binding) => local(Some(binding)),
                None => Callee::Unknown,
            },
        }
    }

    /// The signature of silt's own `name` (`list.map`, `println` of the
    /// prelude, a variant: `Some`, `list.Stop`), with each parameter
    /// where the signature writes it, and its stdlib markdown: its own
    /// section (of the globals page, for one of the prelude).
    fn builtin_signature(
        &self,
        name: &str,
    ) -> Option<(String, Vec<ParameterInformation>, Option<String>)> {
        let doc_text = self.builtin_docs.get(name).cloned();
        // A variant: as its enum declares it, `Some(a)`.
        let Some(row) = crate::builtins::registry::registry().named(name) else {
            let def = DefInfo {
                ty: None,
                signature: None,
                params: Vec::new(),
                fields: Some(super::trait_hover::builtin_variant_fields(name)?),
                doc: None,
            };
            let (label, params_info) = build_signature_from_def(name, &def);
            return Some((label, params_info, doc_text));
        };
        let (label, ranges) = row.qualified_signature(row.module)?;
        let params_info = ranges
            .into_iter()
            .map(|range| ParameterInformation {
                label: ParameterLabel::LabelOffsets(range),
                documentation: None,
            })
            .collect::<Vec<_>>();
        Some((label, params_info, doc_text))
    }

    /// The help of a call of the signature `label`, whose parameters are
    /// `params_info`, documented by `doc_text`, at its parameter
    /// `active_param`.
    fn signature(
        &self,
        label: String,
        params_info: Vec<ParameterInformation>,
        doc_text: Option<String>,
        active_param: u32,
    ) -> SignatureHelp {
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

        SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation,
                parameters: Some(params_info),
                active_parameter: Some(active_param),
            }],
            active_signature: Some(0),
            active_parameter: Some(active_param),
        }
    }
}

// ── Signature help helpers ─────────────────────────────────────────

/// What a call's callee is.
enum Callee<'a> {
    /// A local binding (or a parameter), of this type if it is known.
    Local(Option<Type>),
    /// A definition of the document itself, as it keeps it.
    Own(&'a DefInfo),
    /// A definition of a file.
    Definition(DefInfo),
    /// One of silt's own, by its qualified name (`list.map`, `Some`).
    Builtin(String),
    /// Nothing the analysis says.
    Unknown,
}

/// The signature of a local of the type `ty`, as hover writes the type
/// (`Fn(Int) -> Int`), each parameter's type a parameter; `None` for a
/// local that is no function.
fn local_signature(ty: &Type) -> Option<(String, Vec<ParameterInformation>)> {
    let Type::Fun(params, ret) = super::ast_walk::written_with_letters(ty) else {
        return None;
    };
    let mut label = "Fn(".to_string();
    let mut params_info = Vec::new();
    for (i, param) in params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let start = label.len() as u32;
        label.push_str(&param.to_string());
        params_info.push(ParameterInformation {
            label: ParameterLabel::LabelOffsets([start, label.len() as u32]),
            documentation: None,
        });
    }
    label.push_str(&format!(") -> {ret}"));
    Some((label, params_info))
}

pub(super) fn build_signature_from_def(
    name: &str,
    def: &DefInfo,
) -> (String, Vec<ParameterInformation>) {
    // A variant: as its enum declares it, `Rect(Float, Float)`.
    if let Some(fields) = &def.fields {
        let mut label = format!("{name}(");
        let mut params_info = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                label.push_str(", ");
            }
            let start = label.len() as u32;
            label.push_str(field);
            params_info.push(ParameterInformation {
                label: ParameterLabel::LabelOffsets([start, label.len() as u32]),
                documentation: None,
            });
        }
        label.push(')');
        return (label, params_info);
    }

    let mut label = format!("fn {name}(");
    let mut params_info = Vec::new();

    // (A generic function with its variables named, `xs: m(Int)`.)
    let ty = def.signature.as_ref().map(|s| &s.ty).or(def.ty.as_ref());
    if let Some(Type::Fun(param_types, ret)) = ty {
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
        // The bounds, as hover writes them (`where a: Display`).
        if let Some((_, bounds)) =
            (def.signature.as_ref()).and_then(|s| s.text.split_once(" where "))
        {
            label.push_str(&format!(" where {bounds}"));
        }
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
/// that are complete (the index of the one being written), the piped
/// value of `x |> f(` counted as the first, and the byte its last name
/// (`f`) starts at: the innermost `(` that is
/// not closed. A list, a record, a block or a string interpolation that
/// is open inside it is part of the argument being written, and its
/// commas are its own.
fn call_site(tokens: &[Tok]) -> Option<(String, u32, usize)> {
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
    let at = tokens[paren - 1].span.start as usize;
    // The argument a pipe gives the call comes before those written.
    let piped = |path: usize| {
        let at = (paren.checked_sub(path + 1)).and_then(|i| tokens.get(i));
        commas + u32::from(at.is_some_and(|tok| tok.kind == Token::Pipe))
    };
    // `m.f(`: a member of a module, or `Tr.f(`, a method through its
    // trait; `m.Tr.f(`, through a trait of a module. A longer path is a
    // field of a value, which has no signature here.
    if is_dot(paren.checked_sub(2)) {
        let module = name_at(paren.checked_sub(3))?;
        if is_dot(paren.checked_sub(4)) {
            let outer = name_at(paren.checked_sub(5))?;
            if is_dot(paren.checked_sub(6)) {
                return None;
            }
            return Some((format!("{outer}.{module}.{name}"), piped(5), at));
        }
        return Some((format!("{module}.{name}"), piped(3), at));
    }
    Some((name, piped(1), at))
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_signature_from_def ─────────────────────────────────

    #[test]
    fn test_build_signature_simple() {
        let def = DefInfo {
            signature: None,
            fields: None,
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
            signature: None,
            fields: None,
            ty: None,
            params: vec!["x".into(), "y".into()],
            doc: None,
        };
        let (label, params) = build_signature_from_def("foo", &def);
        assert_eq!(label, "fn foo(x, y)");
        assert_eq!(params.len(), 2);
    }
}
