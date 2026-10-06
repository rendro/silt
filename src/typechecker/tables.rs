use super::*;

// ── Type declaration info ───────────────────────────────────────────

/// Information about a declared enum type.
#[derive(Debug, Clone)]
pub(super) struct EnumInfo {
    pub(super) params: Vec<Symbol>,
    /// The actual TyVar ids assigned to each type parameter (same order as `params`).
    pub(super) param_var_ids: Vec<TyVar>,
    pub(super) variants: Vec<VariantInfo>,
    /// Package symbol where this enum was originally declared. Used by
    /// the trait-orphan check in `register_trait_impl` to determine
    /// whether the impl's target type is local to the current package.
    /// The builtin enums (the builtin environment's) carry the
    /// sentinel `intern("__builtin__")`; user enums carry the
    /// `current_package` value at the time their decl was processed,
    /// or `intern("__builtin__")` when there is no enclosing package
    /// (a host module).
    pub(super) defined_in: Symbol,
}

#[derive(Debug, Clone)]
pub(super) struct VariantInfo {
    pub(super) name: Symbol,
    pub(super) field_types: Vec<Type>,
}

/// Information about a declared record type.
#[derive(Debug, Clone)]
pub(super) struct RecordInfo {
    pub(super) fields: Vec<(Symbol, Type)>,
    /// Package symbol where this record was originally declared. See
    /// `EnumInfo::defined_in` for semantics — used by the trait-orphan
    /// check in `register_trait_impl`.
    pub(super) defined_in: Symbol,
}

/// Information about a declared trait.
#[derive(Debug, Clone)]
pub(super) struct TraitInfo {
    /// Type-parameter names on the trait itself (e.g.
    /// `trait TryInto(b)` yields `[b]`). Empty for parameter-less
    /// traits — the common case.
    pub(super) params: Vec<Symbol>,
    /// Fresh TyVars allocated for each trait parameter at
    /// `register_trait_decl`. Stored so impls (and where clauses with
    /// trait args) can substitute their args into the trait's method
    /// signatures via `substitute_vars`. Parallel to `params`.
    pub(super) param_var_ids: Vec<TyVar>,
    /// Bounds declared directly on trait params, e.g.
    /// `trait HashTable(k) where k: Hash`. Each `(param_name, trait_name)`
    /// entry is checked at `register_trait_impl` against the concrete
    /// type the impl supplies for that param.
    pub(super) param_where_clauses: Vec<(Symbol, TraitKey)>,
    /// Supertrait names (e.g. `trait Ordered: Equal` yields `[Equal]`).
    /// Implementing this trait on a type requires every supertrait to also
    /// be implemented for the same type (validated in
    /// `validate_trait_impls`). `expand_with_supertraits` walks this list
    /// transitively to enable supertrait method calls inside `where`
    /// clauses.
    pub(super) supertraits: Vec<TraitKey>,
    /// Parallel to `supertraits`: the TypeExpr args supplied to each
    /// supertrait reference. For `trait Sub(a): Super(a)` the entry for
    /// `Super` is `[TypeExpr::Named("a")]`. Empty when the supertrait
    /// is referenced without args. The `expand_with_supertraits_args`
    /// path uses these to propagate the enclosing trait's args into
    /// the supertrait's `trait_arg_bindings` during where-clause
    /// activation. Arg-less entries keep the bare-name behaviour.
    pub(super) supertrait_args: Vec<Vec<TypeExpr>>,
    /// The methods, each with its type: a complete signature, in which
    /// `Self` is `self_var` and the trait's parameters are
    /// `param_var_ids`. An impl's method has this type, with the impl's
    /// type for `Self` and its trait arguments for the parameters.
    pub(super) methods: Vec<(Symbol, Type)>,
    /// The bounds each method's own `where` clauses put on its type
    /// variables, with the bound's trait arguments. They are part of the
    /// method's signature: in force in its default body and in every
    /// impl's body, and owed by every call.
    pub(super) method_bounds: HashMap<Symbol, Vec<MethodBound>>,
    /// The variable `Self` is in the methods' types.
    pub(super) self_var: TyVar,
    /// The name of each variable the trait's declaration writes: `Self`,
    /// the parameters, and the type variables of the methods'
    /// annotations. Where a body is checked against a method's type (a
    /// default method's, an impl's), the variables are rigid, by these
    /// names.
    pub(super) var_names: Vec<(TyVar, Symbol)>,
    /// Default method bodies declared inside the trait. Maps method name
    /// to the full FnDecl (with body), checked once the trait's module
    /// is. Impls that omit a method whose name appears here are not
    /// "missing method" errors: the method is registered for the impl's
    /// type with the trait's signature, and the checked FnDecl is copied
    /// into the impl's `methods` (`share_default_methods`), where the
    /// compiler compiles it like a method the impl wrote.
    pub(super) default_method_bodies: HashMap<Symbol, FnDecl>,
    /// The module that declares the trait without `pub`, and its name:
    /// the trait's methods can be called only in that module. `None` for
    /// a `pub` trait.
    pub(super) private_to: Option<(crate::session::ModuleId, Symbol)>,
    /// Associated-type declarations on this trait. Each entry carries a
    /// name and the trait bounds the impl-supplied binding must satisfy.
    /// Empty for traits with no associated types (the common case).
    pub(super) assoc_types: Vec<AssocTypeInfo>,
    /// Package symbol where this trait was originally declared. Used
    /// by the trait-orphan check in `register_trait_impl` to determine
    /// whether the impl's trait is local to the current package.
    /// Built-in traits registered through `builtin_trait_decls` carry
    /// the sentinel `intern("__builtin__")`; user traits carry the
    /// `current_package` value at decl-processing time, or
    /// `intern("__builtin__")` when there is no enclosing package
    /// (a check outside a session).
    pub(super) defined_in: Symbol,
}

