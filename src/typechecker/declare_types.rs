use super::*;

/// The type variables of a type declaration, for
/// [`TypeChecker::resolve_declared`].
pub(super) struct DeclVars {
    /// Its parameters, and the variables reported as not being one.
    known: std::collections::HashSet<Symbol>,
    /// Each lowercase name its body writes where a type stands, and each
    /// row (keyed `__row__r`, as `param_vars` keys it), in the order of
    /// its first use, with the place of that use.
    written: Vec<(Symbol, Span)>,
}

impl DeclVars {
    fn of(td: &TypeDecl) -> Self {
        fn write(written: &mut Vec<(Symbol, Span)>, name: Symbol, span: Span) {
            if !written.iter().any(|(seen, _)| *seen == name) {
                written.push((name, span));
            }
        }
        fn walk(te: &TypeExpr, written: &mut Vec<(Symbol, Span)>) {
            match &te.kind {
                TypeExprKind::Named {
                    module: None,
                    name,
                    name_span,
                } if resolve(*name).starts_with(char::is_lowercase) => {
                    write(written, *name, *name_span)
                }
                TypeExprKind::Named { .. } | TypeExprKind::SelfType => {}
                TypeExprKind::Generic { args, .. } | TypeExprKind::Tuple(args) => {
                    args.iter().for_each(|arg| walk(arg, written));
                }
                TypeExprKind::Function(params, ret) => {
                    params.iter().for_each(|param| walk(param, written));
                    walk(ret, written);
                }
                TypeExprKind::AssocProj { receiver, .. } => walk(receiver, written),
                TypeExprKind::AnonRecord { fields, tail } => {
                    fields.iter().for_each(|(_, ty)| walk(ty, written));
                    if let Some((row, span)) = tail {
                        write(written, intern(&format!("__row__{row}")), *span);
                    }
                }
            }
        }
        let mut written = Vec::new();
        match &td.body {
            TypeBody::Enum(variants) => (variants.iter())
                .flat_map(|variant| &variant.fields)
                .for_each(|te| walk(te, &mut written)),
            TypeBody::Record(fields) => fields.iter().for_each(|f| walk(&f.ty, &mut written)),
            TypeBody::Alias(target) => walk(target, &mut written),
        }
        DeclVars {
            known: td.params.iter().copied().collect(),
            written,
        }
    }

    /// Where `name` is first written in the body.
    fn first_use(&self, name: Symbol) -> Option<usize> {
        self.written
            .iter()
            .position(|(written, _)| *written == name)
    }
}

impl TypeChecker {
    // ── Register type declarations ──────────────────────────────────

    pub(super) fn register_type_decl(&mut self, td: &TypeDecl, env: &mut TypeEnv) {
        // Reject redefinition of reserved type-system sentinel
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
        // A type-decl whose name shadows a builtin
        // scalar/container type (Int, Float, Bool, String, Unit, List,
        // Map, Set, Channel, Tuple, Fn, Fun, Handle, Bytes,
        // TcpListener, TcpStream — the authoritative list is
        // `BUILTIN_TYPES` in `src/types/builtins.rs`) is rejected at
        // the declaration with one diagnostic: it would overwrite the
        // builtin binding, and what is said of the type's structural
        // traits would go by fields the *builtin* type does not have
        // (a cascade of "unknown method 'x' on type Int" errors with no
        // place). Mirrors the variant-shadow error shape below.
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
        let mut known = DeclVars::of(td);

        // Phase D: type aliases follow a parallel registration path
        // — resolve the target with each param bound to a fresh
        // TyVar, detect cycles, then register into the canonical
        // alias registry. Aliases never appear in `enums` /
        // `records`, have no structural traits of their own (they
        // are the target's), and emit no constructor bindings.
        // Early-return before the stamps at the end of the function.
        if let TypeBody::Alias(target_te) = &td.body {
            self.register_type_alias(td, target_te, &mut param_vars, &mut known);
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
                        .map(|te| self.resolve_declared(td, te, &mut known, &mut param_vars, true))
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
                // (e.g. `type Box(a) { Box(a) }`) because the variant
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
                    };
                    env.define(td.name, scheme);
                }

                self.tables.enums.insert(
                    ty,
                    EnumInfo {
                        params: td.params.clone(),
                        param_var_ids: var_ids,
                        variants: variant_infos,
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
                        let ty =
                            self.resolve_declared(td, &f.ty, &mut known, &mut param_vars, true);
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
                    },
                );

