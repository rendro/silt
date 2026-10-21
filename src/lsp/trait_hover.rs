//! Hover on a trait's name: its signature (the trait's header and its
//! methods, without their default bodies) and its documentation, as a
//! type shows its declaration.

use crate::ast::{
    Decl, FnDecl, PatternKind, Qualifier, TraitDecl, TraitRef, TypeExpr, TypeExprKind, WhereClause,
};
use crate::defs::TraitId;
use crate::intern::resolve;

use super::Server;
use super::state::Document;

impl Server {
    /// What hover shows for the trait `id` in the session of the open
    /// document `doc`: the trait's signature, and below it its doc
    /// comment (one of silt's own: its documentation).
    pub(super) fn trait_hover_text(&self, doc: &Document, id: TraitId) -> Option<String> {
        let module = doc.module.as_ref()?;
        let session = &self.projects.get(&module.project)?.session;
        let def = session.defs().get(id.0);
        let (signature, doc_text) = if def.module.is_builtin() || !def.span.is_in_source() {
            let (name, module) = crate::defs::builtin_trait_of_id(id.0)?;
            match module {
                // A trait of the prelude: the checker's declaration.
                None => {
                    let decl = crate::typechecker::builtin_trait_decls()
                        .into_iter()
                        .find(|t| resolve(t.name) == name)?;
                    (signature(&decl, None), decl.doc.clone())
                }
                // A builtin module's (`json.Encode`): the declaration of
                // the registry, and its section of the module's page.
                Some(module) => {
                    let text = crate::builtins::registry::registry()
                        .modules
                        .iter()
                        .find(|m| m.name == module)?
                        .traits;
                    let program = crate::builtins::registry::parse(text);
                    let decl = declared(&program.decls, |t| resolve(t.name) == name)?;
                    let section = self.builtin_docs.get(&format!("{module}.{name}"));
                    // (Shown as the page writes it, without the `pub` of
                    // the registry's text.)
                    let signature = signature(decl, Some(text));
                    let signature = signature.strip_prefix("pub ").unwrap_or(&signature);
                    (signature.to_string(), section.map(|s| below_signature(s)))
                }
            }
        } else {
            let analysis = session.module_analysis(def.module)?;
            let decl = declared(&analysis.ast.decls, |t| t.name_span == def.span)?;
            let file = session.graph().module(def.module).file?;
            let source = session.sources().get(file)?;
            (signature(decl, Some(&source.text)), decl.doc.clone())
        };
        let mut value = format!("```silt\n{signature}\n```");
        if let Some(text) = doc_text.filter(|text| !text.is_empty()) {
            value.push_str(&format!("\n\n---\n\n{text}"));
        }
        Some(value)
    }
}

/// The trait of `decls` that `is` picks.
fn declared(decls: &[Decl], is: impl Fn(&TraitDecl) -> bool) -> Option<&TraitDecl> {
    decls.iter().find_map(|decl| match decl {
        Decl::Trait(t) if is(t) => Some(t),
        _ => None,
    })
}

/// A builtin module's section on its trait, without the declaration it
/// opens with (hover shows it above).
fn below_signature(section: &str) -> String {
    let rest = section.trim_start();
    let Some(after) = rest.strip_prefix("```") else {
        return section.to_string();
    };
    match after.split_once("\n```") {
        Some((_, below)) => below.trim().to_string(),
        None => section.to_string(),
    }
}

/// The signature of the trait `t`: its header and each method's
/// signature (its default body left out) and associated type, one a
/// line. With the text `source` of the file that declares it, each
/// part is written as it is there; a trait of the prelude, which no
/// file declares, is written from its declaration.
fn signature(t: &TraitDecl, source: Option<&str>) -> String {
    let Some(text) = source else {
        return written(t);
    };
    let slice = |start: u32, end: u32| text.get(start as usize..end as usize).unwrap_or_default();
    // The header runs to the brace that opens the body: the first one
    // outside the brackets of the trait's arguments and bounds.
    let mut depth = 0i32;
    let header_end = text
        .char_indices()
        .skip_while(|(at, _)| *at < t.name_span.end as usize)
        .find(|(_, c)| match c {
            '(' | '[' => {
                depth += 1;
                false
            }
            ')' | ']' => {
                depth -= 1;
                false
            }
            '{' => depth == 0,
            _ => false,
        })
        .map_or(t.span.end as usize, |(at, _)| at);
    let header = slice(t.span.start, header_end as u32).trim_end();
    let mut members: Vec<(u32, String)> = t
        .methods
        .iter()
        .map(|f| {
            let end = match f.is_signature_only {
                true => f.span.end,
                false => f.body.span.start,
            };
            (
                f.span.start,
                indented(text, f.span.start, slice(f.span.start, end)),
            )
        })
        .collect();
    for assoc in &t.assoc_types {
        let start = assoc.span.start;
        members.push((start, indented(text, start, slice(start, assoc.span.end))));
    }
    members.sort_by_key(|(start, _)| *start);
    assemble(header, members.into_iter().map(|(_, member)| member))
}