/// A bound on a type variable of a method: the variable, the trait and
/// the trait's arguments.
pub(super) type MethodBound = (TyVar, TraitKey, Vec<Type>);

/// Information about a single associated-type declaration inside a
/// trait. Bounds are stored as `(trait_name, trait_args)` pairs — the
/// `trait_args` are the AST `TypeExpr`s captured at decl time and
/// resolved against the impl's binding type at impl-registration time.
#[derive(Debug, Clone)]
pub(super) struct AssocTypeInfo {
    pub(super) name: Symbol,
    pub(super) bounds: Vec<(TraitKey, Vec<TypeExpr>)>,
    pub(super) span: Span,
}

/// A registered trait method implementation (new trait system).
#[derive(Debug, Clone)]
pub(crate) struct MethodEntry {
    pub(super) method_type: Type,
    pub(super) span: Span,
    pub(super) is_auto_derived: bool,
    /// GAP (round 17 F3): name of the trait that provided this method.
    /// Used for coherence diagnostics when two distinct traits supply
    /// a method with the same name for the same target type. `None`
    /// for auto-derived entries (Showable on every type, etc.) that
    /// don't participate in user-visible coherence rules.
    pub(super) trait_name: Option<TraitKey>,
    /// Trait constraints that must hold at every call site of this
    /// method. Accumulated from:
    ///
    /// (a) impl-level `where` clauses on the trait-impl header
    ///     (`trait Greet for Box(a) where a: Greet { ... }`) — attached
    ///     to every method in the impl.
    /// (b) method-level `where` clauses on individual impl methods
    ///     (`fn greet(self) -> String where a: Greet { ... }`) — where
    ///     `a` is either an impl-level binder or a method param binder.
    ///
    /// TyVars here live in the same TyVar space as `method_type`, so
    /// `instantiate_method_entry` can substitute both through a shared
    /// mapping. Empty for auto-derived entries and for impls with no
    /// where clauses (which is every impl today prior to this feature).
    ///
    /// The third tuple slot carries the trait args for parameterized
    /// trait bounds — `where a: TryInto(Int)` stores `[Int]` so
    /// `verify_trait_obligation` can reject mismatched impls (e.g.
    /// `TryInto(Float) for ...` against a `where a: TryInto(Int)` bound).
    /// Empty for parameterless traits and the legacy parameterless
    /// where-clause path.
    pub(super) method_constraints: Vec<(TyVar, TraitKey, Vec<Type>)>,
}

