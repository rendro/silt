//! Dictionaries in the compiled code (`crate::bytecode::dict`): the
//! hidden parameters of the definitions that take them, the code that
//! makes or finds the dictionary the checker's evidence names, and the
//! templates of the impls.

use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::{Decl, TraitImpl};
use crate::bytecode::dict::{Dict, Method, Recipe, Template};
use crate::bytecode::{Asm, Globals};
use crate::defs::{ImplArgs, TraitId, TypeId};
use crate::diagnostic::{Code, Diagnostic};
use crate::evidence::{Evidence, IMPL_HEADER, Layout, Shape};
use crate::intern::{Symbol, intern, resolve};
use crate::source::Span;
use crate::value::Value;

use super::{Compiler, Local};

/// An impl: its trait, its type's head and its trait arguments.
type ImplKey = (TraitId, TypeId, ImplArgs);

/// The name of the `k`th hidden parameter of the definition `owner`, a
/// name no program can write.
pub(super) fn dict_name(owner: Span, k: usize) -> Symbol {
    intern(&format!(
        "<dict {:?}:{}:{} {k}>",
        owner.file, owner.start, owner.end
    ))
}

/// The name of the local that keeps the dictionary of the method
/// `owner` of an impl, for the functions in it (`keep_frame_dict`).
fn frame_dict_name(owner: Span) -> Symbol {
    intern(&format!(
        "<dict {:?}:{}:{} of the impl>",
        owner.file, owner.start, owner.end
    ))
}

/// A defect in silt: evidence the compiler cannot turn into code.
fn no_dictionary(span: Span, what: &str) -> Diagnostic {
    Diagnostic::error(
        Code::CompilerBug,
        span,
        format!("compiler bug: {what} reached the compiler; the typechecker reports it"),
    )
}

/// What an impl's header has too many of when its dictionary cannot hold
/// them.
const HEADER_PREDICATES: &str = "predicates of an impl's header";

/// The error of a definition that has `count` of `what`, more than the
/// `max` the dictionary instructions can name.
fn limit(what: &'static str, count: usize, max: usize, span: Span) -> Diagnostic {
    crate::bytecode::emit::limit_diagnostic(crate::bytecode::ops::Limit { what, count, max }, span)
}

/// The trait `t` has no more methods, supertraits and `where` clauses on
/// its parameters than the dictionary instructions can name (`CallDict`,
/// `DictMethod`, `SuperDict`, `WhereDict`, which name one by its place):
/// the error counts what the trait declares.
fn trait_limits(t: &crate::ast::TraitDecl) -> Result<(), Diagnostic> {
    let places = |operand: usize| operand + 1;
    let parts = [
        (
            "methods of a trait",
            t.methods.len(),
            places(usize::from(u16::MAX)),
        ),
        (
            "supertraits of a trait",
            t.supertraits.len(),
            places(usize::from(u8::MAX)),
        ),
        (
            "where clauses of a trait's parameters",
            t.param_where_clauses.len(),
            places(usize::from(u8::MAX)),
        ),
    ];
    for (what, count, max) in parts {
        if count > max {
            return Err(limit(what, count, max, t.span));
        }
    }
    Ok(())
}

/// Enter the template of each built-in impl of a trait of type
/// constructors (`builtins::applicative::IMPLS`) the globals do not have
/// yet. (After the program's own templates: theirs keep their places.
/// One that a program impl's recipe names is entered with it.)
fn enter_builtin_templates(globals: &mut Globals) {
    for (trait_name, type_name, _) in crate::builtins::applicative::IMPLS {
        if let (Some(tr), Some(ty)) = (
            crate::defs::builtin_trait_id(trait_name),
            crate::defs::builtin_type_id(type_name),
        ) {
            builtin_template(globals, tr, ty);
        }
    }
}

/// The template of the built-in impl of the trait `tr` for the type
/// `ty`, entered the first time: its methods are builtins, and its
/// supertraits' dictionaries are those of the type's built-in impls of
/// them. `None` where silt has no such impl.
fn builtin_template(globals: &mut Globals, tr: TraitId, ty: TypeId) -> Option<Arc<Template>> {
    if let Some(template) = globals.template(tr, ty, ImplArgs::none()) {
        return Some(template.clone());
    }
    let (trait_name, type_name, rows) =
        (crate::builtins::applicative::IMPLS.iter()).find(|(t, y, _)| {
            crate::defs::builtin_trait_id(t) == Some(tr)
                && crate::defs::builtin_type_id(y) == Some(ty)
        })?;
    let methods = (rows.iter())
        .map(|row| {
            let row = crate::builtins::applicative::method_row(row);
            Method::Builtin(row.expect("a method of a built-in impl is a row").id)
        })
        .collect();
    let decls = crate::typechecker::builtin_trait_decls();
    let decl = decls.iter().find(|decl| resolve(decl.name) == *trait_name);
    let supers = (decl.into_iter().flat_map(|decl| &decl.supertraits))
        .filter_map(|sup| {
            let sup = crate::defs::builtin_trait_id(&resolve(sup.name))?;
            Some(Recipe::Dict {
                template: builtin_template(globals, sup, ty)?,
                subs: Box::new([]),
            })
        })
        .collect();
    let template = Arc::new(Template {
        index: globals.next_template_index(),
        tr,
        ty,
        args: ImplArgs::none(),
        name: format!("{trait_name} for {type_name}"),
        descriptor: None,
        full: false,
        methods,
        supers,
        wheres: Box::new([]),
    });
    globals.add_template(template.clone());
    Some(template)
}

