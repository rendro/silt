use super::*;

// ── Type declaration info ───────────────────────────────────────────

/// Information about a declared enum type.
#[derive(Debug, Clone)]
pub(super) struct EnumInfo {
    pub(super) params: Vec<Symbol>,
    /// The actual TyVar ids assigned to each type parameter (same order as `params`).
    pub(super) param_var_ids: Vec<TyVar>,
    pub(super) variants: Vec<VariantInfo>,
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
    /// is referenced without args. `declare_bound` gives a variable
    /// bounded by this trait the supertrait's bound at these arguments.
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
    pub(super) method_bounds: HashMap<Symbol, Vec<Pred>>,
    /// The variable `Self` is in the methods' types.
    pub(super) self_var: TyVar,
    /// The name of each variable the trait's declaration writes: `Self`,
    /// the parameters, and the type variables of the methods'
    /// annotations. Where a body is checked against a method's type (a
    /// default method's, an impl's), the variables are rigid, by these
    /// names.
    pub(super) var_names: Vec<(TyVar, Symbol)>,
    /// Default method bodies declared inside the trait. Maps method name
    /// to the FnDecl as written. Impls that omit a method whose name
    /// appears here are not "missing method" errors: the method is
    /// registered for the impl's type with the trait's signature, and
    /// the impl's method is the trait's body, which is checked once
    /// with the trait and compiled once.
    pub(super) default_method_bodies: HashMap<Symbol, FnDecl>,
    /// The module that declares the trait without `pub`, and its name:
    /// the trait's methods can be called only in that module. `None` for
    /// a `pub` trait.
    pub(super) private_to: Option<(crate::session::ModuleId, Symbol)>,
    /// Associated-type declarations on this trait. Each entry carries a
    /// name and the trait bounds the impl-supplied binding must satisfy.
    /// Empty for traits with no associated types (the common case).
    pub(super) assoc_types: Vec<AssocTypeInfo>,
}

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

/// A method a type has: an impl's (a row of [`ImplMethods`]), or a
/// structural trait's (`structural`), which is no row: every declared
/// type has those four names, and whether it has the trait is the
/// structural judgement's to say (`TypeChecker::method_entry`).
#[derive(Debug, Clone)]
pub(crate) struct MethodEntry {
    pub(super) method_type: Type,
    pub(super) structural: bool,
    /// The trait whose impl provides the method. `None` for a
    /// structural trait's method (`structural`), which is no impl's.
    pub(super) trait_name: Option<TraitKey>,
    /// What every use of the method owes, on the variables of
    /// `method_type`: the `where` clauses of the impl's header
    /// (`trait Greet for Box(a) where a: Greet`) and the bounds the
    /// trait declares for the method. None for a structural trait's.
    pub(super) preds: Vec<Pred>,
}

/// A table of the session (a map or a set, `M`, with keys `K`) that
/// notes each row when it is entered: `take_added` gives the keys
/// entered since it was last asked that are still in the table. Read
/// like the map or set it holds; `insert` and `entry` are its own.
///
/// A row belongs to the module whose check entered it, and
/// [`Tables::forget`] removes exactly those: no check writes over a row
/// of another module (`overwritten` counts the attempts, which are a
/// fault of the checker; the earlier row is kept).
#[derive(Clone)]
pub(super) struct Table<M, K> {
    rows: M,
    added: Vec<K>,
    /// The keys of `added`, to ask whether a row is the check's own.
    own: std::collections::HashSet<K>,
    overwritten: usize,
}

impl<M: Default, K> Default for Table<M, K> {
    fn default() -> Self {
        Table {
            rows: M::default(),
            added: Vec::new(),
            own: std::collections::HashSet::new(),
            overwritten: 0,
        }
    }
}

impl<M, K> std::ops::Deref for Table<M, K> {
    type Target = M;
    fn deref(&self) -> &M {
        &self.rows
    }
}