/// What the checks of one session share: the type variables, the
/// types, traits and impls every module declares (the builtins' first),
/// and the scheme of every definition a module offers, once the module
/// is checked. A module's own top-level values are in its check's
/// environment while it is checked.
#[derive(Clone, Default)]
pub struct Tables {
    pub(super) vars: TyVarSupply,
    /// Declared enum types.
    pub(super) enums: HashMap<TypeRef, EnumInfo>,
    /// Declared record types.
    pub(super) records: HashMap<TypeRef, RecordInfo>,
    /// Declared traits.
    pub(super) traits: HashMap<TraitKey, TraitInfo>,
    /// Method table: (type, method_name) → method entry. For a type and
    /// name that the impls of two traits provide (two modules' `Show`
    /// for `Int`), it holds the one the module being checked means (see
    /// `TypeChecker::select_visible_methods`).
    pub(super) method_table: HashMap<(TypeRef, Symbol), MethodEntry>,
    /// Every method of a written trait impl, by its type, its name and
    /// its trait.
    pub(super) trait_methods: HashMap<(TypeRef, Symbol, TraitKey), MethodEntry>,
    /// Tracks which (trait_name, type) pairs have been implemented.
    pub(super) trait_impl_set: std::collections::HashSet<(TraitKey, TypeRef)>,
    /// Round 93: `(trait_name, canonical type name)` pairs for which a
    /// user-declared record / enum CANNOT soundly support the built-in
    /// trait because some field / variant payload does not satisfy it
    /// (computed structurally and recursively). Value = full diagnostic
    /// message naming the offending field and its type. Every pair here
    /// had its pre-stamped `trait_impl_set` entry and auto-derived
    /// `method_table` entry removed by `synthesize_auto_derive_impls`.
    /// Consulted by the operator-operand checks in `inference.rs` and to
    /// enrich "unknown method" diagnostics at `.equal()` / `.compare()`
    /// / `.hash()` call sites.
    pub(super) auto_derive_negatives: HashMap<(TraitKey, TypeRef), String>,
    /// GAP-2: Maps `(trait_name, type_name)` → the span of the
    /// `trait T for U { ... }` declaration, so the missing-method
    /// diagnostic in `validate_trait_impls` can point at the impl
    /// block's real source location.
    pub(super) trait_impl_spans: HashMap<(TraitKey, TypeRef), Span>,
    /// Maps `(trait_name, target_head)` → impl-level where-clause
    /// obligations expressed as `(target_arg_index, required_trait,
    /// required_trait_args)` triples. Populated from
    /// `register_trait_impl` so that constraint resolution at call
    /// sites can recursively verify that the concrete type arguments
    /// of the matched impl themselves satisfy the impl's own where
    /// clauses (e.g. `Box(Box(String)): Greet` with
    /// `trait Greet for Box(a) where a: Greet` must reject because
    /// String does not impl Greet, even though `(Greet, Box)` is in
    /// `trait_impl_set`).
    ///
    /// The third tuple slot carries the trait args for parameterized
    /// bounds — `trait Use for List(a) where a: Conv(Int)` stores
    /// `[Int]` so the recursive `verify_trait_obligation` step can
    /// reject `Conv(String) for Int` (a mismatched impl) instead of
    /// silently accepting any `Conv(*) for Int`. Empty for
    /// parameterless trait bounds.
    pub(super) impl_constraints: HashMap<(TraitKey, TypeRef), Vec<(usize, TraitKey, Vec<Type>)>>,
    /// Maps `(trait_name, target_head)` → the resolved trait args supplied
    /// at impl site. For `trait TryInto(Float) for String { ... }` this
    /// stores `(TryInto, String) -> [Float]`. `verify_trait_obligation`
    /// consults this when the where-clause bound also carries trait args
    /// (e.g. `where a: TryInto(Int)`) so that a concrete mismatch (Int vs
    /// Float) is rejected — closing the soundness hole where parameterized-
    /// trait where-clause verification previously ignored trait args.
    /// Absent for parameter-less traits.
    pub(super) impl_trait_args: HashMap<(TraitKey, TypeRef), Vec<Type>>,
    /// Maps `(trait_name, target_head)` → the impl's full (canonicalized)
    /// self type as constructed by `register_trait_impl`. Coherence
    /// guarantees at most one user impl per key. Consulted by
    /// `verify_trait_obligation` AFTER the head-membership check so that
    /// an alias-expanded impl with concrete self-type args (e.g.
    /// `type Bytes2 = List(Int)`; `trait Total for Bytes2` stores
    /// `List(Int)`) rejects obligations on a mismatched instantiation
    /// like `List(String)`. Without this, where-bound verification was
    /// head-keyed only: `(Total, "List")` in `trait_impl_set` satisfied
    /// ANY `List(T)`, and the Int-assuming method body ran on String
    /// elements at runtime. Generic impls (`for List(a)`) store `Var`
    /// args, which the positional comparison treats as wildcards, so
    /// they keep matching every instantiation. Absent for impls that
    /// never pass through `register_trait_impl` (builtin pre-stamps,
    /// auto-derive synthesis) — the check silently skips those.
    pub(super) impl_self_types: HashMap<(TraitKey, TypeRef), Type>,
    /// Maps record type names to their type parameter TyVar ids.
    pub(super) record_param_var_ids: HashMap<TypeRef, Vec<TyVar>>,
    /// Phase D: declared type aliases. Tracks every alias the
    /// typechecker has seen this run so `resolve_type_expr`
    /// knows whether an uppercase identifier should fall through to
    /// the builtins / record-or-enum lookup or be treated as an
    /// alias. The actual alias body lives on
    /// [`crate::types::canonical::Resolver`] (the session-scoped
    /// alias / assoc-binding store the canonicaliser reads); this
    /// set is just a fast-path so the typechecker doesn't have to go
    /// through the resolver on every type-expr resolution.
    pub(super) type_aliases: std::collections::HashSet<TypeRef>,
    /// Phase D: parameter arity of each declared alias, for arity-
    /// error diagnostics at use sites. Parallel to `type_aliases`
    /// (the alias is in `type_aliases` iff it is a key here).
    pub(super) type_alias_arity: HashMap<TypeRef, usize>,
    /// The builtin types whose derived impls the builtin environment
    /// holds already, so a check does not derive them again.
    pub(super) builtin_derived: std::collections::HashSet<TypeRef>,
    /// The session's canonical alias / associated-type-binding
    /// registries. Populated as the typechecker processes user
    /// `type ... = ...` decls and trait impls; consumed by
    /// `canonicalize` and `canonical_head` at every read site, and by
    /// the compiler when it keys impl methods.
    pub(super) resolver: crate::types::canonical::Resolver,
    /// The scheme of each definition of a checked module: a function, a
    /// `let`, a variant's constructor, a type written as a value. (A
    /// builtin's is the builtin scope's.)
    pub(super) schemes: HashMap<crate::defs::DefId, Scheme>,
    /// What each module's check added to the tables, so that it can be
    /// forgotten when the module is checked again.
    pub(super) rows: HashMap<crate::session::ModuleId, Rows>,
    /// The name of each module checked, to tell two types of one name
    /// apart in a message (`a.Pt`, `b.Pt`).
    pub(super) module_names: HashMap<crate::session::ModuleId, Symbol>,
}

