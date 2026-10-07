use super::*;

impl TypeChecker {
    // ── Register type declarations ──────────────────────────────────

    pub(super) fn register_type_decl(&mut self, td: &TypeDecl, env: &mut TypeEnv) {
        // BROKEN #1: Reject redefinition of reserved type-system sentinel
        // names. `TypeOf` is used internally as the head of
        // `Type::builtin("TypeOf", [..])` to represent a type
        // descriptor (e.g. the runtime value produced by `Employee` when
        // used as a first-class type argument to `json.parse`). A user
        // declaring `type TypeOf(a) { Foo(a) }` would bind `Foo` as a
        // constructor returning a value structurally indistinguishable
        // from that internal descriptor, which silently typechecks and
        // then fails at runtime with "type argument must be a record
        // type". Reject at declaration time for a clear diagnostic. See
        // the sibling guard for builtin trait names in
        // `register_trait_decl` below.
        let td_name_str = resolve(td.name);
        if td_name_str == "TypeOf" {
            self.error(
                Code::InvalidTypeDeclaration,
                format!("'{td_name_str}' is a reserved type name used by the type system"),
                td.name_span,
            );
            return;
        }
        // Round 80 BROKEN B3: a type-decl whose name shadows a builtin
        // scalar/container type (Int, Float, Bool, String, Unit, List,
        // Range, Map, Set, Channel, Tuple, Fn, Fun, Handle, Bytes,
        // TcpListener, TcpStream — the authoritative list is
        // `BUILTIN_TYPES` in `src/types/builtins.rs`) silently overwrote
        // the builtin binding, and what was then said of the type's
        // structural traits went by fields the *builtin* type does
        // not have, producing an unspanned cascade of "unknown method
        // 'x' on type Int" errors. Reject at the declaration site with
        // a single clear diagnostic. Mirrors the variant-shadow error
        // shape below.
        if crate::types::builtins::lookup(td_name_str.as_str()).is_some() {
            self.errors.push(
                Diagnostic::error(
                    Code::InvalidTypeDeclaration,
                    td.name_span,
                    format!("type '{td_name_str}' shadows builtin type '{td_name_str}'"),
                )
                .with_help("choose a different name"),
            );
            return;
        }
        let ty = self.own_type(td.name);
        // B2: populate the span hint used by `resolve_type_expr` for any
        // arity error on field / variant type annotations.
        let prev_type_span = self.current_type_anno_span.replace(td.span);
        // Create a mapping from type param names to placeholder type vars
        let mut param_vars: HashMap<Symbol, Type> = HashMap::new();
        for p in &td.params {
            let tv = self.fresh_var();
            param_vars.insert(*p, tv);
        }

        // Phase D: type aliases follow a parallel registration path
        // — resolve the target with each param bound to a fresh
        // TyVar, detect cycles, then register into the canonical
        // alias registry. Aliases never appear in `enums` /
        // `records`, have no structural traits of their own (they
        // are the target's), and emit no constructor bindings.
        // Early-return before the stamps at the end of the function.
        if let TypeBody::Alias(target_te) = &td.body {
            self.register_type_alias(td, target_te, &mut param_vars);
            self.current_type_anno_span = prev_type_span;
            return;
        }

        match &td.body {
            TypeBody::Enum(variants) => {
                let mut variant_infos = Vec::new();
                let variant_defs = self.variant_resolutions(ty);

                // Compute the TyVar ids for each type parameter once,
                // before the variant loop (they are the same for every variant).
                let var_ids: Vec<TyVar> = td
                    .params
                    .iter()
                    .map(|p| match &param_vars[p] {
                        Type::Var(v) => *v,
                        _ => unreachable!(),
                    })
                    .collect();

                // G3: detect duplicate variant names within the same enum.
                // Previously `type Color { Red, Green, Red }` compiled
                // silently — the second `Red` overwrote the first's
                // constructor binding and no diagnostic was emitted.
                let mut seen_variants: std::collections::HashSet<Symbol> =
                    std::collections::HashSet::new();
                let mut first_variant: HashMap<Symbol, Span> = HashMap::new();
                for variant in variants {
                    if !seen_variants.insert(variant.name) {
                        let mut d = Diagnostic::error(
                            Code::DuplicateDeclaration,
                            variant.name_span,
                            format!("duplicate variant '{}' in enum '{}'", variant.name, td.name),
                        );
                        if let Some(&first) = first_variant.get(&variant.name) {
                            d = d.with_label(first, "first declared here");
                        }
                        self.errors.push(d);
                    } else {
                        first_variant.insert(variant.name, variant.name_span);
                    }
                }

                for variant in variants {
                    let field_types: Vec<Type> = variant
                        .fields
                        .iter()
                        .map(|te| self.resolve_type_expr(te, &mut param_vars))
                        .collect();

                    variant_infos.push(VariantInfo {
                        name: variant.name,
                        field_types: field_types.clone(),
                    });

                    // Register the constructor in the type environment
                    let type_params: Vec<Type> =
                        td.params.iter().map(|p| param_vars[p].clone()).collect();

                    let result_type = if type_params.is_empty() {
                        Type::Generic(ty, vec![])
                    } else {
                        Type::Generic(ty, type_params)
                    };

                    let scheme = Scheme {
                        vars: var_ids.clone(),
                        ty: if field_types.is_empty() {
                            // No-arg constructor is just a value
                            result_type
                        } else {
                            Type::Fun(field_types, Box::new(result_type))
                        },
                        preds: vec![],
                        optional_last_param: false,
                    };
                    // The variant's definition's scheme, which a use of it
                    // reads: two enums may have variants of one name.
                    if let Some(crate::defs::Res::Def(id)) = variant_defs.get(&variant.name) {
                        self.tables.schemes.insert(*id, scheme);
                    }
                }

                // Register the enum type name as a value so it can be
                // passed to `type a` parameters (`json.parse(body, Color)`,
                // user-defined decoders, etc.). Mirrors the record path.
                // Skipped when a variant shares the enum's name
                // (e.g. `type Box(T) { Box(T) }`) because the variant
                // constructor is already bound under the same symbol.
                let variant_shares_name = variant_infos.iter().any(|v| v.name == td.name);
                if !variant_shares_name {
                    let enum_ty = if td.params.is_empty() {
                        Type::Generic(ty, vec![])
                    } else {
                        let args: Vec<Type> =
                            td.params.iter().map(|p| param_vars[p].clone()).collect();
                        Type::Generic(ty, args)
                    };
                    let scheme = Scheme {
                        vars: var_ids.clone(),
                        ty: Type::type_of(enum_ty),
                        preds: vec![],
                        optional_last_param: false,
                    };
                    env.define(td.name, scheme);
                }

                self.tables.enums.insert(
                    ty,
                    EnumInfo {
                        params: td.params.clone(),
                        param_var_ids: var_ids,
                        variants: variant_infos,
                        defined_in: self.defining_package(),
                    },
                );
            }
            TypeBody::Record(fields) => {
                // G2: detect duplicate field names in the same record.
                // Previously `type R { a: Int, a: String }` compiled
                // silently and the first field's type was overwritten
                // by the second at the VM record layout level.
                let mut seen_fields: std::collections::HashSet<Symbol> =
                    std::collections::HashSet::new();
                let mut first_field: HashMap<Symbol, Span> = HashMap::new();
                for f in fields {
                    if !seen_fields.insert(f.name) {
                        let mut d = Diagnostic::error(
                            Code::DuplicateRecordField,
                            f.name_span,
                            format!("duplicate field '{}' in record type '{}'", f.name, td.name),
                        );
                        if let Some(&first) = first_field.get(&f.name) {
                            d = d.with_label(first, "first declared here");
                        }
                        self.errors.push(d);
                    } else {
                        first_field.insert(f.name, f.name_span);
                    }
                }
                let field_types: Vec<(Symbol, Type)> = fields
                    .iter()
                    .map(|f| {
                        let ty = self.resolve_type_expr(&f.ty, &mut param_vars);
                        (f.name, ty)
                    })
                    .collect();

                // Store param_var_ids for parameterized record types
                if !td.params.is_empty() {
                    let var_ids: Vec<TyVar> = td
                        .params
                        .iter()
                        .map(|p| match &param_vars[p] {
                            Type::Var(v) => *v,
                            _ => unreachable!(),
                        })
                        .collect();
                    self.tables.record_param_var_ids.insert(ty, var_ids);
                }

                self.tables.records.insert(
                    ty,
                    RecordInfo {
                        fields: field_types.clone(),
                        defined_in: self.defining_package(),
                    },
                );

                // Register the record type name as a value so it can be
                // passed to a `type a` parameter, e.g. `json.parse(body, Employee)`.
                // The value is a TYPE DESCRIPTOR at runtime (represented
                // as `Value::TypeDescriptor(name)`), so its type must
                // be `TypeOf(Employee)` rather than `Employee` itself —
                // otherwise the typechecker would let users write things
                // like `Employee.field` or use the descriptor as an
                // instance (T2 audit fix; mirrors primitive descriptors).
                //
                // For parameterized records (`type Box(a) { ... }`),
                // fresh type vars are generated for each param so
                // `json.parse(Box, ...)` can unify with a monomorphic
                // instance at the call site.
                let record_ty = Type::Generic(ty, Vec::new());
                let scheme = if td.params.is_empty() {
                    Scheme {
                        vars: vec![],
                        ty: Type::type_of(record_ty),
                        preds: vec![],
                        optional_last_param: false,
                    }
                } else {
                    // Re-use the param TyVars that parameterize the
                    // record's fields so the descriptor type is
                    // `forall a. TypeOf(Box(a))` — generalizing makes
                    // each call instantiate its own fresh vars.
                    let var_ids: Vec<TyVar> = td
                        .params
                        .iter()
                        .map(|p| match &param_vars[p] {
                            Type::Var(v) => *v,
                            _ => unreachable!(),
                        })
                        .collect();
                    let args: Vec<Type> = td.params.iter().map(|p| param_vars[p].clone()).collect();
                    let generic_record = Type::Generic(ty, args);
                    Scheme {
                        vars: var_ids,
                        ty: Type::type_of(generic_record),
                        preds: vec![],
                        optional_last_param: false,
                    }
                };
                env.define(td.name, scheme);
            }
            TypeBody::Alias(_) => {
                // Phase D: handled by the early-return path above.
                // This arm is unreachable but kept for exhaustiveness.
                unreachable!("alias decls handled before this match");
            }
        }

        // The structural traits of the type: `Display`, `Equal`,
        // `Compare`, `Hash`. The stamp is provisional:
        // `enforce_structural_gate` takes a trait away again from a type
        // whose fields or payloads lack it (a record that holds a
        // function has no `Equal`), with the reason. No impl is made:
        // the VM has each natively, over the structure of the value.
        let dummy_span = td.span;
        for trait_name in STRUCTURAL_TRAIT_NAMES {
            self.tables
                .trait_impl_set
                .insert((TraitKey::builtin(trait_name), ty));
        }
        // The methods of the structural traits.
        let builtin_methods: &[(&str, Type)] = &[
            (
                "display",
                Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
            ),
            (
                "equal",
                Type::Fun(
                    vec![self.fresh_var(), self.fresh_var()],
                    Box::new(Type::Bool),
                ),
            ),
            (
                "compare",
                Type::Fun(
                    vec![self.fresh_var(), self.fresh_var()],
                    Box::new(Type::Int),
                ),
            ),
            (
                "hash",
                Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
            ),
        ];
        for (method_name, method_type) in builtin_methods {
            self.tables.method_table.insert(
                (ty, intern(method_name)),
                MethodEntry {
                    method_type: method_type.clone(),
                    span: dummy_span,
                    structural: true,
                    trait_name: None,
                    preds: Vec::new(),
                },
            );
        }
        self.current_type_anno_span = prev_type_span;
    }

