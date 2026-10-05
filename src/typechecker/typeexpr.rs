use super::*;

impl TypeChecker {
    // ── Resolved names ──────────────────────────────────────────────

    /// The definition a resolver slot names, if it names one.
    pub(super) fn res_def(&self, res: Option<crate::defs::Res>) -> Option<crate::defs::Def> {
        match res {
            Some(crate::defs::Res::Def(id)) => self.def(id),
            _ => None,
        }
    }

    /// The definition `id`: the session's, or for a checker that has no
    /// program (the builtins), a builtin's.
    pub(super) fn def(&self, id: crate::defs::DefId) -> Option<crate::defs::Def> {
        match &self.defs {
            Some(defs) => Some(*defs.get(id)),
            None => names::builtin_def(id),
        }
    }

    /// The type of a type definition (a record, an enum, an alias). A
    /// builtin type is known by its id alone: the builtin environment,
    /// which the builtin definitions are made from, asks for them.
    pub(super) fn def_type(&self, id: crate::defs::DefId) -> Option<TypeRef> {
        if let Some((name, _)) = crate::defs::builtin_types().get(id.0 as usize) {
            return Some(TypeRef {
                id: crate::defs::TypeId(id),
                name: intern(name),
            });
        }
        let def = self.def(id)?;
        match def.kind {
            crate::defs::DefKind::Type(ty) => Some(TypeRef {
                id: ty,
                name: def.name,
            }),
            crate::defs::DefKind::TypeAlias => Some(TypeRef {
                id: crate::defs::TypeId(id),
                name: def.name,
            }),
            _ => None,
        }
    }

    /// The trait of a trait definition. A builtin trait is known by its
    /// id alone (see `def_type`).
    pub(super) fn def_trait(&self, id: crate::defs::DefId) -> Option<TraitKey> {
        let first = crate::defs::builtin_types().len();
        if let Some(k) = (id.0 as usize).checked_sub(first)
            && let Some(name) = crate::defs::BUILTIN_TRAITS.get(k)
        {
            return Some(TraitKey::builtin(name));
        }
        let def = self.def(id)?;
        match def.kind {
            crate::defs::DefKind::Trait(t) => Some(TraitKey {
                id: t,
                name: def.name,
            }),
            _ => None,
        }
    }

    /// The type a resolver slot names.
    pub(super) fn res_type(&self, res: Option<crate::defs::Res>) -> Option<TypeRef> {
        match res {
            Some(crate::defs::Res::Def(id)) => self.def_type(id),
            _ => None,
        }
    }

    /// The type a name written in a type position names: its resolution;
    /// for a name with none (the checker wrote it itself), the module's
    /// own type of that name, else the builtin type of that name.
    pub(super) fn named_type(
        &self,
        res: Option<crate::defs::Res>,
        name: Symbol,
    ) -> Option<TypeRef> {
        if res.is_some() {
            return self.res_type(res);
        }
        if let Some(ty) = self.own_types.get(&name) {
            return Some(*ty);
        }
        let name_str = resolve(name);
        let name_str = if name_str == "()" {
            "Unit"
        } else {
            name_str.as_str()
        };
        crate::defs::builtin_type_id(name_str).map(|id| TypeRef { id, name })
    }

    /// Whether a type name, or a variant's resolution, names a type
    /// declaration the module's check rejected (see `rejected_types`).
    pub(super) fn names_rejected(&self, res: Option<crate::defs::Res>, name: Symbol) -> bool {
        if self.rejected_types.is_empty() {
            return false;
        }
        self.named_type(res, name)
            .or_else(|| self.res_variant_enum(res))
            .is_some_and(|ty| self.rejected_types.contains(&ty))
    }

    /// The number of type parameters of a record, enum or alias type;
    /// `None` for a type that is none of these.
    pub(super) fn type_arity(&self, ty: TypeRef) -> Option<usize> {
        self.tables
            .record_param_var_ids
            .get(&ty)
            .map(|v| v.len())
            .or_else(|| self.tables.enums.get(&ty).map(|e| e.params.len()))
            .or_else(|| self.tables.records.contains_key(&ty).then_some(0))
            .or_else(|| self.tables.type_alias_arity.get(&ty).copied())
    }