/// What one module's check added to the session's [`Tables`].
#[derive(Clone, Default)]
pub struct Rows {
    /// The earlier modules' variables the check bound.
    trail: Vec<TyVar>,
    types: Vec<TypeRef>,
    traits: Vec<TraitKey>,
    methods: Vec<(TypeRef, Symbol)>,
    trait_methods: Vec<(TypeRef, Symbol, TraitKey)>,
    impls: Vec<(TraitKey, TypeRef)>,
    schemes: Vec<crate::defs::DefId>,
}

/// The keys of the session's tables, to tell what a check added.
pub struct TableKeys {
    types: std::collections::HashSet<TypeRef>,
    traits: std::collections::HashSet<TraitKey>,
    methods: std::collections::HashSet<(TypeRef, Symbol)>,
    trait_methods: std::collections::HashSet<(TypeRef, Symbol, TraitKey)>,
    impls: std::collections::HashSet<(TraitKey, TypeRef)>,
    schemes: std::collections::HashSet<crate::defs::DefId>,
}

impl Tables {
    /// The tables a session starts from: the builtins'.
    pub fn for_session() -> Tables {
        builtin_env().tables.clone()
    }

    /// The records of the tables: each record type with its fields.
    pub fn record_fields(&self) -> HashMap<TypeRef, Vec<(Symbol, Type)>> {
        self.records
            .iter()
            .map(|(ty, info)| (*ty, info.fields.clone()))
            .collect()
    }