/// The methods of a trait as a program declares them, each with the
/// place of its first parameter of type `Self` (`self`, or one
/// written `x: Self`, or `xs: Self(a)` of a trait of type
/// constructors).
pub(super) fn declared_methods(t: &crate::ast::TraitDecl) -> Vec<(String, Option<u8>)> {
    t.methods
        .iter()
        .map(|method| {
            let at = method.params.iter().position(|param| match &param.ty {
                Some(ty) => match &ty.kind {
                    crate::ast::TypeExprKind::SelfType => true,
                    crate::ast::TypeExprKind::Generic {
                        module: None, name, ..
                    } => *name == intern("Self"),
                    _ => false,
                },
                None => matches!(param.pattern.kind,
                    crate::ast::PatternKind::Ident(name) if name == intern("self")),
            });
            (
                resolve(method.name),
                at.and_then(|at| u8::try_from(at).ok()),
            )
        })
        .collect()
}

impl Compiler<'_> {
    /// What the definition `owner` takes before its written parameters.
    pub(super) fn shape(&self, owner: Span) -> Option<Shape> {
        self.owners.get(&owner).copied()
    }

    /// The number of hidden parameters of a definition of the shape
    /// `shape`: a function's or a `let`'s bounds; a method's, its
    /// trait's bounds for it (the dictionary of its impl is not one: a
    /// call leaves it below the arguments, in the slot of the callee).
    pub(super) fn hidden_count(shape: Shape) -> usize {
        usize::from(shape.own)
    }

    /// Enter the hidden parameters of the definition `owner`, whose
    /// function is being compiled: its first locals.
    pub(super) fn hidden_params(&mut self, owner: Span) -> usize {
        let Some(shape) = self.shape(owner) else {
            return 0;
        };
        let n = Self::hidden_count(shape);
        if shape.layout != Layout::Scheme {
            self.ctx_mut().frame_dict = Some(owner);
            self.emitter().takes_dict_slot();
        }
        let ctx = self.ctx_mut();
        let depth = ctx.scope_depth;
        for k in 0..n {
            ctx.locals.push(Local {
                name: dict_name(owner, k),
                depth,
                slot: k,
            });
        }
        self.emitter().hidden_params(n);
        n
    }

    /// Push the value of the hidden parameter `k` of the definition
    /// `owner`, which the code being compiled is in.
    fn load_hidden(&mut self, owner: Span, k: usize, span: Span) -> Result<(), Diagnostic> {
        let name = dict_name(owner, k);
        if let Some(slot) = self.resolve_local(name) {
            self.emit(Asm::GetLocal { slot }, span)
        } else if let Some(index) = self.resolve_upvalue(name) {
            self.emit(Asm::GetUpvalue { index }, span)
        } else {
            Err(no_dictionary(
                span,
                "a bound of a definition the code is not in",
            ))
        }
    }

    /// Push the dictionary of the impl whose method `owner` the code
    /// being compiled is in: the one a call left below the arguments, or
    /// the local that keeps it for a function in the method.
    pub(super) fn load_frame_dict(&mut self, owner: Span, span: Span) -> Result<(), Diagnostic> {
        // (The dictionary of an impl whose header asks nothing is the
        // same wherever the method runs.)
        if let Some(&index) = self.own_dicts.get(&owner) {
            let k = self.add_constant(Value::Dict(Dict::Of(index)), span)?;
            return self.emit(Asm::Constant { k }, span);
        }
        let name = frame_dict_name(owner);
        if let Some(slot) = self.resolve_local(name) {
            self.emit(Asm::GetLocal { slot }, span)
        } else if self.ctx().frame_dict == Some(owner) {
            self.emit(Asm::FrameDict, span)
        } else if let Some(index) = self.resolve_upvalue(name) {
            self.emit(Asm::GetUpvalue { index }, span)
        } else {
            Err(no_dictionary(
                span,
                "the dictionary of a method the code is not in",
            ))
        }
    }

    /// Keep the dictionary of the impl of `method`, whose parameters are
    /// bound, in a local, if functions in the method's body may need it
    /// (a closure, or a `let` general in a bound): they capture it.
    pub(super) fn keep_frame_dict(
        &mut self,
        method: &crate::ast::FnDecl,
    ) -> Result<(), Diagnostic> {
        let owner = method.body.span;
        if self.ctx().frame_dict != Some(owner) || self.own_dicts.contains_key(&owner) {
            return Ok(());
        }
        let nested = super::any_expr(&method.body, &mut |e| {
            matches!(e.kind, crate::ast::ExprKind::Lambda { .. })
                || (e.span != owner && self.shape(e.span).is_some())
        });
        if nested {
            self.emit(Asm::FrameDict, owner)?;
            self.add_local(frame_dict_name(owner), owner)?;
        }
        Ok(())
    }

    /// Push the dictionary the evidence `ev` names.
    pub(super) fn compile_evidence(&mut self, ev: &Evidence, span: Span) -> Result<(), Diagnostic> {
        if let Some(dict) = self.constant_dict(ev) {
            let k = self.add_constant(dict, span)?;
            return self.emit(Asm::Constant { k }, span);
        }
        match ev {
            Evidence::Impl {
                tr,
                ty,
                args,
                subs,
                full,
            } => {
                let template = self.template_of((*tr, *ty, *args), span)?;
                for sub in subs {
                    self.compile_evidence(sub, span)?;
                }
                let mut n = subs.len();
                if template.full {
                    self.compile_full_type(full.as_deref(), span)?;
                    n += 1;
                }
                let proto = Value::Dict(Dict::Of(template.index));
                let k = self.add_constant(proto, span)?;
                self.emit(Asm::MakeDict { k, n }, span)
            }
            Evidence::Param { owner, k } => {
                let k = usize::from(*k);
                let Some(shape) = self.shape(*owner) else {
                    return Err(no_dictionary(span, "a bound of a definition with none"));
                };
                let (header, own) = (usize::from(shape.header), usize::from(shape.own));
                match shape.layout {
                    Layout::Scheme => self.load_hidden(*owner, k, span),
                    // The header's bounds are the impl's dictionary's;
                    // after the trait's bounds for the method, the trait
                    // of `self` (an impl for every function or tuple) is
                    // the impl's dictionary itself.
                    Layout::ImplMethod if k < header => {
                        self.load_frame_dict(*owner, span)?;
                        self.emit(Asm::DictSub { k }, span)
                    }
                    Layout::ImplMethod if k < header + own => {
                        self.load_hidden(*owner, k - header, span)
                    }
                    Layout::ImplMethod => self.load_frame_dict(*owner, span),
                    // `Self: Trait` is the impl's dictionary; after the
                    // trait's bounds for the method, its `where` clauses
                    // on its parameters are the dictionary's.
                    Layout::Default if k == 0 => self.load_frame_dict(*owner, span),
                    Layout::Default if k <= own => self.load_hidden(*owner, k - 1, span),
                    Layout::Default => {
                        self.load_frame_dict(*owner, span)?;
                        self.emit(Asm::WhereDict { index: k - 1 - own }, span)
                    }
                }
            }
            Evidence::Super { of, index, .. } => {
                self.compile_evidence(of, span)?;
                let index = usize::from(*index);
                self.emit(Asm::SuperDict { index }, span)
            }
            Evidence::Pending(late) => {
                let slot = (self.globals.pending_slot(*late))
                    .ok_or_else(|| super::too_many_globals(self.globals.len() + 1, span))?;
                self.emit(Asm::GetGlobal { slot }, span)
            }
            Evidence::Structural { .. } | Evidence::Native { .. } => Err(no_dictionary(
                span,
                "the dictionary of a trait found by the value",
            )),
            Evidence::Missing => Err(no_dictionary(span, "a bound with no evidence")),
        }
    }

    /// The dictionary `ev` names when it is the same wherever the code
    /// runs: an impl whose header's dictionaries are too.
    fn constant_dict(&self, ev: &Evidence) -> Option<Value> {
        let Evidence::Impl {
            tr,
            ty,
            args,
            subs,
            full,
        } = ev
        else {
            return None;
        };
        let template = self.globals.template(*tr, *ty, *args)?.clone();
        let mut subs = (subs.iter())
            .map(|sub| self.constant_dict(sub))
            .collect::<Option<Vec<Value>>>()?;
        if template.full {
            subs.push(self.type_constant(full.as_deref()?)?);
        }
        Some(Value::Dict(Dict::new(&template, subs.into_iter())))
    }

    /// Whether each type variable in `ty` has a value where the code runs
    /// (`compile_type_var`).
    fn has_value(&mut self, ty: &crate::types::Type) -> bool {
        use crate::types::Type;
        match ty {
            Type::Rigid(r) => {
                let local = super::type_var_name(r.name);
                self.resolve_local(local).is_some()
                    || self.resolve_upvalue(local).is_some()
                    || (self.decode_params.as_ref())
                        .is_some_and(|(_, params)| params.contains(&r.name))
            }
            Type::Var(_) => false,
            _ if self.type_constant(ty).is_some() => true,
            _ => match self.type_parts(ty) {
                Some((_, parts)) => parts.into_iter().all(|part| self.has_value(part)),
                None => false,
            },
        }
    }

    /// Push the value of the type variable `name` where the code runs: of
    /// a `type name` parameter, or a type parameter of the header of the
    /// impl whose method the code is in, if its dictionary carries its
    /// type (or it is the `decode` of a `json.Decode` impl).
    fn compile_type_var(&mut self, name: Symbol, span: Span) -> Result<(), Diagnostic> {
        let local = super::type_var_name(name);
        if let Some(slot) = self.resolve_local(local) {
            return self.emit(Asm::GetLocal { slot }, span);
        }
        if let Some(index) = self.resolve_upvalue(local) {
            return self.emit(Asm::GetUpvalue { index }, span);
        }
        if let Some((owner, params)) = &self.decode_params
            && let Some(k) = params.iter().position(|param| *param == name)
            && let Ok(k) = u8::try_from(k)
        {
            let owner = *owner;
            return self.compile_type_param(owner, k, span);
        }
        Err(super::checker_missed(span, "a type variable with no value"))
    }

    /// Push the value of the type `full` of a dictionary that carries it:
    /// a constant, or made of the values its type variables have where
    /// the code runs (a `type a` parameter, a parameter of the header of
    /// an impl whose dictionary carries its type). Unit if it has none:
    /// the dictionary's type is then its impl's head.
    fn compile_full_type(
        &mut self,
        full: Option<&crate::types::Type>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        use crate::types::Type;
        let Some(ty) = full.filter(|ty| self.has_value(ty)) else {
            return self.emit(Asm::Unit, span);
        };
        if let Some(value) = self.type_constant(ty) {
            let k = self.add_constant(value, span)?;
            return self.emit(Asm::Constant { k }, span);
        }
        if let Type::Rigid(r) = ty {
            return self.compile_type_var(r.name, span);
        }
        let Some((head, parts)) = self.type_parts(ty) else {
            return self.emit(Asm::Unit, span);
        };
        for part in &parts {
            self.compile_full_type(Some(part), span)?;
        }
        let ty = self.add_constant(Value::TypeDescriptor(head), span)?;
        self.emit(
            Asm::MakeType {
                ty,
                count: parts.len(),
            },
            span,
        )
    }

    /// The template of the impl `key`.
    fn template_of(&self, key: ImplKey, span: Span) -> Result<Arc<Template>, Diagnostic> {
        let (tr, ty, args) = key;
        (self.globals.template(tr, ty, args).cloned()).ok_or_else(|| {
            no_dictionary(span, "the dictionary of an impl the program does not have")
        })
    }

    /// Push the dictionaries `bounds` names, in order: their number.
    pub(super) fn pass_dicts(
        &mut self,
        bounds: &[Evidence],
        span: Span,
    ) -> Result<usize, Diagnostic> {
        for ev in bounds {
            self.compile_evidence(ev, span)?;
        }
        Ok(bounds.len())
    }

    /// Give each trait a program declares its methods, in order, and
    /// each impl of one the dictionary template of its type: those of
    /// the modules this program installs.
    pub(super) fn enter_templates(&self, globals: &mut Globals) -> Result<(), Diagnostic> {
        let units: Vec<_> = (self.units.modules.iter().enumerate())
            .filter(|(index, _)| !self.units.earlier.installed.contains(index))
            .map(|(_, unit)| unit)
            .collect();
        // (The traits first: an impl may come before its trait. The
        // builtin traits whose impls are passed as dictionaries are
        // every program's.)
        for decl in crate::typechecker::builtin_trait_decls() {
            if let Some(id) = crate::defs::builtin_trait_id(&resolve(decl.name))
                && crate::defs::has_dictionary(id)
            {
                globals.note_trait_methods(id, declared_methods(&decl));
            }
        }
        for unit in &units {
            for decl in &unit.program.decls {
                if let Decl::Trait(t) = decl
                    && let Some(id) = self.declared_trait(unit.id, t.name)
                {
                    trait_limits(t)?;
                    globals.note_trait_methods(id, declared_methods(t));
                }
            }
        }
        // (In the order of the program: a template's place is the same
        // in each compile of it.)
        let mut impls: HashMap<ImplKey, &TraitImpl> = HashMap::new();
        let mut keys = Vec::new();
        for unit in &units {
            for decl in &unit.program.decls {
                if let Decl::TraitImpl(ti) = decl
                    && let (Some(ty), Some(t)) = (self.impl_type(ti), self.impl_trait(ti))
                    && crate::defs::has_dictionary(t)
                {
                    self.header_limit(ti)?;
                    if impls.insert((t, ty, ti.args_key), ti).is_none() {
                        keys.push((t, ty, ti.args_key));
                    }
                }
            }
        }
        for key in keys {
            self.enter_template(key, &impls, globals, 0)?;
        }
        enter_builtin_templates(globals);
        Ok(())
    }

    /// The header of the impl `ti` asks no more traits a program declares
    /// than its dictionary holds (as many as `MakeDict` takes): the error,
    /// at the impl, counts what the header asks, whether or not the impl
    /// is used.
    fn header_limit(&self, ti: &TraitImpl) -> Result<(), Diagnostic> {
        let max = usize::from(u8::MAX);
        let asked = self.header_dicts(ti);
        match asked > max {
            true => Err(limit(HEADER_PREDICATES, asked, max, ti.span)),
            false => Ok(()),
        }
    }

    /// The trait the `where` clause `wc` names.
    fn where_trait(&self, wc: &crate::ast::WhereClause) -> Option<TraitId> {
        match wc.trait_res {
            Some(crate::defs::Res::Def(id)) => match self.units.defs.get(id).kind {
                crate::defs::DefKind::Trait(t) => Some(t),
                _ => None,
            },
            _ => None,
        }
    }

    /// The number of dictionaries the header of the impl `ti` asks.
    fn header_dicts(&self, ti: &TraitImpl) -> usize {
        (ti.where_clauses.iter())
            .filter(|wc| self.where_trait(wc).is_some_and(crate::defs::has_dictionary))
            .count()
    }

    /// Whether the dictionaries of the impl `ti` carry their type in full
    /// (`Template::full`): an impl for a generic type, over its
    /// parameters, whose header asks `json.Decode` of one.
    fn carries_full_type(&self, ti: &TraitImpl) -> bool {
        let decode = crate::defs::builtin_trait_id("Decode");
        ti.instance.is_none()
            && !ti.target_param_names.is_empty()
            && (ti.where_clauses.iter()).any(|wc| decode.is_some() && self.where_trait(wc) == decode)
    }

    /// The dictionary of the impl of each method of an impl whose header
    /// asks nothing: the place of its template, by the method's body
    /// (`load_frame_dict`).
    pub(super) fn own_dicts_of(&self, globals: &Globals) -> HashMap<Span, u32> {
        let mut own = HashMap::new();
        for (index, unit) in self.units.modules.iter().enumerate() {
            if self.units.earlier.installed.contains(&index) {
                continue;
            }
            for decl in &unit.program.decls {
                let Decl::TraitImpl(ti) = decl else { continue };
                let (Some(ty), Some(t)) = (self.impl_type(ti), self.impl_trait(ti)) else {
                    continue;
                };
                let Some(template) = globals.template(t, ty, ti.args_key) else {
                    continue;
                };
                // (One that carries its type differs by the type.)
                if template.full {
                    continue;
                }
                for method in &ti.methods {
                    let owner = method.body.span;
                    if self.shape(owner).is_some_and(|shape| shape.header == 0) {
                        own.insert(owner, template.index);
                    }
                }
            }
        }
        own
    }

    /// `enter_templates` for the impl `key` (and, first, for the impls
    /// its recipes name).
    fn enter_template(
        &self,
        key: ImplKey,
        impls: &HashMap<ImplKey, &TraitImpl>,
        globals: &mut Globals,
        depth: usize,
    ) -> Result<Option<Arc<Template>>, Diagnostic> {
        if let Some(template) = globals.template(key.0, key.1, key.2) {
            return Ok(Some(template.clone()));
        }
        let Some(ti) = impls.get(&key) else {
            // (A built-in impl, the supertrait's of a program's impl for a
            // builtin type: `trait Sized: Functor` for `Option(_)`.)
            let builtin =
                (key.2 == ImplArgs::none()).then(|| builtin_template(globals, key.0, key.1));
            return Ok(builtin.flatten());
        };
        if depth > 32 {
            return Err(no_dictionary(
                ti.span,
                "impls whose dictionaries make each other",
            ));
        }
        let (tr, ty, args) = key;
        let methods = (globals.trait_methods(tr).unwrap_or_default().to_vec())
            .into_iter()
            .map(|(name, _)| globals.method_at(tr, ty, args, &name).unwrap_or(u16::MAX))
            .map(Method::Global)
            .collect();
        // (A supertrait's impl is for the same type: one that carries
        // it is given this dictionary's, after the header's.)
        let full = self.carries_full_type(ti);
        let own = full.then(|| u16::try_from(self.header_dicts(ti)).unwrap_or(u16::MAX));
        let mut supers = Vec::new();
        for (_, ev) in &ti.evidence.supers {
            supers.push(self.recipe(ev, own, impls, globals, depth)?);
        }
        let mut wheres = Vec::new();
        for ev in &ti.evidence.wheres {
            wheres.push(self.recipe(ev, None, impls, globals, depth)?);
        }
        let shown_args = &ti.shown_args;
        let template = Arc::new(Template {
            index: globals.next_template_index(),
            tr,
            ty,
            args,
            name: format!(
                "{}{shown_args} for {}",
                self.units.defs.get(tr.0).name,
                self.type_info(ty).name
            ),
            // (An impl for an instance, `Box(Int)`, gives it in full.)
            descriptor: Some(
                (ti.instance.as_ref().and_then(|ty| self.type_constant(ty))).unwrap_or_else(|| {
                    self.type_descriptor(crate::types::TypeRef {
                        id: ty,
                        name: self.units.defs.get(ty.0).name,
                    })
                }),
            ),
            full,
            methods,
            supers: supers.into(),
            wheres: wheres.into(),
        });
        globals.add_template(template.clone());
        Ok(Some(template))
    }

    /// How the dictionary `ev` names is made from the dictionaries of an
    /// impl's header, and `own`, the place among them of the impl's type
    /// in full if the dictionary made is of the same type and the impl's
    /// carries it.
    fn recipe(
        &self,
        ev: &Evidence,
        own: Option<u16>,
        impls: &HashMap<ImplKey, &TraitImpl>,
        globals: &mut Globals,
        depth: usize,
    ) -> Result<Recipe, Diagnostic> {
        Ok(match ev {
            Evidence::Impl {
                tr, ty, args, subs, ..
            } => {
                match self.enter_template((*tr, *ty, *args), impls, globals, depth + 1)? {
                    Some(template) => {
                        let mut parts = Vec::new();
                        for sub in subs {
                            parts.push(self.recipe(sub, None, impls, globals, depth + 1)?);
                        }
                        if template.full {
                            parts.push(own.map_or(Recipe::ByValue, Recipe::Header));
                        }
                        Recipe::Dict {
                            template,
                            subs: parts.into(),
                        }
                    }
                    None => Recipe::ByValue,
                }
            }
            Evidence::Param { owner, k } if *owner == IMPL_HEADER => Recipe::Header(*k),
            Evidence::Super { of, index, .. } => {
                Recipe::Super(Box::new(self.recipe(of, own, impls, globals, depth)?), *index)
            }
            _ => Recipe::ByValue,
        })
    }
}

