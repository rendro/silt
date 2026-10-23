//! Hover on a trait's name: its signature (the trait's header and its
//! methods, without their default bodies) and its documentation, as a
//! type shows its declaration. And hover on a type of a builtin module,
//! which no file declares: its declaration and its documentation.

use crate::ast::{
    Decl, FnDecl, PatternKind, Qualifier, TraitDecl, TraitRef, TypeBody, TypeDecl, TypeExpr,
    TypeExprKind, WhereClause,
};
use crate::defs::{TraitId, TypeId};
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
            let (decl, q) = self.trait_decl(doc, id)?;
            match q {
                // A trait of the prelude: the checker's declaration.
                None => (signature(&decl, None), decl.doc.clone()),
                // A builtin module's (`json.Encode`): the declaration of
                // the registry, and its section of the module's page.
                // It is written as a program names it: the trait and the
                // module's types with the module (`json.Value`).
                Some(q) => {
                    let section = self.builtin_docs.get(&format!("{}.{}", q.module, decl.name));
                    (written(&decl, Some(&q)), section.map(|s| below_signature(s)))
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

impl Server {
    /// The declaration of the trait `id` in the session of the open
    /// document `doc`, with how a builtin module's names are written
    /// (`None` for a trait of a file or of the prelude).
    fn trait_decl(&self, doc: &Document, id: TraitId) -> Option<(TraitDecl, Option<Qualify>)> {
        let module = doc.module.as_ref()?;
        let session = &self.projects.get(&module.project)?.session;
        let def = session.defs().get(id.0);
        if !def.module.is_builtin() && def.span.is_in_source() {
            let analysis = session.module_analysis(def.module)?;
            let decl = declared(&analysis.ast.decls, |t| t.name_span == def.span)?;
            return Some((decl.clone(), None));
        }
        let (name, module) = crate::defs::builtin_trait_of_id(id.0)?;
        match module {
            None => crate::typechecker::builtin_trait_decls()
                .into_iter()
                .find(|t| resolve(t.name) == name)
                .map(|decl| (decl, None)),
            Some(module) => {
                let registry = crate::builtins::registry::registry();
                let of = registry.modules.iter().find(|m| m.name == module)?;
                let program = crate::builtins::registry::parse(of.traits);
                let decl = declared(&program.decls, |t| resolve(t.name) == name)?;
                Some((decl.clone(), Some(Qualify::of(of))))
            }
        }
    }

    /// The trait `prefix` names in the open document `doc` (`Greet`, a
    /// trait of the prelude, `m.Greet` through an imported module), as
    /// the module's analysis binds it.
    pub(super) fn trait_written(&self, doc: &Document, prefix: &str) -> Option<TraitId> {
        use crate::typechecker::names::Binding;
        let scope = &self.checked_module(doc)?.scope;
        let module = doc.module.as_ref()?;
        let session = &self.projects.get(&module.project)?.session;
        let (_, builtin) = crate::typechecker::names::builtins();
        let binding = match prefix.split_once('.') {
            None => {
                let name = crate::intern::intern(prefix);
                (scope.values.get(&name))
                    .or_else(|| scope.types.get(&name))
                    .or_else(|| builtin.prelude.values.get(&name))
                    .or_else(|| builtin.prelude.types.get(&name))
            }
            Some((m, t)) => {
                let Some(Binding::Module(id)) = scope.values.get(&crate::intern::intern(m)) else {
                    return None;
                };
                let exports = match id.is_builtin() {
                    true => builtin.modules.get(id),
                    false => session.module_analysis(*id).map(|a| &a.scope.exports),
                }?;
                let name = crate::intern::intern(t);
                exports.values.get(&name).or_else(|| exports.types.get(&name))
            }
        };
        let Some(Binding::Def(id)) = binding else {
            return None;
        };
        match session.defs().get(*id).kind {
            crate::defs::DefKind::Trait(t) => Some(t),
            _ => None,
        }
    }

    /// The methods of the trait `id` a program calls through it
    /// (`Greet.greet(x)`): those that take a value of the type, but
    /// `Display`'s and `Equal`'s, each with its signature.
    pub(super) fn trait_call_methods(&self, doc: &Document, id: TraitId) -> Vec<TraitMethod> {
        let Some((decl, q)) = self.trait_decl(doc, id) else {
            return Vec::new();
        };
        let uncalled = crate::defs::builtin_trait_of_id(id.0)
            .is_some_and(|(name, _)| matches!(name, "Display" | "Equal"));
        decl.methods
            .iter()
            .filter(|f| !uncalled && takes_self(f))
            .map(|f| trait_method(q.as_ref(), f))
            .collect()
    }
}

/// A method of a trait as a call through the trait shows it.
pub(super) struct TraitMethod {
    pub(super) name: crate::intern::Symbol,
    /// `fn greet(self) -> String`.
    pub(super) label: String,
    /// Where each parameter is in `label`.
    pub(super) params: Vec<[u32; 2]>,
    pub(super) doc: Option<String>,
}

/// Whether the method `f` takes a value of the trait's type: a
/// parameter `self`, or one of type `Self`.
fn takes_self(f: &FnDecl) -> bool {
    f.params.iter().any(|p| {
        matches!(&p.pattern.kind, PatternKind::Ident(name) if resolve(*name) == "self")
            || matches!(&p.ty, Some(TypeExpr { kind: TypeExprKind::SelfType, .. }))
    })
}

/// The method `f` as `method` writes it, with where each parameter is.
fn trait_method(q: Option<&Qualify>, f: &FnDecl) -> TraitMethod {
    let mut label = format!("fn {}(", f.name);
    let mut params = Vec::new();
    for (k, p) in f.params.iter().enumerate() {
        if k > 0 {
            label.push_str(", ");
        }
        let name = match &p.pattern.kind {
            PatternKind::Ident(name) => resolve(*name),
            _ => "_".to_string(),
        };
        let text = match &p.ty {
            Some(ty) => format!("{name}: {}", type_expr(q, ty)),
            None => name,
        };
        let start = label.len() as u32;
        label.push_str(&text);
        params.push([start, label.len() as u32]);
    }
    label.push(')');
    if let Some(ret) = &f.return_type {
        label.push_str(&format!(" -> {}", type_expr(q, ret)));
    }
    label.push_str(&where_clauses(q, &f.where_clauses));
    TraitMethod {
        name: f.name,
        label,
        params,
        doc: f.doc.clone(),
    }
}

impl Server {
    /// What hover shows for silt's own type `id`: its declaration as the
    /// registry has it (a builtin module's written as a program names
    /// it: `json.Value`), and below it its documentation, if it has any
    /// (no type of the prelude has).
    pub(super) fn builtin_type_hover_text(&self, doc: &Document, id: TypeId) -> Option<String> {
        let module = doc.module.as_ref()?;
        let def = self.projects.get(&module.project)?.session.defs().get(id.0);
        let name = resolve(def.name);
        let Some(module) = def.module.builtin_name() else {
            return prelude_type_declaration(&name).map(|text| format!("```silt\n{text}\n```"));
        };
        let registry = crate::builtins::registry::registry();
        let of = registry.modules.iter().find(|m| m.name == module)?;
        let q = Qualify::of(of);
        let program = crate::builtins::registry::parse(of.types);
        let declaration = match program.decls.iter().find_map(|decl| match decl {
            Decl::Type(td) if resolve(td.name) == name => Some(td),
            _ => None,
        }) {
            Some(td) => type_declaration(td, Some(&q)),
            // An opaque handle: its name and its type arguments.
            None => {
                let (_, arity) = of.opaque.iter().find(|(opaque, _)| *opaque == name)?;
                let params: Vec<String> = (0..*arity)
                    .map(|k| char::from(b'a' + k as u8).to_string())
                    .collect();
                match params.is_empty() {
                    true => format!("type {module}.{name}"),
                    false => format!("type {module}.{name}({})", params.join(", ")),
                }
            }
        };
        let mut value = format!("```silt\n{declaration}\n```");
        if let Some(text) = crate::builtins::registry::docs::type_docs()
            .get(&format!("{module}.{name}"))
            .map(|section| below_signature(section))
            .filter(|text| !text.is_empty())
        {
            value.push_str(&format!("\n\n---\n\n{text}"));
        }
        Some(value)
    }
}

/// The declaration of the prelude's type `name`: an enum's
/// (`Option(a)`), one variant a line; another's its name and its type
/// parameters (`type Int`, `type Map(k, v)`). `None` for a name no
/// program writes (`TypeOf`).
fn prelude_type_declaration(name: &str) -> Option<String> {
    let program = crate::builtins::registry::parse(crate::builtins::registry::PRELUDE_TYPES);
    if let Some(td) = program.decls.iter().find_map(|decl| match decl {
        Decl::Type(td) if resolve(td.name) == name => Some(td),
        _ => None,
    }) {
        return Some(type_declaration(td, None));
    }
    // Programs write `()` for it: say the two are one type.
    if name == "Unit" {
        return Some("type Unit = ()".to_string());
    }
    let params = match crate::types::builtins::lookup(name)?.arity? {
        0 => "",
        1 => "(a)",
        2 => "(k, v)",
        _ => return None,
    };
    Some(format!("type {name}{params}"))
}

/// The declaration of a builtin type `td`, one variant or field a line,
/// its names written as `q` qualifies them (a builtin module's), or as
/// they are (the prelude's).
fn type_declaration(td: &TypeDecl, q: Option<&Qualify>) -> String {
    let mut header = format!("type {}", named(q, None, td.name));
    if !td.params.is_empty() {
        let params: Vec<String> = td.params.iter().map(|p| resolve(*p)).collect();
        header.push_str(&format!("({})", params.join(", ")));
    }
    let members: Vec<String> = match &td.body {
        TypeBody::Enum(variants) => variants
            .iter()
            .map(|v| format!("  {}{},", v.name, args(q, &v.fields)))
            .collect(),
        TypeBody::Record(fields) => fields
            .iter()
            .map(|f| format!("  {}: {},", f.name, type_expr(q, &f.ty)))
            .collect(),
        TypeBody::Alias(target) => return format!("{header} = {}", type_expr(q, target)),
    };
    assemble(&header, members.into_iter())
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
        return written(t, None);
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

/// The names of a builtin module that a program writes with the module
/// (`json.Value`), and the module.
struct Qualify {
    module: &'static str,
    names: Vec<&'static str>,
}

impl Qualify {
    /// The names of the builtin module `of`: its types, its opaque
    /// handles and its traits.
    fn of(of: &'static crate::builtins::registry::Module) -> Qualify {
        let names = (of.type_decls.iter().map(|t| t.name))
            .chain(of.opaque.iter().map(|(name, _)| *name))
            .chain(of.trait_names.iter().copied())
            .collect();
        Qualify {
            module: of.name,
            names,
        }
    }
}

/// The name `name`, after the qualifier `module`, as a program writes
/// it: with the module of `q` if it is one of its names, with its own
/// module if it is a type of another builtin module.
fn named(q: Option<&Qualify>, module: Option<Qualifier>, name: crate::intern::Symbol) -> String {
    let Some(q) = q.filter(|_| module.is_none()) else {
        return Qualifier::written(module, name);
    };
    let text = resolve(name);
    if q.names.contains(&text.as_str()) {
        return format!("{}.{text}", q.module);
    }
    // A type of another builtin module (`DateTime` in `fs.FileStat`).
    match crate::defs::builtin_types()
        .iter()
        .find(|(ty, _)| *ty == text)
    {
        Some((_, Some(other))) => format!("{other}.{text}"),
        _ => text,
    }
}

/// The signature of the trait `t` from its declaration alone, its names
/// qualified as `q` says.
fn written(t: &TraitDecl, q: Option<&Qualify>) -> String {
    let mut header = format!("trait {}", named(q, None, t.name));
    if !t.params.is_empty() {
        let params: Vec<String> = t.params.iter().map(|p| resolve(*p)).collect();
        header.push_str(&format!("({})", params.join(", ")));
    }
    if !t.supertraits.is_empty() {
        let supers: Vec<String> = t.supertraits.iter().map(|r| trait_ref(q, r)).collect();
        header.push_str(&format!(": {}", supers.join(" + ")));
    }
    header.push_str(&where_clauses(q, &t.param_where_clauses));
    let mut members: Vec<String> = t
        .assoc_types
        .iter()
        .map(|assoc| {
            let bounds: Vec<String> = assoc.bounds.iter().map(|r| trait_ref(q, r)).collect();
            match bounds.is_empty() {
                true => format!("  type {}", assoc.name),
                false => format!("  type {}: {}", assoc.name, bounds.join(" + ")),
            }
        })
        .collect();
    members.extend(t.methods.iter().map(|f| format!("  {}", method(q, f))));
    assemble(&header, members.into_iter())
}

/// `fn name(params) -> ret where ...` of the method `f`.
fn method(q: Option<&Qualify>, f: &FnDecl) -> String {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| {
            let name = match &p.pattern.kind {
                PatternKind::Ident(name) => resolve(*name),
                _ => "_".to_string(),
            };
            match &p.ty {
                Some(ty) => format!("{name}: {}", type_expr(q, ty)),
                None => name,
            }
        })
        .collect();
    let mut out = format!("fn {}({})", f.name, params.join(", "));
    if let Some(ret) = &f.return_type {
        out.push_str(&format!(" -> {}", type_expr(q, ret)));
    }
    out.push_str(&where_clauses(q, &f.where_clauses));
    out
}

fn where_clauses(q: Option<&Qualify>, clauses: &[WhereClause]) -> String {
    if clauses.is_empty() {
        return String::new();
    }
    let clauses: Vec<String> = clauses
        .iter()
        .map(|c| {
            let name = named(q, c.trait_module, c.trait_name);
            format!("{}: {name}{}", c.type_param, args(q, &c.trait_args))
        })
        .collect();
    format!(" where {}", clauses.join(", "))
}

fn trait_ref(q: Option<&Qualify>, r: &TraitRef) -> String {
    format!("{}{}", named(q, r.module, r.name), args(q, &r.args))
}

/// `(a, b)` of the type arguments `args`; nothing for none.
fn args(q: Option<&Qualify>, args: &[TypeExpr]) -> String {
    match args.is_empty() {
        true => String::new(),
        false => {
            let args: Vec<String> = args.iter().map(|a| type_expr(q, a)).collect();
            format!("({})", args.join(", "))
        }
    }
}

fn type_expr(q: Option<&Qualify>, te: &TypeExpr) -> String {
    let all = |types: &[TypeExpr]| types.iter().map(|t| type_expr(q, t)).collect::<Vec<_>>();
    match &te.kind {
        TypeExprKind::Named { module, name, .. } => named(q, *module, *name),
        TypeExprKind::Generic {
            module,
            name,
            args: a,
            ..
        } => format!("{}{}", named(q, *module, *name), args(q, a)),
        TypeExprKind::Tuple(elems) => format!("({})", all(elems).join(", ")),
        TypeExprKind::Function(params, ret) => {
            format!("Fn({}) -> {}", all(params).join(", "), type_expr(q, ret))
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
                type_expr(q, receiver),
                named(q, *trait_module, *trait_name)
            ),
        },
        TypeExprKind::AnonRecord { fields, tail } => {
            let mut parts: Vec<String> = fields
                .iter()
                .map(|(name, ty)| format!("{name}: {}", type_expr(q, ty)))
                .collect();
            if let Some((rest, _)) = tail {
                parts.push(format!("...{rest}"));
            }
            format!("{{{}}}", parts.join(", "))
        }
    }
}