    /// The trait a trait name names: its resolution; for a name with none
    /// (the checker wrote it itself), the module's own trait of that
    /// name, else the builtin trait of that name.
    pub(super) fn named_trait(
        &self,
        res: Option<crate::defs::Res>,
        name: Symbol,
    ) -> Option<TraitKey> {
        match res {
            Some(crate::defs::Res::Def(id)) => self.def_trait(id),
            Some(_) => None,
            None => self.own_traits.get(&name).copied().or_else(|| {
                crate::defs::builtin_trait_id(&resolve(name)).map(|id| TraitKey { id, name })
            }),
        }
    }

    /// The trait the impl `ti` implements.
    pub(super) fn impl_trait(&self, ti: &TraitImpl) -> Option<TraitKey> {
        self.named_trait(ti.trait_res, ti.trait_name)
    }

    /// The type the impl `ti` is for, as impls are keyed: the canonical
    /// head of its target (`Range` is `List`, an alias is the type it
    /// stands for).
    pub(super) fn impl_target(&self, ti: &TraitImpl) -> Option<TypeRef> {
        let ty = self.named_type(ti.target_res, ti.target_type)?;
        Some(canonical_head(&self.tables.resolver, ty))
    }

    /// What each variant of the enum `ty` resolves to, by name. The
    /// builtin environment, which the builtin definitions are made from,
    /// has none: its derived impls name the variants of builtin enums,
    /// whose names are unique, by their enum (see `ctor_target`).
    pub(super) fn variant_resolutions(&self, ty: TypeRef) -> HashMap<Symbol, crate::defs::Res> {
        let Some(defs) = &self.defs else {
            return HashMap::new();
        };
        defs.variants(ty.id.0)
            .iter()
            .copied()
            .filter_map(|id| Some((self.def(id)?.name, crate::defs::Res::Def(id))))
            .collect()
    }

    /// The enum of the variant a constructor pattern names: the one the
    /// resolver resolved it to. The checker resolves the patterns it
    /// makes itself as it makes them, except in the builtin environment,
    /// whose derived impls write a variant of a builtin enum with its
    /// enum (`Weekday.Monday`), and builtin type names are unique.
    pub(super) fn pattern_variant_enum(
        &self,
        res: Option<crate::defs::Res>,
        qualifier: &[Qualifier],
    ) -> Option<TypeRef> {
        match res {
            None if self.defs.is_none() => {
                qualifier.last().and_then(|q| self.named_type(None, q.name))
            }
            res => self.res_variant_enum(res),
        }
    }

    /// The enum a resolver slot naming a variant names it of.
    pub(super) fn res_variant_enum(&self, res: Option<crate::defs::Res>) -> Option<TypeRef> {
        let def = self.res_def(res)?;
        match def.kind {
            crate::defs::DefKind::Variant { ty, .. } => self.def_type(ty.0),
            _ => None,
        }
    }

    /// Resolve a TypeExpr AST node to our internal Type representation.
    pub(super) fn resolve_type_expr(
        &mut self,
        te: &TypeExpr,
        param_vars: &mut HashMap<Symbol, Type>,
    ) -> Type {
        // Phase B: canonicalise the result so user-written `Range(T)`
        // annotations arrive at the unifier (and at every other
        // typechecker consumer) as `List(T)`. The user's source
        // spelling is irrelevant downstream — diagnostic display of
        // value-side types (e.g. the `Range(Int)` from a `1..n`
        // expression) is preserved because expression-level inference
        // (`ExprKind::Range` in `inference.rs`) keeps producing
        // `Type::Range`. The unify cross-arm at `unify` (`mod.rs:1492`)
        // therefore still fires for the asymmetric case of a
        // (canonicalised) annotation meeting an internally-inferred
        // Range, but only the value side carries Range past this
        // point.
        //
        // Idempotence note: canonicalize is structurally idempotent
        // (see canonicalize_idempotent test in src/types/canonical.rs),
        // so the recursive `self.resolve_type_expr(...)` calls inside
        // the match each canonicalise their subtree, and the outer
        // call canonicalises the already-canonical result — a no-op.
        let resolved = self.resolve_type_expr_inner(te, param_vars);
        crate::types::canonical::canonicalize(&self.tables.resolver, &resolved)
    }