/// (For `get_mut`, `remove` and `retain`: what enters a row goes
/// through `insert` or `entry` below, which a call finds first.)
impl<M, K> std::ops::DerefMut for Table<M, K> {
    fn deref_mut(&mut self) -> &mut M {
        &mut self.rows
    }
}

impl<'a, M, K> IntoIterator for &'a Table<M, K>
where
    &'a M: IntoIterator,
{
    type Item = <&'a M as IntoIterator>::Item;
    type IntoIter = <&'a M as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        (&self.rows).into_iter()
    }
}

impl<K: Copy + Eq + std::hash::Hash, V> Table<HashMap<K, V>, K> {
    /// Enter the row, or write the check's own row again. A row of
    /// another module's check stays as it is.
    pub(super) fn insert(&mut self, key: K, value: V) -> Option<V> {
        if self.rows.contains_key(&key) && !self.own.contains(&key) {
            self.overwritten += 1;
            return Some(value);
        }
        let before = self.rows.insert(key, value);
        if before.is_none() {
            self.added.push(key);
            self.own.insert(key);
        }
        before
    }

    pub(super) fn entry(&mut self, key: K) -> std::collections::hash_map::Entry<'_, K, V> {
        if !self.rows.contains_key(&key) {
            self.added.push(key);
            self.own.insert(key);
        }
        self.rows.entry(key)
    }

    fn take_added(&mut self) -> Vec<K> {
        let mut added = std::mem::take(&mut self.added);
        self.own.clear();
        added.retain(|key| self.rows.contains_key(key));
        added
    }

    /// How often the check under way tried to write over a row that is
    /// not its own, since this was last asked.
    fn take_overwritten(&mut self) -> usize {
        std::mem::take(&mut self.overwritten)
    }
}

impl<K: Copy + Eq + std::hash::Hash> Table<std::collections::HashSet<K>, K> {
    pub(super) fn insert(&mut self, key: K) -> bool {
        let new = self.rows.insert(key);
        if new {
            self.added.push(key);
            self.own.insert(key);
        }
        new
    }

    /// The keys entered so far by the check under way, in the order
    /// they were entered.
    pub(super) fn added(&self) -> impl Iterator<Item = K> + '_ {
        self.added
            .iter()
            .copied()
            .filter(|key| self.rows.contains(key))
    }

    fn take_added(&mut self) -> Vec<K> {
        let mut added = std::mem::take(&mut self.added);
        self.own.clear();
        added.retain(|key| self.rows.contains(key));
        added
    }
}

/// The structural traits, each with its method.
pub(super) const STRUCTURAL_METHODS: [(&str, &str); 4] = [
    ("Display", "display"),
    ("Equal", "equal"),
    ("Compare", "compare"),
    ("Hash", "hash"),
];

/// The key of an impl's method: the type the impl is for, the method's
/// name, the impl's trait.
pub(super) type ImplMethodKey = (TypeRef, Symbol, TraitKey);

/// The methods of the session's impls: one row for each method of each
/// impl, by its type, its name and its trait, entered by the module that
/// writes the impl. The impls of two traits that each have a method `m`
/// for one type are two rows; what `x.m()` means in a module is decided
/// from the rows each time (`TypeChecker::method_entry`), never written
/// back. `providers` is an index of the rows, kept with them.
#[derive(Clone, Default)]
pub(super) struct ImplMethods {
    rows: Table<HashMap<ImplMethodKey, MethodEntry>, ImplMethodKey>,
    /// For a type and a method name, the traits whose impls have rows.
    by_name: HashMap<(TypeRef, Symbol), Vec<TraitKey>>,
}

impl ImplMethods {
    /// Enter the method `method` of the impl of `tr` for `ty`.
    pub(super) fn insert(&mut self, ty: TypeRef, method: Symbol, tr: TraitKey, entry: MethodEntry) {
        if self.rows.insert((ty, method, tr), entry).is_none() {
            self.by_name.entry((ty, method)).or_default().push(tr);
        }
    }

