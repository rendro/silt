use super::*;

// ── The builtin environment ─────────────────────────────────────────

/// What every check starts from: the checker and the environment once
/// the builtin functions, types, traits and derived impls are registered.
/// Built once per thread and shared by every check made on it (see
/// [`builtin_env`]).
pub(super) struct BuiltinEnv {
    /// A checker with no tables: what each check starts from.
    checker: TypeChecker,
    /// The builtin types, traits, impls and type variables: what a
    /// session's tables start from.
    pub(super) tables: Tables,
    /// The scope of the builtin names: the parent of every program's
    /// top-level scope.
    root: Rc<TypeEnv>,
    /// The derived impls of the builtin types (`Display`, `Equal`, ...
    /// for `IoError`, `Weekday`, ...), checked: a program compiles them
    /// once, see [`builtin_derived_impls`].
    impls: Rc<Vec<Decl>>,
}

impl BuiltinEnv {
    fn build() -> Self {
        let mut checker = TypeChecker::new();
        let mut env = TypeEnv::new();
        // With no `current_package`, every builtin decl is stamped with
        // the `__builtin__` sentinel by `defining_package()`, which the
        // orphan rule relies on: `trait Display for List(a)` in user code
        // must not look trait-local.
        checker.register_builtins(&mut env);
        register_builtin_trait_impls(&mut checker);
        // Each builtin variant is bound as `Enum.Variant` too, which is
        // what a resolved use of it reads.
        let mut variants: Vec<(Symbol, Scheme)> = Vec::new();
        for (enum_name, info) in &checker.tables.enums {
            for variant in &info.variants {
                if let Some(scheme) = env.lookup(variant.name) {
                    variants.push((
                        intern(&format!("{enum_name}.{}", variant.name)),
                        scheme.clone(),
                    ));
                }
            }
        }
        for (name, scheme) in variants {
            env.define(name, scheme);
        }
        // Every builtin scheme is generalized, so the bodies below are
        // checked without walking the builtin scope for free variables.
        env.closed = env.free_vars(&checker).is_empty();
        // Derive the builtin types' impls once, as a check of a program
        // with no declarations would, and check their bodies. They are
        // registered in a scope over the builtin one, so the scopes their
        // bodies open share the builtin scope instead of copying it, and
        // what they bind is then moved into the builtin scope.
        let root = Rc::new(env);
        let mut scope = TypeEnv::child_of(root.clone());
        let mut impls = Vec::new();
        checker.synthesize_auto_derive_impls(&mut impls);
        for decl in &impls {
            if let Decl::TraitImpl(ti) = decl {
                if let Some(target) = checker.impl_target(ti) {
                    checker.tables.builtin_derived.insert(target);
                }
                checker.register_trait_impl(ti);
            }
        }
        checker.check_decl_bodies(&mut impls, &mut scope);
        checker.finalize_deferred_checks();
        debug_assert!(
            checker.errors.is_empty(),
            "the builtin derived impls check: {:?}",
            checker.errors
        );
        checker.errors.clear();
        let bindings = std::mem::take(&mut scope.bindings);
        drop(scope);
        let mut env = Rc::try_unwrap(root).expect("no scope over the builtin scope is left");
        env.bindings.extend(bindings);
        env.closed = false;
        env.closed = env.free_vars(&checker).is_empty();
        let tables = std::mem::take(&mut checker.tables);
        BuiltinEnv {
            checker,
            tables,
            root: Rc::new(env),
            impls: Rc::new(impls),
        }
    }

    /// A fresh checker, with no tables, and an empty top-level scope over
    /// the builtins.
    pub(super) fn start(&self) -> (TypeChecker, TypeEnv) {
        (self.checker.clone(), TypeEnv::child_of(self.root.clone()))
    }
}

thread_local! {
    /// The builtin environment of this thread, with the interner
    /// generation it was built in. Symbols are per thread, and
    /// `intern::reset` invalidates them, so the cache is too.
    static BUILTIN_ENV: std::cell::RefCell<Option<(u64, Rc<BuiltinEnv>)>> =
        const { std::cell::RefCell::new(None) };
}