/// The header and the members, one a line, in braces.
fn assemble(header: &str, members: impl Iterator<Item = String>) -> String {
    let members: Vec<String> = members.collect();
    match members.is_empty() {
        true => format!("{header} {{}}"),
        false => format!("{header} {{\n{}\n}}", members.join("\n")),
    }
}

/// `part`, which starts at byte `start` of `text`, as a member of the
/// trait: indented by two spaces, a line of it that goes on below the
/// first as far further in as it is in the file.
fn indented(text: &str, start: u32, part: &str) -> String {
    let line_start = text[..start as usize].rfind('\n').map_or(0, |at| at + 1);
    let column = text[line_start..start as usize].chars().count();
    let mut lines = part.trim_end().lines();
    let mut out = format!("  {}", lines.next().unwrap_or_default());
    for line in lines {
        let kept = line.chars().take(column).all(char::is_whitespace);
        let line = match kept {
            true => line.chars().skip(column).collect::<String>(),
            false => line.trim_start().to_string(),
        };
        out.push_str(&format!("\n  {line}"));
    }
    out
}

/// The signature of the trait `t` from its declaration alone.
fn written(t: &TraitDecl) -> String {
    let mut header = format!("trait {}", t.name);
    if !t.params.is_empty() {
        let params: Vec<String> = t.params.iter().map(|p| resolve(*p)).collect();
        header.push_str(&format!("({})", params.join(", ")));
    }
    if !t.supertraits.is_empty() {
        let supers: Vec<String> = t.supertraits.iter().map(trait_ref).collect();
        header.push_str(&format!(": {}", supers.join(" + ")));
    }
    header.push_str(&where_clauses(&t.param_where_clauses));
    let mut members: Vec<String> = t
        .assoc_types
        .iter()
        .map(|assoc| {
            let bounds: Vec<String> = assoc.bounds.iter().map(trait_ref).collect();
            match bounds.is_empty() {
                true => format!("  type {}", assoc.name),
                false => format!("  type {}: {}", assoc.name, bounds.join(" + ")),
            }
        })
        .collect();
    members.extend(t.methods.iter().map(|f| format!("  {}", method(f))));
    assemble(&header, members.into_iter())
}

/// `fn name(params) -> ret where ...` of the method `f`.
fn method(f: &FnDecl) -> String {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| {
            let name = match &p.pattern.kind {
                PatternKind::Ident(name) => resolve(*name),
                _ => "_".to_string(),
            };
            match &p.ty {
                Some(ty) => format!("{name}: {}", type_expr(ty)),
                None => name,
            }
        })
        .collect();
    let mut out = format!("fn {}({})", f.name, params.join(", "));
    if let Some(ret) = &f.return_type {
        out.push_str(&format!(" -> {}", type_expr(ret)));
    }
    out.push_str(&where_clauses(&f.where_clauses));
    out
}

fn where_clauses(clauses: &[WhereClause]) -> String {
    if clauses.is_empty() {
        return String::new();
    }
    let clauses: Vec<String> = clauses
        .iter()
        .map(|c| {
            let name = Qualifier::written(c.trait_module, c.trait_name);
            format!("{}: {name}{}", c.type_param, args(&c.trait_args))
        })
        .collect();
    format!(" where {}", clauses.join(", "))
}

fn trait_ref(r: &TraitRef) -> String {
    format!("{}{}", Qualifier::written(r.module, r.name), args(&r.args))
}

/// `(a, b)` of the type arguments `args`; nothing for none.
fn args(args: &[TypeExpr]) -> String {
    match args.is_empty() {
        true => String::new(),
        false => {
            let args: Vec<String> = args.iter().map(type_expr).collect();
            format!("({})", args.join(", "))
        }
    }
}

fn type_expr(te: &TypeExpr) -> String {
    match &te.kind {
        TypeExprKind::Named { module, name, .. } => Qualifier::written(*module, *name),
        TypeExprKind::Generic {
            module,
            name,
            args: a,
            ..
        } => format!("{}{}", Qualifier::written(*module, *name), args(a)),
        TypeExprKind::Tuple(elems) => {
            let elems: Vec<String> = elems.iter().map(type_expr).collect();
            format!("({})", elems.join(", "))
        }
        TypeExprKind::Function(params, ret) => {
            let params: Vec<String> = params.iter().map(type_expr).collect();
            format!("Fn({}) -> {}", params.join(", "), type_expr(ret))
        }
        TypeExprKind::SelfType => "Self".to_string(),
        TypeExprKind::AssocProj {
            receiver,
            trait_module,
            trait_name,
            trait_name_span,
            assoc_name,
        } => match (&receiver.kind, trait_name_span) {
            (TypeExprKind::SelfType, None) => format!("Self::{assoc_name}"),
            _ => format!(
                "<{} as {}>::{assoc_name}",
                type_expr(receiver),
                Qualifier::written(*trait_module, *trait_name)
            ),
        },
        TypeExprKind::AnonRecord { fields, tail } => {
            let mut parts: Vec<String> = fields
                .iter()
                .map(|(name, ty)| format!("{name}: {}", type_expr(ty)))
                .collect();
            if let Some((rest, _)) = tail {
                parts.push(format!("...{rest}"));
            }
            format!("{{{}}}", parts.join(", "))
        }
    }
}
