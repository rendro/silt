//! `json.Decode`: whether a type has it by its structure is the json
//! decoder's own judgement (`decode_refusal` with json's [`Decoding`]),
//! which also gives the text of a refusal: "`json.parse` cannot decode
//! `Holder`: field `s` has type `Set(Int)`, which has no decoder". A type
//! with a written impl has it, and is not looked into, at the top and as
//! the type of a field.
//!
//! A refusal names the decoder the need comes from (`Asker`) and, in a
//! note, the function the use calls and each one that passes the type on
//! to it in turn. What a declared bound asks for is what its body gives
//! the variable to, known once the module's bodies are checked: the
//! refusals are reported then (`settle_decode_gaps`).
//!
//! [`Decoding`]: crate::builtins::registry::decoding::Decoding

use super::solve::Origin;
use super::*;
use crate::builtins::registry::decoding::Decoding;

/// A type that lacks `json.Decode` at a use, reported once the module's
/// bodies are checked.
#[derive(Clone, Debug)]
pub(crate) struct DecodeGap {
    subject: Type,
    origin: Origin,
    asker: Option<Asker>,
    /// The modules whose definitions, at this use, ask for the type: each
    /// may decode an opaque type of its own.
    owners: Vec<crate::session::ModuleId>,
}

/// A route by which a body hands the type of an annotation variable to a
/// decoder or an encoder (`TypeChecker::owner_routes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OwnerRoute {
    /// To a definition of this module (none), or through definitions of
    /// other modules (each of them).
    Direct(Vec<crate::session::ModuleId>),
    /// On to the bound of a variable of a definition of this module.
    Passed(TyVar),
}

impl TypeChecker {
    /// The trait `json.Decode`.
    pub(super) fn is_decode(tr: TraitKey) -> bool {
        tr.is_builtin("Decode")
    }

    /// Whether `tr` has `json.Decode` among its supertraits, at any
    /// depth: a bound of it asks what a bound of `json.Decode` does.
    pub(super) fn implies_decode(&self, tr: TraitKey) -> bool {
        !Self::is_decode(tr) && self.supertraits_of(tr).into_iter().any(Self::is_decode)
    }

    /// Whether `tr` has `json.Encode` among its supertraits, at any
    /// depth: a bound of it encodes what a bound of `json.Encode` does.
    pub(super) fn implies_encode(&self, tr: TraitKey) -> bool {
        !tr.is_builtin("Encode")
            && (self.supertraits_of(tr).into_iter()).any(|tr| tr.is_builtin("Encode"))
    }

    /// Whether `head` is a generic type, of the program or a builtin
    /// container (`List`, `Map`): an impl of a trait that has
    /// `json.Decode` among its supertraits is refused for it over type
    /// variables (`Box(a)`), as a decoder builds a value of one type.
    pub(super) fn generic_for_decode(&self, head: TypeRef) -> bool {
        self.tables.is_generic(head)
            || builtin_type_name(head)
                .is_some_and(|name| crate::module::BUILTIN_GENERIC_CONTAINER_NAMES.contains(&name))
    }

    /// The refusal of the impl at `span` of `tr`, a trait that has
    /// `json.Decode` among its supertraits, for `head` (the type
    /// `self_type`, `Box(Int)`), if the json decoder has no value of the
    /// type (`json.parse(s, T)`'s judgement): its impl would decode it.
    /// Whether an opaque type may be decoded where it is is the use's to
    /// say (`opaque_decode_refusal`).
    pub(super) fn decode_impl_refusal(
        &mut self,
        tr: TraitKey,
        head: TypeRef,
        self_type: Option<Type>,
        span: Span,
    ) -> Option<Diagnostic> {
        if !self.implies_decode(tr) {
            return None;
        }
        let ty = (self_type.map(|ty| self.apply(&ty)))
            .or_else(|| self.type_instance(head))
            .unwrap_or_else(|| Type::Generic(head, Vec::new()));
        let ty = crate::types::canonical::canonicalize(self.tables.resolver(), &ty);
        // (A generic type over type variables is refused where its impl
        // is declared.)
        if self.generic_for_decode(head) && !free_vars_in(&ty).is_empty() {
            return None;
        }
        // (As the type's own module sees it: where an opaque type, or
        // one with an opaque type in it, is decoded is each use's to say.)
        let module = self.def(head.id.0).map(|def| def.module);
        if self
            .opaque_decode_refusal(&ty, span, module.as_slice())
            .is_some()
        {
            return None;
        }
        let refusal = self.decode_refusal(
            "json.parse",
            Decoding::JSON,
            None,
            &ty,
            module.as_slice(),
            span,
        )?;
        // (The decoder's reason, after what it names: "field `b` has
        // type `Box(Int)`, which has no decoder". A date or a time has no
        // type argument to speak of: what the decoders take it as.)
        use crate::typeinfo::ty;
        let reason = match head.id {
            ty::DATE | ty::TIME | ty::DATE_TIME => {
                "it is decoded only as the type of a record's field"
            }
            _ => (refusal.message.split_once(": ")).map_or("", |(_, reason)| reason),
        };
        // (A type with impls for other instantiations of it only: what a
        // decode of it says, the impl there is.)
        let only = match reason.is_empty() {
            true => Some(match self.only_decode_impl(head) {
                Some(only) => format!("the only impl is for `{}`", self.show_type(&only)),
                None => "it has no `json.Decode` impl".to_string(),
            }),
            false => None,
        };
        let reason = only.as_deref().unwrap_or(reason);
        let shown = self.show_type(&ty);
        let message = format!(
            "trait `{}` cannot be implemented for `{shown}`: it has `json.Decode` as a \
             supertrait, and `{shown}` cannot be decoded: {reason}",
            self.show_trait(tr)
        );
        // (The decoder's help is for a call, not for an impl.)
        Some(Diagnostic::error(Code::InvalidTraitImpl, span, message))
    }