/// The derived impls of the builtin types, checked. Every program
/// compiles them once (`Compiler::compile_program`), so a method call on
/// a builtin type's value finds its method.
pub fn builtin_derived_impls() -> Rc<Vec<Decl>> {
    builtin_env().impls.clone()
}

/// Whether the builtin scope binds `name` (`int.parse`).
pub(super) fn builtin_env_has(name: Symbol) -> bool {
    builtin_env().root.bindings.contains_key(&name)
}

/// The builtin environment, built on first use and after each
/// `intern::reset`.
pub(super) fn builtin_env() -> Rc<BuiltinEnv> {
    let generation = crate::intern::generation();
    if let Some(env) = BUILTIN_ENV.with(|cell| match &*cell.borrow() {
        Some((built_in, env)) if *built_in == generation => Some(env.clone()),
        _ => None,
    }) {
        return env;
    }
    let env = Rc::new(BuiltinEnv::build());
    BUILTIN_ENV.with(|cell| *cell.borrow_mut() = Some((generation, env.clone())));
    env
}

/// The scheme of the builtin definition `def`: the one the builtin scope
/// binds under its name (`println`, `list.map`, `Weekday.Monday`,
/// `Option`).
pub(super) fn builtin_scheme(def: &crate::defs::Def) -> Option<Scheme> {
    let key = match def.kind {
        crate::defs::DefKind::Variant { ty, .. } => {
            let enum_name = names::builtin_def(ty.0)?.name;
            format!("{enum_name}.{}", def.name)
        }
        _ => match def.module.builtin_name() {
            Some(module) if !def.is_type() => format!("{module}.{}", def.name),
            _ => resolve(def.name),
        },
    };
    builtin_env().root.lookup(intern(&key)).cloned()
}

/// The builtin names, as the resolver enters them: every name the builtin
/// scope binds, each builtin enum with the arity of each variant, and the
/// builtin traits.
pub(super) struct BuiltinNames {
    pub bindings: Vec<Symbol>,
    pub enums: Vec<(Symbol, Vec<(Symbol, usize)>)>,
    pub traits: Vec<Symbol>,
}

pub(super) fn builtin_names() -> BuiltinNames {
    let env = builtin_env();
    let mut bindings: Vec<Symbol> = env.root.bindings.keys().copied().collect();
    bindings.sort_by_key(|name| resolve(*name));
    let mut enums: Vec<(Symbol, Vec<(Symbol, usize)>)> = env
        .tables
        .enums
        .iter()
        .map(|(name, info)| {
            let variants = info
                .variants
                .iter()
                .map(|v| (v.name, v.field_types.len()))
                .collect();
            (name.name, variants)
        })
        .collect();
    enums.sort_by_key(|(name, _)| resolve(*name));
    let mut traits: Vec<Symbol> = env.tables.traits.keys().map(|t| t.name).collect();
    traits.sort_by_key(|name| resolve(*name));
    BuiltinNames {
        bindings,
        enums,
        traits,
    }
}

/// Return a map of builtin qualified names to their type signature strings.
/// Used by the LSP to show type info in completions.
pub fn builtin_type_signatures() -> std::collections::HashMap<String, String> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut sigs = std::collections::HashMap::new();
    for (name, scheme) in &env.bindings {
        let name_str = resolve(*name);
        if name_str.contains('.') {
            let ty = checker.instantiate(scheme);
            sigs.insert(name_str, format!("{ty}"));
        }
    }
    sigs
}

