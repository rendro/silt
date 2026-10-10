//! The resolver: what each name of a module means.
//!
//! It runs on a module after parsing and before inference. It enters the
//! module's top-level declarations in the session's [`DefTable`], builds
//! the module's [`ModuleScope`] from them and from its imports, and writes
//! a [`Res`] on every AST node that names something: identifiers, the
//! head of `m.f` when `m` is a module, constructor and record patterns,
//! record literals, type names and trait references.
//!
//! The import rule lives here:
//!
//! - `import m` binds `m`, and only `m`: a member is written `m.x`.
//! - `import m.{ a, B }` binds `a` and `B`, each of which must be a
//!   member of `m`; importing an enum does not bind its variants.
//! - `import m as n` is `import m`, bound as `n`.
//! - The builtin modules follow the same rule; the prelude (primitive
//!   types, `Option`, `Result`, their variants, `print`, `println` and
//!   `panic`) needs no import, and a module's own declarations and
//!   imports shadow it.
//! - Two enums of a scope may have variants of one name: a bare use of
//!   the name is ambiguous, and `C.Red` / `D.Red` say which.
//!
//! A name that resolves to nothing is reported here, with the module or
//! the import that would supply it. A name from a module that failed to
//! load resolves to [`Res::Error`] silently: that module is reported once,
//! at its import.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast::*;
use crate::defs::{
    ANON_RECORD, BuiltinDefs, Def, DefId, DefKind, DefTable, Res, TraitId, TypeId, Vis,
    builtin_types,
};
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, intern, resolve};
use crate::session::ModuleId;
use crate::source::Span;

use super::suggest::suggest_similar;

/// What a top-level name of a scope is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Def(DefId),
    Module(ModuleId),
    /// Variants of several enums with one name: a bare use is an error.
    Ambiguous(Vec<DefId>),
    /// A name from a module that failed to load.
    Poisoned,
}

/// The names a module offers its importers.
#[derive(Clone, Debug, Default)]
pub struct Exports {
    pub values: HashMap<Symbol, Binding>,
    pub types: HashMap<Symbol, Binding>,
    /// The module's own definitions that are not `pub`, so that an
    /// importer asking for one is told it is private.
    pub private: HashMap<Symbol, DefId>,
}

impl Exports {
    /// The member `name`: a value, else a type or trait.
    fn member(&self, name: Symbol) -> Option<&Binding> {
        self.values.get(&name).or_else(|| self.types.get(&name))
    }
}

/// The top-level names of a module: its own declarations and what its
/// imports bind, in two namespaces (values; types and traits).
#[derive(Clone, Debug, Default)]
pub struct ModuleScope {
    pub values: HashMap<Symbol, Binding>,
    pub types: HashMap<Symbol, Binding>,
    /// For a REPL cell: the variants of the enums it imports from the
    /// earlier cell, which a later cell sees as the earlier one did. Its
    /// own declarations shadow them.
    implied: HashMap<Symbol, Binding>,
    /// What an importer of the module sees.
    pub exports: Exports,
}

impl ModuleScope {
    /// For a REPL cell: the variants of the enums it imports from the
    /// earlier cell.
    pub fn implied(&self) -> &HashMap<Symbol, Binding> {
        &self.implied
    }
}

/// What an import of a module names, for the resolver.
#[derive(Clone, Copy)]
pub enum Imported<'a> {
    /// A builtin module (its pseudo-module).
    Builtin(ModuleId),
    /// A module of the graph, resolved already.
    Module(ModuleId, &'a ModuleScope),
    /// An earlier REPL cell.
    Cell(ModuleId, &'a ModuleScope),
    /// A module that failed to load, or closes a cycle.
    Poisoned,
}

/// What kind of module is resolved.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    File,
    /// A REPL cell: it offers everything it declares and everything it
    /// sees through the earlier cell.
    Cell,
    /// An embedder's host module: bodiless signatures.
    Host,
}

/// What resolving a module gives.
pub struct Resolution {
    pub scope: ModuleScope,
    pub diagnostics: Vec<Diagnostic>,
}

// ── The builtins ─────────────────────────────────────────────────────

/// The builtin pseudo-modules: what the prelude binds and what each
/// builtin module offers.
#[derive(Debug, Default)]
pub struct BuiltinScopes {
    pub prelude: Exports,
    pub modules: HashMap<ModuleId, Exports>,
}

thread_local! {
    static BUILTINS: RefCell<Option<(u64, Arc<BuiltinDefs>, Arc<BuiltinScopes>)>> =
        const { RefCell::new(None) };
}

/// The builtin definitions and scopes, built once per thread (and again
/// after each `intern::reset`).
pub fn builtins() -> (Arc<BuiltinDefs>, Arc<BuiltinScopes>) {
    let generation = crate::intern::generation();
    if let Some(found) = BUILTINS.with(|cell| match &*cell.borrow() {
        Some((built, defs, scopes)) if *built == generation => Some((defs.clone(), scopes.clone())),
        _ => None,
    }) {
        return found;
    }
    let (defs, scopes) = build_builtins();
    let (defs, scopes) = (Arc::new(defs), Arc::new(scopes));
    BUILTINS.with(|cell| *cell.borrow_mut() = Some((generation, defs.clone(), scopes.clone())));
    (defs, scopes)
}

/// The builtin definition `id`; `None` for the id of a module's
/// definition.
pub fn builtin_def(id: DefId) -> Option<Def> {
    builtins().0.defs.get(id.0 as usize).cloned()
}

/// A new definition table over the builtins.
pub fn new_def_table() -> DefTable {
    DefTable::new(builtins().0)
}

/// The builtin definitions, from the builtin registry: the types, the
/// traits, each enum's variants, the prelude's functions, then each
/// module's rows. A row of a cargo feature that is not built is not
/// offered; neither are the rows of a module that is not built.
fn build_builtins() -> (BuiltinDefs, BuiltinScopes) {
    let registry = crate::builtins::registry::registry();
    let enums: HashMap<&str, &[(&str, usize)]> = registry
        .types()
        .map(|(_, ty)| (ty.name, ty.variants()))
        .filter(|(_, variants)| !variants.is_empty())
        .collect();
    let mut defs = BuiltinDefs::default();
    let mut scopes = BuiltinScopes::default();
    let add = |defs: &mut BuiltinDefs, module: ModuleId, name: Symbol, kind: DefKind| {
        let id = DefId(defs.defs.len() as u32);
        defs.defs.push(Def {
            module,
            name,
            span: Span::BUILTIN,
            vis: Vis::Pub,
            kind,
        });
        id
    };
    // The types first, so that their ids are those of
    // `defs::builtin_types`; then each enum's variants.
    let mut prelude_exports = Exports::default();
    let mut module_exports: HashMap<ModuleId, Exports> = HashMap::new();
    let mut types = Vec::new();
    for (k, (name, module)) in builtin_types().iter().enumerate() {
        let ty = TypeId(DefId(k as u32));
        let module_id = module.map_or(ModuleId::PRELUDE, |m| {
            ModuleId::builtin(m).expect("a builtin module")
        });
        let name = intern(name);
        let id = add(&mut defs, module_id, name, DefKind::Type(ty));
        debug_assert_eq!(id, ty.0);
        if resolve(name) != ANON_RECORD {
            let exports = match module {
                None => &mut prelude_exports,
                Some(_) => module_exports.entry(module_id).or_default(),
            };
            exports.types.insert(name, Binding::Def(id));
        }
        types.push((id, module_id, name));
    }
    // The traits, so that their ids are those of
    // `defs::builtin_trait_id`.
    let mut traits = Vec::new();
    for name in crate::defs::BUILTIN_TRAITS {
        let name = intern(name);
        let trait_id = TraitId(DefId(defs.defs.len() as u32));
        let id = add(&mut defs, ModuleId::PRELUDE, name, DefKind::Trait(trait_id));
        traits.push((id, name));
    }
    for (id, module_id, name) in types {
        let Some(variants) = enums.get(resolve(name).as_str()) else {
            continue;
        };
        let exports = if module_id == ModuleId::PRELUDE {
            &mut prelude_exports
        } else {
            module_exports.entry(module_id).or_default()
        };
        let mut ids = Vec::new();
        for (ordinal, (variant, arity)) in variants.iter().enumerate() {
            let variant = intern(variant);
            let v = add(
                &mut defs,
                module_id,
                variant,
                DefKind::Variant {
                    ty: TypeId(id),
                    ordinal: ordinal as u16,
                    arity: *arity as u16,
                },
            );
            exports.values.insert(variant, Binding::Def(v));
            ids.push(v);
        }
        defs.variants.insert(id, ids);
    }

    // The prelude's functions and traits.
    let prelude = ModuleId::PRELUDE;
    let mut exports = prelude_exports;
    for name in crate::module::builtin_free_function_names() {
        let name = intern(name);
        let id = add(&mut defs, prelude, name, DefKind::Fn);
        exports.values.insert(name, Binding::Def(id));
    }
    for (id, name) in traits {
        exports.types.insert(name, Binding::Def(id));
    }
    scopes.prelude = exports;

    // The builtin modules' functions and constants. A module that is
    // not built keeps its types and variants, so that a use of one is
    // answered with the module and the feature it needs; nothing can
    // import it (the session rejects the import).
    for module in &registry.modules {
        let id = ModuleId::builtin(module.name).expect("a builtin module");
        let mut exports = module_exports.remove(&id).unwrap_or_default();
        let mut members: Vec<Symbol> = module.enabled_rows().map(|row| intern(row.name)).collect();
        members.sort_by_key(|m| resolve(*m));
        for member in members {
            let def = add(&mut defs, id, member, DefKind::Fn);
            exports.values.insert(member, Binding::Def(def));
        }
        scopes.modules.insert(id, exports);
    }
    // A type a module shares with the module that declares it
    // (`ParseError` is the error of `int.parse` and of `float.parse`) is
    // reached through both, and so are its variants.
    for module in registry.enabled_modules() {
        let id = ModuleId::builtin(module.name).expect("a builtin module");
        for (owner, ty_name) in module.shares {
            let owner = ModuleId::builtin(owner).expect("a builtin module");
            let ty_name = intern(ty_name);
            let Some(Binding::Def(ty)) = scopes.modules[&owner].types.get(&ty_name).cloned() else {
                continue;
            };
            let variants = defs.variants.get(&ty).cloned().unwrap_or_default();
            let exports = scopes.modules.get_mut(&id).expect("the module's exports");
            exports.types.insert(ty_name, Binding::Def(ty));
            for v in variants {
                exports
                    .values
                    .insert(defs.defs[v.0 as usize].name, Binding::Def(v));
            }
        }
    }
    (defs, scopes)
}