    /// The one instantiation of `head` with a `json.Decode` impl, where
    /// the program wrote impls for some instantiations of it and that is
    /// the one (`Box(Int)`).
    pub(super) fn only_decode_impl(&self, head: TypeRef) -> Option<Type> {
        let tr = TraitKey::builtin("Decode");
        let instances: Vec<Type> = (self.tables.impl_args_of(tr, head).iter())
            .filter_map(|at| self.impl_instance(&(tr, head, *at)))
            .collect();
        match instances.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        }
    }

    /// Whether the program wrote a `json.Decode` impl for `head`: every
    /// decoder (json's, toml's, postgres's) decodes it by the impl, and
    /// does not look into its fields.
    pub(super) fn has_decode_impl(&self, head: TypeRef) -> bool {
        !self.by_structure(TraitKey::builtin("Decode"), head)
    }

    /// Whether `ty`, a decoded type, has in it a type with `json.Decode`
    /// impls for some instantiations only whose arguments are type
    /// variables that parameters holding a type give where the code runs
    /// (`Box(t)`, `List(Box(t))`): which impl decodes it, if one does, is
    /// each use's to say (`owe_held_decode`).
    pub(super) fn held_instance_in(&self, ty: &Type) -> bool {
        let tr = TraitKey::builtin("Decode");
        match ty {
            Type::Generic(head, args) => {
                let vars = free_vars_in(ty);
                (self.has_instances(tr, *head)
                    && !vars.is_empty()
                    && vars.iter().all(|v| self.held_by_param(*v)))
                    || args.iter().any(|arg| self.held_instance_in(arg))
            }
            Type::List(t) | Type::Set(t) => self.held_instance_in(t),
            Type::Map(k, v) => self.held_instance_in(k) || self.held_instance_in(v),
            Type::Tuple(parts) => parts.iter().any(|part| self.held_instance_in(part)),
            _ => false,
        }
    }

    /// Owe, of each use of the definition the decode at `origin` is in,
    /// the decoder's judgement of `whole` (`held_instance_in`) at the
    /// types the use gives the variables in it, as an annotation
    /// variable's need is owed: a `Pred::Given` of the tuple of the
    /// variables (`held_needs`).
    pub(super) fn owe_held_decode(&mut self, whole: &Type, origin: Origin) {
        let builtin = origin.callee.unwrap_or_else(|| intern("json.parse"));
        let check = crate::types::BuiltinCheck::Decodes {
            builtin,
            decoding: Decoding::JSON,
            written: None,
        };
        let vars = free_vars_in(whole);
        let Type::Var(stand) = self.fresh_var() else {
            return;
        };
        self.tables
            .held_needs
            .insert(stand, (whole.clone(), vars.clone(), check));
        // (A use names the argument that gives the one variable.)
        let given = Pred::Given {
            subject: held_subject(&vars),
            given: stand,
            written: None,
            passer: builtin,
            body: None,
            group: None,
        };
        self.want(given, origin);
    }

    /// Whether a `json.Decode` impl the program wrote decodes `head` at
    /// the type arguments `args`: its impl for every instantiation, or
    /// one for an instantiation `args` could be (`Box(Int)`).
    pub(super) fn has_decode_impl_at(&self, head: TypeRef, args: &[Type]) -> bool {
        let tr = TraitKey::builtin("Decode");
        self.has_decode_impl(head)
            && (!self.has_instances(tr, head) || {
                let ty = Type::Generic(head, args.to_vec());
                !self.impls_fitting(tr, head, Some(&ty), &[]).is_empty()
            })
    }

    /// The use `origin` owes `json.Decode` of `ty`, a type with no
    /// written impl of it, for `asker`: refused, if the decoder has no
    /// value of the type, once the module's bodies are checked.
    pub(super) fn want_decode(
        &mut self,
        ty: &Type,
        origin: Origin,
        asker: Option<Asker>,
        owners: Vec<crate::session::ModuleId>,
    ) {
        // (A type held with its arguments left open, `let t = List`, is
        // refused as one given so.)
        if self.holds_open_vars(ty) {
            self.open_type_arguments.insert(origin.span);
        }
        // (As it is now: a type a scope's end picks for a variable in it
        // is no part of what it was given; what a later use decides, an
        // annotation, is, judged in `settle_decode_gaps`.)
        self.decode_gaps.push(DecodeGap {
            subject: self.apply(ty),
            origin,
            asker,
            owners,
        });
    }

    /// A body owes `json.Decode` of its annotation variable `r`, for
    /// `asker`: what its declared bound asks for.
    pub(super) fn note_bound_asker(&mut self, r: RigidId, asker: Option<Asker>) {
        let var = self.rigid_rep_var(r.var);
        if let Some(asker) = asker
            && asker.by != Asked::Bound(var)
            && asker.by != Asked::Unknown
        {
            self.bound_askers.entry(var).or_insert(asker);
        }
    }

    /// A body gives a decoder its annotation variable `r` inside the type
    /// given at `span` (`Box(a)`), for `asker`: that type. Given to a
    /// function whose bound's body gives its variable inside a type, it
    /// is inside that type, at the variable's place (`load(s, a)`, then
    /// `json.parse(s, Box(b))`: `Box(a)`).
    pub(super) fn note_bound_within(&mut self, r: RigidId, asker: Option<Asker>, span: Span) {
        let var = self.rigid_rep_var(r.var);
        let Some(given) = self.decoded_at.get(&span).cloned() else {
            return;
        };
        let within = match asker.map(|asker| asker.by) {
            Some(Asked::Builtin(_)) if !matches!(given, Type::Rigid(_)) => given,
            Some(Asked::Bound(passed)) => {
                let passed = self.rigid_rep_var(passed);
                let Some(inner) = self.bound_within.get(&passed) else {
                    return;
                };
                map_rigid(inner, &mut |p| match self.rigid_rep_var(p.var) == passed {
                    true => given.clone(),
                    false => Type::Rigid(p),
                })
            }
            _ => return,
        };
        self.bound_within.entry(var).or_insert(within);
    }

    /// A body hands its annotation variable `r` to a decoder or an
    /// encoder (`encode`) in a definition of `owner` (none: of this
    /// module), or on to the bound of the variable `passed` of a
    /// definition of this module: a route of `r` (`owner_routes`). Its
    /// own bound (a call of itself) is none.
    pub(super) fn note_owner_route(
        &mut self,
        r: RigidId,
        encode: bool,
        (mut owners, passed): (Vec<crate::session::ModuleId>, Option<TyVar>),
        asker: Option<Asker>,
    ) {
        let var = self.rigid_rep_var(r.var);
        // (A builtin decoder or encoder, `json.parse`, is this module's
        // route.)
        owners.retain(|module| module.builtin_name().is_none());
        owners.sort();
        owners.dedup();
        let route = match (owners.is_empty(), passed.map(|p| self.rigid_rep_var(p))) {
            (true, Some(passed)) if passed == var => return,
            (true, Some(passed)) => OwnerRoute::Passed(passed),
            _ => OwnerRoute::Direct(owners),
        };
        let route = (route, asker);
        let routes = self.owner_routes.entry((var, encode)).or_default();
        if !routes.contains(&route) {
            routes.push(route);
        }
    }

    /// The modules on the route of the predicate of `ev`: the one whose
    /// definition owes it and those its bound hands the type on to.
    pub(super) fn owners_at(
        &self,
        ev: Option<super::evidence::EvRef>,
    ) -> Vec<crate::session::ModuleId> {
        let Some(ev) = ev else {
            return Vec::new();
        };
        let mut owners: Vec<crate::session::ModuleId> =
            self.owed_for.get(&ev).copied().into_iter().collect();
        owners.extend(self.owed_also.get(&ev).into_iter().flatten());
        owners
    }

    /// The asker of a route by which the decoder's bound on `var` refuses
    /// an opaque type of `module`: one whose owner is not that module.
    fn refused_route(&self, var: TyVar, module: crate::session::ModuleId) -> Option<Asker> {
        let routes = self.owner_routes.get(&(var, false))?;
        (routes.iter()).find_map(|(route, asker)| {
            let owners = match route {
                OwnerRoute::Direct(owners) => owners.clone(),
                OwnerRoute::Passed(passed) => self.route_owner(*passed, false),
            };
            (!owners.contains(&module)).then_some(*asker).flatten()
        })
    }

    /// The module of an opaque type of another module that decoding `ty`
    /// builds, at the top or in a type argument at any depth.
    fn opaque_module_in(&self, ty: &Type) -> Option<crate::session::ModuleId> {
        match ty {
            Type::Generic(head, args) => {
                let opaque = self
                    .def(head.id.0)
                    .filter(|def| def.vis == crate::defs::Vis::Opaque && def.module != self.module);
                (opaque.map(|def| def.module))
                    .or_else(|| args.iter().find_map(|arg| self.opaque_module_in(arg)))
            }
            Type::List(t) | Type::Set(t) | Type::Channel(t) => self.opaque_module_in(t),
            Type::Map(k, v) => self
                .opaque_module_in(k)
                .or_else(|| self.opaque_module_in(v)),
            Type::Tuple(parts) => parts.iter().find_map(|part| self.opaque_module_in(part)),
            _ => None,
        }
    }

    /// The modules whose definitions are on every route by which the body
    /// decodes (or `encodes`) the type of the annotation variable `var`:
    /// any of them decodes or encodes its own opaque types there. None
    /// where a route is this module's own.
    pub(super) fn route_owner(&self, var: TyVar, encode: bool) -> Vec<crate::session::ModuleId> {
        let var = self.rigid_rep_var(var);
        self.route_owner_in(var, encode, &mut Vec::new())
            .unwrap_or_default()
    }

    /// `route_owner`, not following the variables `seen` again: none if
    /// every route leads back to one (it says nothing).
    fn route_owner_in(
        &self,
        var: TyVar,
        encode: bool,
        seen: &mut Vec<TyVar>,
    ) -> Option<Vec<crate::session::ModuleId>> {
        if seen.contains(&var) {
            return None;
        }
        let Some(routes) = self.owner_routes.get(&(var, encode)) else {
            return Some(Vec::new());
        };
        seen.push(var);
        let mut found: Option<Vec<crate::session::ModuleId>> = None;
        for (route, _) in routes {
            let owners = match route {
                OwnerRoute::Direct(owners) => Some(owners.clone()),
                OwnerRoute::Passed(passed) => self.route_owner_in(*passed, encode, seen),
            };
            found = match (found, owners) {
                (found, None) => found,
                (None, owners) => owners,
                (Some(known), Some(owners)) => {
                    Some(known.into_iter().filter(|m| owners.contains(m)).collect())
                }
            };
        }
        seen.pop();
        found
    }

    /// The opaque encodings a use of a bound of the module owes
    /// (`encode_deferred`): refused unless the bound's owner encodes the
    /// type.
    pub(super) fn settle_encode_owners(&mut self) {
        for (ty, span, var, passer) in std::mem::take(&mut self.encode_deferred) {
            let ty = self.apply(&ty);
            let owners = self.route_owner(var, true);
            if let Some(mut refusal) = self.opaque_encode_refusal(&ty, span, &owners) {
                // (The way the function the use calls hands it on.)
                if let Some(note) =
                    passer.and_then(|passer| self.encode_note(passer, Asked::Bound(var), &ty))
                {
                    refusal = refusal.with_note(note);
                }
                self.refuse_opaque(span, refusal);
            }
        }
    }

    /// What the use at the evidence `ev` hands the type to through
    /// another module's `json.Encode` bound (`encode_asked`).
    pub(super) fn encode_asked_of(&self, ev: Option<super::evidence::EvRef>) -> Option<Asked> {
        (self.encode_asked).or_else(|| ev.and_then(|ev| self.encode_asked_at.get(&ev).copied()))
    }

    /// What the body hands the type of the predicate `wanted[i]`, a
    /// `json.Encode` it owes, to: the bound of a definition of the module
    /// the use calls, another module's definition, or the encoder; and
    /// the function the use calls.
    pub(super) fn encode_asker(&self, i: usize) -> Option<Asker> {
        let wanted = &self.wanted[i];
        let ev = Some(wanted.ev);
        let callee = wanted.origin.callee;
        let passing = ev.and_then(|ev| self.encode_passed.get(&ev).copied());
        let by = match passing {
            Some(var) => Asked::Bound(var),
            None => (self.encode_asked_of(ev)).or_else(|| {
                (self
                    .owners_at(ev)
                    .iter()
                    .all(|module| module.builtin_name().is_some()))
                .then_some(callee.map(Asked::Builtin))
                .flatten()
            })?,
        };
        Some(Asker { by, passer: callee })
    }

    /// The note of a refusal to encode `ty`, which `passer`, the function
    /// the use calls, hands on as `asked` says: the functions on the way
    /// and the encoder, following on each variable a route by which the
    /// opaque type in `ty` is refused, where there is one.
    pub(super) fn encode_note(&self, passer: Symbol, asked: Asked, ty: &Type) -> Option<String> {
        let refused = self.opaque_module_in(ty);
        let (links, encoder) = self.encoded_through(asked, refused)?;
        let (passer, links) = self.through_let(Some(passer), passer, links);
        Some(super::builtin_checks::passes_note(passer, &links, encoder))
    }

    /// The function a note says passes a type on, and the links after
    /// it, at a use whose callee is written `callee`: a `let` of another
    /// module's definition (`let oenc = other.enc`) passes it to that
    /// definition (`oenc` passes it to `other.enc`, which ...).
    pub(super) fn through_let(
        &self,
        callee: Option<Symbol>,
        passer: Symbol,
        mut links: Vec<Symbol>,
    ) -> (Symbol, Vec<Symbol>) {
        let Some((callee, target)) =
            callee.and_then(|callee| Some((callee, *self.let_links.get(&callee)?)))
        else {
            return (passer, links);
        };
        if passer == callee {
            links.insert(0, target);
            return (callee, links);
        }
        if target == passer || resolve(target).ends_with(&format!(".{passer}")) {
            links.insert(0, target);
            return (callee, links);
        }
        (passer, links)
    }

    /// The functions a `json.Encode` bound's body hands its type through
    /// on the way to the encoder, and the encoder, as `asked` says.
    fn encoded_through(
        &self,
        mut asked: Asked,
        refused: Option<crate::session::ModuleId>,
    ) -> Option<(Vec<Symbol>, Symbol)> {
        let mut links = Vec::new();
        let mut seen = Vec::new();
        loop {
            let var = match asked {
                Asked::Builtin(encoder) => return Some((links, encoder)),
                // (Another module's routes, each ending in its encoder: the
                // one that refuses the type, where there is one.)
                Asked::Through(_, through) => {
                    let routes = Asked::routes(through);
                    let route = (refused.and_then(|module| {
                        routes.iter().find(|(owners, _)| !owners.contains(&module))
                    }))
                    .or(routes.first())?;
                    let (encoder, on_the_way) = route.1.split_last()?;
                    links.extend(on_the_way.iter().copied());
                    return Some((links, *encoder));
                }
                Asked::Unknown => return None,
                Asked::Bound(var) => self.rigid_rep_var(var),
            };
            if seen.contains(&var) {
                return None;
            }
            seen.push(var);
            // (A variable of a definition with no annotation: what its
            // body hands it to.)
            // (Of those that refuse the type, the first.)
            let Some(routes) = self.owner_routes.get(&(var, true)) else {
                let askers = self.encode_askers.get(&var)?;
                let (_, asker) = (refused.and_then(|module| {
                    (askers.iter()).find(|(owners, _)| !owners.contains(&module))
                }))
                .or(askers.first())?;
                if !matches!(asker.by, Asked::Builtin(_)) {
                    links.extend(asker.passer);
                }
                asked = asker.by;
                continue;
            };
            let route_owners = |route: &OwnerRoute| match route {
                OwnerRoute::Direct(owners) => owners.clone(),
                OwnerRoute::Passed(passed) => self.route_owner(*passed, true),
            };
            let asker = (refused.and_then(|module| {
                (routes.iter()).find(|(route, asker)| {
                    asker.is_some() && !route_owners(route).contains(&module)
                })
            }))
            .or_else(|| routes.iter().find(|(_, asker)| asker.is_some()))
            .and_then(|(_, asker)| *asker)?;
            if !matches!(asker.by, Asked::Builtin(_)) {
                links.extend(asker.passer);
            }
            asked = asker.by;
        }
    }

    /// The routes of the `json.Encode` bound on `var` as another module
    /// reads them (`Asked::Through`): of each, the modules of other
    /// modules' definitions on it, and the functions on the way with the
    /// encoder last.
    fn encode_routes(&self, var: TyVar) -> Vec<crate::types::ThroughRoute> {
        let var = self.rigid_rep_var(var);
        let mut found = Vec::new();
        for (route, asker) in self.owner_routes.get(&(var, true)).into_iter().flatten() {
            let Some(asker) = asker else {
                continue;
            };
            let owners = match route {
                OwnerRoute::Direct(owners) => owners.clone(),
                OwnerRoute::Passed(passed) => self.route_owner(*passed, true),
            };
            let mut links: Vec<Symbol> = match asker.by {
                Asked::Builtin(_) => Vec::new(),
                _ => asker.passer.into_iter().collect(),
            };
            let Some((rest, encoder)) = self.encoded_through(asker.by, None) else {
                continue;
            };
            links.extend(rest);
            links.push(encoder);
            found.push((owners, self.exported_links(links)));
        }
        found
    }

    /// The functions on a route of a definition of the module, as
    /// another module reads them: one of this module by its module's
    /// name (`other.tonly`), as the call there writes it, and so a
    /// method of one of its types (`other.Box.decode`).
    fn exported_links(&self, links: Vec<Symbol>) -> Vec<Symbol> {
        (links.into_iter())
            .map(|link| self.exported_link(link))
            .collect()
    }

    /// The function or type's method `link` of a route as another
    /// module reads it (`exported_links`).
    fn exported_link(&self, link: Symbol) -> Symbol {
        if self.is_cell {
            return link;
        }
        let own_type = |name: &str| {
            (self.defs.as_ref()).is_some_and(|defs| {
                (defs.named(self.module, intern(name)).iter())
                    .any(|id| matches!(defs.get(*id).kind, crate::defs::DefKind::Type(_)))
            })
        };
        match resolve(link).split_once('.') {
            Some((head, _)) if !own_type(head) => link,
            _ => intern(&format!("{}.{link}", self.module_name)),
        }
    }

    /// Keep, of each top-level function of the module, the owners of
    /// each `json.Decode` and `json.Encode` bound that has some, for a
    /// use in another module or a later REPL input
    /// (`TypeTables::def_owners`).
    pub(super) fn record_def_owners(&mut self, program: &Program, env: &TypeEnv) {
        let Some(defs) = &self.defs else {
            return;
        };
        // (The module's definitions by name, once.)
        let ids: HashMap<Symbol, crate::defs::DefId> = (defs.of_module(self.module).iter())
            .map(|id| (defs.get(*id).name, *id))
            .collect();
        let mut found = Vec::new();
        let mut found_encoders = Vec::new();
        for decl in &program.decls {
            // (A function's bounds by their routes; a `let`'s by the uses
            // its value owes them for.)
            let (names, is_fn): (Vec<Symbol>, bool) = match decl {
                Decl::Fn(f) => (vec![f.name], true),
                Decl::Let { .. } => (
                    (crate::parser::top_level_binders(decl).into_iter())
                        .map(|(name, _, _)| name)
                        .collect(),
                    false,
                ),
                _ => continue,
            };
            for name in names {
                let Some(scheme) = env.lookup(name) else {
                    continue;
                };
                let Some(&id) = ids.get(&name) else {
                    continue;
                };
                let mut owners = HashMap::new();
                let mut encoders = HashMap::new();
                for pred in &scheme.preds {
                    if let Pred::Trait {
                        tr,
                        subject: Type::Var(v),
                        ..
                    } = pred
                        && (Self::is_decode(*tr) || tr.is_builtin("Encode"))
                    {
                        let encode = tr.is_builtin("Encode");
                        // (What its body hands an encode bound's type to,
                        // for the note of a refusal in another module.)
                        let routes = match encode && is_fn {
                            true => self.encode_routes(*v),
                            false => Vec::new(),
                        };
                        if let Some(encoder) = routes.first().and_then(|(_, l)| l.last()) {
                            encoders.insert(*v, Asked::Through(*encoder, Asked::joined(&routes)));
                        }
                        let found = match is_fn {
                            true => self.route_owner(*v, encode),
                            false => {
                                (self.scheme_owners.get(&(*v, encode)).cloned()).unwrap_or_default()
                            }
                        };
                        if !found.is_empty() {
                            owners.insert((*v, encode), found);
                        }
                    }
                }
                if !owners.is_empty() {
                    found.push((id, owners));
                }
                if !encoders.is_empty() {
                    found_encoders.push((id, encoders));
                }
            }
        }
        self.tables.def_owners.extend(found);
        self.tables.def_encoders.extend(found_encoders);
    }

    /// The type a use gives a decoder through the declared bound of
    /// `asker` (the bound's body gives its variable inside `Box(a)`),
    /// `ty` being what the use gives the variable: `Box(Set(Int))`, if
    /// nothing else in it is left open. The bound `header` of an impl of
    /// a trait that has `json.Decode` among its supertraits gives its
    /// variable inside the impl's type (`Dec for Box(a) where a:
    /// json.Decode`), in whichever module the impl is.
    fn decoded_whole(
        &self,
        asker: Option<Asker>,
        header: Option<(ImplKey, usize)>,
        ty: &Type,
    ) -> Option<Type> {
        let whole = match (header, asker.map(|asker| asker.by)) {
            (Some((key, index)), _) => {
                if !self.implies_decode(key.0) {
                    return None;
                }
                let Some(Pred::Trait {
                    tr,
                    subject: Type::Var(var),
                    ..
                }) = self.tables.impl_preds.get(&key)?.get(index)
                else {
                    return None;
                };
                if !Self::is_decode(*tr) {
                    return None;
                }
                let within = self.tables.impl_self_types.get(&key)?;
                let mut whole = substitute_vars(within, &HashMap::from([(*var, ty.clone())]));
                // (The impl's other variables, which the use gives too, are
                // not known here.)
                if free_vars_in(&whole)
                    .iter()
                    .any(|v| free_vars_in(within).contains(v))
                {
                    return None;
                }
                whole = self.apply(&whole);
                whole
            }
            (None, Some(Asked::Bound(var))) => {
                let var = self.rigid_rep_var(var);
                let within = self.bound_within.get(&var)?;
                let mut open = false;
                let whole = map_rigid(within, &mut |r| match self.rigid_rep_var(r.var) == var {
                    true => ty.clone(),
                    false => {
                        open = true;
                        Type::Rigid(r)
                    }
                });
                if open {
                    return None;
                }
                self.apply(&whole)
            }
            _ => return None,
        };
        let whole = crate::types::canonical::canonicalize(self.tables.resolver(), &whole);
        free_vars_in(&whole).is_empty().then_some(whole)
    }

    /// What the `json.Decode` bound of an impl's header on `var` asks
    /// for, where no body of the module says: the impl's type is decoded
    /// as its trait's methods decode `Self` (`json.parse(s, Self)` of a
    /// trait with `json.Decode` among its supertraits), or by the impl's
    /// own `decode` (`Box.decode`).
    fn impl_asker(&self, var: TyVar) -> Option<Asker> {
        let &(tr, target) = self.impl_decode_vars.get(&var)?;
        if Self::is_decode(tr) {
            return Some(Asker {
                by: Asked::Builtin(intern(&format!("{target}.decode"))),
                passer: None,
            });
        }
        let self_var = self.tables.traits.get(&tr)?.self_var;
        self.bound_askers
            .get(&self.rigid_rep_var(self_var))
            .copied()
    }

    /// The decoder `asker` comes from: a declared bound asks for what
    /// its body gives the variable to, if that is known.
    pub(super) fn asking_decoder(&self, asker: Option<Asker>) -> Option<Symbol> {
        self.asked_decoder(asker?.by, &mut Vec::new())
    }

    /// The decoder `asked` comes from, not looking at the bounds `seen`
    /// again. A trait's bound on a method's variable, whose body is the
    /// impls', asks for what each of the module's impls of the method
    /// asks for, if they ask for one.
    fn asked_decoder(&self, asked: Asked, seen: &mut Vec<TyVar>) -> Option<Symbol> {
        let var = match asked {
            Asked::Builtin(name) | Asked::Through(name, _) => return Some(name),
            Asked::Unknown => return None,
            Asked::Bound(var) => self.rigid_rep_var(var),
        };
        if seen.contains(&var) {
            return None;
        }
        seen.push(var);
        if let Some(asker) = self
            .bound_askers
            .get(&var)
            .copied()
            .or_else(|| self.impl_asker(var))
        {
            return self.asked_decoder(asker.by, seen);
        }
        let mut found = None;
        for var in self.bound_impls.get(&var).into_iter().flatten() {
            match (found, self.asked_decoder(Asked::Bound(*var), seen)) {
                (_, None) => {}
                (None, decoder) => found = decoder,
                (Some(one), Some(other)) if one != other => return None,
                _ => {}
            }
        }
        found
    }

    /// The functions and methods a declared bound's body passes its type
    /// through on the way to the decoder, the first first (`mid`, `load`
    /// for `top` calling `mid` calling `load` calling `json.parse`, and
    /// a trait's method its bound is declared for, `Loader.load`): as
    /// far as each is known. On each variable, a route by which the
    /// opaque type of the module `refused` is refused, where there is
    /// one: the route the error is about.
    fn passed_through_refused(
        &self,
        asker: Option<Asker>,
        refused: Option<crate::session::ModuleId>,
    ) -> Vec<Symbol> {
        let routes = self.passed_through_routes(asker, Some(refused));
        (routes.into_iter().next()).map_or_else(Vec::new, |(_, links)| links)
    }

    /// The routes of `passed_through_refused`: with `Some(refused)`, the
    /// one it follows; with `None`, one for each route of another
    /// module's definition the way leads to (`bridge.hmixed`, which
    /// hands the type to `mail.load` and to `json.parse`), each with
    /// the modules that decode the type on it, so that a module that
    /// passes the type on to that definition keeps them apart.
    fn passed_through_routes(
        &self,
        asker: Option<Asker>,
        refused: Option<Option<crate::session::ModuleId>>,
    ) -> Vec<crate::types::ThroughRoute> {
        let mut links = Vec::new();
        let mut seen = Vec::new();
        let mut asked = asker.map(|asker| asker.by);
        loop {
            let var = match asked {
                Some(Asked::Bound(var)) => self.rigid_rep_var(var),
                // (Another module's links, as it read them.)
                // (Of another module's routes, the one that refuses the
                // type, where there is one.)
                Some(Asked::Through(_, through)) => {
                    let routes = Asked::routes(through);
                    let Some(refused) = refused else {
                        return (routes.into_iter())
                            .map(|(owners, route)| {
                                (owners, links.iter().copied().chain(route).collect())
                            })
                            .collect();
                    };
                    // (A route through a function of the type's own
                    // module, `mail.load`, decodes it: not the one that
                    // refuses it.)
                    let own = |module: crate::session::ModuleId, links: &[Symbol]| {
                        let name = self.tables.module_names.get(&module).copied();
                        name.is_some_and(|name| {
                            let prefix = format!("{name}.");
                            links.iter().any(|link| resolve(*link).starts_with(&prefix))
                        })
                    };
                    let route = match refused {
                        Some(module) => routes.iter().find(|(owners, links)| {
                            !owners.contains(&module) && !own(module, links)
                        }),
                        None => routes.first(),
                    };
                    match (route, refused) {
                        (Some((_, route)), _) => links.extend(route.iter().copied()),
                        // (None known to refuse it: what is known, the
                        // functions before the type's own module's.)
                        (None, Some(module)) => {
                            let name = self.tables.module_names.get(&module).copied();
                            let prefix = name.map(|name| format!("{name}."));
                            let first = routes.first().map(|(_, route)| route.as_slice());
                            links.extend((first.unwrap_or(&[]).iter().copied()).take_while(
                                |link| {
                                    prefix
                                        .as_ref()
                                        .is_none_or(|p| !resolve(*link).starts_with(p))
                                },
                            ));
                        }
                        (None, None) => {}
                    }
                    break;
                }
                _ => break,
            };
            if seen.contains(&var) {
                break;
            }
            if !seen.is_empty() {
                links.extend(self.bound_methods.get(&var));
            }
            seen.push(var);
            let next = (refused
                .flatten()
                .and_then(|module| self.refused_route(var, module)))
            .or_else(|| self.bound_askers.get(&var).copied())
            .or_else(|| self.impl_asker(var));
            let Some(next) = next else {
                break;
            };
            links.extend(next.passer);
            asked = Some(next.by);
        }
        vec![(Vec::new(), links)]
    }

    /// `asker` as another module or a later REPL entry reads it: the
    /// decoder, where it is known.
    pub(super) fn settled_asker(&self, asker: Option<Asker>) -> Option<Asker> {
        let routes: Vec<crate::types::ThroughRoute> = (self.settled_routes(asker).into_iter())
            .map(|(owners, links)| (owners, self.exported_links(links)))
            .collect();
        // (A decode of a type of the module, `Box.decode`, by the
        // module's name, as a link is.)
        let decoder = (self.asking_decoder(asker)).map(|decoder| match resolve(decoder) {
            name if name.starts_with(|c: char| c.is_ascii_uppercase()) => {
                self.exported_link(decoder)
            }
            _ => decoder,
        });
        let by = match decoder {
            Some(decoder) if routes.iter().all(|(_, links)| links.is_empty()) => {
                Asked::Builtin(decoder)
            }
            Some(decoder) => Asked::Through(decoder, Asked::joined(&routes)),
            None => Asked::Unknown,
        };
        asker.map(|asker| Asker { by, ..asker })
    }

    /// The routes of `asker`'s bound as another module reads them: of a
    /// variable whose body takes several, each with its owners and links
    /// (a note names the one that refuses a type); else the one.
    fn settled_routes(&self, asker: Option<Asker>) -> Vec<crate::types::ThroughRoute> {
        let single = || self.passed_through_routes(asker, None);
        let Some(Asked::Bound(var)) = asker.map(|asker| asker.by) else {
            return single();
        };
        let var = self.rigid_rep_var(var);
        let Some(routes) = self.owner_routes.get(&(var, false)).filter(|r| r.len() > 1) else {
            return single();
        };
        let mut settled = Vec::new();
        for (route, route_asker) in routes {
            let Some(route_asker) = route_asker else {
                continue;
            };
            let owners = match route {
                OwnerRoute::Direct(owners) => owners.clone(),
                OwnerRoute::Passed(passed) => self.route_owner(*passed, false),
            };
            let inner = Some(Asker {
                by: route_asker.by,
                passer: None,
            });
            for (more, inner) in self.passed_through_routes(inner, None) {
                let mut owners = owners.clone();
                for module in more {
                    if !owners.contains(&module) {
                        owners.push(module);
                    }
                }
                let links = (route_asker.passer.into_iter()).chain(inner).collect();
                settled.push((owners, links));
            }
        }
        if settled.is_empty() {
            return single();
        }
        settled
    }

    /// Report each type found to lack `json.Decode`, naming the decoder
    /// its need comes from and the function that passes it on; and decide
    /// what the `json.Decode` bounds of the module's traits' methods ask
    /// for, which another module reads.
    pub(super) fn settle_decode_gaps(&mut self) {
        let own_traits: Vec<TraitKey> = (self.tables.traits.keys())
            .filter(|key| self.tables.traits.is_own(key))
            .copied()
            .collect();
        for key in own_traits {
            let Some(info) = self.tables.traits.get(&key) else {
                continue;
            };
            let mut bounds = info.method_bounds.clone();
            for pred in bounds.values_mut().flatten() {
                if let Pred::Trait { asker, .. } = pred
                    && asker.is_some()
                {
                    *asker = self.settled_asker(*asker);
                }
            }
            if let Some(info) = self.tables.traits.get_mut(&key) {
                info.method_bounds = bounds;
            }
        }
        // (A value that is one of two decoders, `mail.load` or
        // `json.parse`, owes each one's: the decoder's own, which no
        // function passes the type to, is the one reported of the two.)
        let mut gaps = std::mem::take(&mut self.decode_gaps);
        gaps.sort_by_key(|gap| gap.asker.is_some_and(|asker| asker.passer.is_some()));
        for gap in gaps {
            // (What a later use decided of a part, an annotation, is the
            // type's; what a scope's end fixed to `()` is not.)
            // (Decided in full only: a type variable it was decided as,
            // `a` that only a value holds, is judged as on the call, as
            // needing the type in full.)
            let decided: HashMap<TyVar, Type> = (free_vars_in(&gap.subject).into_iter())
                .filter(|v| !self.reaches_defaulted(&Type::Var(*v)))
                .map(|v| (v, self.apply(&Type::Var(v))))
                .filter(|(_, ty)| self.given_where_it_runs(ty))
                .collect();
            let subject = substitute_vars(&gap.subject, &decided);
            let ty = crate::types::canonical::canonicalize(self.tables.resolver(), &subject);
            // (A bound whose body hands the type to another module's
            // definition is judged as that module sees it.)
            let mut owners = gap.owners.clone();
            if let Some(Asked::Bound(var)) = gap.asker.map(|asker| asker.by) {
                owners.extend(self.route_owner(var, false));
            }
            // A decoder the bound's body hands the type to (`toml.parse`,
            // `postgres.query`) refused it here: that refusal, with the
            // note naming the function that passes it on, is the one
            // error.
            let whole = self.decoded_whole(gap.asker, gap.origin.header, &ty);
            if (self.lacking).contains(&self.refused_argument(gap.origin.span, &ty))
                || (whole.as_ref()).is_some_and(|whole| {
                    (self.lacking).contains(&self.refused_argument(gap.origin.span, whole))
                })
            {
                self.refused_calls.push(gap.origin.span);
                continue;
            }
            // (A use refused as one that needs its type in full,
            // `twice([Box { v: x }], s)`, is refused once.)
            if self.full_type_refused.contains(&gap.origin.span) {
                self.refused_calls.push(gap.origin.span);
                continue;
            }
            let decoder = self.asking_decoder(gap.asker);
            // The function or method that passes the type on, as the use
            // writes it (`store.load`).
            let passer = (gap.asker.and_then(|asker| asker.passer))
                .map(|passer| self.passer_at(passer, &gap.origin));
            // (Where no decoder is known, what the use calls is the one
            // that decodes; a use that names nothing, a supertrait an
            // impl's declaration owes: `json.decode`.)
            let named =
                (decoder.or(passer).or(gap.origin.callee)).unwrap_or_else(|| intern("json.decode"));
            let written = self.decode_aliases.get(&gap.origin.span).copied();
            if let Some(callee) = gap.origin.callee {
                self.use_callees.insert(gap.origin.span, callee);
            }
            // A list, an option, a map or a tuple with a part nothing
            // gives where the code runs (`[]` whose element a later use
            // decides as `a`, which only a value holds): refused as
            // needing its type in full, as a generic type of the program
            // with such a part is (`Box { v: [] }`).
            if super::builtin_checks::decoded_parts(&ty).is_some()
                && !self.holds_open_vars(&ty)
                && let Some(refusal) = (free_vars_in(&ty).iter())
                    .any(|v| !self.held_by_param(*v))
                    .then(|| {
                        self.part_given_by_no_name(
                            &resolve(named),
                            &gap.subject,
                            &ty,
                            gap.origin.span,
                        )
                    })
                    .flatten()
            {
                self.refused_calls.push(gap.origin.span);
                // (One error at one place: the whole type's, `Box(Option(a))`
                // of `ag(Box { v: None }, s)`, there first.)
                let reported =
                    (self.errors.iter()).any(|e| e.span == gap.origin.span && e.is_error());
                if !reported {
                    self.errors.push(refusal);
                }
                continue;
            }
            let Some(mut diagnostic) =
                self.json_decode_refusal(&resolve(named), written, &ty, (&owners, gap.origin.span))
            else {
                continue;
            };
            // (Needed in full: named as a later use decided it, by the
            // letters the program wrote, `Box(List((t, a)))`.)
            if diagnostic.code == Code::InvalidMethodCall
                && diagnostic.message.contains(" in full here: ")
                && let Some(decided) =
                    self.decided_in_full_text(&resolve(named), &gap.subject, gap.origin.span)
            {
                diagnostic = decided;
            }
            // (Said of the type the bound's body gives the decoder, whole,
            // as the decoder refuses it: `Box(Set(Int))`, field `v`. One
            // error for that type at one place.)
            let mut reported = &ty;
            if let Some(whole) = whole.as_ref()
                && let Some(refusal) = self.json_decode_refusal(
                    &resolve(named),
                    None,
                    whole,
                    (&owners, gap.origin.span),
                )
            {
                diagnostic = refusal;
                reported = whole;
            }
            self.refused_calls.push(gap.origin.span);
            if !self.lacking.insert((
                gap.origin.span,
                format!("json.Decode {}", self.show_type(reported)),
            )) {
                continue;
            }
            if let (Some(decoder), Some(passer)) = (decoder, passer) {
                // (Of an opaque type, the route that refuses it.)
                let refused =
                    (self.opaque_module_in(&ty)).filter(|module| !owners.contains(module));
                let mut links = self.passed_through_refused(gap.asker, refused);
                if let Some(before) = self.passed_to.get(&gap.origin.span) {
                    links.splice(0..0, before.iter().copied());
                }
                let (passer, links) = self.through_let(gap.origin.callee, passer, links);
                diagnostic = (diagnostic)
                    .with_note(super::builtin_checks::passes_note(passer, &links, decoder));
            }
            // (One error at one place: the trait the call names refused
            // the type there first, `Dec` of `twice(Box { v: true }, s)`.)
            if (self.errors.iter()).any(|e| e.span == gap.origin.span && e.is_error()) {
                continue;
            }
            self.errors.push(diagnostic);
        }
    }

    /// The refusal of the json decoder `builtin` given `ty` (a use names
    /// it by the alias `written`, if by one) at `span`, if it has no
    /// value of the type: json's decoders' judgement, a type with a
    /// written `json.Decode` impl not looked into.
    /// A definition of `owner` decodes that module's opaque types,
    /// wherever it is called from, in any part of the type.
    pub(super) fn json_decode_refusal(
        &self,
        builtin: &str,
        written: Option<Symbol>,
        ty: &Type,
        (owner, span): (&[crate::session::ModuleId], Span),
    ) -> Option<Diagnostic> {
        match ty {
            Type::Var(_) | Type::Rigid(_) | Type::Error | Type::Never => None,
            Type::Generic(..)
            | Type::Int
            | Type::Float
            | Type::String
            | Type::Bool
            | Type::List(_)
            | Type::Map(..)
            | Type::Tuple(_)
            | Type::Set(_)
            | Type::Channel(_) => {
                self.decode_refusal(builtin, Decoding::JSON, written, ty, owner, span)
            }
            // A type no type argument names (a function, an
            // anonymous record): what a decoder's type argument may be.
            other => {
                let accepted = Decoding::JSON.type_arguments(str::to_string);
                Some(
                    Diagnostic::error(
                        Code::Undecodable,
                        span,
                        format!(
                            "`{builtin}` cannot decode `{}`: its type argument must be {accepted}",
                            self.show_type(other)
                        ),
                    )
                    .with_help(Decoding::JSON.family.type_argument_help()),
                )
            }
        }
    }
}