/// Snapshot every nominal record / enum name registered by
/// `register_builtins`. Used by the round-82 parity test in
/// `tests/typecheck/round82_stdlib_types_registry_tests.rs` to lock the central
/// registry (`module::BUILTIN_STDLIB_TYPE_NAMES`) against runtime
/// state. Routes through a fresh `TypeChecker` so the snapshot reflects
/// every per-module `register` callback's effect on `checker.tables.records`
/// / `checker.tables.enums` — including the `Result`/`Option`/`Step`/
/// `ChannelResult`/`ChannelOp` prelude enums declared directly in
/// `register_builtins` itself.
///
/// Each entry's category (`"record"` vs `"enum"`) is preserved so the
/// test can render a useful diff when the sets diverge.
pub fn registered_builtin_type_names() -> Vec<(String, &'static str)> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for ty in checker.tables.records.keys() {
        out.push((resolve(ty.name), "record"));
    }
    for ty in checker.tables.enums.keys() {
        out.push((resolve(ty.name), "enum"));
    }
    out.sort();
    out
}

/// Return a map of builtin qualified names to their parameter-name lists,
/// indexed in argument order. Sibling registry to
/// `builtin_type_signatures`: signatures carry only types (the rendered
/// `Fn(T1, T2) -> R` form has no `name:` per param), so the LSP
/// `signatureHelp` handler — which needs `ParameterInformation` per
/// arg to drive active-arg highlighting — has nowhere else to look up
/// names.
///
/// Round-71 DX-4 fix (audit): pre-round, `signature_help.rs` emitted
/// `parameters: vec![]` for every builtin call site, so the active-arg
/// highlight was broken across the entire stdlib surface. This
/// registry seeds names for the most-used `list.*`, `string.*`,
/// `map.*`, `set.*`, `io.*` modules. Builtins not present here surface
/// as before with empty parameter info — a follow-up round can extend
/// the coverage.
///
/// Names are deliberately compact (`xs`, `f`, `k`, `v`, `s`, `path`)
/// to mirror the doc comments in `src/typechecker/builtins/*.rs`. A
/// follow-up audit can normalize wording.
pub fn builtin_param_names() -> std::collections::HashMap<&'static str, &'static [&'static str]> {
    let entries: &[(&'static str, &'static [&'static str])] = &[
        // ── list.* ───────────────────────────────────────────────
        ("list.map", &["xs", "f"]),
        ("list.filter", &["xs", "pred"]),
        ("list.fold", &["xs", "init", "f"]),
        ("list.each", &["xs", "f"]),
        ("list.find", &["xs", "pred"]),
        ("list.zip", &["xs", "ys"]),
        ("list.flatten", &["xs"]),
        ("list.sort_by", &["xs", "key"]),
        ("list.flat_map", &["xs", "f"]),
        ("list.filter_map", &["xs", "f"]),
        ("list.any", &["xs", "pred"]),
        ("list.all", &["xs", "pred"]),
        ("list.fold_until", &["xs", "init", "f"]),
        ("list.unfold", &["seed", "f"]),
        ("list.append", &["xs", "x"]),
        ("list.prepend", &["xs", "x"]),
        ("list.concat", &["xs", "ys"]),
        ("list.get", &["xs", "i"]),
        ("list.set", &["xs", "i", "x"]),
        ("list.take", &["xs", "n"]),
        ("list.drop", &["xs", "n"]),
        ("list.enumerate", &["xs"]),
        ("list.head", &["xs"]),
        ("list.tail", &["xs"]),
        ("list.last", &["xs"]),
        ("list.reverse", &["xs"]),
        ("list.sort", &["xs"]),
        ("list.unique", &["xs"]),
        ("list.contains", &["xs", "x"]),
        ("list.length", &["xs"]),
        ("list.group_by", &["xs", "key"]),
        ("list.index_of", &["xs", "x"]),
        ("list.remove_at", &["xs", "i"]),
        ("list.min_by", &["xs", "key"]),
        ("list.max_by", &["xs", "key"]),
        ("list.sum", &["xs"]),
        ("list.sum_float", &["xs"]),
        ("list.product", &["xs"]),
        ("list.product_float", &["xs"]),
        ("list.scan", &["xs", "init", "f"]),
        ("list.intersperse", &["xs", "sep"]),
        // ── string.* ─────────────────────────────────────────────
        ("string.from", &["x"]),
        ("string.split", &["s", "sep"]),
        ("string.join", &["xs", "sep"]),
        ("string.trim", &["s"]),
        ("string.trim_start", &["s"]),
        ("string.trim_end", &["s"]),
        ("string.char_code", &["s"]),
        ("string.from_char_code", &["code"]),
        ("string.contains", &["s", "needle"]),
        ("string.replace", &["s", "from", "to"]),
        ("string.length", &["s"]),
        ("string.byte_length", &["s"]),
        ("string.to_upper", &["s"]),
        ("string.to_lower", &["s"]),
        ("string.starts_with", &["s", "prefix"]),
        ("string.ends_with", &["s", "suffix"]),
        ("string.chars", &["s"]),
        ("string.repeat", &["s", "n"]),
        ("string.index_of", &["s", "needle"]),
        ("string.last_index_of", &["s", "needle"]),
        ("string.split_at", &["s", "i"]),
        ("string.lines", &["s"]),
        ("string.starts_with_at", &["s", "i", "prefix"]),
        ("string.slice", &["s", "start", "end"]),
        ("string.pad_left", &["s", "width", "pad"]),
        ("string.pad_right", &["s", "width", "pad"]),
        ("string.is_empty", &["s"]),
        ("string.is_alpha", &["s"]),
        ("string.is_digit", &["s"]),
        ("string.is_upper", &["s"]),
        ("string.is_lower", &["s"]),
        ("string.is_alnum", &["s"]),
        ("string.is_whitespace", &["s"]),
        // ── map.* ────────────────────────────────────────────────
        ("map.get", &["m", "k"]),
        ("map.set", &["m", "k", "v"]),
        ("map.delete", &["m", "k"]),
        ("map.contains", &["m", "k"]),
        ("map.keys", &["m"]),
        ("map.values", &["m"]),
        ("map.merge", &["m", "other"]),
        ("map.length", &["m"]),
        ("map.filter", &["m", "pred"]),
        ("map.map", &["m", "f"]),
        ("map.entries", &["m"]),
        ("map.from_entries", &["entries"]),
        ("map.each", &["m", "f"]),
        ("map.update", &["m", "k", "default", "f"]),
        // ── set.* ────────────────────────────────────────────────
        ("set.new", &[]),
        ("set.from_list", &["xs"]),
        ("set.to_list", &["s"]),
        ("set.contains", &["s", "x"]),
        ("set.insert", &["s", "x"]),
        ("set.remove", &["s", "x"]),
        ("set.length", &["s"]),
        ("set.union", &["s", "other"]),
        ("set.intersection", &["s", "other"]),
        ("set.difference", &["s", "other"]),
        ("set.symmetric_difference", &["s", "other"]),
        ("set.is_subset", &["s", "other"]),
        ("set.map", &["s", "f"]),
        ("set.filter", &["s", "pred"]),
        ("set.each", &["s", "f"]),
        ("set.fold", &["s", "init", "f"]),
        // ── io.* ─────────────────────────────────────────────────
        ("io.inspect", &["x"]),
        ("io.read_file", &["path"]),
        ("io.write_file", &["path", "contents"]),
        ("io.read_line", &[]),
        ("io.args", &[]),
    ];
    entries.iter().copied().collect()
}

/// Return a map of every built-in name (qualified or bare) to its
/// markdown doc string, for the LSP to render in hover / completion /
/// signature-help. Includes everything registered with
/// `env.define_with_doc` / `env.attach_doc` under
/// `src/typechecker/builtins/` plus the unqualified globals
/// (`println`, `panic`, `Some`, `Ok`, …) registered in
/// `register_builtins` itself. Names without a registered doc are
/// omitted; callers do an `Option<&str>` lookup.
///
/// The map is keyed by the resolved (string) name so LSP code can
/// look up `"list.map"` directly without going through the intern
/// table — symmetric with `builtin_type_signatures`.
pub fn builtin_docs() -> std::collections::HashMap<String, String> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut docs = std::collections::HashMap::new();
    for (name, doc) in &env.builtin_docs {
        docs.insert(resolve(*name), doc.clone());
    }
    docs
}

/// Test-only: the sorted names (qualified or bare) of every
/// function-typed binding registered by `register_builtins`.
#[doc(hidden)]
pub fn builtin_function_names() -> Vec<String> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut out: Vec<String> = env
        .bindings
        .iter()
        .filter(|(_, s)| matches!(s.ty, Type::Fun(_, _)))
        .map(|(name, _)| resolve(*name))
        .collect();
    out.sort();
    out
}