// ── Resolving a module ───────────────────────────────────────────────

/// Resolve `program`, the module `module`: enter its definitions in
/// `defs`, bind its imports (`imports`, by the module name written after
/// `import`), and write what each name means on the AST.
pub fn resolve_module(
    program: &mut Program,
    module: ModuleId,
    kind: ModuleKind,
    imports: &HashMap<Symbol, Imported<'_>>,
    defs: &mut DefTable,
) -> Resolution {
    defs.clear_module(module);
    let (_, builtins) = builtins();
    let mut scope = ModuleScope::default();
    let mut diagnostics = Vec::new();
    declare(program, module, kind, defs, &mut scope);
    bind_imports(
        program,
        kind,
        imports,
        defs,
        &builtins,
        &mut scope,
        &mut diagnostics,
    );
    // What a declaration that did not parse would have bound is
    // declared, and what it means is unknown: a use of it is no error.
    for name in &program.unknown {
        scope.values.entry(*name).or_insert(Binding::Poisoned);
        scope.types.entry(*name).or_insert(Binding::Poisoned);
    }
    let mut resolver = Resolver {
        defs,
        kind,
        scope: &scope,
        imports,
        builtins: &builtins,
        locals: Vec::new(),
        previous: HashMap::new(),
        diagnostics,
    };
    for decl in &mut program.decls {
        resolver.decl(decl);
    }
    let mut diagnostics = resolver.diagnostics;
    if kind == ModuleKind::File {
        report_private_in_public(program, module, defs, &mut diagnostics);
    }
    Resolution { scope, diagnostics }
}

/// The module an import names.
fn import_module(target: &ImportTarget) -> Symbol {
    match target {
        ImportTarget::Module(m) | ImportTarget::Items(m, _) | ImportTarget::Alias(m, _, _) => *m,
    }
}

/// Enter the definitions of `program` and bind them in `scope`.
fn declare(
    program: &Program,
    module: ModuleId,
    kind: ModuleKind,
    defs: &mut DefTable,
    scope: &mut ModuleScope,
) {
    let vis = |is_pub: bool| {
        if is_pub || kind != ModuleKind::File {
            Vis::Pub
        } else {
            Vis::Private
        }
    };
    let mut own: Vec<(Symbol, DefId, bool)> = Vec::new();
    for decl in &program.decls {
        match decl {
            Decl::Fn(f) => {
                let def_kind = if kind == ModuleKind::Host {
                    DefKind::Host
                } else {
                    DefKind::Fn
                };
                let id = defs.add(Def {
                    module,
                    name: f.name,
                    span: f.name_span,
                    vis: vis(f.is_pub),
                    kind: def_kind,
                });
                own.push((f.name, id, false));
            }
            Decl::Let {
                pattern, is_pub, ..
            } => {
                let mut binders = Vec::new();
                pattern_binders(pattern, &mut binders);
                for (name, span) in binders {
                    let id = defs.add(Def {
                        module,
                        name,
                        span,
                        vis: vis(*is_pub),
                        kind: DefKind::Let,
                    });
                    own.push((name, id, false));
                }
            }
            Decl::Type(td) => {
                let placeholder = DefKind::TypeAlias;
                let id = defs.add(Def {
                    module,
                    name: td.name,
                    span: td.name_span,
                    vis: vis(td.is_pub),
                    kind: placeholder,
                });
                let ty = TypeId(id);
                match &td.body {
                    TypeBody::Alias(_) => {}
                    TypeBody::Record(_) => set_kind(defs, id, DefKind::Type(ty)),
                    TypeBody::Enum(variants) => {
                        set_kind(defs, id, DefKind::Type(ty));
                        for (ordinal, variant) in variants.iter().enumerate() {
                            let v = defs.add(Def {
                                module,
                                name: variant.name,
                                span: variant.name_span,
                                vis: vis(td.is_pub),
                                kind: DefKind::Variant {
                                    ty,
                                    ordinal: ordinal as u16,
                                    arity: variant.fields.len() as u16,
                                },
                            });
                            defs.add_variant(id, v);
                            own.push((variant.name, v, false));
                        }
                    }
                }
                own.push((td.name, id, true));
            }
            Decl::Trait(t) => {
                let id = defs.add(Def {
                    module,
                    name: t.name,
                    span: t.name_span,
                    vis: vis(t.is_pub),
                    kind: DefKind::TypeAlias,
                });
                set_kind(defs, id, DefKind::Trait(TraitId(id)));
                own.push((t.name, id, true));
            }
            Decl::TraitImpl(_) | Decl::Import(..) => {}
        }
    }
    for (name, id, is_type) in own {
        let def = *defs.get(id);
        let namespace = if is_type {
            &mut scope.types
        } else {
            &mut scope.values
        };
        bind_variant_aware(namespace, name, id, defs);
        if def.vis == Vis::Pub {
            let exported = if is_type {
                &mut scope.exports.types
            } else {
                &mut scope.exports.values
            };
            bind_variant_aware(exported, name, id, defs);
        } else {
            scope.exports.private.entry(name).or_insert(id);
        }
    }
}

/// Change the kind of the definition `id`, entered a moment ago: a type
/// or trait is named by its own id, known once it has one.
fn set_kind(defs: &mut DefTable, id: DefId, kind: DefKind) {
    defs.set_kind(id, kind);
}