impl Compiler<'_> {
    /// Compile `value`, the value of the `let` of `name` that is general
    /// in a bound, as the function of its dictionaries that makes the
    /// value (each use calls it with its own), and leave the function on
    /// the stack. `own_name`: the name the `let` gives a closure that is
    /// the value.
    pub(super) fn compile_thunk(
        &mut self,
        name: Symbol,
        value: &crate::ast::Expr,
        own_name: Option<Symbol>,
    ) -> Result<(), Diagnostic> {
        let span = value.span;
        let hidden = self.shape(span).map_or(0, Self::hidden_count);
        // (The closures in it are named as they would be without it.)
        let closures = self.contexts.last().map(|around| around.closures.clone());
        self.begin_function(resolve(name), hidden, span)?;
        if let (Some(closures), Some(context)) = (closures, self.contexts.last_mut()) {
            context.closures = closures;
        }
        self.hidden_params(span);
        self.closure_name = own_name;
        self.in_tail_position = true;
        self.compile_expr(value)?;
        self.in_tail_position = false;
        self.emit_body_return(span)?;
        let (func, captures) = self.end_function(span)?;
        let closure = Arc::new(crate::bytecode::VmClosure::new(Arc::new(func), vec![]));
        let f = self.add_constant(Value::VmClosure(closure), span)?;
        self.emit(
            Asm::MakeClosure {
                f,
                captures: &captures,
            },
            span,
        )
    }

    /// Compile the name `name` (at `at` in `pattern`) of the `let` of
    /// `pattern` to `value`, which is general in a bound: the function of
    /// the name's dictionaries that makes the value again, with them in
    /// their places among the value's (`Program::pattern_lets`; a bound of
    /// the value that only another name has is unit there, which no part
    /// of the value this name takes uses), and takes the name's part. Leave
    /// it on the stack; the number of the name's dictionaries.
    pub(super) fn compile_pattern_thunk(
        &mut self,
        pattern: &crate::ast::Pattern,
        value: &crate::ast::Expr,
        name: Symbol,
        at: Span,
    ) -> Result<usize, Diagnostic> {
        let span = value.span;
        let places = self.pattern_lets.get(&at).cloned().unwrap_or_default();
        let all = self.shape(span).map_or(0, Self::hidden_count);
        let closures = self.contexts.last().map(|around| around.closures.clone());
        self.begin_function(resolve(name), places.len(), span)?;
        if let (Some(closures), Some(context)) = (closures, self.contexts.last_mut()) {
            context.closures = closures;
        }
        let ctx = self.ctx_mut();
        let depth = ctx.scope_depth;
        for k in 0..places.len() {
            ctx.locals.push(Local {
                name: dict_name(at, k),
                depth,
                slot: k,
            });
        }
        self.emitter().hidden_params(places.len());
        for place in 0..all {
            match places.iter().position(|p| usize::from(*p) == place) {
                Some(slot) => self.emit(Asm::GetLocal { slot }, span)?,
                None => self.emit(Asm::Unit, span)?,
            }
            self.add_local(dict_name(span, place), span)?;
        }
        self.compile_expr(value)?;
        self.add_local(intern("__let_val__"), span)?;
        self.compile_pattern_bind_checked(pattern, span)?;
        let slot = (self.resolve_local(name))
            .ok_or_else(|| no_dictionary(at, "a name its pattern does not bind"))?;
        self.emit(Asm::GetLocal { slot }, span)?;
        self.emit(Asm::Return, span)?;
        let (func, captures) = self.end_function(span)?;
        let closure = Arc::new(crate::bytecode::VmClosure::new(Arc::new(func), vec![]));
        let f = self.add_constant(Value::VmClosure(closure), span)?;
        self.emit(
            Asm::MakeClosure {
                f,
                captures: &captures,
            },
            span,
        )?;
        Ok(places.len())
    }

    /// Fill the slots of the dictionaries earlier REPL entries left to
    /// this one (`Program::late_evidence`), first.
    pub(super) fn fill_late_dicts(
        &mut self,
        program: &crate::ast::Program,
    ) -> Result<(), Diagnostic> {
        let span = Span::BUILTIN;
        for (late, ev) in &program.late_evidence {
            self.compile_evidence(ev, span)?;
            let slot = (self.globals.pending_slot(*late))
                .ok_or_else(|| super::too_many_globals(self.globals.len() + 1, span))?;
            self.emit(Asm::SetGlobal { slot }, span)?;
            self.emit(Asm::Pop, span)?;
        }
        Ok(())
    }
}