/// Test-only: iterate `(qualified_name, doc)` for every built-in name
/// that has a registered doc. Used by the parity walker
/// (`tests/meta/docs_stdlib_println_parity_tests.rs`) to scan inlined
/// markdown for `\`\`\`silt` fenced blocks with `println(...) --
/// expected` annotations and run them against `silt run`.
#[doc(hidden)]
pub fn iter_builtin_docs() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = builtin_docs().into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Test-only introspection: collect the auto-derived trait-impl and
/// method registrations of the builtin init, for the derive-policy locks
/// in `tests/cli/trait_init_parity_tests.rs`.
///
/// Returns `(trait_impls, method_keys)` where:
/// - `trait_impls` is the set of `"Trait:Type"` pairs registered in
///   `trait_impl_set`.
/// - `method_keys` is the set of `"Type.method"` pairs in
///   `method_table`.
///
/// Stringifies the `Symbol` keys so test code doesn't need access to
/// the crate-private `Symbol`/`intern` types.
#[doc(hidden)]
pub fn __trait_init_fingerprint_check_program() -> (
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
) {
    use std::collections::BTreeSet;
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    register_builtin_trait_impls(&mut checker);
    let trait_impls: BTreeSet<String> = checker
        .tables
        .trait_impl_set
        .iter()
        .map(|(tr, ty)| format!("{}:{}", resolve(tr.name), resolve(ty.name)))
        .collect();
    let method_keys: BTreeSet<String> = checker
        .tables
        .method_table
        .keys()
        .map(|(ty, m)| format!("{}.{}", resolve(ty.name), resolve(*m)))
        .collect();
    (trait_impls, method_keys)
}