/// What a use gives the variables `vars` of a decoded type, as one type
/// (`owe_held_decode`): the one variable, or the tuple of several.
pub(super) fn held_subject(vars: &[TyVar]) -> Type {
    match vars {
        [one] => Type::Var(*one),
        _ => Type::Tuple(vars.iter().map(|v| Type::Var(*v)).collect()),
    }
}

impl TypeChecker {
    /// Keep each use of a decoder in `program`, before the types are
    /// written final (`decoder_uses`). A decoder is a builtin that decodes
    /// into the type its argument names (`json.parse`, `toml.parse_list`,
    /// `postgres.query`), and the `decode` method of a type written before
    /// a dot (`Box(t).decode(v)`).
    pub(super) fn keep_decoder_uses(&mut self, program: &mut Program) {
        let decoding = self.decode_places.clone();
        let mut uses = Vec::new();
        let mut visit = |expr: &mut Expr| {
            let ExprKind::Call(callee, args) = &expr.kind else {
                return;
            };
            let call = expr.span;
            let mut keep = |arg: &Expr, callee: Symbol, span: Span| match &arg.ty {
                Some(Type::Generic(head, _)) if head.is_builtin(crate::defs::TYPE_OF) => {
                    let (mut names, mut held) = (Vec::new(), Vec::new());
                    self.type_leaves(arg, &mut names, &mut held);
                    uses.push(DecoderUse {
                        span,
                        call,
                        callee,
                        names,
                        held,
                        value: None,
                    });
                }
                Some(ty) => uses.push(DecoderUse {
                    span,
                    call,
                    callee,
                    names: Vec::new(),
                    held: Vec::new(),
                    value: Some(ty.clone()),
                }),
                None => {}
            };
            let label = Self::callee_label(callee).unwrap_or_else(|| intern("it"));
            if let Some(name) = self.decoder_named(callee) {
                (args.iter())
                    .filter(|arg| {
                        matches!(&arg.ty, Some(Type::Generic(head, _))
                            if head.is_builtin(crate::defs::TYPE_OF))
                    })
                    .for_each(|arg| keep(arg, name, arg.span));
            } else {
                // A function or method whose body hands what it is given
                // to a decoder (`load(s, L)`, `twice([], s)`): the
                // argument a decode is owed at.
                // (A call given as an argument is a use of its own,
                // owed at its place: not this one's.)
                for arg in (args.iter())
                    .filter(|arg| decoding.contains(&arg.span))
                    .filter(|arg| !matches!(arg.kind, ExprKind::Call(..) | ExprKind::Pipe(..)))
                {
                    keep(arg, label, arg.span);
                }
                // A method called through the trait (`Dec.again(b, s)`),
                // owed at the callee: of the value it is called on.
                if decoding.contains(&callee.span)
                    && let Some(receiver) = args.first()
                    && let ExprKind::FieldAccess(_, method, _) = &callee.kind
                {
                    keep(receiver, *method, callee.span);
                }
            }
            if let ExprKind::FieldAccess(receiver, field, _) = &callee.kind
                && resolve(*field) == "decode"
                && crate::defs::builtin_trait_id("Decode").is_some_and(
                    |t| matches!(callee.res, Some(crate::defs::Res::Def(id)) if id == t.0),
                )
            {
                keep(receiver, *field, receiver.span);
            }
        };
        for decl in &mut program.decls {
            match decl {
                Decl::Fn(f) => super::resolve::each_expr_mut(&mut f.body, &mut visit),
                Decl::TraitImpl(ti) => {
                    for m in &mut ti.methods {
                        super::resolve::each_expr_mut(&mut m.body, &mut visit);
                    }
                }
                Decl::Trait(t) => {
                    for m in t.methods.iter_mut().filter(|m| !m.is_signature_only) {
                        super::resolve::each_expr_mut(&mut m.body, &mut visit);
                    }
                }
                Decl::Let { value, .. } => super::resolve::each_expr_mut(value, &mut visit),
                _ => {}
            }
        }
        self.decoder_uses.extend(uses);
    }

