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
        let shown = self.show_type(&ty);
        let message = format!(
            "trait `{}` cannot be implemented for `{shown}`: it has `json.Decode` as a \
             supertrait, and `{shown}` cannot be decoded: {reason}",
            self.show_trait(tr)
        );
        // (The decoder's help is for a call, not for an impl.)
        Some(Diagnostic::error(Code::InvalidTraitImpl, span, message))
    }

    /// Whether the program wrote a `json.Decode` impl for `head`: every
    /// decoder (json's, toml's, postgres's) decodes it by the impl, and
    /// does not look into its fields.
    pub(super) fn has_decode_impl(&self, head: TypeRef) -> bool {
        !self.by_structure(TraitKey::builtin("Decode"), head)
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
        // (As it is now: what a variable in it is bound to later, a
        // type a scope's end picks, is no part of what it was given.)
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
        for (ty, span, var) in std::mem::take(&mut self.encode_deferred) {
            let ty = self.apply(&ty);
            let owners = self.route_owner(var, true);
            if let Some(refusal) = self.opaque_encode_refusal(&ty, span, &owners) {
                self.refuse_opaque(span, refusal);
            }
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
        let mut found = Vec::new();
        for decl in &program.decls {
            let Decl::Fn(f) = decl else {
                continue;
            };
            let Some(scheme) = env.lookup(f.name) else {
                continue;
            };
            let Some(id) = (defs.of_module(self.module).iter())
                .copied()
                .find(|id| defs.get(*id).name == f.name)
            else {
                continue;
            };
            let mut owners = HashMap::new();
            for pred in &scheme.preds {
                if let Pred::Trait {
                    tr,
                    subject: Type::Var(v),
                    ..
                } = pred
                    && (Self::is_decode(*tr) || tr.is_builtin("Encode"))
                {
                    let encode = tr.is_builtin("Encode");
                    let found = self.route_owner(*v, encode);
                    if !found.is_empty() {
                        owners.insert((*v, encode), found);
                    }
                }
            }
            if !owners.is_empty() {
                found.push((id, owners));
            }
        }
        self.tables.def_owners.extend(found);
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
        if let Some(asker) = self.bound_askers.get(&var) {
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
    /// far as each is known.
    fn passed_through(&self, asker: Option<Asker>) -> Vec<Symbol> {
        self.passed_through_refused(asker, None)
    }

    /// `passed_through`, following on each variable a route by which the
    /// opaque type of the module `refused` is refused, where there is one:
    /// the route the error is about.
    fn passed_through_refused(
        &self,
        asker: Option<Asker>,
        refused: Option<crate::session::ModuleId>,
    ) -> Vec<Symbol> {
        let mut links = Vec::new();
        let mut seen = Vec::new();
        let mut asked = asker.map(|asker| asker.by);
        loop {
            let var = match asked {
                Some(Asked::Bound(var)) => self.rigid_rep_var(var),
                // (Another module's links, as it read them.)
                Some(Asked::Through(_, through)) => {
                    links.extend(Asked::links(through));
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
            let next = (refused.and_then(|module| self.refused_route(var, module)))
                .or_else(|| self.bound_askers.get(&var).copied());
            let Some(next) = next else {
                break;
            };
            links.extend(next.passer);
            asked = Some(next.by);
        }
        links
    }

    /// `asker` as another module or a later REPL entry reads it: the
    /// decoder, where it is known.
    pub(super) fn settled_asker(&self, asker: Option<Asker>) -> Option<Asker> {
        let links = self.passed_through(asker);
        let by = match self.asking_decoder(asker) {
            Some(decoder) if links.is_empty() => Asked::Builtin(decoder),
            Some(decoder) => Asked::Through(decoder, Asked::joined(&links)),
            None => Asked::Unknown,
        };
        asker.map(|asker| Asker { by, ..asker })
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
        for gap in std::mem::take(&mut self.decode_gaps) {
            let ty = crate::types::canonical::canonicalize(self.tables.resolver(), &gap.subject);
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
            let Some(mut diagnostic) =
                self.json_decode_refusal(&resolve(named), written, &ty, (&owners, gap.origin.span))
            else {
                continue;
            };
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
                let links = self.passed_through_refused(gap.asker, refused);
                diagnostic = (diagnostic)
                    .with_note(super::builtin_checks::passes_note(passer, &links, decoder));
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