    pub(super) fn get(&self, ty: TypeRef, method: Symbol, tr: TraitKey) -> Option<&MethodEntry> {
        self.rows.get(&(ty, method, tr))
    }

    /// The traits whose impls give the type `ty` a method `method`.
    pub(super) fn providers(&self, ty: TypeRef, method: Symbol) -> &[TraitKey] {
        self.by_name
            .get(&(ty, method))
            .map_or(&[], |traits| traits.as_slice())
    }

    /// Every row's key.
    pub(super) fn keys(&self) -> impl Iterator<Item = &ImplMethodKey> {
        self.rows.keys()
    }

    fn remove(&mut self, key: &ImplMethodKey) {
        if self.rows.remove(key).is_none() {
            return;
        }
        let name = (key.0, key.1);
        if let Some(traits) = self.by_name.get_mut(&name) {
            traits.retain(|tr| *tr != key.2);
            if traits.is_empty() {
                self.by_name.remove(&name);
            }
        }
    }
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
    pub(super) enums: Table<HashMap<TypeRef, EnumInfo>, TypeRef>,
    /// Declared record types.
    pub(super) records: Table<HashMap<TypeRef, RecordInfo>, TypeRef>,
    /// Declared traits.
    pub(super) traits: Table<HashMap<TraitKey, TraitInfo>, TraitKey>,
    /// The methods of every impl, by type, name and trait.
    pub(super) impl_methods: ImplMethods,
    /// Where each module reported that a type has no method of some
    /// name: the place, the type, the name. Once every module of a
    /// program is checked, a trait out of the module's reach that has
    /// such a method is named there ([`out_of_reach_helps`]).
    pub(super) unknown_methods:
        HashMap<crate::session::ModuleId, Vec<(crate::source::Span, TypeRef, Symbol)>>,
    /// Tracks which (trait_name, type) pairs have been implemented.
    pub(super) trait_impl_set:
        Table<std::collections::HashSet<(TraitKey, TypeRef)>, (TraitKey, TypeRef)>,
    /// GAP-2: Maps `(trait_name, type_name)` → the span of the
    /// `trait T for U { ... }` declaration, so the missing-method
    /// diagnostic in `validate_trait_impls` can point at the impl
    /// block's real source location.
    pub(super) trait_impl_spans: HashMap<(TraitKey, TypeRef), Span>,
    /// What each impl's header asks of its type variables
    /// (`trait Greet for Box(a) where a: Greet`), on the variables of the
    /// impl's self type (`impl_self_types`): a use that the impl answers
    /// owes them at the subject's parts (`verify_trait_obligation`).
    pub(super) impl_preds: HashMap<(TraitKey, TypeRef), Vec<Pred>>,
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
    /// never pass through `register_trait_impl` (the stamps of the
    /// structural traits) — the check silently skips those.
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
    pub(super) type_aliases: Table<std::collections::HashSet<TypeRef>, TypeRef>,
    /// Phase D: parameter arity of each declared alias, for arity-
    /// error diagnostics at use sites. Parallel to `type_aliases`
    /// (the alias is in `type_aliases` iff it is a key here).
    pub(super) type_alias_arity: HashMap<TypeRef, usize>,
    /// The session's canonical alias / associated-type-binding
    /// registries. Populated as the typechecker processes user
    /// `type ... = ...` decls and trait impls; consumed by
    /// `canonicalize` and `canonical_head` at every read site, and by
    /// the compiler when it keys impl methods.
    pub(super) resolver: crate::types::canonical::Resolver,
    /// The scheme of each definition of a checked module: a function, a
    /// `let`, a variant's constructor, a type written as a value. (A
    /// builtin's is the builtin scope's.)
    pub(super) schemes: Table<HashMap<crate::defs::DefId, Scheme>, crate::defs::DefId>,
    /// The parameters of a function (or of the closure a top-level
    /// `let` holds) whose function type returns `()` because the body
    /// calls them as a statement: the parameter's index and the
    /// statement. A module that calls the function of another names the
    /// statement too.
    pub(super) statement_units: HashMap<crate::defs::DefId, Vec<(usize, Span)>>,
    /// What the check of each REPL cell left waiting for the type of a
    /// `let` that a later cell may decide (see [`Waiting`]).
    pub(super) waiting: HashMap<crate::session::ModuleId, Waiting>,
    /// What each module's check added to the tables, so that it can be
    /// forgotten when the module is checked again.
    pub(super) rows: HashMap<crate::session::ModuleId, Rows>,
    /// The name of each module checked, to tell two types of one name
    /// apart in a message (`a.Pt`, `b.Pt`).
    pub(super) module_names: HashMap<crate::session::ModuleId, Symbol>,
}