impl Compiler<'_> {
    /// The builtin `expr` names as a value, if it is one that is passed
    /// dictionaries (`list.traverse`), with their evidence.
    pub(super) fn builtin_with_dicts<'e>(
        &self,
        expr: &'e crate::ast::Expr,
    ) -> Option<(&'static crate::builtins::registry::Row, &'e [Evidence])> {
        if self.callee_call.is_some()
            || !matches!(
                expr.kind,
                crate::ast::ExprKind::Ident(_) | crate::ast::ExprKind::FieldAccess(..)
            )
        {
            return None;
        }
        let Some(crate::evidence::Site::Ref(bounds)) = expr.ev.as_deref() else {
            return None;
        };
        let name = self
            .builtin_function(expr.res)
            .filter(|_| !bounds.is_empty())?;
        Some((crate::builtins::registry::registry().named(&name)?, bounds))
    }

    /// The builtin `row`, which is passed the dictionaries `bounds`
    /// names, as a value: a function of the builtin's arguments that
    /// calls it with them, bound to the dictionaries as a function with
    /// bounds is (`list.traverse` given to a function).
    pub(super) fn compile_builtin_value(
        &mut self,
        row: &crate::builtins::registry::Row,
        bounds: &[Evidence],
        span: Span,
    ) -> Result<(), Diagnostic> {
        let argc = row.arity();
        self.begin_function(row.qualified(), argc, span)?;
        for slot in 0..argc {
            self.emit(Asm::GetLocal { slot }, span)?;
        }
        let builtin = row.id;
        self.emit(Asm::CallBuiltin { builtin, argc }, span)?;
        self.emit(Asm::Return, span)?;
        let (func, _) = self.end_function(span)?;
        let closure = crate::bytecode::VmClosure::new(Arc::new(func), vec![]);
        let k = self.add_constant(Value::VmClosure(Arc::new(closure)), span)?;
        self.emit(Asm::Constant { k }, span)?;
        let n = self.pass_dicts(bounds, span)?;
        self.emit(Asm::Bind { n }, span)
    }

    /// Whether the use `expr` of a definition with bounds names a `let`
    /// that is general in one: the function of its dictionaries that
    /// makes its value (`compile_thunk`), called where it is used.
    pub(super) fn makes_its_value(&self, expr: &crate::ast::Expr) -> bool {
        match expr.res {
            Some(crate::defs::Res::Local) => true,
            Some(crate::defs::Res::Def(id)) => {
                self.units.defs.get(id).kind == crate::defs::DefKind::Let
            }
            _ => false,
        }
    }

    /// The callee `callee` of a call just compiled is a function with
    /// bounds: push its dictionaries (they are its first arguments).
    /// Their number, and, of a call in tail position (`tail`), the
    /// number of the first that are the frame's own first values, which
    /// the call keeps where they are instead (`emit_call_keeping`).
    pub(super) fn callee_dicts_of(
        &mut self,
        callee: &crate::ast::Expr,
        span: Span,
        tail: bool,
    ) -> Result<(usize, usize), Diagnostic> {
        match callee.ev.as_deref() {
            Some(crate::evidence::Site::Ref(bounds)) if !self.makes_its_value(callee) => {
                let keep = if tail { self.kept_dicts(bounds) } else { 0 };
                Ok((self.pass_dicts(&bounds[keep..], span)?, keep))
            }
            _ => Ok((0, 0)),
        }
    }

    /// How many of the dictionaries `bounds` are those the function
    /// being compiled was passed first, in their order: in its frame's
    /// first slots.
    fn kept_dicts(&self, bounds: &[Evidence]) -> usize {
        let own = |(i, ev): (usize, &Evidence)| match ev {
            Evidence::Param { owner, k } => {
                usize::from(*k) == i
                    && self
                        .shape(*owner)
                        .is_some_and(|s| s.layout == Layout::Scheme)
                    && self.resolve_local(dict_name(*owner, i)) == Some(i)
            }
            _ => false,
        };
        bounds.iter().enumerate().take_while(|&ev| own(ev)).count()
    }

    /// `emit_call`, of a call that keeps the frame's first `keep` values
    /// as its callee's first arguments.
    pub(super) fn emit_call_keeping(
        &mut self,
        argc: usize,
        keep: usize,
        tail: bool,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if keep == 0 {
            return self.emit_call(argc, tail, span);
        }
        self.emit(Asm::TailCallKeep { argc, keep }, span)?;
        self.emit(Asm::Return, span)
    }
}