    /// Drop each refusal of a use as one whose type nothing decides
    /// (`json.parse(s, Box(Box(Unit)))`) with another error inside it, an
    /// undefined name: that error is the one, and left nothing to decide.
    pub(super) fn drop_ambiguities_over_errors(&mut self) {
        let inner: Vec<Span> = (self.errors.iter())
            .filter(|e| e.is_error() && e.code != Code::AmbiguousType)
            .map(|e| e.span)
            .collect();
        self.errors.retain(|e| {
            e.code != Code::AmbiguousType
                || !(inner.iter()).any(|at| {
                    at.file == e.span.file && e.span.start <= at.start && at.end <= e.span.end
                })
        });
    }

    /// Refuse, once the module's bodies are checked, each decoder use
    /// (`decoder_uses`) given a type with a generic type or alias written
    /// alone in it (`Box`, `List(L)`) whose type arguments nothing decides,
    /// no name holding a type gives where the code runs, or a scope's end
    /// fixed to `()`: the compiler builds the decoder from the type the
    /// checker decided, and that is no one type. The one check behind the
    /// routes that refuse such a use with a text of their own, which come
    /// first: a use with an error at its place is not judged again.
    pub(super) fn refuse_undecided_decoder_types(&mut self) {
        for use_ in std::mem::take(&mut self.decoder_uses) {
            let DecoderUse {
                span,
                call,
                callee,
                names,
                held,
                value,
            } = use_;
            let held: Vec<TyVar> = (held.iter())
                .flat_map(|ty| free_vars_in(&self.apply(ty)))
                .collect();
            // (A value: a part of its type a scope's end fixed to `()`.)
            let undecided = value.is_some_and(|ty| self.reaches_defaulted(&ty))
                || names.iter().any(|ty| {
                    self.reaches_defaulted(ty)
                        || free_vars_in(&self.apply(ty))
                            .iter()
                            .any(|v| !held.contains(v))
                });
            #[cfg(test)]
            if undecided {
                JUDGED_UNDECIDED
                    .with(|judged| judged.borrow_mut().push(resolve(callee).to_string()));
            }
            // (An error anywhere in the call speaks for it.)
            let reported = (self.errors.iter()).any(|e| {
                e.is_error()
                    && e.span.file == call.file
                    && e.span.start < call.end.max(call.start + 1)
                    && call.start < e.span.end.max(e.span.start + 1)
            });
            if !undecided || reported {
                continue;
            }
            let message = format!(
                "cannot infer the type this use of '{callee}' needs to implement trait 'json.Decode'"
            );
            self.errors.push(
                Diagnostic::error(Code::AmbiguousType, span, message)
                    .with_help("annotate the value it is given"),
            );
        }
    }