/// Bind `name` to the definition `id` in `namespace`. A variant whose name
/// another enum's variant has already is bound to the ambiguity set of
/// both; any other name keeps its first binding (a name declared twice is
/// reported by the parser).
fn bind_variant_aware(
    namespace: &mut HashMap<Symbol, Binding>,
    name: Symbol,
    id: DefId,
    defs: &DefTable,
) {
    let is_variant = |id: DefId| matches!(defs.get(id).kind, DefKind::Variant { .. });
    match namespace.get_mut(&name) {
        None => {
            namespace.insert(name, Binding::Def(id));
        }
        Some(Binding::Def(existing)) if is_variant(*existing) && is_variant(id) => {
            if *existing != id {
                let first = *existing;
                namespace.insert(name, Binding::Ambiguous(vec![first, id]));
            }
        }
        Some(Binding::Ambiguous(ids)) if is_variant(id) => {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Some(_) => {}
    }
}

/// Bind the names the imports of `program` bring.
fn bind_imports(
    program: &Program,
    kind: ModuleKind,
    imports: &HashMap<Symbol, Imported<'_>>,
    defs: &DefTable,
    builtins: &BuiltinScopes,
    scope: &mut ModuleScope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut item_bindings: HashMap<Symbol, (Binding, Span)> = HashMap::new();
    for decl in &program.decls {
        let Decl::Import(target, _) = decl else {
            continue;
        };
        let module = import_module(target);
        let imported = imports.get(&module).copied().unwrap_or(Imported::Poisoned);
        let exports: Option<&Exports> = match imported {
            Imported::Builtin(id) => builtins.modules.get(&id),
            Imported::Module(_, s) | Imported::Cell(_, s) => Some(&s.exports),
            Imported::Poisoned => None,
        };
        let module_binding = match imported {
            Imported::Builtin(id) | Imported::Module(id, _) | Imported::Cell(id, _) => {
                Binding::Module(id)
            }
            Imported::Poisoned => Binding::Poisoned,
        };
        match target {
            ImportTarget::Module(m) => {
                scope.values.entry(*m).or_insert(module_binding);
            }
            ImportTarget::Alias(_, alias, _) => {
                scope.values.entry(*alias).or_insert(module_binding);
            }
            ImportTarget::Items(m, items) => {
                let is_cell = matches!(imported, Imported::Cell(..));
                for (item, item_span) in items {
                    // An item two `import m.{ ... }` lines bind: fine when
                    // both name one definition, an error otherwise.
                    let named = exports.and_then(|e| e.member(*item)).cloned();
                    match item_bindings.get(item) {
                        Some((first, _)) if Some(first) == named.as_ref() => continue,
                        Some((_, first_span)) => {
                            diagnostics.push(
                                Diagnostic::error(
                                    Code::DuplicateTopLevel,
                                    *item_span,
                                    format!(
                                        "'{item}' is bound twice at the top level: by the import \
                                         and by the import here"
                                    ),
                                )
                                .with_label(*first_span, "first bound here, by the import")
                                .with_note("a top-level name can be bound only once"),
                            );
                            continue;
                        }
                        None => {
                            if let Some(binding) = &named {
                                item_bindings.insert(*item, (binding.clone(), *item_span));
                            }
                        }
                    }
                    let Some(exports) = exports else {
                        scope.values.entry(*item).or_insert(Binding::Poisoned);
                        scope.types.entry(*item).or_insert(Binding::Poisoned);
                        continue;
                    };
                    let value = exports.values.get(item).cloned();
                    let ty = exports.types.get(item).cloned();
                    if value.is_none() && ty.is_none() {
                        let builtin = match imported {
                            Imported::Builtin(id) => id.builtin_name(),
                            _ => None,
                        };
                        diagnostics.push(missing_item(*m, builtin, *item, *item_span, exports));
                        scope.values.entry(*item).or_insert(Binding::Poisoned);
                        continue;
                    }
                    if let Some(Binding::Ambiguous(ids)) = &value {
                        diagnostics.push(ambiguous_import(*m, *item, *item_span, ids, defs));
                        scope.values.entry(*item).or_insert(Binding::Poisoned);
                        continue;
                    }
                    if let Some(binding) = value {
                        bind_imported(&mut scope.values, *item, binding.clone(), defs);
                        if kind == ModuleKind::Cell && is_cell {
                            bind_imported(&mut scope.exports.values, *item, binding, defs);
                        }
                    }
                    if let Some(binding) = ty {
                        // An enum imported from an earlier REPL cell
                        // brings its variants: the cell saw them.
                        if is_cell && let Binding::Def(id) = &binding {
                            for v in defs.variants(*id).to_vec() {
                                let name = defs.get(v).name;
                                bind_variant_aware(&mut scope.implied, name, v, defs);
                                if kind == ModuleKind::Cell {
                                    bind_variant_aware(&mut scope.exports.values, name, v, defs);
                                }
                            }
                        }
                        bind_imported(&mut scope.types, *item, binding.clone(), defs);
                        if kind == ModuleKind::Cell && is_cell {
                            bind_imported(&mut scope.exports.types, *item, binding, defs);
                        }
                    }
                }
            }
        }
    }
}

/// Bind an imported item: a variant joins the ambiguity set of a
/// same-named variant in scope; anything else keeps the binding in scope
/// (the parser reports a name bound twice).
fn bind_imported(
    namespace: &mut HashMap<Symbol, Binding>,
    name: Symbol,
    binding: Binding,
    defs: &DefTable,
) {
    match binding {
        Binding::Def(id) => bind_variant_aware(namespace, name, id, defs),
        other => {
            namespace.entry(name).or_insert(other);
        }
    }
}

/// Report each private record, enum or trait of `module` that a public
/// declaration names: a `pub fn`'s parameter and return types and its
/// where-bounds, a `pub type`'s fields, variants and alias target. An
/// importer could use the public declaration but never name the private
/// item it leaks.
fn report_private_in_public(
    program: &Program,
    module: ModuleId,
    defs: &DefTable,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // A type alias is transparent: a private one stands for a type the
    // importer can name.
    let private = |res: Option<Res>| match res {
        Some(Res::Def(id)) => {
            let def = defs.get(id);
            (def.module == module
                && def.vis == Vis::Private
                && !matches!(def.kind, DefKind::TypeAlias))
            .then_some(def)
        }
        _ => None,
    };
    let mut report = |def: &Def, span: Span, (owner, owner_name): (String, Symbol)| {
        let what = if matches!(def.kind, DefKind::Trait(_)) {
            "trait"
        } else {
            "type"
        };
        diagnostics.push(
            Diagnostic::error(
                Code::PrivateItem,
                span,
                format!("private {what} '{}' in the signature of {owner}", def.name),
            )
            .with_label(
                def.span,
                format!("'{}' is declared without `pub`", def.name),
            )
            .with_help(format!(
                "mark `{}` `pub`, or drop the `pub` of '{owner_name}'",
                def.name
            )),
        );
    };
    fn type_names(te: &TypeExpr, out: &mut Vec<(Option<Res>, Span)>) {
        match &te.kind {
            TypeExprKind::Named { name_span, .. } => out.push((te.res, *name_span)),
            TypeExprKind::Generic {
                name_span, args, ..
            } => {
                out.push((te.res, *name_span));
                for a in args {
                    type_names(a, out);
                }
            }
            TypeExprKind::Tuple(elems) => elems.iter().for_each(|e| type_names(e, out)),
            TypeExprKind::Function(params, ret) => {
                params.iter().for_each(|p| type_names(p, out));
                type_names(ret, out);
            }
            TypeExprKind::AssocProj { receiver, .. } => type_names(receiver, out),
            TypeExprKind::AnonRecord { fields, .. } => {
                fields.iter().for_each(|(_, t)| type_names(t, out))
            }
            TypeExprKind::SelfType => {}
        }
    }
    for decl in &program.decls {
        let mut names: Vec<(Option<Res>, Span)> = Vec::new();
        let owner = match decl {
            Decl::Fn(f) if f.is_pub => {
                for p in &f.params {
                    if let Some(ty) = &p.ty {
                        type_names(ty, &mut names);
                    }
                }
                if let Some(ret) = &f.return_type {
                    type_names(ret, &mut names);
                }
                for wc in &f.where_clauses {
                    names.push((wc.trait_res, wc.trait_name_span));
                    wc.trait_args.iter().for_each(|a| type_names(a, &mut names));
                }
                (format!("public fn '{}'", f.name), f.name)
            }
            Decl::Type(td) if td.is_pub => {
                match &td.body {
                    TypeBody::Enum(variants) => variants
                        .iter()
                        .flat_map(|v| &v.fields)
                        .for_each(|t| type_names(t, &mut names)),
                    TypeBody::Record(fields) => {
                        fields.iter().for_each(|f| type_names(&f.ty, &mut names))
                    }
                    TypeBody::Alias(target) => type_names(target, &mut names),
                }
                (format!("public type '{}'", td.name), td.name)
            }
            _ => continue,
        };
        let mut seen: HashSet<DefId> = HashSet::new();
        for (res, span) in names {
            if let Some(def) = private(res)
                && let Some(Res::Def(id)) = res
                && seen.insert(id)
            {
                report(def, span, owner.clone());
            }
        }
    }
}

/// `import m.{ item }` where `m` does not offer `item`.
fn missing_item(
    module: Symbol,
    builtin: Option<&str>,
    item: Symbol,
    span: Span,
    exports: &Exports,
) -> Diagnostic {
    // A member of a builtin module that this build lacks.
    if let Some(builtin) = builtin
        && let Some(feature) = crate::module::member_missing_feature(builtin, &resolve(item))
    {
        return Diagnostic::error(
            Code::NotExported,
            span,
            crate::module::needs_feature(&format!("`{builtin}.{item}`"), feature),
        )
        .with_help(format!("rebuild silt with `--features {feature}`"));
    }
    if exports.private.contains_key(&item) {
        return Diagnostic::error(
            Code::PrivateItem,
            span,
            format!("'{item}' is private to module '{module}'"),
        )
        .with_help(format!(
            "mark it `pub` in module '{module}' to import it from another module"
        ));
    }
    let candidates: Vec<String> = exports
        .values
        .keys()
        .chain(exports.types.keys())
        .map(|name| resolve(*name))
        .collect();
    let mut d = Diagnostic::error(
        Code::NotExported,
        span,
        format!("module '{module}' has no member '{item}'"),
    );
    if let Some(similar) = suggest_similar(&resolve(item), candidates.iter()) {
        d = d.with_help(format!("did you mean `{similar}`?"));
    }
    d
}

/// `import m.{ Red }` where two enums of `m` declare `Red`.
fn ambiguous_import(
    module: Symbol,
    item: Symbol,
    span: Span,
    ids: &[DefId],
    defs: &DefTable,
) -> Diagnostic {
    let enums: Vec<String> = ids
        .iter()
        .filter_map(|id| defs.variant_type(*id))
        .map(|ty| resolve(ty.name))
        .collect();
    let mut d = Diagnostic::error(
        Code::AmbiguousVariant,
        span,
        format!(
            "'{item}' is a variant of more than one enum of module '{module}' ({})",
            enums.join(", ")
        ),
    );
    if let Some(first) = enums.first() {
        d = d.with_help(format!(
            "import the enum instead and write `{first}.{item}`: `import {module}.{{ {first} }}`"
        ));
    }
    d
}

/// The names `pattern` binds, with their spans.
fn pattern_binders(pattern: &Pattern, out: &mut Vec<(Symbol, Span)>) {
    match &pattern.kind {
        PatternKind::Ident(name) => out.push((*name, pattern.span)),
        PatternKind::Tuple(parts) | PatternKind::Constructor { args: parts, .. } => {
            for part in parts {
                pattern_binders(part, out);
            }
        }
        PatternKind::Record { fields, .. } | PatternKind::AnonRecord { fields, .. } => {
            for (field, field_span, sub) in fields {
                match sub {
                    Some(sub) => pattern_binders(sub, out),
                    None => out.push((*field, *field_span)),
                }
            }
            if let PatternKind::AnonRecord {
                rest: Some((rest, rest_span)),
                ..
            } = &pattern.kind
            {
                out.push((*rest, *rest_span));
            }
        }
        PatternKind::List(elems, rest) => {
            for elem in elems {
                pattern_binders(elem, out);
            }
            if let Some(rest) = rest {
                pattern_binders(rest, out);
            }
        }
        // Every alternative binds the same names: they are the names of
        // the first.
        PatternKind::Or(alts) => {
            if let Some(first) = alts.first() {
                pattern_binders(first, out);
            }
        }
        PatternKind::Map(entries) => {
            for (_, sub) in entries {
                pattern_binders(sub, out);
            }
        }
        PatternKind::Wildcard
        | PatternKind::Int(_)
        | PatternKind::Float(_)
        | PatternKind::Bool(_)
        | PatternKind::StringLit(..)
        | PatternKind::Range(..)
        | PatternKind::FloatRange(..)
        | PatternKind::Pin(_) => {}
    }
}

// ── The walk ─────────────────────────────────────────────────────────

/// What a name in a value position is.
enum Found {
    Local,
    Binding(Binding),
}

/// How a name is written, for messages: `Red`, `m.Red`, `m.C.Red`.
fn written(segments: &[Symbol]) -> String {
    segments
        .iter()
        .map(|s| resolve(*s))
        .collect::<Vec<_>>()
        .join(".")
}

struct Resolver<'a> {
    defs: &'a DefTable,
    kind: ModuleKind,
    scope: &'a ModuleScope,
    imports: &'a HashMap<Symbol, Imported<'a>>,
    builtins: &'a BuiltinScopes,
    /// The local bindings in scope, innermost last.
    locals: Vec<Vec<Symbol>>,
    /// Top-level names that mean an earlier REPL cell's definition while
    /// the initializer of a cell's `let` that binds them again is
    /// resolved (`let x = x + 1`).
    previous: HashMap<Symbol, Binding>,
    diagnostics: Vec<Diagnostic>,
}

