use super::*;

// ── The builtin environment ─────────────────────────────────────────

/// What every check starts from: the checker and the environment once
/// the builtin functions, types and traits are registered.
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
}

impl BuiltinEnv {
    fn build() -> Self {
        let mut checker = TypeChecker::new();
        let mut env = TypeEnv::new();
        register_prelude(&mut checker, &mut env);
        enter_registry(&mut checker, &mut env);
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
        let mut tables = std::mem::take(&mut checker.tables);
        // (The builtins' rows are no module's.)
        tables.begin_rows();
        BuiltinEnv {
            checker,
            tables,
            root: Rc::new(env),
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

/// Return a map of builtin qualified names to their type signature strings.
/// Used by the LSP to show type info in completions.
pub fn builtin_type_signatures() -> std::collections::HashMap<String, String> {
    let env = builtin_env();
    let (mut checker, _) = env.start();
    checker.tables = env.tables.clone();
    let mut sigs = std::collections::HashMap::new();
    for (name, scheme) in &env.root.bindings {
        let name_str = resolve(*name);
        if name_str.contains('.') && crate::module::is_builtin_module(module_of(&name_str)) {
            let ty = checker.instantiate(scheme);
            sigs.insert(name_str, format!("{ty}"));
        }
    }
    sigs
}

/// The part of a qualified name before its first dot.
fn module_of(qualified: &str) -> &str {
    qualified.split('.').next().unwrap_or(qualified)
}

// ── What the builtin scope is made of ───────────────────────────────

/// Bind the types that are values: the descriptors of the primitive and
/// container types.
fn register_prelude(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // A primitive type written as a value is its descriptor, of the type
    // `TypeOf(T)`, not a value of the type: `Int * 2` does not check.
    for name in crate::module::BUILTIN_PRIMITIVE_NAMES {
        let inner = match *name {
            "Int" => Type::Int,
            "Float" => Type::Float,
            "String" => Type::String,
            "Bool" => Type::Bool,
            _ => unreachable!(),
        };
        env.define(intern(name), Scheme::mono(Type::type_of(inner)));
    }
    // A container type written as a value (`make(List)` for a `type a`
    // parameter) is a descriptor of any of its instances. `Tuple` has a
    // descriptor at run time only.
    for name in crate::module::BUILTIN_GENERIC_CONTAINER_NAMES {
        let (vars, ty) = match *name {
            "List" => {
                let (a, av) = checker.fresh_tv();
                (vec![av], Type::List(Box::new(a)))
            }
            "Set" => {
                let (a, av) = checker.fresh_tv();
                (vec![av], Type::Set(Box::new(a)))
            }
            "Channel" => {
                let (a, av) = checker.fresh_tv();
                (vec![av], Type::Channel(Box::new(a)))
            }
            "Map" => {
                let (k, kv) = checker.fresh_tv();
                let (v, vv) = checker.fresh_tv();
                (vec![kv, vv], Type::Map(Box::new(k), Box::new(v)))
            }
            _ => continue,
        };
        env.define(
            intern(name),
            Scheme {
                vars,
                preds: vec![],
                ty: Type::type_of(ty),
                optional_last_param: false,
            },
        );
    }
}

/// Enter the builtin registry: the prelude's enums and functions and,
/// for each module that is built, its type declarations and its rows. Each module's text
/// is parsed and elaborated as a module's declarations are
/// (`register_type_decl`, `register_fn_decl`), so a row's parameter
/// types, `where` bounds and type variables are what its signature
/// says. A module's function is bound as `module.name`, the prelude's
/// and a variant by its name.
fn enter_registry(checker: &mut TypeChecker, env: &mut TypeEnv) {
    use crate::builtins::registry::{self, registry};
    let registry = registry();
    // A declaration names the type it declares by its name; the builtin
    // definitions say which variant of it each name is.
    checker.own_types = crate::defs::builtin_types()
        .iter()
        .enumerate()
        .map(|(k, (name, _))| {
            let name = intern(name);
            let id = crate::defs::TypeId(crate::defs::DefId(k as u32));
            (name, TypeRef { id, name })
        })
        .collect();
    checker.defs = Some(std::sync::Arc::new(names::new_def_table()));

    // The types of every module first: a signature or a field may name
    // a type of a later module (`fs.FileStat` holds a `time.DateTime`).
    let mut types: Vec<TypeDecl> = Vec::new();
    let texts = std::iter::once(registry::PRELUDE_TYPES)
        .chain(registry.enabled_modules().map(|module| module.types));
    for text in texts {
        for decl in registry::parse(text).decls {
            if let Decl::Type(td) = decl {
                types.push(td);
            }
        }
    }
    for td in &types {
        let ty = checker.own_type(td.name);
        match &td.body {
            TypeBody::Enum(_) => {
                checker.tables.enums.insert(
                    ty,
                    EnumInfo {
                        variants: Vec::new(),
                        params: td.params.clone(),
                        param_var_ids: Vec::new(),
                    },
                );
            }
            TypeBody::Record(_) => {
                checker
                    .tables
                    .records
                    .insert(ty, RecordInfo { fields: Vec::new() });
            }
            TypeBody::Alias(_) => {}
        }
    }
    // A declaration also binds its type as a value and stamps the impls
    // a program's type derives. A builtin type is not a value, and which
    // traits it derives is `register_builtin_trait_impls`'s to say.
    let impls = checker.tables.trait_impl_set.clone();
    let mut as_values = TypeEnv::new();
    for td in &types {
        checker.register_type_decl(td, &mut as_values);
    }
    checker.tables.trait_impl_set = impls;
    // Each variant's scheme is the builtin scope's, under its name.
    let (defs, _) = names::builtins();
    for variants in defs.variants.values() {
        for id in variants {
            if let Some(scheme) = checker.tables.schemes.remove(id) {
                env.define(defs.defs[id.0 as usize].name, scheme);
            }
        }
    }

    checker.registry_rows = true;
    for decl in registry::parse(registry::PRELUDE_FNS).decls {
        if let Decl::Fn(f) = decl {
            checker.register_fn_decl(&f, env);
        }
    }
    for module in registry.enabled_modules() {
        for decl in registry::parse(&module.text()).decls {
            let Decl::Fn(f) = decl else {
                continue;
            };
            let mut scope = TypeEnv::new();
            checker.register_fn_decl(&f, &mut scope);
            let Some(mut scheme) = scope.bindings.remove(&f.name) else {
                continue;
            };
            let name = resolve(f.name);
            let row = module.row(&name).expect("the row the text was made from");
            if row.is_constant()
                && let Type::Fun(_, value) = scheme.ty
            {
                scheme = Scheme::mono(*value);
            }
            scheme.optional_last_param = row.optional_last;
            env.define(intern(&format!("{}.{name}", module.name)), scheme);
        }
        // The module's error enum implements `Error` natively
        // (`Vm::dispatch_trait_method`), and `Display` with it.
        if let Some(error) = module.error {
            let ty = TypeRef::builtin(error);
            let self_ty = Type::Generic(ty, vec![]);
            for (trait_name, method) in [("Error", "message"), ("Display", "display")] {
                let key = TraitKey::builtin(trait_name);
                checker.tables.trait_impl_set.insert((key, ty));
                checker.tables.impl_methods.insert(
                    ty,
                    intern(method),
                    key,
                    MethodEntry {
                        method_type: Type::Fun(vec![self_ty.clone()], Box::new(Type::String)),
                        structural: false,
                        trait_name: Some(key),
                        receiver: true,
                        preds: Vec::new(),
                    },
                );
            }
        }
    }

    assert!(
        checker.errors.is_empty(),
        "the builtin registry does not check: {:?}",
        checker.errors
    );
    checker.registry_rows = false;
    checker.defs = None;
    checker.own_types.clear();
}

/// Test-only introspection: collect the structural-trait and method
/// registrations of the builtin init, for the policy locks
/// in `tests/cli/trait_init_parity_tests.rs`.
///
/// Returns `(trait_impls, method_keys)` where:
/// - `trait_impls` is the set of `"Trait:Type"` pairs registered in
///   `trait_impl_set`.
/// - `method_keys` is the set of `"Type.method"` pairs the builtins
///   have (`Tables::methods`).
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
    register_prelude(&mut checker, &mut env);
    enter_registry(&mut checker, &mut env);
    register_builtin_trait_impls(&mut checker);
    let trait_impls: BTreeSet<String> = checker
        .tables
        .trait_impl_set
        .iter()
        .map(|(tr, ty)| format!("{}:{}", resolve(tr.name), resolve(ty.name)))
        .collect();
    let method_keys: BTreeSet<String> = checker
        .tables
        .methods()
        .iter()
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
/// Used by `tests/typecheck/unified_trait_registration_tests.rs`.
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

#[cfg(test)]
mod tests;