    /// The refusal at `span` of `ty` (`subject` with what later uses
    /// decided in full) as needing its type in full: named as a later use
    /// decided it, by the letter the program wrote (`List((a, Int))`), or
    /// by its place where nothing decided it.
    fn part_given_by_no_name(
        &self,
        builtin: &str,
        subject: &Type,
        ty: &Type,
        span: Span,
    ) -> Option<Diagnostic> {
        (self.decided_in_full_text(builtin, subject, span))
            .or_else(|| self.in_full_refusal(builtin, ty, span))
    }

    /// The refusal at `span` of `subject` as needing its type in full,
    /// named as later uses decided it, where they decided it with a
    /// letter the program wrote that only a value holds (`List((a,
    /// Int))`); none where a part is still undecided.
    fn decided_in_full_text(
        &self,
        builtin: &str,
        subject: &Type,
        span: Span,
    ) -> Option<Diagnostic> {
        let decided =
            crate::types::canonical::canonicalize(self.tables.resolver(), &self.apply(subject));
        let unvalued: Vec<Symbol> = (super::inference::rigid_vars(&decided).into_iter())
            .filter(|r| !self.valued_vars.contains(&r.var))
            .map(|r| r.name)
            .collect();
        if unvalued.is_empty() || !free_vars_in(&decided).is_empty() {
            return None;
        }
        let (said, help) = super::inference::known_only_by_type(&unvalued, &unvalued);
        let message = format!(
            "`{builtin}` needs the type `{}` in full here: {said}",
            self.show_type(&decided)
        );
        let diagnostic = Diagnostic::error(Code::InvalidMethodCall, span, message);
        Some(match help {
            Some(help) => diagnostic.with_help(help),
            None => diagnostic,
        })
    }