impl Compiler<'_> {
    /// The trait of a dictionary whose method the access `callee`
    /// (`Tr.m`, `T.m`, `a.m` for a `type a`) calls, if it is one: a
    /// trait a program declares, with the evidence of the call.
    pub(super) fn dict_call<'e>(
        &self,
        callee: &'e crate::ast::Expr,
    ) -> Option<(TraitId, &'e Evidence, &'e [Evidence])> {
        let Some(crate::defs::Res::Def(id)) = callee.res else {
            return None;
        };
        let crate::defs::DefKind::Trait(tr) = self.units.defs.get(id).kind else {
            return None;
        };
        match callee.ev.as_deref() {
            Some(crate::evidence::Site::Method { recv, own })
                if crate::defs::has_dictionary(tr) =>
            {
                Some((tr, recv, own))
            }
            _ => None,
        }
    }

    /// The trait `tr` as an operand names it (`Globals::trait_index`).
    fn trait_operand(&mut self, tr: TraitId, span: Span) -> Result<u16, Diagnostic> {
        let name = resolve(self.units.defs.get(tr.0).name);
        self.globals.trait_index(tr, name).ok_or_else(|| {
            let limit = crate::bytecode::ops::Limit {
                what: "traits whose methods a program calls",
                count: usize::from(u16::MAX) + 2,
                max: usize::from(u16::MAX) + 1,
            };
            crate::bytecode::emit::limit_diagnostic(limit, span)
        })
    }

    /// The method `method` of the trait `tr`: its place among the
    /// trait's, and that of its `self`.
    fn method_place(
        &self,
        tr: TraitId,
        method: Symbol,
        span: Span,
    ) -> Result<(usize, Option<u8>), Diagnostic> {
        (self.globals.trait_method(tr, &resolve(method)))
            .ok_or_else(|| no_dictionary(span, "a method its trait does not declare"))
    }

    /// Call the method `method` of the trait `tr` through the dictionary
    /// `recv` names, with the dictionaries `own` names and the arguments
    /// `args` (in tail position: `tail`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn compile_dict_call(
        &mut self,
        tr: TraitId,
        recv: &Evidence,
        own: &[Evidence],
        method: Symbol,
        args: &[&crate::ast::Expr],
        span: Span,
        tail: bool,
    ) -> Result<(), Diagnostic> {
        let (index, at) = self.method_place(tr, method, span)?;
        if let Some((Method::Builtin(builtin), _)) = self.checked_call(tr, recv, method, index) {
            // The method of a built-in impl at a known type: a call of
            // its builtin (`Functor.map([1], f)` is `list.map([1], f)`).
            let dicts = self.pass_dicts(own, span)?;
            self.compile_operands(args.iter().copied())?;
            let argc = dicts + args.len();
            return self.emit(Asm::CallBuiltin { builtin, argc }, span);
        }
        if let Some((Method::Global(slot), template)) = self.checked_call(tr, recv, method, index) {
            // The method of an impl whose header asks nothing, through
            // its dictionary: a call of the method's global, with the
            // receiver checked.
            self.emit(Asm::GetGlobal { slot }, span)?;
            let dicts = self.pass_dicts(own, span)?;
            self.compile_operands(args.iter().copied())?;
            let argc = dicts + args.len();
            let Some(at) = at else {
                return self.emit_call(argc, tail, span);
            };
            let at = dicts + usize::from(at);
            if !tail {
                return self.emit(Asm::CallChecked { argc, at, template }, span);
            }
            self.emit(Asm::TailCallChecked { argc, at, template }, span)?;
            return self.emit(Asm::Return, span);
        }
        // (The dictionary is in the slot of the callee: the method takes
        // it from there.)
        self.compile_evidence(recv, span)?;
        let dicts = self.pass_dicts(own, span)?;
        self.compile_operands(args.iter().copied())?;
        let argc = dicts + args.len();
        let at = at.map_or(usize::from(u8::MAX), |at| dicts + usize::from(at));
        match tail {
            true => {
                self.emit(
                    Asm::TailCallDict {
                        method: index,
                        argc,
                        at,
                    },
                    span,
                )?;
                self.emit(Asm::Return, span)
            }
            false => self.emit(
                Asm::CallDict {
                    method: index,
                    argc,
                    at,
                },
                span,
            ),
        }
    }

    /// The method `method` (the trait's `index`th) of the impl whose
    /// dictionary `recv` names, and the place of the impl's template, if
    /// the dictionary is the same wherever the code runs and the method
    /// is the impl's own (a trait's default takes the dictionary of the
    /// impl it runs for).
    fn checked_call(
        &self,
        tr: TraitId,
        recv: &Evidence,
        method: Symbol,
        index: usize,
    ) -> Option<(Method, usize)> {
        let Some(Value::Dict(Dict::Of(template))) = self.constant_dict(recv) else {
            return None;
        };
        let found = *self.globals.template_at(template)?.methods.get(index)?;
        let default = self.globals.default_method(tr, &resolve(method));
        let own = match found {
            Method::Global(slot) => slot != u16::MAX && default != Some(slot),
            Method::Builtin(_) => true,
        };
        (own && u16::try_from(template).is_ok()).then_some((found, template as usize))
    }

    /// The method `method` of the trait `tr`, through the dictionary
    /// `recv` names, as a value: with that dictionary and those `own`
    /// names bound.
    pub(super) fn compile_dict_method_value(
        &mut self,
        tr: TraitId,
        recv: &Evidence,
        own: &[Evidence],
        method: Symbol,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let (index, _) = self.method_place(tr, method, span)?;
        self.compile_evidence(recv, span)?;
        let of = self.trait_operand(tr, span)?;
        self.emit(Asm::DictMethod { method: index, of }, span)?;
        let n = 1 + self.pass_dicts(own, span)?;
        self.emit(Asm::Bind { n }, span)
    }
}

impl Compiler<'_> {
    /// `begin_function` for a function the program wrote with `written`
    /// parameters, after `hidden` dictionaries: the limit of parameters
    /// is the VM's less the hidden ones, and a function over it is
    /// reported with the parameters the program wrote.
    pub(super) fn begin_function_after(
        &mut self,
        name: String,
        written: usize,
        hidden: usize,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let max = usize::from(u8::MAX).saturating_sub(hidden);
        if hidden > 0 && written > max {
            let limit = crate::bytecode::ops::Limit {
                what: "parameters of a function",
                count: written,
                max,
            };
            return Err(
                crate::bytecode::emit::limit_diagnostic(limit, span).with_help(
                    "each bound on a trait the program declares takes one of the 255 places",
                ),
            );
        }
        self.begin_function(name, hidden + written, span)
    }
}