impl Resolver<'_> {
    fn error(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }

    /// The help for a builtin module used without its import.
    fn import_help(&self, module: &str) -> String {
        if let Some(feature) = crate::module::missing_feature(module) {
            return crate::module::needs_feature(&format!("module `{module}`"), feature);
        }
        match self.kind {
            ModuleKind::Cell => format!("enter `import {module}` first"),
            _ => format!("add `import {module}` at the top of the file"),
        }
    }

    // ── Scopes ──

    fn push(&mut self) {
        self.locals.push(Vec::new());
    }

    fn pop(&mut self) {
        self.locals.pop();
    }

    fn bind_local(&mut self, name: Symbol) {
        if self.locals.is_empty() {
            self.push();
        }
        self.locals.last_mut().expect("a scope").push(name);
    }

    fn bind_pattern_locals(&mut self, pattern: &Pattern) {
        let mut binders = Vec::new();
        pattern_binders(pattern, &mut binders);
        for (name, _) in binders {
            self.bind_local(name);
        }
    }

    fn is_local(&self, name: Symbol) -> bool {
        self.locals.iter().any(|scope| scope.contains(&name))
    }

    /// The exports of the module a binding names.
    fn module_exports(&self, id: ModuleId) -> Option<&Exports> {
        if id.is_builtin() {
            return self.builtins.modules.get(&id);
        }
        self.imports.values().find_map(|imported| match imported {
            Imported::Module(m, scope) | Imported::Cell(m, scope) if *m == id => {
                Some(&scope.exports)
            }
            _ => None,
        })
    }

    /// The name of the module `id` as its import wrote it.
    fn module_name(&self, id: ModuleId) -> Option<Symbol> {
        if let Some(name) = id.builtin_name() {
            return Some(intern(name));
        }
        self.imports
            .iter()
            .find_map(|(name, imported)| match imported {
                Imported::Module(m, _) | Imported::Cell(m, _) if *m == id => Some(*name),
                _ => None,
            })
    }

    /// A top-level value name: the module's own and imported names, the
    /// types used as values, then the prelude.
    fn lookup_global_value(&self, name: Symbol) -> Option<Binding> {
        self.previous
            .get(&name)
            .or_else(|| self.scope.values.get(&name))
            .or_else(|| self.scope.types.get(&name))
            .or_else(|| self.scope.implied.get(&name))
            .or_else(|| self.builtins.prelude.values.get(&name))
            .or_else(|| self.builtins.prelude.types.get(&name))
            .cloned()
    }

    /// What an earlier REPL cell, imported into this one, binds `name`
    /// to.
    fn earlier_cell_value(&self, name: Symbol) -> Option<Binding> {
        self.imports.values().find_map(|imported| match imported {
            Imported::Cell(_, scope) => scope.exports.values.get(&name).cloned(),
            _ => None,
        })
    }

    fn lookup_value(&self, name: Symbol) -> Option<Found> {
        if self.is_local(name) {
            return Some(Found::Local);
        }
        self.lookup_global_value(name).map(Found::Binding)
    }

    /// A type or trait name.
    fn lookup_type(&self, name: Symbol) -> Option<Binding> {
        self.scope
            .types
            .get(&name)
            .or_else(|| self.builtins.prelude.types.get(&name))
            .cloned()
    }

    fn binding_res(&self, binding: &Binding) -> Res {
        match binding {
            Binding::Def(id) => Res::Def(*id),
            Binding::Module(id) => Res::Module(*id),
            Binding::Ambiguous(_) | Binding::Poisoned => Res::Error,
        }
    }

    /// Where `name`, which this module does not bind, can be found: each
    /// imported module that offers it (by the name the module is bound
    /// as, if it is bound), and each builtin module that offers it.
    fn elsewhere(&self, name: Symbol) -> Vec<(Symbol, Option<Symbol>)> {
        let mut found: Vec<(Symbol, Option<Symbol>)> = Vec::new();
        let mut seen: HashSet<Symbol> = HashSet::new();
        // Modules bound by a name in this module.
        let mut bound: Vec<(Symbol, ModuleId)> = self
            .scope
            .values
            .iter()
            .filter_map(|(n, b)| match b {
                Binding::Module(id) => Some((*n, *id)),
                _ => None,
            })
            .collect();
        bound.sort_by_key(|(n, _)| resolve(*n));
        for (bound_as, id) in bound {
            if let (Some(exports), Some(module)) = (self.module_exports(id), self.module_name(id))
                && exports.member(name).is_some()
                && seen.insert(module)
            {
                found.push((module, Some(bound_as)));
            }
        }
        let mut imported: Vec<(Symbol, ModuleId)> = self
            .imports
            .iter()
            .filter_map(|(n, i)| match i {
                Imported::Module(id, _) | Imported::Builtin(id) => Some((*n, *id)),
                _ => None,
            })
            .collect();
        imported.sort_by_key(|(n, _)| resolve(*n));
        for (module, id) in imported {
            if let Some(exports) = self.module_exports(id)
                && exports.member(name).is_some()
                && seen.insert(module)
            {
                found.push((module, None));
            }
        }
        // A builtin module that is not imported is named for a type or a
        // variant only: a lowercase name is too common a word.
        if !resolve(name).starts_with(char::is_uppercase) {
            return found;
        }
        for module in crate::module::builtin_modules() {
            let module_sym = intern(module);
            if let Some(id) = ModuleId::builtin(module)
                && let Some(exports) = self.builtins.modules.get(&id)
                && exports.member(name).is_some()
                && seen.insert(module_sym)
            {
                found.push((module_sym, None));
            }
        }
        found
    }

    /// An imported module (by its import name) that declares `name`
    /// without `pub`.
    fn private_in_import(&self, name: Symbol) -> Option<Symbol> {
        let mut found: Vec<Symbol> = self
            .imports
            .iter()
            .filter_map(|(module, imported)| match imported {
                Imported::Module(_, scope) if scope.exports.private.contains_key(&name) => {
                    Some(*module)
                }
                _ => None,
            })
            .collect();
        found.sort_by_key(|m| resolve(*m));
        found.first().copied()
    }

    /// The help for `name`, which resolves to nothing here but which the
    /// modules `elsewhere` offer.
    fn elsewhere_help(&self, name: Symbol, elsewhere: &[(Symbol, Option<Symbol>)]) -> String {
        let (module, bound_as) = elsewhere[0];
        if let Some(feature) = crate::module::missing_feature(&resolve(module)) {
            return format!(
                "`{name}` is in module `{module}`, which is not part of this build of silt: it \
                 needs the cargo feature `{feature}`"
            );
        }
        match bound_as {
            Some(bound_as) => format!(
                "did you mean `{bound_as}.{name}`? or import the name: `import {module}.{{ {name} }}`"
            ),
            None => format!(
                "`{name}` is in module `{module}`: write `{module}.{name}` after `import {module}`, \
                 or add `import {module}.{{ {name} }}`"
            ),
        }
    }

    /// Report `AmbiguousVariant` for the bare use of `name` at `span`.
    fn ambiguous(&mut self, name: Symbol, span: Span, ids: &[DefId], qualifier: Option<Symbol>) {
        let enums: Vec<(String, Span)> = ids
            .iter()
            .filter_map(|id| {
                let ty = self.defs.variant_type(*id)?;
                Some((resolve(ty.name), self.defs.get(*id).span))
            })
            .collect();
        let prefix = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
        let mut d = Diagnostic::error(
            Code::AmbiguousVariant,
            span,
            format!(
                "'{prefix}{name}' is ambiguous: it is a variant of {}",
                enums
                    .iter()
                    .map(|(e, _)| format!("'{prefix}{e}'"))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
        );
        for (e, decl) in &enums {
            if *decl != Span::BUILTIN {
                d = d.with_label(*decl, format!("variant of '{e}'"));
            }
        }
        let options: Vec<String> = enums
            .iter()
            .map(|(e, _)| format!("`{prefix}{e}.{name}`"))
            .collect();
        d = d.with_help(format!("write {}", options.join(" or ")));
        self.error(d);
    }

    // ── Declarations ──

    fn decl(&mut self, decl: &mut Decl) {
        match decl {
            Decl::Fn(f) => self.fn_decl(f),
            Decl::Type(td) => match &mut td.body {
                TypeBody::Enum(variants) => {
                    for variant in variants {
                        for field in &mut variant.fields {
                            self.type_expr(field);
                        }
                    }
                }
                TypeBody::Record(fields) => {
                    for field in fields {
                        self.type_expr(&mut field.ty);
                    }
                }
                TypeBody::Alias(target) => self.type_expr(target),
            },
            Decl::Trait(t) => {
                for sup in &mut t.supertraits {
                    self.trait_ref(sup);
                }
                for assoc in &mut t.assoc_types {
                    for bound in &mut assoc.bounds {
                        self.trait_ref(bound);
                    }
                }
                for wc in &mut t.param_where_clauses {
                    self.where_clause(wc);
                }
                for m in &mut t.methods {
                    self.fn_decl(m);
                }
            }
            Decl::TraitImpl(ti) => {
                ti.trait_res = self.trait_name(ti.trait_module, ti.trait_name, ti.trait_name_span);
                ti.target_res =
                    self.type_name(ti.target_module, ti.target_type, ti.target_type_span);
                for arg in &mut ti.trait_args {
                    self.type_expr(arg);
                }
                for arg in &mut ti.target_type_args {
                    self.type_expr(arg);
                }
                for wc in &mut ti.where_clauses {
                    self.where_clause(wc);
                }
                for binding in &mut ti.assoc_type_bindings {
                    self.type_expr(&mut binding.ty);
                }
                for m in &mut ti.methods {
                    self.fn_decl(m);
                }
            }
            Decl::Let {
                pattern, ty, value, ..
            } => {
                if let Some(ty) = ty {
                    self.type_expr(ty);
                }
                // A REPL cell's `let` that binds a name an earlier cell
                // bound reads the earlier value in its initializer.
                if self.kind == ModuleKind::Cell {
                    let mut binders = Vec::new();
                    crate::parser::pattern_binders(pattern, &mut binders);
                    for (name, _) in binders {
                        if let Some(binding) = self.earlier_cell_value(name) {
                            self.previous.insert(name, binding);
                        }
                    }
                }
                self.expr(value);
                self.previous.clear();
                // The binders are the module's definitions; the patterns'
                // constructors and records still name something.
                self.pattern(pattern);
            }
            Decl::Import(..) => {}
        }
    }

    fn where_clause(&mut self, wc: &mut WhereClause) {
        wc.trait_res = self.trait_name(wc.trait_module, wc.trait_name, wc.trait_name_span);
        for arg in &mut wc.trait_args {
            self.type_expr(arg);
        }
    }

    fn trait_ref(&mut self, r: &mut TraitRef) {
        r.res = self.trait_name(r.module, r.name, r.span);
        for arg in &mut r.args {
            self.type_expr(arg);
        }
    }

    fn fn_decl(&mut self, f: &mut FnDecl) {
        for wc in &mut f.where_clauses {
            self.where_clause(wc);
        }
        if let Some(ret) = &mut f.return_type {
            self.type_expr(ret);
        }
        self.push();
        for param in &mut f.params {
            if let Some(ty) = &mut param.ty {
                self.type_expr(ty);
            }
            self.pattern(&mut param.pattern);
            self.bind_pattern_locals(&param.pattern);
        }
        self.expr(&mut f.body);
        self.pop();
    }

    // ── Types and traits ──

    fn type_expr(&mut self, te: &mut TypeExpr) {
        match &mut te.kind {
            TypeExprKind::Named {
                module,
                name,
                name_span,
            } => {
                te.res = self.type_name(*module, *name, *name_span);
            }
            TypeExprKind::Generic {
                module,
                name,
                name_span,
                args,
            } => {
                te.res = self.type_name(*module, *name, *name_span);
                for arg in args {
                    self.type_expr(arg);
                }
            }
            TypeExprKind::Tuple(elems) => {
                for e in elems {
                    self.type_expr(e);
                }
            }
            TypeExprKind::Function(params, ret) => {
                for p in params {
                    self.type_expr(p);
                }
                self.type_expr(ret);
            }
            TypeExprKind::SelfType => {}
            TypeExprKind::AssocProj {
                receiver,
                trait_module,
                trait_name,
                ..
            } => {
                let (module, name) = (*trait_module, *trait_name);
                self.type_expr(receiver);
                if resolve(name) != "__no_enclosing_trait__" {
                    te.res = self.trait_name(module, name, te.span);
                }
            }
            TypeExprKind::AnonRecord { fields, .. } => {
                for (_, t) in fields {
                    self.type_expr(t);
                }
            }
        }
    }

    /// A type name in a type position, `Shape` or `m.Shape`. A lowercase
    /// bare name is a type variable.
    fn type_name(&mut self, module: Option<Qualifier>, name: Symbol, span: Span) -> Option<Res> {
        self.type_or_trait(module, name, span, false)
    }

    fn trait_name(&mut self, module: Option<Qualifier>, name: Symbol, span: Span) -> Option<Res> {
        self.type_or_trait(module, name, span, true)
    }

    /// Resolve a type or trait name. `None` leaves a bare name no module
    /// offers to the checker, which reports it where it knows what the
    /// name was for.
    fn type_or_trait(
        &mut self,
        module: Option<Qualifier>,
        name: Symbol,
        span: Span,
        is_trait: bool,
    ) -> Option<Res> {
        let what = if is_trait { "trait" } else { "type" };
        let Some(module) = module else {
            let name_str = resolve(name);
            if name_str.starts_with(char::is_lowercase) {
                return Some(Res::Local);
            }
            if let Some(binding) = self.lookup_type(name) {
                return Some(self.binding_res(&binding));
            }
            // `_a` is a type variable, unless a type is named so.
            if name_str.starts_with('_') {
                return Some(Res::Local);
            }
            let elsewhere = self.elsewhere(name);
            if elsewhere.is_empty() {
                // A private type or trait of an imported module: the
                // checker would find it by its name.
                if let Some(module) = self.private_in_import(name) {
                    self.error(
                        Diagnostic::error(
                            Code::UnresolvedName,
                            span,
                            format!("unknown {what} '{name}'"),
                        )
                        .with_help(format!(
                            "module '{module}' has a {what} '{name}', but it is private there"
                        )),
                    );
                    return Some(Res::Error);
                }
                return None;
            }
            let help = self.elsewhere_help(name, &elsewhere);
            self.error(
                Diagnostic::error(
                    Code::UnresolvedName,
                    span,
                    format!("unknown {what} '{name}'"),
                )
                .with_help(help),
            );
            return Some(Res::Error);
        };
        let target = match self.scope.values.get(&module.name) {
            Some(Binding::Module(id)) => *id,
            Some(Binding::Poisoned) => return Some(Res::Error),
            _ => {
                self.not_a_module(module, &[name], what);
                return Some(Res::Error);
            }
        };
        let Some(exports) = self.module_exports(target) else {
            return Some(Res::Error);
        };
        match exports.types.get(&name).cloned() {
            Some(binding) => Some(self.binding_res(&binding)),
            None => {
                let d = if exports.private.contains_key(&name) {
                    Diagnostic::error(
                        Code::PrivateItem,
                        span,
                        format!("{what} '{name}' is private to module '{}'", module.name),
                    )
                } else {
                    let candidates: Vec<String> =
                        exports.types.keys().map(|t| resolve(*t)).collect();
                    let mut d = Diagnostic::error(
                        Code::NotExported,
                        span,
                        format!("module '{}' has no {what} '{name}'", module.name),
                    );
                    if let Some(c) = suggest_similar(&resolve(name), candidates.iter()) {
                        d = d.with_help(format!("did you mean `{c}`?"));
                    }
                    d
                };
                self.error(d);
                Some(Res::Error)
            }
        }
    }

    /// `module.x...` where `module` is not an imported module.
    fn not_a_module(&mut self, module: Qualifier, rest: &[Symbol], what: &str) {
        let module_str = resolve(module.name);
        let mut segments = vec![module.name];
        segments.extend_from_slice(rest);
        if let Some(help) = self.alias_help(module.name) {
            let code = if what == "trait" {
                Code::UnknownTrait
            } else {
                Code::UndefinedType
            };
            self.error(
                Diagnostic::error(
                    code,
                    module.span,
                    format!("undefined {what} '{}'", written(&segments)),
                )
                .with_help(help),
            );
        } else if crate::module::is_builtin_module(&module_str) {
            self.error(
                Diagnostic::error(
                    Code::ModuleNotImported,
                    module.span,
                    format!("module '{module_str}' is not imported"),
                )
                .with_help(self.import_help(&module_str)),
            );
        } else {
            let mut d = Diagnostic::error(
                Code::UndefinedType,
                module.span,
                format!(
                    "undefined {what} '{}' — no module '{module_str}' in scope; import it with \
                     `import {module_str}`",
                    written(&segments)
                ),
            );
            if what == "trait" {
                d.code = Code::UnknownTrait;
            }
            self.error(d);
        }
    }

    // ── Patterns ──

    fn pattern(&mut self, pattern: &mut Pattern) {
        let span = pattern.span;
        match &mut pattern.kind {
            PatternKind::Constructor {
                qualifier,
                name,
                name_span,
                args,
            } => {
                let res = self.constructor_pattern(qualifier, *name, *name_span, span);
                for arg in args {
                    self.pattern(arg);
                }
                pattern.res = res;
            }
            PatternKind::Record {
                module,
                name,
                name_span,
                fields,
                ..
            } => {
                let (module, name, name_span) = (*module, *name, *name_span);
                for (_, _, sub) in fields.iter_mut() {
                    if let Some(sub) = sub {
                        self.pattern(sub);
                    }
                }
                if let Some(name) = name {
                    pattern.res = self.record_name(module, name, name_span, span, true);
                }
            }
            PatternKind::Tuple(parts) | PatternKind::Or(parts) => {
                for p in parts {
                    self.pattern(p);
                }
            }
            PatternKind::AnonRecord { fields, .. } => {
                for (_, _, sub) in fields {
                    if let Some(sub) = sub {
                        self.pattern(sub);
                    }
                }
            }
            PatternKind::List(elems, rest) => {
                for e in elems {
                    self.pattern(e);
                }
                if let Some(rest) = rest {
                    self.pattern(rest);
                }
            }
            PatternKind::Map(entries) => {
                for (_, sub) in entries {
                    self.pattern(sub);
                }
            }
            PatternKind::Wildcard
            | PatternKind::Ident(_)
            | PatternKind::Int(_)
            | PatternKind::Float(_)
            | PatternKind::Bool(_)
            | PatternKind::StringLit(..)
            | PatternKind::Range(..)
            | PatternKind::FloatRange(..) => {}
            // `^x`: the value the name has, a local or a top-level one.
            // A name that resolves to nothing is reported by the checker.
            PatternKind::Pin(name) => {
                pattern.res = self.lookup_value(*name).map(|found| match found {
                    Found::Local => Res::Local,
                    Found::Binding(binding) => self.binding_res(&binding),
                });
            }
        }
    }

    /// The variant a constructor pattern names: `Red`, `C.Red`, `m.Red`,
    /// `m.C.Red`.
    fn constructor_pattern(
        &mut self,
        qualifier: &[Qualifier],
        name: Symbol,
        name_span: Span,
        span: Span,
    ) -> Option<Res> {
        match qualifier {
            [] => match self.lookup_global_value(name) {
                Some(Binding::Def(id)) => match self.defs.get(id).kind {
                    DefKind::Variant { .. } => Some(Res::Def(id)),
                    // A record or another name: the checker says what
                    // pattern it wants instead.
                    _ => None,
                },
                Some(Binding::Ambiguous(ids)) => {
                    self.ambiguous(name, name_span, &ids, None);
                    Some(Res::Error)
                }
                Some(Binding::Poisoned) => Some(Res::Error),
                Some(Binding::Module(_)) => None,
                None => {
                    let elsewhere = self.elsewhere(name);
                    let mut d = Diagnostic::error(
                        Code::UnresolvedName,
                        span,
                        format!("undefined constructor '{name}' in pattern"),
                    );
                    if !elsewhere.is_empty() {
                        d = d.with_help(self.elsewhere_help(name, &elsewhere));
                    }
                    self.error(d);
                    Some(Res::Error)
                }
            },
            [q] => {
                // A module, or the owning enum.
                if self.is_local(q.name) {
                    return self.shadowed_qualifier(*q, name, span, true);
                }
                match self.lookup_global_value(q.name) {
                    Some(Binding::Module(id)) => self.module_variant(id, *q, &[], name, span),
                    Some(Binding::Poisoned) => Some(Res::Error),
                    Some(Binding::Def(ty)) if self.defs.get(ty).is_type() => {
                        self.enum_variant(ty, &resolve(q.name), name, span)
                    }
                    None if crate::module::is_builtin_module(&resolve(q.name)) => {
                        self.not_a_module(*q, &[name], "type");
                        Some(Res::Error)
                    }
                    _ => {
                        let mut d = Diagnostic::error(
                            Code::UndefinedConstructor,
                            span,
                            format!(
                                "undefined constructor '{}' in pattern — '{}' is neither an \
                                 imported module nor an enum type",
                                written(&[q.name, name]),
                                q.name
                            ),
                        );
                        let elsewhere = self.elsewhere(q.name);
                        if !elsewhere.is_empty() {
                            d = d.with_help(self.elsewhere_help(q.name, &elsewhere));
                        }
                        self.error(d);
                        Some(Res::Error)
                    }
                }
            }
            [m, e, ..] => match self.scope.values.get(&m.name).cloned() {
                Some(Binding::Module(id)) => self.module_variant(id, *m, &[*e], name, span),
                Some(Binding::Poisoned) => Some(Res::Error),
                _ => {
                    let why = match self.lookup_global_value(m.name) {
                        Some(Binding::Def(d)) if self.defs.get(d).is_type() => "an enum type",
                        _ => "not an imported module",
                    };
                    let why = if why == "an enum type" {
                        format!("'{}' is an enum type, not a module", m.name)
                    } else {
                        format!("'{}' is not an imported module", m.name)
                    };
                    self.error(Diagnostic::error(
                        Code::UndefinedConstructor,
                        span,
                        format!(
                            "undefined constructor '{}' in pattern — {why}",
                            written(&[m.name, e.name, name])
                        ),
                    ));
                    Some(Res::Error)
                }
            },
        }
    }

    /// A pattern or literal qualified by a name a local binding shadows.
    fn shadowed_qualifier(
        &mut self,
        q: Qualifier,
        name: Symbol,
        span: Span,
        in_pattern: bool,
    ) -> Option<Res> {
        let module = matches!(self.scope.values.get(&q.name), Some(Binding::Module(_)));
        let where_ = if in_pattern { " in pattern" } else { "" };
        let d = if module {
            Diagnostic::error(
                Code::Shadowing,
                span,
                format!(
                    "cannot use '{q}' as a module qualifier for '{q}.{name}'{where_}: a local \
                     binding named '{q}' shadows module '{q}' here; rename the binding or the \
                     import",
                    q = q.name
                ),
            )
        } else if in_pattern {
            Diagnostic::error(
                Code::UndefinedConstructor,
                span,
                format!(
                    "undefined constructor '{q}.{name}' in pattern — '{q}' is neither an \
                     imported module nor an enum type",
                    q = q.name
                ),
            )
        } else {
            Diagnostic::error(
                Code::UndefinedType,
                span,
                format!(
                    "undefined type '{q}.{name}' — no module '{q}' in scope; import it with \
                     `import {q}`",
                    q = q.name
                ),
            )
        };
        self.error(d);
        Some(Res::Error)
    }

    /// The variant `name` of the module `id` (written `q`), through the
    /// enum `via` when it is written (`m.C.Red`).
    fn module_variant(
        &mut self,
        id: ModuleId,
        q: Qualifier,
        via: &[Qualifier],
        name: Symbol,
        span: Span,
    ) -> Option<Res> {
        let Some(exports) = self.module_exports(id) else {
            return Some(Res::Error);
        };
        if let [e] = via {
            return match exports.types.get(&e.name).cloned() {
                Some(Binding::Def(ty)) => {
                    let written = format!("{}.{}", q.name, e.name);
                    self.enum_variant(ty, &written, name, span)
                }
                Some(_) => Some(Res::Error),
                None => {
                    let variant_of = match exports.values.get(&e.name) {
                        Some(Binding::Def(v)) => self.defs.variant_type(*v).map(|ty| ty.name),
                        _ => None,
                    };
                    let d = if let Some(ty) = variant_of {
                        Diagnostic::error(
                            Code::NoSuchVariant,
                            e.span,
                            format!(
                                "'{q}.{e}' is a variant of enum '{q}.{ty}', not a type: write \
                                 `{q}.{e}` or `{q}.{ty}.{e}`",
                                q = q.name,
                                e = e.name
                            ),
                        )
                    } else if exports.private.contains_key(&e.name) {
                        Diagnostic::error(
                            Code::PrivateItem,
                            e.span,
                            format!("type '{}' is private to module '{}'", e.name, q.name),
                        )
                    } else {
                        Diagnostic::error(
                            Code::NotExported,
                            e.span,
                            format!("module '{}' has no type '{}'", q.name, e.name),
                        )
                    };
                    self.error(d);
                    Some(Res::Error)
                }
            };
        }
        match exports.values.get(&name).cloned() {
            Some(Binding::Def(v)) if matches!(self.defs.get(v).kind, DefKind::Variant { .. }) => {
                Some(Res::Def(v))
            }
            Some(Binding::Ambiguous(ids)) => {
                self.ambiguous(name, span, &ids, Some(q.name));
                Some(Res::Error)
            }
            Some(Binding::Poisoned) => Some(Res::Error),
            _ => {
                let d = if matches!(exports.types.get(&name), Some(Binding::Def(_))) {
                    Diagnostic::error(
                        Code::InvalidPatternUse,
                        span,
                        format!(
                            "'{name}' is a record type; use record-pattern syntax `{q}.{name} \
                             {{ ... }}` instead of constructor-pattern syntax",
                            q = q.name
                        ),
                    )
                } else if exports.private.contains_key(&name) {
                    Diagnostic::error(
                        Code::PrivateItem,
                        span,
                        format!("variant '{name}' is private to module '{}'", q.name),
                    )
                } else {
                    let candidates: Vec<String> = exports
                        .values
                        .iter()
                        .filter(|(_, b)| match b {
                            Binding::Def(d) => {
                                matches!(self.defs.get(*d).kind, DefKind::Variant { .. })
                            }
                            Binding::Ambiguous(_) => true,
                            _ => false,
                        })
                        .map(|(n, _)| resolve(*n))
                        .collect();
                    let mut d = Diagnostic::error(
                        Code::NoSuchVariant,
                        span,
                        format!("module '{}' has no variant '{name}'", q.name),
                    );
                    if let Some(c) = suggest_similar(&resolve(name), candidates.iter()) {
                        d = d.with_help(format!("did you mean `{c}`?"));
                    }
                    d
                };
                self.error(d);
                Some(Res::Error)
            }
        }
    }

    /// The variant `name` of the type `ty`, written `written`.
    fn enum_variant(&mut self, ty: DefId, written: &str, name: Symbol, span: Span) -> Option<Res> {
        if let Some(v) = self
            .defs
            .variants(ty)
            .iter()
            .copied()
            .find(|v| self.defs.get(*v).name == name)
        {
            return Some(Res::Def(v));
        }
        // The variant of another enum, if the name is one here.
        let owner = match self.lookup_global_value(name) {
            Some(Binding::Def(v)) => self.defs.variant_type(v).map(|ty| ty.name),
            _ => None,
        };
        let message = match owner {
            Some(owner) => {
                format!("'{name}' is not a variant of enum '{written}' (it belongs to '{owner}')")
            }
            None => format!("enum '{written}' has no variant '{name}'"),
        };
        self.error(Diagnostic::error(Code::NoSuchVariant, span, message));
        Some(Res::Error)
    }

    /// The record type of a record literal or pattern.
    fn record_name(
        &mut self,
        module: Option<Qualifier>,
        name: Symbol,
        name_span: Span,
        span: Span,
        in_pattern: bool,
    ) -> Option<Res> {
        match module {
            None => match self.lookup_type(name) {
                Some(binding) => Some(self.binding_res(&binding)),
                None => {
                    let elsewhere = self.elsewhere(name);
                    if elsewhere.is_empty() {
                        return None;
                    }
                    let help = self.elsewhere_help(name, &elsewhere);
                    let where_ = if in_pattern { " in pattern" } else { "" };
                    self.error(
                        Diagnostic::error(
                            Code::UnresolvedName,
                            name_span,
                            format!("undefined record type '{name}'{where_}"),
                        )
                        .with_help(help),
                    );
                    Some(Res::Error)
                }
            },
            Some(q) => {
                if self.is_local(q.name) {
                    let module = matches!(self.scope.values.get(&q.name), Some(Binding::Module(_)));
                    if module {
                        return self.shadowed_qualifier(q, name, span, in_pattern);
                    }
                }
                let target = match self.scope.values.get(&q.name) {
                    Some(Binding::Module(id)) => *id,
                    Some(Binding::Poisoned) => return Some(Res::Error),
                    _ => {
                        let where_ = if in_pattern { " in pattern" } else { "" };
                        let d = match self.alias_help(q.name) {
                            Some(help) => Diagnostic::error(
                                Code::UndefinedType,
                                span,
                                format!("undefined type '{}.{name}'{where_}", q.name),
                            )
                            .with_help(help),
                            None => Diagnostic::error(
                                Code::UndefinedType,
                                span,
                                format!(
                                    "undefined type '{m}.{name}'{where_} — no module '{m}' in \
                                     scope; import it with `import {m}`",
                                    m = q.name
                                ),
                            ),
                        };
                        self.error(d);
                        return Some(Res::Error);
                    }
                };
                let Some(exports) = self.module_exports(target) else {
                    return Some(Res::Error);
                };
                match exports.types.get(&name).cloned() {
                    Some(binding) => Some(self.binding_res(&binding)),
                    None => {
                        let d = if exports.private.contains_key(&name) {
                            Diagnostic::error(
                                Code::PrivateItem,
                                span,
                                format!("type '{name}' is private to module '{}'", q.name),
                            )
                        } else {
                            let candidates: Vec<String> = exports
                                .types
                                .iter()
                                .filter(|(_, b)| match b {
                                    Binding::Def(d) => {
                                        matches!(self.defs.get(*d).kind, DefKind::Type(_))
                                    }
                                    _ => false,
                                })
                                .map(|(n, _)| resolve(*n))
                                .collect();
                            let mut d = Diagnostic::error(
                                Code::UndefinedType,
                                span,
                                format!("module '{}' has no record type '{name}'", q.name),
                            );
                            if let Some(c) = suggest_similar(&resolve(name), candidates.iter()) {
                                d = d.with_help(format!("did you mean `{c}`?"));
                            }
                            d
                        };
                        self.error(d);
                        Some(Res::Error)
                    }
                }
            }
        }
    }

    // ── Expressions ──

    fn block(&mut self, stmts: &mut [Stmt]) {
        self.push();
        for stmt in stmts {
            match stmt {
                Stmt::Let { pattern, ty, value } => {
                    if let Some(ty) = ty {
                        self.type_expr(ty);
                    }
                    self.expr(value);
                    self.pattern(pattern);
                    self.bind_pattern_locals(pattern);
                }
                Stmt::When {
                    pattern,
                    expr,
                    else_body,
                } => {
                    self.expr(expr);
                    self.expr(else_body);
                    self.pattern(pattern);
                    self.bind_pattern_locals(pattern);
                }
                Stmt::WhenBool {
                    condition,
                    else_body,
                } => {
                    self.expr(condition);
                    self.expr(else_body);
                }
                Stmt::Expr(e) => self.expr(e),
            }
        }
        self.pop();
    }

    fn expr(&mut self, expr: &mut Expr) {
        let span = expr.span;
        match &mut expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::StringLit(..)
            | ExprKind::Unit => {}
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.expr(e);
                    }
                }
            }
            ExprKind::List(elems) => {
                for elem in elems {
                    match elem {
                        ListElem::Single(e) | ListElem::Spread(e) => self.expr(e),
                    }
                }
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::SetLit(elems) | ExprKind::Tuple(elems) | ExprKind::Recur(elems) => {
                for e in elems {
                    self.expr(e);
                }
            }
            ExprKind::Ident(name) => {
                let name = *name;
                expr.res = Some(self.ident(name, span));
            }
            ExprKind::FieldAccess(..) => {
                expr.res = self.field_access(expr);
            }
            ExprKind::Binary(a, _, b) | ExprKind::Pipe(a, b) | ExprKind::Range(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Unary(_, e) | ExprKind::QuestionMark(e) => self.expr(e),
            ExprKind::Ascription(e, ty) => {
                self.expr(e);
                self.type_expr(ty);
            }
            ExprKind::Call(callee, args) => {
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Lambda { params, body } => {
                self.push();
                for param in params {
                    if let Some(ty) = &mut param.ty {
                        self.type_expr(ty);
                    }
                    self.pattern(&mut param.pattern);
                    self.bind_pattern_locals(&param.pattern);
                }
                self.expr(body);
                self.pop();
            }
            ExprKind::RecordCreate {
                module,
                name,
                name_span,
                fields,
            } => {
                let (module, name, name_span) = (*module, *name, *name_span);
                for (_, e) in fields.iter_mut() {
                    self.expr(e);
                }
                expr.res = self.record_name(module, name, name_span, span, false);
            }
            ExprKind::RecordUpdate { expr: base, fields } => {
                self.expr(base);
                for (_, e) in fields {
                    self.expr(e);
                }
            }
            ExprKind::AnonRecord { spread, fields } => {
                if let Some(s) = spread {
                    self.expr(s);
                }
                for (_, e) in fields {
                    self.expr(e);
                }
            }
            ExprKind::Match {
                expr: scrutinee,
                arms,
            } => {
                if let Some(s) = scrutinee {
                    self.expr(s);
                }
                for arm in arms {
                    self.push();
                    self.pattern(&mut arm.pattern);
                    self.bind_pattern_locals(&arm.pattern);
                    if let Some(g) = &mut arm.guard {
                        self.expr(g);
                    }
                    self.expr(&mut arm.body);
                    self.pop();
                }
            }
            ExprKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            ExprKind::Block(stmts) => self.block(stmts),
            ExprKind::Loop { bindings, body } => {
                for (_, _, init) in bindings.iter_mut() {
                    self.expr(init);
                }
                self.push();
                for (name, _, _) in bindings.iter() {
                    self.bind_local(*name);
                }
                self.expr(body);
                self.pop();
            }
        }
    }

    /// A name in a value position.
    fn ident(&mut self, name: Symbol, span: Span) -> Res {
        match self.lookup_value(name) {
            Some(Found::Local) => Res::Local,
            // Modules and values are different namespaces: a bare name
            // where a module of the name is imported is the prelude's
            // value of that name when there is one (`import println`, then
            // `println("x")`); `println.hello()` is the module's.
            Some(Found::Binding(Binding::Module(_)))
                if let Some(binding @ Binding::Def(_)) =
                    self.builtins.prelude.values.get(&name) =>
            {
                self.binding_res(binding)
            }
            Some(Found::Binding(Binding::Ambiguous(ids))) => {
                self.ambiguous(name, span, &ids, None);
                Res::Error
            }
            Some(Found::Binding(binding)) => self.binding_res(&binding),
            None => {
                self.unresolved_value(name, span);
                Res::Error
            }
        }
    }

    /// Report a value name that resolves to nothing.
    fn unresolved_value(&mut self, name: Symbol, span: Span) {
        let name_str = resolve(name);
        let message = format!("undefined variable '{name_str}'");
        let foreign = match name_str.as_str() {
            "break" | "continue" => {
                Some("silt has no 'break'/'continue' — return early or restructure the recursion")
            }
            "if" => Some(
                "silt has no 'if' keyword — use 'match cond { ... }' with a 'true -> ...' and a 'false -> ...' arm",
            ),
            "self" => Some(
                "`self` is bound only as the first parameter of a trait method; name the \
                 value as a parameter instead",
            ),
            "while" | "for" => Some(
                "silt has no 'while'/'for' keywords — use tail-recursive 'loop' or 'list.each' / \
                 'list.map'",
            ),
            _ => None,
        };
        let help = match foreign {
            Some(hint) => Some(hint.to_string()),
            None => {
                let elsewhere = self.elsewhere(name);
                if !elsewhere.is_empty() {
                    Some(self.elsewhere_help(name, &elsewhere))
                } else if let Some(help) = self.alias_help(name) {
                    Some(help)
                } else if let Some(module) = self.items_import_of(name) {
                    Some(format!(
                        "`import {module}.{{ ... }}` binds only the names it lists; add \
                         `import {module}` to write `{module}.<name>`"
                    ))
                } else {
                    let candidates = self.visible_names();
                    suggest_similar(&name_str, candidates.iter())
                        .map(|hint| format!("did you mean `{hint}`?"))
                        .or_else(|| {
                            let module = self.builtin_function_module(name)?;
                            Some(format!(
                                "`{name}` is in module `{module}`: write `{module}.{name}` \
                                 after `import {module}`, or add `import {module}.{{ {name} }}`"
                            ))
                        })
                }
            }
        };
        let mut d = Diagnostic::error(Code::UnresolvedName, span, message);
        if let Some(help) = help {
            d = d.with_help(help);
        }
        self.error(d);
    }

    /// The name the module imported as `name` is bound as, when an
    /// `import name as n` binds it.
    fn alias_of(&self, name: Symbol) -> Option<Symbol> {
        let target = match self.imports.get(&name)? {
            Imported::Builtin(id) | Imported::Module(id, _) | Imported::Cell(id, _) => *id,
            Imported::Poisoned => return None,
        };
        let mut bound: Vec<Symbol> = self
            .scope
            .values
            .iter()
            .filter_map(|(n, b)| (*b == Binding::Module(target) && *n != name).then_some(*n))
            .collect();
        bound.sort_by_key(|n| resolve(*n));
        bound.first().copied()
    }

    /// The help for the module `name` used by its name where an `as`
    /// import binds it under another.
    fn alias_help(&self, name: Symbol) -> Option<String> {
        let bound_as = self.alias_of(name)?;
        Some(format!(
            "module `{name}` is imported as `{bound_as}` here: write `{bound_as}.<name>`"
        ))
    }

    /// The first builtin module (in the order of `builtin_modules`) with a
    /// function `name`.
    fn builtin_function_module(&self, name: Symbol) -> Option<&'static str> {
        crate::module::builtin_modules().iter().copied().find(|m| {
            ModuleId::builtin(m)
                .and_then(|id| self.builtins.modules.get(&id))
                .is_some_and(|exports| exports.values.contains_key(&name))
        })
    }

    /// The module `name` names, when the module imports items of it but
    /// does not bind the module itself.
    fn items_import_of(&self, name: Symbol) -> Option<Symbol> {
        self.imports.contains_key(&name).then_some(name)
    }

    /// The names a value position can see, for a "did you mean".
    fn visible_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .locals
            .iter()
            .flatten()
            .chain(self.scope.values.keys())
            .chain(self.scope.implied.keys())
            .chain(self.builtins.prelude.values.keys())
            .map(|n| resolve(*n))
            .filter(|n| n != "self")
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// A field access: `m.f`, `m.Circle`, `C.Red`, `m.C.Red` name a member
    /// (returned); anything else is a field or method of a value, whose
    /// head is resolved as an expression.
    fn field_access(&mut self, expr: &mut Expr) -> Option<Res> {
        let span = expr.span;
        let ExprKind::FieldAccess(obj, field, field_span) = &mut expr.kind else {
            return None;
        };
        let (field, field_span) = (*field, *field_span);
        // `m.C.Red`: the inner access names a type of a module.
        if let ExprKind::FieldAccess(head, enum_name, enum_span) = &mut obj.kind
            && let ExprKind::Ident(m) = &head.kind
            && !self.is_local(*m)
            && let Some(Binding::Module(id)) = self.scope.values.get(m).cloned()
            && resolve(*enum_name).starts_with(char::is_uppercase)
            && resolve(field).starts_with(char::is_uppercase)
        {
            let (m, enum_name, enum_span) = (*m, *enum_name, *enum_span);
            head.res = Some(Res::Module(id));
            let res = self.module_variant(
                id,
                Qualifier {
                    name: m,
                    span: head.span,
                },
                &[Qualifier {
                    name: enum_name,
                    span: enum_span,
                }],
                field,
                field_span,
            );
            if let Some(Res::Def(v)) = res
                && let Some(ty) = self.defs.variant_type(v)
            {
                let ty_id = match ty.kind {
                    DefKind::Type(t) => Some(t.0),
                    _ => None,
                };
                obj.res = ty_id.map(Res::Def);
            } else {
                obj.res = Some(Res::Error);
            }
            return res;
        }
        let ExprKind::Ident(head_name) = obj.kind else {
            self.expr(obj);
            return None;
        };
        let head_span = obj.span;
        if self.is_local(head_name) {
            obj.res = Some(Res::Local);
            return None;
        }
        match self.lookup_global_value(head_name) {
            Some(Binding::Module(id)) => {
                obj.res = Some(Res::Module(id));
                Some(self.module_member(id, head_name, field, span))
            }
            Some(Binding::Poisoned) => {
                obj.res = Some(Res::Error);
                Some(Res::Error)
            }
            Some(Binding::Def(d)) if self.defs.get(d).is_type() => {
                obj.res = Some(Res::Def(d));
                if resolve(field).starts_with(char::is_uppercase) && self.enum_like(d) {
                    self.enum_variant(d, &resolve(head_name), field, span)
                } else {
                    None
                }
            }
            Some(Binding::Ambiguous(ids)) => {
                self.ambiguous(head_name, head_span, &ids, None);
                obj.res = Some(Res::Error);
                None
            }
            Some(Binding::Def(d)) => {
                obj.res = Some(Res::Def(d));
                None
            }
            None if crate::module::is_builtin_module(&resolve(head_name))
                && self.alias_of(head_name).is_none() =>
            {
                let module_str = resolve(head_name);
                let mut d = Diagnostic::error(
                    Code::ModuleNotImported,
                    span,
                    format!("module '{module_str}' is not imported"),
                )
                .with_help(self.import_help(&module_str));
                // An import of a module this build lacks would not help.
                if crate::module::missing_feature(&module_str).is_none() {
                    d = d.with_fix(
                        format!("Add import for `{module_str}`"),
                        vec![(Span::point(span.file, 0), format!("import {module_str}\n"))],
                    );
                }
                self.error(d);
                obj.res = Some(Res::Error);
                Some(Res::Error)
            }
            // `m.x` after `import m.{ ... }` of a module that failed to
            // load: the failure is reported at the import.
            None if matches!(self.imports.get(&head_name), Some(Imported::Poisoned)) => {
                obj.res = Some(Res::Error);
                Some(Res::Error)
            }
            None => {
                self.unresolved_value(head_name, head_span);
                obj.res = Some(Res::Error);
                Some(Res::Error)
            }
        }
    }

    /// Whether the type `d` is an enum (one with variants).
    fn enum_like(&self, d: DefId) -> bool {
        !self.defs.variants(d).is_empty()
    }

    /// The member `field` of the module `id`, bound as `bound_as`.
    fn module_member(&mut self, id: ModuleId, bound_as: Symbol, field: Symbol, span: Span) -> Res {
        let Some(exports) = self.module_exports(id) else {
            return Res::Error;
        };
        match exports.member(field).cloned() {
            Some(Binding::Ambiguous(ids)) => {
                self.ambiguous(field, span, &ids, Some(bound_as));
                Res::Error
            }
            Some(binding) => self.binding_res(&binding),
            None => {
                let d = missing_item(bound_as, id.builtin_name(), field, span, exports);
                self.error(d);
                Res::Error
            }
        }
    }
}