    /// Phase D: register a `type Foo(...) = <target>` alias.
    ///
    /// Resolves `target_te` to a [`Type`] (with the alias's params bound
    /// to fresh `TyVar`s already populated in `param_vars`), detects
    /// cycles, then writes the entry into the typechecker's session-
    /// scoped [`crate::types::canonical::Resolver`] via
    /// [`crate::types::canonical::Resolver::register_alias`].
    ///
    /// Cycle detection traverses the resolved target looking for any
    /// reference back to the alias being declared (or to another alias
    /// that — transitively — references this one). Implementation: walk
    /// the target's free `Type::Generic` heads and ask
    /// the registry whether the head is an alias whose own target
    /// reaches `td.name`. The walk uses an explicit "in-progress" set
    /// keyed on alias name so a chain `A -> B -> A` produces the
    /// expected diagnostic at one of the two ends.
    fn register_type_alias(
        &mut self,
        td: &TypeDecl,
        target_te: &TypeExpr,
        param_vars: &mut HashMap<Symbol, Type>,
    ) {
        // The alias name itself must already be in `type_aliases` (the
        // pre-pass placeholder loop populated it). Mark it as in-
        // progress so any reference back to this alias inside its own
        // target — direct or indirect — is detected as a cycle.
        let alias = self.own_type(td.name);
        self.tables.type_aliases.insert(alias);
        self.tables.type_alias_arity.insert(alias, td.params.len());

        // Round 74 Fix #3: snapshot the declared parameter names BEFORE
        // resolving the target so we can detect undeclared free tyvars
        // — `resolve_type_expr_inner` lazily inserts a fresh `Type::Var`
        // into `param_vars` for any lowercase identifier it encounters,
        // including ones the user forgot to declare in `td.params`.
        // Without this guard, `type AnyList = List(a)` (no `(a)` after
        // `AnyList`) silently allocated one shared TyVar reused across
        // every use site, breaking polymorphism (each site would unify
        // with the FIRST use's element type and reject every other).
        let declared_params: std::collections::HashSet<Symbol> =
            td.params.iter().copied().collect();

        // Resolve the target with this alias's params bound. We use
        // `resolve_type_expr_inner` here (not the public canonicalising
        // wrapper): canonicalisation would eagerly expand any alias
        // already in the registry, collapsing a multi-step chain
        // (`A -> B -> A`) to a one-step self-reference and obscuring
        // the diagnostic. The inner resolver leaves alias references
        // as `Type::Generic(alias_name, args)` so `find_alias_cycle`
        // can walk the full chain by lookup_alias-driven recursion.
        // The outer registration step (`register_alias` below) then
        // stores the un-expanded target — canonicalisation expands at
        // every use site instead.
        let target_ty = self.resolve_type_expr_inner(target_te, param_vars);

        // Round 74 Fix #3: any name `param_vars` gained during
        // resolution that wasn't in `declared_params` is an undeclared
        // free tyvar in the alias target. Emit a clear diagnostic that
        // names the offending identifier and suggests adding it to the
        // alias header. Multiple undeclared names produce one
        // diagnostic per name (sorted for stable output).
        let mut undeclared: Vec<Symbol> = param_vars
            .keys()
            .copied()
            .filter(|name| !declared_params.contains(name))
            .collect();
        undeclared.sort_by_key(|s| resolve(*s));
        for name in undeclared {
            let name_str = resolve(name);
            self.error(Code::InvalidTypeDeclaration,
                format!(
                    "undeclared type parameter '{name_str}' in alias target — did you mean `type {}({name_str}) = ...`?",
                    resolve(td.name)
                ),
                target_te.span,
            );
        }

        // Detect cycles before registering. Build a chain that names
        // every alias visited; if `td.name` appears, report it.
        let mut visiting: Vec<TypeRef> = vec![alias];
        if let Some(cycle) = self.find_alias_cycle(&target_ty, &mut visiting) {
            // Format the cycle as `A -> B -> A` for clarity. `cycle` is
            // the Vec of names from the original `td.name` through
            // each alias in the chain that closes the loop.
            let chain: Vec<String> = cycle.iter().map(|t| resolve(t.name)).collect();
            self.error(
                Code::InvalidTypeDeclaration,
                format!(
                    "type alias '{}' forms a cycle: {}",
                    td.name,
                    chain.join(" -> ")
                ),
                target_te.span,
            );
            // Round 79 LATENT TS-L1: when a cycle is detected closing
            // on the alias under registration (`td.name`), every other
            // alias in the cycle chain was registered earlier with a
            // target that pointed back through the now-known-cyclic
            // path. With `type A = B; type B = A`, A registered first
            // (B not yet visible) with target Generic("B"); B then
            // detects the cycle but A is still in the alias map
            // pretending A→Generic("B") is valid, so a later
            // `let v: A = 42` reports "expected B, got Int" instead of
            // a coherent cycle diagnostic. Walk the chain (excluding
            // td.name itself, which never registered) and unregister
            // each entry so use-site canonicalisation no longer sees
            // a half-built alias path.
            for &cycle_name in &cycle {
                if cycle_name != alias {
                    self.tables.resolver.unregister_alias(cycle_name);
                    self.tables.type_aliases.remove(&cycle_name);
                    self.tables.type_alias_arity.remove(&cycle_name);
                }
            }
            // Also drop this alias's placeholder entries — registration
            // is being skipped, and leaving the name in `type_aliases`
            // (the placeholder set) lets later passes treat A as a
            // valid alias that just happens to have no resolver entry.
            self.tables.type_aliases.remove(&alias);
            self.tables.type_alias_arity.remove(&alias);
            // Skip registration so the canonicaliser doesn't loop on a
            // self-referential expansion at any later use site.
            return;
        }

        // Collect the alias-param TyVar ids in source order.
        let param_var_ids: Vec<TyVar> = td
            .params
            .iter()
            .map(|p| match param_vars.get(p) {
                Some(Type::Var(v)) => *v,
                _ => {
                    // Unreachable: register_type_decl seeded every
                    // td.params entry into `param_vars` as a fresh
                    // Type::Var before reaching this branch.
                    unreachable!("alias param missing from param_vars")
                }
            })
            .collect();

        self.tables.resolver.register_alias(
            alias,
            crate::types::canonical::AliasInfo {
                params: td.params.clone(),
                param_var_ids,
                target: target_ty,
            },
        );
    }