                // Register the record type name as a value so it can be
                // passed to a `type a` parameter, e.g. `json.parse(body, Employee)`.
                // The value is a TYPE DESCRIPTOR at runtime (represented
                // as `Value::TypeDescriptor(name)`), so its type must
                // be `TypeOf(Employee)` rather than `Employee` itself —
                // otherwise the typechecker would let users write things
                // like `Employee.field` or use the descriptor as an
                // instance (as primitive descriptors are).
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

        self.current_type_anno_span = prev_type_span;
    }

    /// The type `te` written in the declaration `td` (`canonical`: with
    /// aliases expanded), with the type variables `known` holds, which
    /// are the declaration's parameters and the variables reported
    /// already.
    ///
    /// Resolving a type adds every lowercase name it meets to
    /// `param_vars`, so a variable the declaration does not declare
    /// after its name (`type Maybe { Some(a) }`) would be one variable
    /// for every use of the type, fixed by the first. Such a variable
    /// is reported, and stands for an error type from then on, so that
    /// the uses of the type say nothing more of it. A record row
    /// (`{x: Int, ...r}`) cannot be declared, so a type declaration
    /// has none.
    fn resolve_declared(
        &mut self,
        td: &TypeDecl,
        te: &TypeExpr,
        vars: &mut DeclVars,
        param_vars: &mut HashMap<Symbol, Type>,
        canonical: bool,
    ) -> Type {
        let resolve_in =
            |checker: &mut Self, param_vars: &mut HashMap<Symbol, Type>| match canonical {
                true => checker.resolve_type_expr(te, param_vars),
                false => checker.resolve_type_expr_inner(te, param_vars),
            };
        let ty = resolve_in(self, param_vars);
        let mut undeclared: Vec<Symbol> = (param_vars.keys())
            .copied()
            .filter(|name| !vars.known.contains(name))
            .collect();
        if undeclared.is_empty() {
            return ty;
        }
        undeclared.sort_by_key(|name| (vars.first_use(*name), resolve(*name)));
        // The parameters the declaration needs, for the help: the
        // declared ones, then each variable of the body in the order of
        // its first use.
        let declared = (td.params.iter().copied())
            .chain(vars.written.iter().map(|(name, _)| *name))
            .filter(|name| !resolve(*name).starts_with("__row__"))
            .filter(|name| {
                self.case_mismatched_type_name(&resolve(*name), false)
                    .is_none()
            })
            .fold(Vec::new(), |mut names, name| {
                if !names.contains(&name) {
                    names.push(name);
                }
                names
            })
            .iter()
            .map(|name| resolve(*name))
            .collect::<Vec<_>>()
            .join(", ");
        for name in undeclared {
            vars.known.insert(name);
            param_vars.insert(name, Type::Error);
            let at = match vars.first_use(name) {
                Some(index) => vars.written[index].1,
                None => te.span,
            };
            let text = resolve(name);
            let diagnostic = match text.strip_prefix("__row__") {
                Some(row) => {
                    let reason = if td.params.iter().any(|p| resolve(*p) == row) {
                        "a row cannot be a type parameter".to_string()
                    } else {
                        format!("'...{row}' would be one row for every use of '{}'", td.name)
                    };
                    Diagnostic::error(
                        Code::InvalidTypeDeclaration,
                        at,
                        format!("a type declaration cannot leave a record open: {reason}"),
                    )
                    .with_help("write the record's fields out")
                }
                None => Diagnostic::error(
                    Code::InvalidTypeDeclaration,
                    at,
                    format!("type variable '{text}' is not a parameter of '{}'", td.name),
                )
                .with_help(format!(
                    "declare it after the type's name: `type {}({declared})`",
                    td.name
                )),
            };
            self.errors.push(diagnostic);
        }
        resolve_in(self, param_vars)
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
        known: &mut DeclVars,
    ) {
        // The alias name itself must already be in `type_aliases` (the
        // pre-pass placeholder loop populated it). Mark it as in-
        // progress so any reference back to this alias inside its own
        // target — direct or indirect — is detected as a cycle.
        let alias = self.own_type(td.name);
        self.tables.type_aliases.insert(alias);
        self.tables.type_alias_arity.insert(alias, td.params.len());

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
        let target_ty = self.resolve_declared(td, target_te, known, param_vars, false);

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
            // When a cycle is detected closing
            // on the alias under registration (`td.name`), every other
            // alias in the cycle chain was registered earlier with a
            // target that points back through the now-known-cyclic
            // path. With `type A = B; type B = A`, A registered first
            // (B not yet visible) with target Generic("B"); B then
            // detects the cycle, and A left in the alias map would make
            // a later `let v: A = 42` report "expected B, got Int"
            // instead of the cycle. Walk the chain (excluding
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
            Type::List(inner) | Type::Set(inner) | Type::Channel(inner) => {
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