/// Test-only introspection for the built-in trait declarations
/// (Display/Compare/Equal/Hash). Returns one tuple per registered
/// trait in the fixed order Display, Compare, Equal, Hash:
///
///   (trait_name, method_name, method_arity, return_type_string,
///    supertrait_args_count, default_method_bodies_count,
///    params_count, supertraits_count, param_where_clauses_count)
///
/// Used by `tests/meta/typechecker_builtin_trait_registration_parity_tests.rs`
/// to lock the semantics of the round-61 dead-code collapse: the four
/// near-identical TraitInfo construction blocks were replaced with a
/// single parameterised helper, and this fingerprint proves the
/// before/after shapes are identical.
#[doc(hidden)]
pub fn __builtin_trait_registration_fingerprint() -> Vec<(
    String,
    String,
    usize,
    String,
    usize,
    usize,
    usize,
    usize,
    usize,
)> {
    let mut checker = TypeChecker::new();
    register_builtin_trait_impls(&mut checker);
    let names = ["Display", "Compare", "Equal", "Hash"];
    let mut out = Vec::new();
    for name in names {
        let info = checker
            .tables
            .traits
            .get(&TraitKey::builtin(name))
            .unwrap_or_else(|| panic!("built-in trait {name} not registered"));
        assert_eq!(
            info.methods.len(),
            1,
            "built-in trait {name} should have exactly one method, got {}",
            info.methods.len()
        );
        let (method_sym, method_ty) = &info.methods[0];
        let (arity, ret_str) = match method_ty {
            Type::Fun(params, ret) => (params.len(), format!("{ret:?}")),
            other => panic!("built-in trait {name} method type is not Fun: {other:?}"),
        };
        out.push((
            name.to_string(),
            resolve(*method_sym),
            arity,
            ret_str,
            info.supertrait_args.len(),
            info.default_method_bodies.len(),
            info.params.len(),
            info.supertraits.len(),
            info.param_where_clauses.len(),
        ));
    }
    out
}