    pub(super) fn resolve_type_expr_inner(
        &mut self,
        te: &TypeExpr,
        param_vars: &mut HashMap<Symbol, Type>,
    ) -> Type {
        match &te.kind {
            TypeExprKind::Named { module, name, .. } => {
                // What the name means, the resolver said: nothing (it
                // reported why, or the type comes from a module that
                // failed to load), a type variable, or a type.
                if te.res == Some(crate::defs::Res::Error) {
                    return Type::Error;
                }
                if module.is_none()
                    && let Some(tv) = param_vars.get(name)
                {
                    // A type parameter variable.
                    return tv.clone();
                }
                let name_str = resolve(*name);
                if self.names_rejected(te.res, *name) {
                    return Type::Error;
                }
                let Some(ty) = self.named_type(te.res, *name) else {
                    // Lowercase names in type annotations are type variables
                    // (e.g., `a` in `List(a)` or `fn foo(x: a) -> a`)
                    let first_char = name_str.chars().next().unwrap_or('A');
                    if first_char.is_lowercase() {
                        // ... unless the name is a known type spelled
                        // in lowercase (`x: int`): that is a typo for
                        // the type, not a type variable.
                        if self.case_mismatched_type_name(&name_str, false).is_some() {
                            self.error(
                                Code::UnknownType,
                                self.unknown_type_message(&name_str, false),
                                te.span,
                            );
                            return Type::Error;
                        }
                        let tv = self.fresh_var();
                        param_vars.insert(*name, tv.clone());
                        return tv;
                    }
                    // B3 (round 60): an uppercase name that refers to
                    // nothing is reported here rather than becoming a
                    // ghost type that cascades into "does not implement
                    // Display" and "type mismatch" far from the
                    // annotation.
                    self.error(
                        Code::UnknownType,
                        self.unknown_type_message(&name_str, false),
                        te.span,
                    );
                    return Type::Error;
                };
                if let Some(builtin) = builtin_type_name(ty) {
                    match builtin {
                        "Int" => return Type::Int,
                        "Float" => return Type::Float,
                        "Bool" => return Type::Bool,
                        "String" => return Type::String,
                        "Unit" => return Type::Unit,
                        // A container without explicit type params gets
                        // a fresh variable for each. Range is a nominal
                        // alias for List (see Type::Range in
                        // src/types.rs); inference is bidirectional at
                        // unify time.
                        "List" => return Type::List(Box::new(self.fresh_var())),
                        "Range" => return Type::Range(Box::new(self.fresh_var())),
                        "Map" => {
                            return Type::Map(
                                Box::new(self.fresh_var()),
                                Box::new(self.fresh_var()),
                            );
                        }
                        "Set" => return Type::Set(Box::new(self.fresh_var())),
                        "Channel" => return Type::Channel(Box::new(self.fresh_var())),
                        // Opaque resource / value types from builtin
                        // modules: the shape the builtin schemes produce
                        // (round 72 GAP G1).
                        _ if let Some(arity) = opaque_arity(builtin) => {
                            let args = (0..arity).map(|_| self.fresh_var()).collect();
                            return Type::Generic(ty, args);
                        }
                        _ => {}
                    }
                }
                // A record, enum or alias type. If the type is
                // parameterized and the user wrote it bare (no type
                // args), instantiate a fresh type variable for each
                // parameter so distinct uses don't cross-pollute
                // through the shared template TyVars (T1 audit fix).
                // Aliases resolve to a `Type::Generic(alias, args)`
                // head that the canonicaliser will expand.
                let known = self.tables.records.contains_key(&ty)
                    || self.tables.enums.contains_key(&ty)
                    || self.tables.type_aliases.contains(&ty);
                if !known {
                    self.error(
                        Code::UnknownType,
                        self.unknown_type_message(&name_str, false),
                        te.span,
                    );
                    return Type::Error;
                }
                let arity = self.type_arity(ty).unwrap_or(0);
                let args: Vec<Type> = (0..arity).map(|_| self.fresh_var()).collect();
                Type::Generic(ty, args)
            }
            TypeExprKind::Generic { name, args, .. } => {
                // A type the resolver resolved to nothing takes any
                // arguments; nothing is known about it.
                if te.res == Some(crate::defs::Res::Error) {
                    for arg in args {
                        let _ = self.resolve_type_expr_inner(arg, param_vars);
                    }
                    return Type::Error;
                }
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| self.resolve_type_expr(a, param_vars))
                    .collect();
                let name_str = resolve(*name);
                if self.names_rejected(te.res, *name) {
                    return Type::Error;
                }
                let ty = self.named_type(te.res, *name);
                if let Some(builtin) = ty.and_then(builtin_type_name) {
                    let n = resolved_args.len();
                    let mut it = resolved_args.clone().into_iter();
                    let mut arg = |this: &mut Self| it.next().unwrap_or_else(|| this.fresh_var());
                    match (builtin, n) {
                        ("List", 0 | 1) => return Type::List(Box::new(arg(self))),
                        ("Range", 0 | 1) => return Type::Range(Box::new(arg(self))),
                        ("Map", 0 | 2) => {
                            let k = arg(self);
                            let v = arg(self);
                            return Type::Map(Box::new(k), Box::new(v));
                        }
                        ("Set", 0 | 1) => return Type::Set(Box::new(arg(self))),
                        ("Channel", 0 | 1) => return Type::Channel(Box::new(arg(self))),
                        // Opaque arity-0 builtin types accept the
                        // empty-paren surface form `Bytes()` (mirrors
                        // `List()`/`Map()` etc.) and report `Bytes(Int)`
                        // (round 72 GAP G1).
                        (b, n) if let Some(arity) = opaque_arity(b) => {
                            if n == arity {
                                return Type::Generic(ty.expect("a builtin type"), resolved_args);
                            }
                            let err_span = self.current_type_anno_span.unwrap_or(te.span);
                            self.error(
                                Code::ArityMismatch,
                                format!(
                                    "type argument count mismatch for builtin type '{b}': expected {arity}, got {n}"
                                ),
                                err_span,
                            );
                            return Type::Error;
                        }
                        _ => {}
                    }
                }
                // B2: enforce arity for user-declared parameterized
                // records, enums and aliases. A mismatched arity used to
                // be dropped at unify time, leaving a runtime type error
                // at first use of the field. A parameterless record has
                // arity 0, so `Point(Bool)` is reported too.
                // B3 (round 60): the generic form `Frobnitz(Int)` for an
                // undeclared `Frobnitz` reports "unknown type
                // 'Frobnitz'" at the annotation span, matching the
                // bare-name path.
                let Some((ty, expected)) = ty.and_then(|ty| Some((ty, self.type_arity(ty)?)))
                else {
                    self.error(
                        Code::UnknownType,
                        self.unknown_type_message(&name_str, true),
                        te.span,
                    );
                    return Type::Error;
                };
                if expected != resolved_args.len() {
                    let kind = if self.tables.records.contains_key(&ty) {
                        "record"
                    } else if self.tables.type_aliases.contains(&ty) {
                        "alias"
                    } else {
                        "enum"
                    };
                    let err_span = self.current_type_anno_span.unwrap_or(te.span);
                    self.error(Code::ArityMismatch,
                        format!(
                            "type argument count mismatch for {kind} '{name}': expected {expected}, got {}",
                            resolved_args.len()
                        ),
                        err_span,
                    );
                    // Return Error so the subsequent unify doesn't
                    // cascade a second "arity mismatch" diagnostic
                    // (the Generic/Generic arm would re-detect the
                    // same problem). The first report already has
                    // the user-facing span; extras only confuse.
                    return Type::Error;
                }
                Type::Generic(ty, resolved_args)
            }
            TypeExprKind::Tuple(elems) => {
                // `()` is the canonical unit type — not a zero-arity tuple.
                if elems.is_empty() {
                    return Type::Unit;
                }
                let types: Vec<Type> = elems
                    .iter()
                    .map(|e| self.resolve_type_expr(e, param_vars))
                    .collect();
                Type::Tuple(types)
            }
            TypeExprKind::Function(params, ret) => {
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type_expr(p, param_vars))
                    .collect();
                let ret_type = self.resolve_type_expr(ret, param_vars);
                Type::Fun(param_types, Box::new(ret_type))
            }
            TypeExprKind::SelfType => {
                if let Some(ty) = param_vars.get(&intern("Self")) {
                    ty.clone()
                } else {
                    // Self used outside of a trait context
                    self.fresh_var()
                }
            }
            TypeExprKind::AssocProj {
                receiver,
                trait_name,
                assoc_name,
                ..
            } => {
                if te.res == Some(crate::defs::Res::Error) {
                    return Type::Error;
                }
                // Build a `Type::AssocProj` whose receiver is the
                // resolved receiver type. The canonicaliser at
                // `resolve_type_expr`'s wrapper layer reduces it to
                // the impl's binding when the receiver is concrete
                // and a binding is registered; otherwise the
                // projection stays abstract.
                //
                // Sentinel-trait check: the parser emits
                // `__no_enclosing_trait__` when `Self::X` was written
                // outside a trait/impl body. Surface a clear
                // diagnostic at use-site so the user understands the
                // syntax requires an enclosing trait.
                let recv_ty = self.resolve_type_expr_inner(receiver, param_vars);
                if resolve(*trait_name) == "__no_enclosing_trait__" {
                    self.error(
                        Code::InvalidTypeAnnotation,
                        "`Self::Item` is only valid inside a trait or trait-impl body; \
                         use the qualified form `<T as Trait>::Item` here"
                            .to_string(),
                        te.span,
                    );
                    return Type::Error;
                }
                // Resolve the assoc-type name to its declaring trait.
                // `Self::Item` inside `trait Sub: Super` may reference
                // `Item` declared on `Super`; in that case the AssocProj
                // must use `Super` as the trait_name so the binding
                // (registered under `(Super, target, Item)`) is found
                // by canonicalize. Walk the supertrait chain in DFS
                // order (LIFO via `Vec::pop`); the first popped trait
                // that declares the assoc-type wins.
                let Some(trait_key) = self.named_trait(te.res, *trait_name) else {
                    self.error(
                        Code::UnknownTrait,
                        format!("unknown trait '{trait_name}'"),
                        te.span,
                    );
                    return Type::Error;
                };
                let declaring_trait = self
                    .find_assoc_type_declaring_trait(trait_key, *assoc_name)
                    .unwrap_or(trait_key);
                Type::AssocProj {
                    receiver: Box::new(recv_ty),
                    trait_name: declaring_trait,
                    assoc_name: *assoc_name,
                }
            }
            TypeExprKind::AnonRecord { fields, tail } => {
                use std::collections::BTreeMap;
                let mut field_map: BTreeMap<Symbol, Type> = BTreeMap::new();
                let mut seen: std::collections::HashSet<Symbol> = std::collections::HashSet::new();
                for (n, t) in fields {
                    if !seen.insert(*n) {
                        self.error(
                            Code::DuplicateRecordField,
                            format!("duplicate field '{}' in anon record type", n),
                            te.span,
                        );
                        continue;
                    }
                    field_map.insert(*n, self.resolve_type_expr(t, param_vars));
                }
                let row_tail = match tail {
                    None => RowTail::Closed,
                    Some(rname) => {
                        // Row-tail variable name: bind it through param_vars
                        // so multiple annotations in the same scope sharing
                        // a row name refer to the same row variable. The
                        // entry in param_vars is `Type::Var(id)` for normal
                        // type vars; for row tails we still use the same
                        // map but carry the id directly.
                        let key = intern(&format!("__row__{}", resolve(*rname)));
                        let id = if let Some(Type::Var(v)) = param_vars.get(&key).cloned() {
                            v
                        } else {
                            let id = self.fresh_tyvar_id();
                            param_vars.insert(key, Type::Var(id));
                            id
                        };
                        RowTail::Var(id)
                    }
                };
                Type::AnonRecord {
                    fields: field_map,
                    tail: row_tail,
                }
            }
        }
    }

    /// Walk the supertrait chain rooted at `trait_name` and return the
    /// first trait whose `assoc_types` declares `assoc_name`. Returns
    /// `None` when neither the trait nor any reachable supertrait
    /// declares the name (the AssocProj will then stay abstract and
    /// the typechecker emits the standard "type does not implement
    /// trait" diagnostic at use-site).
    fn find_assoc_type_declaring_trait(
        &self,
        trait_name: TraitKey,
        assoc_name: Symbol,
    ) -> Option<TraitKey> {
        let mut frontier: Vec<TraitKey> = vec![trait_name];
        let mut seen: std::collections::HashSet<TraitKey> = std::collections::HashSet::new();
        while let Some(t) = frontier.pop() {
            if !seen.insert(t) {
                continue;
            }
            if let Some(info) = self.tables.traits.get(&t) {
                if info.assoc_types.iter().any(|a| a.name == assoc_name) {
                    return Some(t);
                }
                for s in &info.supertraits {
                    if !seen.contains(s) {
                        frontier.push(*s);
                    }
                }
            }
        }
        None
    }
}

/// The number of type arguments of the builtin type `name` when it is
/// opaque (`Bytes`, `tcp.TcpStream`, `task.Handle(a)`, `TypeOf(a)`): it
/// has no variants and no fields. `None` for any other builtin type.
fn opaque_arity(name: &str) -> Option<usize> {
    crate::defs::OPAQUE_TYPE_ARITY
        .iter()
        .find(|(opaque, _)| *opaque == name)
        .map(|(_, arity)| *arity)
}