    /// Whether every part of `ty`, a type a later use decided, is given
    /// where the code runs: no variable a value alone holds; a `type t`
    /// parameter's, or a parameter's that holds a type, is.
    fn given_where_it_runs(&self, ty: &Type) -> bool {
        free_vars_in(ty).iter().all(|v| self.held_by_param(*v))
            && (super::inference::rigid_vars(ty).iter()).all(|r| self.valued_vars.contains(&r.var))
    }

    /// Whether `ty` has in it, also through what the checker learnt of
    /// its variables, one a scope's end fixed to `()` (`defaulted_vars`).
    fn reaches_defaulted(&self, ty: &Type) -> bool {
        free_vars_in(ty).into_iter().any(|v| {
            self.defaulted_vars.contains(&v)
                || matches!(self.tables.vars.subst.get(v), Some(Some(resolved))
                    if self.reaches_defaulted(resolved))
        })
    }

    /// The name of the builtin decoder `callee` calls, as written, if it
    /// calls one.
    fn decoder_named(&self, callee: &Expr) -> Option<Symbol> {
        let def = self.res_def(callee.res)?;
        let module = def.module.builtin_name()?;
        let row = crate::builtins::registry::registry().named(&format!("{module}.{}", def.name))?;
        row.decoding?;
        Some(match &callee.kind {
            ExprKind::FieldAccess(obj, field, _) => match &obj.kind {
                ExprKind::Ident(m) => intern(&format!("{m}.{field}")),
                _ => *field,
            },
            ExprKind::Ident(name) => *name,
            _ => def.name,
        })
    }