/// The checks a REPL cell made on a type still unknown when its check
/// ended: the type of a `let` of the session that is not generalised
/// (`let ch = channel.new(4)`). The cell that decides the type is held
/// to them.
#[derive(Clone, Default)]
pub(super) struct Waiting {
    /// The cell's functions and `let`s. A check in the body of one
    /// waits for as long as something can still run that body.
    pub(super) decls: Vec<CellDecl>,
    pub(super) wanted: Vec<super::solve::Wanted>,
}

/// A function or a `let` of a REPL cell.
#[derive(Clone)]
pub(super) struct CellDecl {
    /// Where it is, from its first token to the end of its body.
    pub(super) span: Span,
    /// What it defines.
    pub(super) defs: Vec<crate::defs::DefId>,
    /// The definitions its body refers to: a cell is bound to what its
    /// names meant when it was entered, so a function an earlier cell
    /// defined runs for as long as one that refers to it does.
    pub(super) refers: Vec<crate::defs::DefId>,
}

/// What one module's check added to the session's [`Tables`].
#[derive(Clone, Default)]
pub struct Rows {
    /// The earlier modules' variables the check bound.
    trail: Vec<TyVar>,
    types: Vec<TypeRef>,
    traits: Vec<TraitKey>,
    impl_methods: Vec<ImplMethodKey>,
    impls: Vec<(TraitKey, TypeRef)>,
    schemes: Vec<crate::defs::DefId>,
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
    /// by, the method's name): those of the impls, and those of the
    /// structural traits (of a builtin type, the traits it is stamped
    /// with; of a type a program declares, all four names).
    pub fn methods(&self) -> Vec<(TypeRef, Symbol)> {
        let mut methods: std::collections::HashSet<(TypeRef, Symbol)> = self
            .impl_methods
            .keys()
            .map(|(ty, method, _)| (*ty, *method))
            .collect();
        let declared = self
            .records
            .keys()
            .chain(self.enums.keys())
            .filter(|ty| builtin_type_name(**ty).is_none());
        for (name, method) in STRUCTURAL_METHODS {
            let tr = TraitKey::builtin(name);
            let method = intern(method);
            methods.extend(
                self.trait_impl_set
                    .iter()
                    .filter(|(t, _)| *t == tr)
                    .map(|(_, ty)| (*ty, method)),
            );
            methods.extend(declared.clone().map(|ty| (*ty, method)));
        }
        methods.into_iter().collect()
    }

    /// Whether a value of the type `ty` has the method of the structural
    /// trait `tr` as a name: a builtin type when it is stamped with the
    /// trait, a type a program declares always (whether it has the
    /// trait is the structural judgement's to say).
    pub(super) fn has_structural_name(&self, tr: TraitKey, ty: TypeRef) -> bool {
        match builtin_type_name(ty) {
            Some(_) => self.trait_impl_set.contains(&(tr, ty)),
            None => self.records.contains_key(&ty) || self.enums.contains_key(&ty),
        }
    }