    /// Every method a value has, as (the type impls key the value's type
    /// by, the method's name): declared, derived and builtin.
    pub fn methods(&self) -> Vec<(TypeRef, Symbol)> {
        self.method_table.keys().copied().collect()
    }

    /// The type aliases and associated-type bindings of the session.
    pub fn resolver(&self) -> &crate::types::canonical::Resolver {
        &self.resolver
    }

    pub(super) fn keys(&self) -> TableKeys {
        TableKeys {
            types: self
                .enums
                .keys()
                .chain(self.records.keys())
                .chain(self.type_aliases.iter())
                .copied()
                .collect(),
            traits: self.traits.keys().copied().collect(),
            methods: self.method_table.keys().copied().collect(),
            trait_methods: self.trait_methods.keys().copied().collect(),
            impls: self.trait_impl_set.iter().copied().collect(),
            schemes: self.schemes.keys().copied().collect(),
        }
    }

    pub(super) fn added_since(&self, before: &TableKeys) -> Rows {
        let after = self.keys();
        Rows {
            trail: self.vars.trail.clone(),
            types: after.types.difference(&before.types).copied().collect(),
            traits: after.traits.difference(&before.traits).copied().collect(),
            methods: after.methods.difference(&before.methods).copied().collect(),
            trait_methods: after
                .trait_methods
                .difference(&before.trait_methods)
                .copied()
                .collect(),
            impls: after.impls.difference(&before.impls).copied().collect(),
            schemes: after.schemes.difference(&before.schemes).copied().collect(),
        }
    }

    /// Forget what the check of `module` added: it is checked again, or
    /// it was a REPL cell that is dropped.
    pub fn forget(&mut self, module: crate::session::ModuleId) {
        let Some(rows) = self.rows.remove(&module) else {
            return;
        };
        self.vars.forget(module, &rows.trail);
        for ty in rows.types {
            self.enums.remove(&ty);
            self.records.remove(&ty);
            self.record_param_var_ids.remove(&ty);
            self.type_aliases.remove(&ty);
            self.type_alias_arity.remove(&ty);
            self.resolver.unregister_alias(ty);
        }
        for t in rows.traits {
            self.traits.remove(&t);
        }
        for key in rows.methods {
            self.method_table.remove(&key);
        }
        for key in rows.trait_methods {
            self.trait_methods.remove(&key);
        }
        for key in rows.impls {
            self.trait_impl_set.remove(&key);
            self.trait_impl_spans.remove(&key);
            self.impl_constraints.remove(&key);
            self.impl_trait_args.remove(&key);
            self.impl_self_types.remove(&key);
            self.auto_derive_negatives.remove(&key);
        }
        for id in rows.schemes {
            self.schemes.remove(&id);
        }
    }
}