    /// The types the names of types alone in `e` stand for (`names`), and
    /// the types the other names in it hold (`held`).
    fn type_leaves(&self, e: &Expr, names: &mut Vec<Type>, held: &mut Vec<Type>) {
        match &e.kind {
            ExprKind::Call(_, args) if self.names_a_type(e) => args
                .iter()
                .for_each(|arg| self.type_leaves(arg, names, held)),
            ExprKind::Tuple(elems) => elems
                .iter()
                .for_each(|elem| self.type_leaves(elem, names, held)),
            _ => {
                let Some(Type::Generic(head, args)) = &e.ty else {
                    return;
                };
                let Some(ty) = args
                    .first()
                    .filter(|_| head.is_builtin(crate::defs::TYPE_OF))
                else {
                    return;
                };
                match self.names_a_type(e) {
                    true => names.push(ty.clone()),
                    false => held.push(ty.clone()),
                }
            }
        }
    }

    /// Whether `e` is a type's or an alias's name, or one applied to
    /// types.
    fn names_a_type(&self, e: &Expr) -> bool {
        let named = match &e.kind {
            ExprKind::Call(callee, _) => callee,
            _ => e,
        };
        matches!(named.kind, ExprKind::Ident(_) | ExprKind::FieldAccess(..))
            && self.res_def(named.res).is_some_and(|def| {
                matches!(
                    def.kind,
                    crate::defs::DefKind::Type(_) | crate::defs::DefKind::TypeAlias
                )
            })
    }
}

#[cfg(test)]
thread_local! {
    /// The decoders `refuse_undecided_decoder_types` judged given a type
    /// nothing decides, also where an earlier refusal speaks for the use:
    /// what a test reads to lock the one check.
    pub(super) static JUDGED_UNDECIDED: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A use of a decoder, kept before the types are written final
/// (`keep_decoder_uses`): its place, what it is called by, and of the
/// type it is given, the types of the names of types alone in it and of
/// the names holding types (`type_leaves`), or the type of the value it
/// is given (`twice([], s)`).
#[derive(Clone, Debug)]
pub(crate) struct DecoderUse {
    span: Span,
    /// The call the use is (an error anywhere in it speaks for it).
    call: Span,
    callee: Symbol,
    names: Vec<Type>,
    held: Vec<Type>,
    value: Option<Type>,
}