    /// Every row of every table, as text in a fixed order: for a test
    /// that compares two states of a session (what an entry that failed
    /// leaves behind is nothing).
    #[doc(hidden)]
    pub fn fingerprint(&self) -> Vec<String> {
        fn keys<K: std::fmt::Debug>(table: &str, keys: impl Iterator<Item = K>) -> Vec<String> {
            keys.map(|key| format!("{table} {key:?}")).collect()
        }
        let mut rows = Vec::new();
        rows.extend(keys("enum", self.enums.keys()));
        rows.extend(keys("record", self.records.keys()));
        rows.extend(keys("record-params", self.record_param_var_ids.keys()));
        rows.extend(keys("alias", self.type_aliases.iter()));
        rows.extend(keys("alias-arity", self.type_alias_arity.keys()));
        rows.extend(keys("trait", self.traits.keys()));
        rows.extend(keys("impl", self.trait_impl_set.iter()));
        rows.extend(keys("impl-span", self.trait_impl_spans.keys()));
        rows.extend(keys("impl-preds", self.impl_preds.keys()));
        rows.extend(keys("impl-args", self.impl_trait_args.keys()));
        rows.extend(keys("impl-self", self.impl_self_types.keys()));
        rows.extend(keys("method", self.impl_methods.keys()));
        rows.extend(keys(
            "method-index",
            self.impl_methods
                .by_name
                .iter()
                .flat_map(|(name, traits)| traits.iter().map(move |tr| (name, tr))),
        ));
        rows.extend(keys("scheme", self.schemes.keys()));
        rows.extend(keys("statement-units", self.statement_units.keys()));
        rows.extend(keys("unknown-methods", self.unknown_methods.keys()));
        rows.extend(keys("waiting", self.waiting.keys()));
        rows.extend(keys("rows-of", self.rows.keys()));
        rows.extend(keys(
            "bound-variable",
            self.vars
                .subst
                .iter()
                .enumerate()
                .filter(|(_, bound)| bound.is_some())
                .map(|(var, _)| var),
        ));
        rows.extend(self.resolver.fingerprint());
        rows.sort();
        rows
    }

    /// How often the check under way tried to write over a row of
    /// another module, since this was last asked: a fault of the
    /// checker, reported as one.
    pub(super) fn take_overwritten(&mut self) -> usize {
        self.enums.take_overwritten()
            + self.records.take_overwritten()
            + self.traits.take_overwritten()
            + self.schemes.take_overwritten()
            + self.impl_methods.rows.take_overwritten()
    }

    /// The type aliases and associated-type bindings of the session.
    pub fn resolver(&self) -> &crate::types::canonical::Resolver {
        &self.resolver
    }

    /// Start the account of what a module's check adds: the rows
    /// entered from here on are the module's (`take_rows`).
    pub(super) fn begin_rows(&mut self) {
        self.take_rows();
    }

    /// The rows entered since `begin_rows`, and still there. Each table
    /// notes a row when it is entered, so the account costs what the
    /// module adds, not what the session holds.
    pub(super) fn take_rows(&mut self) -> Rows {
        let mut types = self.enums.take_added();
        types.extend(self.records.take_added());
        types.extend(self.type_aliases.take_added());
        Rows {
            trail: self.vars.trail.clone(),
            types,
            traits: self.traits.take_added(),
            impl_methods: self.impl_methods.rows.take_added(),
            impls: self.trait_impl_set.take_added(),
            schemes: self.schemes.take_added(),
        }
    }

    /// Forget what the check of `module` added: it is checked again, or
    /// it was a REPL cell that is dropped.
    pub fn forget(&mut self, module: crate::session::ModuleId) {
        self.waiting.remove(&module);
        self.unknown_methods.remove(&module);
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
        for key in rows.impl_methods {
            self.impl_methods.remove(&key);
        }
        for key in rows.impls {
            self.trait_impl_set.remove(&key);
            self.trait_impl_spans.remove(&key);
            self.impl_preds.remove(&key);
            self.impl_trait_args.remove(&key);
            self.impl_self_types.remove(&key);
            self.resolver.unregister_assoc_bindings(key.0, key.1);
        }
        for id in rows.schemes {
            self.schemes.remove(&id);
            self.statement_units.remove(&id);
        }
    }
}