    /// Walk a resolved type and return the visited-alias chain that
    /// closes a cycle, or `None` if no cycle is reachable. The
    /// `visiting` Vec carries the alias names seen so far on this
    /// walk; the head is the alias being declared.
    fn find_alias_cycle(&self, ty: &Type, visiting: &mut Vec<TypeRef>) -> Option<Vec<TypeRef>> {
        match ty {
            Type::Generic(name, args) => {
                if visiting.contains(name) {
                    // Direct or indirect self-reference. Append the
                    // closing name so the diagnostic shows
                    // `A -> B -> A`.
                    let mut chain = visiting.clone();
                    chain.push(*name);
                    return Some(chain);
                }
                if let Some(info) = self.tables.resolver.lookup_alias(*name) {
                    visiting.push(*name);
                    let result = self.find_alias_cycle(&info.target, visiting);
                    visiting.pop();
                    if result.is_some() {
                        return result;
                    }
                }
                for a in args {
                    if let Some(c) = self.find_alias_cycle(a, visiting) {
                        return Some(c);
                    }
                }
                None
            }
            Type::List(inner) | Type::Range(inner) | Type::Set(inner) | Type::Channel(inner) => {
                self.find_alias_cycle(inner, visiting)
            }
            Type::Map(k, v) => self
                .find_alias_cycle(k, visiting)
                .or_else(|| self.find_alias_cycle(v, visiting)),
            Type::Tuple(elems) => {
                for e in elems {
                    if let Some(c) = self.find_alias_cycle(e, visiting) {
                        return Some(c);
                    }
                }
                None
            }
            Type::Fun(params, ret) => {
                for p in params {
                    if let Some(c) = self.find_alias_cycle(p, visiting) {
                        return Some(c);
                    }
                }
                self.find_alias_cycle(ret, visiting)
            }
            Type::AssocProj { receiver, .. } => self.find_alias_cycle(receiver, visiting),
            Type::AnonRecord { fields, .. } => {
                for t in fields.values() {
                    if let Some(c) = self.find_alias_cycle(t, visiting) {
                        return Some(c);
                    }
                }
                None
            }
            Type::Int
            | Type::Float
            | Type::Bool
            | Type::String
            | Type::Unit
            | Type::Var(_)
            | Type::Rigid(_)
            | Type::Error
            | Type::Never => None,
        }
    }
}
