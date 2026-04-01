//! Hindley-Milner type inference and checking for Silt.
//!
//! This module implements Algorithm W-style type inference with:
//! - Type variables and unification
//! - Let-polymorphism (generalization at let bindings)
//! - Exhaustiveness checking for match expressions
//! - Type narrowing after `when` guard statements
//! - Trait constraint checking

use std::collections::HashMap;

use crate::ast::*;
use crate::lexer::Span;
use crate::types::*;

pub use crate::types::{Type, TyVar, Scheme, Severity, TypeError};

// ── Type environment ────────────────────────────────────────────────

/// A typing environment mapping names to type schemes.
#[derive(Debug, Clone)]
struct TypeEnv {
    bindings: HashMap<std::string::String, Scheme>,
    parent: Option<Box<TypeEnv>>,
}

impl TypeEnv {
    fn new() -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            parent: None,
        }
    }

    fn child(&self) -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            parent: Some(Box::new(self.clone())),
        }
    }

    fn define(&mut self, name: std::string::String, scheme: Scheme) {
        self.bindings.insert(name, scheme);
    }

    fn lookup(&self, name: &str) -> Option<&Scheme> {
        if let Some(s) = self.bindings.get(name) {
            Some(s)
        } else if let Some(ref parent) = self.parent {
            parent.lookup(name)
        } else {
            None
        }
    }

    /// Collect all free type variables in the environment.
    fn free_vars(&self, checker: &TypeChecker) -> Vec<TyVar> {
        let mut fvs = Vec::new();
        for scheme in self.bindings.values() {
            let ty = checker.apply(&scheme.ty);
            let mut ty_fvs = free_vars_in(&ty);
            // Remove the scheme's own quantified variables
            ty_fvs.retain(|v| !scheme.vars.contains(v));
            for v in ty_fvs {
                if !fvs.contains(&v) {
                    fvs.push(v);
                }
            }
        }
        if let Some(ref parent) = self.parent {
            for v in parent.free_vars(checker) {
                if !fvs.contains(&v) {
                    fvs.push(v);
                }
            }
        }
        fvs
    }
}

// ── Type declaration info ───────────────────────────────────────────

/// Information about a declared enum type.
#[derive(Debug, Clone)]
struct EnumInfo {
    _name: std::string::String,
    params: Vec<std::string::String>,
    variants: Vec<VariantInfo>,
}

#[derive(Debug, Clone)]
struct VariantInfo {
    name: std::string::String,
    field_types: Vec<Type>,
}

/// Information about a declared record type.
#[derive(Debug, Clone)]
struct RecordInfo {
    _name: std::string::String,
    _params: Vec<std::string::String>,
    fields: Vec<(std::string::String, Type)>,
}

/// Information about a declared trait.
#[derive(Debug, Clone)]
struct TraitInfo {
    _name: std::string::String,
    methods: Vec<(std::string::String, Type)>,
}

/// Information about a trait implementation.
#[derive(Debug, Clone)]
struct TraitImplInfo {
    trait_name: std::string::String,
    target_type: std::string::String,
    methods: Vec<(std::string::String, usize)>,
    span: Span,
}

// ── The type checker ────────────────────────────────────────────────

pub struct TypeChecker {
    /// The substitution: maps type variables to their resolved types.
    subst: Vec<Option<Type>>,
    /// Counter for generating fresh type variables.
    next_var: TyVar,
    /// Declared enum types (type name -> enum info).
    enums: HashMap<std::string::String, EnumInfo>,
    /// Maps variant constructor name -> parent enum type name.
    variant_to_enum: HashMap<std::string::String, std::string::String>,
    /// Declared record types (type name -> record info).
    records: HashMap<std::string::String, RecordInfo>,
    /// Declared traits.
    traits: HashMap<std::string::String, TraitInfo>,
    /// Trait implementations.
    trait_impls: Vec<TraitImplInfo>,
    /// Maps function names to their where clauses as (param_index, trait_name).
    fn_where_clauses: HashMap<std::string::String, Vec<(usize, std::string::String)>>,
    /// Accumulated type errors.
    pub errors: Vec<TypeError>,
    /// Tracks the number of bindings in the enclosing `loop` (if any),
    /// so that `recur` arity can be validated.
    loop_binding_count: Option<usize>,
}

impl TypeChecker {
    pub fn new() -> Self {
        TypeChecker {
            subst: Vec::new(),
            next_var: 0,
            enums: HashMap::new(),
            variant_to_enum: HashMap::new(),
            records: HashMap::new(),
            traits: HashMap::new(),
            trait_impls: Vec::new(),
            fn_where_clauses: HashMap::new(),
            errors: Vec::new(),
            loop_binding_count: None,
        }
    }

    // ── Fresh variables ─────────────────────────────────────────────

    fn fresh_var(&mut self) -> Type {
        let v = self.next_var;
        self.next_var += 1;
        self.subst.push(None);
        Type::Var(v)
    }

    // ── Substitution / apply ────────────────────────────────────────

    /// Walk the substitution chain to find the most resolved type.
    fn apply(&self, ty: &Type) -> Type {
        match ty {
            Type::Var(v) => {
                if let Some(Some(resolved)) = self.subst.get(*v) {
                    self.apply(resolved)
                } else {
                    ty.clone()
                }
            }
            Type::Fun(params, ret) => {
                let params = params.iter().map(|p| self.apply(p)).collect();
                let ret = Box::new(self.apply(ret));
                Type::Fun(params, ret)
            }
            Type::List(inner) => Type::List(Box::new(self.apply(inner))),
            Type::Tuple(elems) => {
                Type::Tuple(elems.iter().map(|e| self.apply(e)).collect())
            }
            Type::Record(name, fields) => {
                let fields = fields
                    .iter()
                    .map(|(n, t)| (n.clone(), self.apply(t)))
                    .collect();
                Type::Record(name.clone(), fields)
            }
            Type::Variant(name, args) => {
                let args = args.iter().map(|a| self.apply(a)).collect();
                Type::Variant(name.clone(), args)
            }
            Type::Generic(name, args) => {
                let args = args.iter().map(|a| self.apply(a)).collect();
                Type::Generic(name.clone(), args)
            }
            Type::Map(k, v) => {
                Type::Map(Box::new(self.apply(k)), Box::new(self.apply(v)))
            }
            _ => ty.clone(),
        }
    }

    // ── Unification ─────────────────────────────────────────────────

    fn unify(&mut self, t1: &Type, t2: &Type, span: Span) {
        let t1 = self.apply(t1);
        let t2 = self.apply(t2);

        match (&t1, &t2) {
            (Type::Error, _) | (_, Type::Error) => {}
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::Bool, Type::Bool)
            | (Type::String, Type::String)
            | (Type::Unit, Type::Unit) => {}

            (Type::Var(v1), Type::Var(v2)) if v1 == v2 => {}

            (Type::Var(v), t) | (t, Type::Var(v)) => {
                if occurs_in(*v, t) {
                    self.error(
                        format!("infinite type: ?{v} occurs in {t}"),
                        span,
                    );
                } else {
                    self.subst[*v] = Some(t.clone());
                }
            }

            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                if p1.len() != p2.len() {
                    self.error(
                        format!(
                            "function arity mismatch: expected {} args, got {}",
                            p1.len(),
                            p2.len()
                        ),
                        span,
                    );
                } else {
                    for (a, b) in p1.iter().zip(p2.iter()) {
                        self.unify(a, b, span);
                    }
                    self.unify(r1, r2, span);
                }
            }

            (Type::List(a), Type::List(b)) => {
                self.unify(a, b, span);
            }

            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                self.unify(k1, k2, span);
                self.unify(v1, v2, span);
            }

            (Type::Tuple(a), Type::Tuple(b)) => {
                if a.len() != b.len() {
                    self.error(
                        format!(
                            "tuple length mismatch: expected {}, got {}",
                            a.len(),
                            b.len()
                        ),
                        span,
                    );
                } else {
                    for (x, y) in a.iter().zip(b.iter()) {
                        self.unify(x, y, span);
                    }
                }
            }

            (Type::Record(n1, f1), Type::Record(n2, f2)) => {
                if n1 != n2 {
                    self.error(
                        format!("record type mismatch: expected {n1}, got {n2}"),
                        span,
                    );
                } else {
                    // Unify fields by name
                    for (name, t1) in f1 {
                        if let Some((_, t2)) = f2.iter().find(|(n, _)| n == name) {
                            self.unify(t1, t2, span);
                        }
                    }
                }
            }

            // Record(name, fields) is compatible with Generic(name, []) when
            // the Generic refers to a record type with no type params.
            (Type::Record(n1, _), Type::Generic(n2, a2)) if n1 == n2 && a2.is_empty() => {}
            (Type::Generic(n1, a1), Type::Record(n2, _)) if n1 == n2 && a1.is_empty() => {}

            (Type::Generic(n1, a1), Type::Generic(n2, a2)) => {
                if n1 != n2 {
                    self.error(
                        format!("type mismatch: expected {n1}, got {n2}"),
                        span,
                    );
                } else if a1.len() != a2.len() {
                    self.error(
                        format!(
                            "type argument count mismatch for {n1}: expected {}, got {}",
                            a1.len(),
                            a2.len()
                        ),
                        span,
                    );
                } else {
                    for (x, y) in a1.iter().zip(a2.iter()) {
                        self.unify(x, y, span);
                    }
                }
            }

            (Type::Variant(n1, a1), Type::Variant(n2, a2)) => {
                if n1 != n2 {
                    self.error(
                        format!("variant mismatch: expected {n1}, got {n2}"),
                        span,
                    );
                } else if a1.len() != a2.len() {
                    self.error(
                        format!(
                            "variant field count mismatch for {n1}: expected {}, got {}",
                            a1.len(),
                            a2.len()
                        ),
                        span,
                    );
                } else {
                    for (x, y) in a1.iter().zip(a2.iter()) {
                        self.unify(x, y, span);
                    }
                }
            }

            _ => {
                self.error(
                    format!("type mismatch: expected {t1}, got {t2}"),
                    span,
                );
            }
        }
    }

    // ── Generalization / Instantiation ──────────────────────────────

    /// Generalize a type into a scheme by quantifying over free variables
    /// not present in the environment.
    fn generalize(&self, env: &TypeEnv, ty: &Type) -> Scheme {
        let ty = self.apply(ty);
        let env_fvs = env.free_vars(self);
        let ty_fvs = free_vars_in(&ty);
        let vars: Vec<TyVar> = ty_fvs
            .into_iter()
            .filter(|v| !env_fvs.contains(v))
            .collect();
        Scheme { vars, ty }
    }

    /// Instantiate a scheme by replacing quantified variables with fresh ones.
    fn instantiate(&mut self, scheme: &Scheme) -> Type {
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for &v in &scheme.vars {
            mapping.insert(v, self.fresh_var());
        }
        substitute_vars(&scheme.ty, &mapping)
    }

    // ── Type name for trait impl matching ────────────────────────────

    /// Convert a resolved Type to a type name string suitable for matching
    /// against `TraitImplInfo.target_type`. Returns `None` if the type is
    /// unresolved (still a type variable) or cannot be mapped to a name.
    fn type_name_for_impl(&self, ty: &Type) -> Option<std::string::String> {
        match ty {
            Type::Int => Some("Int".into()),
            Type::Float => Some("Float".into()),
            Type::Bool => Some("Bool".into()),
            Type::String => Some("String".into()),
            Type::Unit => Some("()".into()),
            Type::Record(name, _) => Some(name.clone()),
            Type::Generic(name, _) => Some(name.clone()),
            Type::Variant(name, _) => {
                // Look up the parent enum name for this variant
                if let Some(enum_name) = self.variant_to_enum.get(name) {
                    Some(enum_name.clone())
                } else {
                    Some(name.clone())
                }
            }
            Type::Var(_) => None, // unresolved
            _ => None,
        }
    }

    // ── Error reporting ─────────────────────────────────────────────

    fn error(&mut self, message: std::string::String, span: Span) {
        self.errors.push(TypeError { message, span, severity: Severity::Error });
    }

    #[allow(dead_code)]
    fn warning(&mut self, message: std::string::String, span: Span) {
        self.errors.push(TypeError { message, span, severity: Severity::Warning });
    }

    // ── Check a full program ────────────────────────────────────────

    pub fn check_program(&mut self, program: &mut Program) {
        let mut env = TypeEnv::new();

        // Register builtins in the type environment
        self.register_builtins(&mut env);

        // Register built-in traits
        {
            let display_self = self.fresh_var();
            self.traits.insert("Display".into(), TraitInfo {
                _name: "Display".into(),
                methods: vec![("display".into(), Type::Fun(vec![display_self], Box::new(Type::String)))],
            });
        }
        {
            let compare_a = self.fresh_var();
            let compare_b = self.fresh_var();
            self.traits.insert("Compare".into(), TraitInfo {
                _name: "Compare".into(),
                methods: vec![("compare".into(), Type::Fun(vec![compare_a, compare_b], Box::new(Type::Int)))],
            });
        }
        {
            let equal_a = self.fresh_var();
            let equal_b = self.fresh_var();
            self.traits.insert("Equal".into(), TraitInfo {
                _name: "Equal".into(),
                methods: vec![("equal".into(), Type::Fun(vec![equal_a, equal_b], Box::new(Type::Bool)))],
            });
        }
        {
            let hash_self = self.fresh_var();
            self.traits.insert("Hash".into(), TraitInfo {
                _name: "Hash".into(),
                methods: vec![("hash".into(), Type::Fun(vec![hash_self], Box::new(Type::Int)))],
            });
        }

        // Register builtin trait implementations for primitive types.
        // These allow where clauses like `where a: Equal` to resolve
        // when `a` is Int, String, Bool, etc.
        {
            let dummy_span = Span { line: 0, col: 0, offset: 0 };
            let primitive_types = ["Int", "Float", "Bool", "String", "()"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            for type_name in &primitive_types {
                for trait_name in &all_traits {
                    self.trait_impls.push(TraitImplInfo {
                        trait_name: trait_name.to_string(),
                        target_type: type_name.to_string(),
                        methods: Vec::new(), // builtin impls have no user-visible methods
                        span: dummy_span,
                    });
                }
            }
            // List and Tuple implement these traits when their elements do,
            // but for now register them unconditionally (a pragmatic choice
            // matching the runtime behavior where Eq/Ord/Hash work on all Values).
            for type_name in &["List", "Tuple", "Map"] {
                for trait_name in &all_traits {
                    self.trait_impls.push(TraitImplInfo {
                        trait_name: trait_name.to_string(),
                        target_type: type_name.to_string(),
                        methods: Vec::new(),
                        span: dummy_span,
                    });
                }
            }
        }

        // First pass: register all type declarations
        for decl in &program.decls {
            if let Decl::Type(td) = decl {
                self.register_type_decl(td, &mut env);
            }
        }

        // Second pass: register all function signatures and trait impls
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) => {
                    self.register_fn_decl(f, &mut env);
                }
                Decl::Trait(t) => {
                    self.register_trait_decl(t);
                }
                Decl::TraitImpl(ti) => {
                    self.register_trait_impl(ti, &mut env);
                }
                _ => {}
            }
        }

        // Validate trait implementations against their declarations
        self.validate_trait_impls();

        // Third pass: type check function bodies (mutable access needed)
        for i in 0..program.decls.len() {
            if let Decl::Fn(ref mut f) = program.decls[i] {
                self.check_fn_body(f, &env);
            }
        }

        // Also check trait impl method bodies (mutable access needed)
        for i in 0..program.decls.len() {
            if let Decl::TraitImpl(ref mut ti) = program.decls[i] {
                for j in 0..ti.methods.len() {
                    self.check_fn_body(&mut ti.methods[j], &env);
                }
            }
        }

        // After all passes, resolve any remaining type variables in annotations
        self.resolve_all_types(program);
    }

    // ── Validate trait implementations ────────────────────────────────

    fn validate_trait_impls(&mut self) {
        // Clone to avoid borrow issues
        let impls = self.trait_impls.clone();
        for impl_info in &impls {
            // Skip builtin/auto-derived impls (registered with dummy span)
            if impl_info.span.line == 0 && impl_info.span.col == 0 && impl_info.methods.is_empty() {
                continue;
            }
            // Check that the trait exists
            let Some(trait_info) = self.traits.get(&impl_info.trait_name) else {
                self.error(
                    format!("trait '{}' is not declared", impl_info.trait_name),
                    impl_info.span,
                );
                continue;
            };

            // Check that all required methods are implemented
            let trait_methods = trait_info.methods.clone();
            for (method_name, trait_method_type) in &trait_methods {
                if let Some((_, impl_arity)) =
                    impl_info.methods.iter().find(|(n, _)| n == method_name)
                {
                    // Check arity matches
                    let expected_arity = count_params(trait_method_type);
                    if *impl_arity != expected_arity {
                        self.error(
                            format!(
                                "method '{}' in trait impl '{}' for '{}' has wrong arity: expected {}, got {}",
                                method_name, impl_info.trait_name, impl_info.target_type, expected_arity, impl_arity
                            ),
                            impl_info.span,
                        );
                    }
                } else {
                    self.error(
                        format!(
                            "trait impl '{}' for '{}' is missing method '{}'",
                            impl_info.trait_name, impl_info.target_type, method_name
                        ),
                        impl_info.span,
                    );
                }
            }
        }
    }

    // ── Register builtins ───────────────────────────────────────────

    /// Helper: create a fresh type variable and return both the Type::Var and
    /// its TyVar id.
    fn fresh_tv(&mut self) -> (Type, TyVar) {
        let t = self.fresh_var();
        let v = match &t { Type::Var(v) => *v, _ => unreachable!() };
        (t, v)
    }

    fn register_builtins(&mut self, env: &mut TypeEnv) {
        // ── print / println: (String) -> () ────────────────────────────
        let str_to_unit = Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Unit),
        ));
        env.define("print".into(), str_to_unit.clone());
        env.define("println".into(), str_to_unit);

        // ── io.inspect: a -> String ──────────────────────────────────
        {
            let (a, av) = self.fresh_tv();
            env.define("io.inspect".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a], Box::new(Type::String)),
            });
        }

        // ── panic: String -> a ─────────────────────────────────────────
        {
            let (a, av) = self.fresh_tv();
            env.define("panic".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::String], Box::new(a)),
            });
        }

        // ── Higher-order list builtins ─────────────────────────────────

        // list.map: (List(a), (a -> b)) -> List(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("list.map".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(b.clone())),
                    ],
                    Box::new(Type::List(Box::new(b))),
                ),
            });
        }

        // list.filter: (List(a), (a -> Bool)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.filter".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.fold: (List(a), b, (b, a) -> b) -> b
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("list.fold".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                    ],
                    Box::new(b),
                ),
            });
        }

        // list.each: (List(a), (a -> ())) -> ()
        {
            let (a, av) = self.fresh_tv();
            env.define("list.each".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::Unit)),
                    ],
                    Box::new(Type::Unit),
                ),
            });
        }

        // list.find: (List(a), (a -> Bool)) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.find".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Generic("Option".into(), vec![a])),
                ),
            });
        }

        // list.zip: (List(a), List(b)) -> List((a, b))
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("list.zip".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::List(Box::new(b.clone())),
                    ],
                    Box::new(Type::List(Box::new(Type::Tuple(vec![a, b])))),
                ),
            });
        }

        // list.flatten: (List(List(a))) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.flatten".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(Type::List(Box::new(a.clone()))))],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.sort_by: (List(a), (a -> b)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("list.sort_by".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(b)),
                    ],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.flat_map: (List(a), (a -> List(b))) -> List(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("list.flat_map".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::List(Box::new(b.clone())))),
                    ],
                    Box::new(Type::List(Box::new(b))),
                ),
            });
        }

        // list.any: (List(a), (a -> Bool)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define("list.any".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Bool),
                ),
            });
        }

        // list.all: (List(a), (a -> Bool)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define("list.all".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Bool),
                ),
            });
        }

        // list.fold_until: (List(a), b, (b, a) -> Step(b)) -> b
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            let (step, stepv) = self.fresh_tv();
            env.define("list.fold_until".into(), Scheme {
                vars: vec![av, bv, stepv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(vec![b.clone(), a], Box::new(step)),
                    ],
                    Box::new(b),
                ),
            });
        }

        // list.unfold: (a, (a) -> Option((b, a))) -> List(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("list.unfold".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        a.clone(),
                        Type::Fun(
                            vec![a.clone()],
                            Box::new(Type::Generic("Option".into(), vec![
                                Type::Tuple(vec![b.clone(), a]),
                            ])),
                        ),
                    ],
                    Box::new(Type::List(Box::new(b))),
                ),
            });
        }

        // len removed from globals -- use list.length, string.length, map.length

        // ── Variant constructors ───────────────────────────────────────

        // Ok(a) -> Result(a, e)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define("Ok".into(), Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![a.clone()],
                    Box::new(Type::Generic("Result".into(), vec![a, e])),
                ),
            });
        }
        // Err(e) -> Result(a, e)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define("Err".into(), Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![e.clone()],
                    Box::new(Type::Generic("Result".into(), vec![a, e])),
                ),
            });
        }
        // Some(a) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("Some".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![a.clone()],
                    Box::new(Type::Generic("Option".into(), vec![a])),
                ),
            });
        }
        // None : Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("None".into(), Scheme {
                vars: vec![av],
                ty: Type::Generic("Option".into(), vec![a]),
            });
        }

        // ── Builtin enum info for Option and Result ────────────────────

        self.enums.insert(
            "Option".into(),
            EnumInfo {
                _name: "Option".into(),
                params: vec!["a".into()],
                variants: vec![
                    VariantInfo {
                        name: "Some".into(),
                        field_types: vec![Type::Var(0)], // placeholder
                    },
                    VariantInfo {
                        name: "None".into(),
                        field_types: vec![],
                    },
                ],
            },
        );
        self.variant_to_enum.insert("Some".into(), "Option".into());
        self.variant_to_enum.insert("None".into(), "Option".into());

        self.enums.insert(
            "Result".into(),
            EnumInfo {
                _name: "Result".into(),
                params: vec!["a".into(), "e".into()],
                variants: vec![
                    VariantInfo {
                        name: "Ok".into(),
                        field_types: vec![Type::Var(0)], // placeholder
                    },
                    VariantInfo {
                        name: "Err".into(),
                        field_types: vec![Type::Var(1)], // placeholder
                    },
                ],
            },
        );
        self.variant_to_enum.insert("Ok".into(), "Result".into());
        self.variant_to_enum.insert("Err".into(), "Result".into());

        // ── Test builtins ──────────────────────────────────────────────

        // test.assert: Bool -> ()
        env.define(
            "test.assert".into(),
            Scheme::mono(Type::Fun(vec![Type::Bool], Box::new(Type::Unit))),
        );

        // test.assert_eq: (a, a) -> ()
        {
            let (a, av) = self.fresh_tv();
            env.define("test.assert_eq".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone(), a], Box::new(Type::Unit)),
            });
        }

        // test.assert_ne: (a, a) -> ()
        {
            let (a, av) = self.fresh_tv();
            env.define("test.assert_ne".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone(), a], Box::new(Type::Unit)),
            });
        }

        // ── Result/Option helpers (top-level) ──────────────────────────

        // result.map_ok: (Result(a,e), (a -> b)) -> Result(b,e)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define("result.map_ok".into(), Scheme {
                vars: vec![av, bv, ev],
                ty: Type::Fun(
                    vec![
                        Type::Generic("Result".into(), vec![a, e.clone()]),
                        Type::Fun(vec![Type::Var(av)], Box::new(b.clone())),
                    ],
                    Box::new(Type::Generic("Result".into(), vec![b, e])),
                ),
            });
        }

        // result.unwrap_or: (Result(a,e), a) -> a
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define("result.unwrap_or".into(), Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![
                        Type::Generic("Result".into(), vec![a.clone(), e]),
                        a.clone(),
                    ],
                    Box::new(a),
                ),
            });
        }

        // ── list module ────────────────────────────────────────────────

        // list.append: (List(a), a) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.append".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), a.clone()],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.concat: (List(a), List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.concat".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::List(Box::new(a.clone())),
                    ],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.get: (List(a), Int) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.get".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), Type::Int],
                    Box::new(Type::Generic("Option".into(), vec![a])),
                ),
            });
        }

        // list.take: (List(a), Int) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.take".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), Type::Int],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.drop: (List(a), Int) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.drop".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), Type::Int],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.enumerate: (List(a)) -> List((Int, a))
        {
            let (a, av) = self.fresh_tv();
            env.define("list.enumerate".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(Type::Tuple(vec![Type::Int, a])))),
                ),
            });
        }

        // list.head: (List(a)) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.head".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::Generic("Option".into(), vec![a])),
                ),
            });
        }

        // list.tail: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.tail".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.last: (List(a)) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.last".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::Generic("Option".into(), vec![a])),
                ),
            });
        }

        // list.reverse: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.reverse".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.sort: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.sort".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
            });
        }

        // list.contains: (List(a), a) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define("list.contains".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), a],
                    Box::new(Type::Bool),
                ),
            });
        }

        // list.length: (List(a)) -> Int
        {
            let (a, av) = self.fresh_tv();
            env.define("list.length".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a))],
                    Box::new(Type::Int),
                ),
            });
        }

        // ── string module ──────────────────────────────────────────────

        // string.split: (String, String) -> List(String)
        env.define("string.split".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::List(Box::new(Type::String))),
        )));

        // string.join: (List(String), String) -> String
        env.define("string.join".into(), Scheme::mono(Type::Fun(
            vec![Type::List(Box::new(Type::String)), Type::String],
            Box::new(Type::String),
        )));

        // string.trim: (String) -> String
        env.define("string.trim".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::String),
        )));

        // string.contains: (String, String) -> Bool
        env.define("string.contains".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Bool),
        )));

        // string.replace: (String, String, String) -> String
        env.define("string.replace".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String, Type::String],
            Box::new(Type::String),
        )));

        // string.length: (String) -> Int
        env.define("string.length".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Int),
        )));

        // string.to_upper: (String) -> String
        env.define("string.to_upper".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::String),
        )));

        // string.to_lower: (String) -> String
        env.define("string.to_lower".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::String),
        )));

        // string.starts_with: (String, String) -> Bool
        env.define("string.starts_with".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Bool),
        )));

        // string.ends_with: (String, String) -> Bool
        env.define("string.ends_with".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Bool),
        )));

        // string.chars: (String) -> List(String)
        env.define("string.chars".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::List(Box::new(Type::String))),
        )));

        // string.repeat: (String, Int) -> String
        env.define("string.repeat".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int],
            Box::new(Type::String),
        )));

        // string.index_of: (String, String) -> Option(Int)
        env.define("string.index_of".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic("Option".into(), vec![Type::Int])),
        )));

        // string.slice: (String, Int, Int) -> String
        env.define("string.slice".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int, Type::Int],
            Box::new(Type::String),
        )));

        // string.pad_left: (String, Int, String) -> String
        env.define("string.pad_left".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int, Type::String],
            Box::new(Type::String),
        )));

        // string.pad_right: (String, Int, String) -> String
        env.define("string.pad_right".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::Int, Type::String],
            Box::new(Type::String),
        )));

        // ── int module ─────────────────────────────────────────────────

        // int.parse: (String) -> Result(Int, String)
        env.define("int.parse".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic("Result".into(), vec![Type::Int, Type::String])),
        )));

        // int.abs: (Int) -> Int
        env.define("int.abs".into(), Scheme::mono(Type::Fun(
            vec![Type::Int],
            Box::new(Type::Int),
        )));

        // int.min: (Int, Int) -> Int
        env.define("int.min".into(), Scheme::mono(Type::Fun(
            vec![Type::Int, Type::Int],
            Box::new(Type::Int),
        )));

        // int.max: (Int, Int) -> Int
        env.define("int.max".into(), Scheme::mono(Type::Fun(
            vec![Type::Int, Type::Int],
            Box::new(Type::Int),
        )));

        // int.to_float: (Int) -> Float
        env.define("int.to_float".into(), Scheme::mono(Type::Fun(
            vec![Type::Int],
            Box::new(Type::Float),
        )));

        // int.to_string: (Int) -> String
        env.define("int.to_string".into(), Scheme::mono(Type::Fun(
            vec![Type::Int],
            Box::new(Type::String),
        )));

        // ── float module ───────────────────────────────────────────────

        // float.parse: (String) -> Result(Float, String)
        env.define("float.parse".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic("Result".into(), vec![Type::Float, Type::String])),
        )));

        // float.round: (Float) -> Int
        env.define("float.round".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::Int),
        )));

        // float.ceil: (Float) -> Int
        env.define("float.ceil".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::Int),
        )));

        // float.floor: (Float) -> Int
        env.define("float.floor".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::Int),
        )));

        // float.abs: (Float) -> Float
        env.define("float.abs".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::Float),
        )));

        // float.min: (Float, Float) -> Float
        env.define("float.min".into(), Scheme::mono(Type::Fun(
            vec![Type::Float, Type::Float],
            Box::new(Type::Float),
        )));

        // float.max: (Float, Float) -> Float
        env.define("float.max".into(), Scheme::mono(Type::Fun(
            vec![Type::Float, Type::Float],
            Box::new(Type::Float),
        )));

        // float.to_string: (Float) -> String  (also accepts (Float, Int) at runtime)
        env.define("float.to_string".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::String),
        )));

        // float.to_int: (Float) -> Int
        env.define("float.to_int".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::Int),
        )));

        // ── io module ──────────────────────────────────────────────────

        // io.read_file: (String) -> Result(String, String)
        env.define("io.read_file".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic("Result".into(), vec![Type::String, Type::String])),
        )));

        // io.write_file: (String, String) -> Result((), String)
        env.define("io.write_file".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic("Result".into(), vec![Type::Unit, Type::String])),
        )));

        // io.read_line: () -> Result(String, String)
        env.define("io.read_line".into(), Scheme::mono(Type::Fun(
            vec![],
            Box::new(Type::Generic("Result".into(), vec![Type::String, Type::String])),
        )));

        // io.args: () -> List(String)
        env.define("io.args".into(), Scheme::mono(Type::Fun(
            vec![],
            Box::new(Type::List(Box::new(Type::String))),
        )));

        // ── map module ─────────────────────────────────────────────────
        // Maps in the interpreter use String keys, so we type them accordingly.
        // map.get: (Map(String, v), String) -> Option(v)
        {
            let (v, vv) = self.fresh_tv();
            env.define("map.get".into(), Scheme {
                vars: vec![vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(Type::String), Box::new(v.clone())),
                        Type::String,
                    ],
                    Box::new(Type::Generic("Option".into(), vec![v])),
                ),
            });
        }

        // map.set: (Map(String, v), String, v) -> Map(String, v)
        {
            let (v, vv) = self.fresh_tv();
            env.define("map.set".into(), Scheme {
                vars: vec![vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(Type::String), Box::new(v.clone())),
                        Type::String,
                        v.clone(),
                    ],
                    Box::new(Type::Map(Box::new(Type::String), Box::new(v))),
                ),
            });
        }

        // map.delete: (Map(String, v), String) -> Map(String, v)
        {
            let (v, vv) = self.fresh_tv();
            env.define("map.delete".into(), Scheme {
                vars: vec![vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(Type::String), Box::new(v.clone())),
                        Type::String,
                    ],
                    Box::new(Type::Map(Box::new(Type::String), Box::new(v))),
                ),
            });
        }

        // map.keys: (Map(String, v)) -> List(String)
        {
            let (v, vv) = self.fresh_tv();
            env.define("map.keys".into(), Scheme {
                vars: vec![vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(Type::String), Box::new(v))],
                    Box::new(Type::List(Box::new(Type::String))),
                ),
            });
        }

        // map.values: (Map(String, v)) -> List(v)
        {
            let (v, vv) = self.fresh_tv();
            env.define("map.values".into(), Scheme {
                vars: vec![vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(Type::String), Box::new(v.clone()))],
                    Box::new(Type::List(Box::new(v))),
                ),
            });
        }

        // map.merge: (Map(String, v), Map(String, v)) -> Map(String, v)
        {
            let (v, vv) = self.fresh_tv();
            env.define("map.merge".into(), Scheme {
                vars: vec![vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(Type::String), Box::new(v.clone())),
                        Type::Map(Box::new(Type::String), Box::new(v.clone())),
                    ],
                    Box::new(Type::Map(Box::new(Type::String), Box::new(v))),
                ),
            });
        }

        // map.length: (Map(String, v)) -> Int
        {
            let (v, vv) = self.fresh_tv();
            env.define("map.length".into(), Scheme {
                vars: vec![vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(Type::String), Box::new(v))],
                    Box::new(Type::Int),
                ),
            });
        }

        // ── channel module ─────────────────────────────────────────────

        // channel.new: (Int) -> Channel  (opaque; use fresh var)
        {
            let (ch, chv) = self.fresh_tv();
            env.define("channel.new".into(), Scheme {
                vars: vec![chv],
                ty: Type::Fun(vec![Type::Int], Box::new(ch)),
            });
        }

        // channel.send: (Channel, a) -> Unit
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("channel.send".into(), Scheme {
                vars: vec![chv, av],
                ty: Type::Fun(vec![ch, a], Box::new(Type::Unit)),
            });
        }

        // channel.receive: (Channel) -> a
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("channel.receive".into(), Scheme {
                vars: vec![chv, av],
                ty: Type::Fun(vec![ch], Box::new(a)),
            });
        }

        // channel.close: (Channel) -> Unit
        {
            let (ch, chv) = self.fresh_tv();
            env.define("channel.close".into(), Scheme {
                vars: vec![chv],
                ty: Type::Fun(vec![ch], Box::new(Type::Unit)),
            });
        }

        // channel.try_send: (Channel, a) -> Bool
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("channel.try_send".into(), Scheme {
                vars: vec![chv, av],
                ty: Type::Fun(vec![ch, a], Box::new(Type::Bool)),
            });
        }

        // channel.try_receive: (Channel) -> Option(a)
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("channel.try_receive".into(), Scheme {
                vars: vec![chv, av],
                ty: Type::Fun(
                    vec![ch],
                    Box::new(Type::Generic("Option".into(), vec![a])),
                ),
            });
        }

        // channel.select: (List(Channel)) -> (Channel, a)
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("channel.select".into(), Scheme {
                vars: vec![chv, av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(ch.clone()))],
                    Box::new(Type::Tuple(vec![ch, a])),
                ),
            });
        }

        // ── task module ────────────────────────────────────────────────

        // task.spawn: (() -> a) -> Handle
        {
            let (a, av) = self.fresh_tv();
            let (h, hv) = self.fresh_tv();
            env.define("task.spawn".into(), Scheme {
                vars: vec![av, hv],
                ty: Type::Fun(
                    vec![Type::Fun(vec![], Box::new(a))],
                    Box::new(h),
                ),
            });
        }

        // task.join: (Handle) -> a
        {
            let (h, hv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("task.join".into(), Scheme {
                vars: vec![hv, av],
                ty: Type::Fun(vec![h], Box::new(a)),
            });
        }

        // task.cancel: (Handle) -> Unit
        {
            let (h, hv) = self.fresh_tv();
            env.define("task.cancel".into(), Scheme {
                vars: vec![hv],
                ty: Type::Fun(vec![h], Box::new(Type::Unit)),
            });
        }

        // ── try ────────────────────────────────────────────────────────

        // try: (() -> a) -> Result(a, String)
        {
            let (a, av) = self.fresh_tv();
            env.define("try".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Fun(vec![], Box::new(a.clone()))],
                    Box::new(Type::Generic("Result".into(), vec![a, Type::String])),
                ),
            });
        }

        // ── result module ──────────────────────────────────────────────

        // result.map_err: (Result(a,e), (e -> f)) -> Result(a,f)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            let (f, fv) = self.fresh_tv();
            env.define("result.map_err".into(), Scheme {
                vars: vec![av, ev, fv],
                ty: Type::Fun(
                    vec![
                        Type::Generic("Result".into(), vec![a.clone(), e.clone()]),
                        Type::Fun(vec![e], Box::new(f.clone())),
                    ],
                    Box::new(Type::Generic("Result".into(), vec![a, f])),
                ),
            });
        }

        // result.flatten: (Result(Result(a,e),e)) -> Result(a,e)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define("result.flatten".into(), Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![Type::Generic("Result".into(), vec![
                        Type::Generic("Result".into(), vec![a.clone(), e.clone()]),
                        e.clone(),
                    ])],
                    Box::new(Type::Generic("Result".into(), vec![a, e])),
                ),
            });
        }

        // result.is_ok: (Result(a,e)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define("result.is_ok".into(), Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![Type::Generic("Result".into(), vec![a, e])],
                    Box::new(Type::Bool),
                ),
            });
        }

        // result.is_err: (Result(a,e)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define("result.is_err".into(), Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![Type::Generic("Result".into(), vec![a, e])],
                    Box::new(Type::Bool),
                ),
            });
        }

        // ── option module ──────────────────────────────────────────────

        // option.map: (Option(a), (a -> b)) -> Option(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("option.map".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Generic("Option".into(), vec![a.clone()]),
                        Type::Fun(vec![a], Box::new(b.clone())),
                    ],
                    Box::new(Type::Generic("Option".into(), vec![b])),
                ),
            });
        }

        // option.unwrap_or: (Option(a), a) -> a
        {
            let (a, av) = self.fresh_tv();
            env.define("option.unwrap_or".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Generic("Option".into(), vec![a.clone()]),
                        a.clone(),
                    ],
                    Box::new(a),
                ),
            });
        }

        // option.to_result: (Option(a), e) -> Result(a, e)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define("option.to_result".into(), Scheme {
                vars: vec![av, ev],
                ty: Type::Fun(
                    vec![
                        Type::Generic("Option".into(), vec![a.clone()]),
                        e.clone(),
                    ],
                    Box::new(Type::Generic("Result".into(), vec![a, e])),
                ),
            });
        }

        // option.is_some: (Option(a)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define("option.is_some".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Generic("Option".into(), vec![a])],
                    Box::new(Type::Bool),
                ),
            });
        }

        // option.is_none: (Option(a)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define("option.is_none".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Generic("Option".into(), vec![a])],
                    Box::new(Type::Bool),
                ),
            });
        }
    }

    // ── Register type declarations ──────────────────────────────────

    fn register_type_decl(&mut self, td: &TypeDecl, env: &mut TypeEnv) {
        // Create a mapping from type param names to placeholder type vars
        let mut param_vars: HashMap<std::string::String, Type> = HashMap::new();
        for p in &td.params {
            let tv = self.fresh_var();
            param_vars.insert(p.clone(), tv);
        }

        match &td.body {
            TypeBody::Enum(variants) => {
                let mut variant_infos = Vec::new();

                for variant in variants {
                    let field_types: Vec<Type> = variant
                        .fields
                        .iter()
                        .map(|te| self.resolve_type_expr(te, &param_vars))
                        .collect();

                    variant_infos.push(VariantInfo {
                        name: variant.name.clone(),
                        field_types: field_types.clone(),
                    });

                    // Register the constructor in the type environment
                    let type_params: Vec<Type> = td
                        .params
                        .iter()
                        .map(|p| param_vars[p].clone())
                        .collect();

                    let result_type = if type_params.is_empty() {
                        Type::Generic(td.name.clone(), vec![])
                    } else {
                        Type::Generic(td.name.clone(), type_params)
                    };

                    let var_ids: Vec<TyVar> = td
                        .params
                        .iter()
                        .map(|p| match &param_vars[p] {
                            Type::Var(v) => *v,
                            _ => unreachable!(),
                        })
                        .collect();

                    if field_types.is_empty() {
                        // No-arg constructor is just a value
                        env.define(
                            variant.name.clone(),
                            Scheme {
                                vars: var_ids,
                                ty: result_type,
                            },
                        );
                    } else {
                        // Constructor function
                        env.define(
                            variant.name.clone(),
                            Scheme {
                                vars: var_ids,
                                ty: Type::Fun(field_types, Box::new(result_type)),
                            },
                        );
                    }

                    self.variant_to_enum
                        .insert(variant.name.clone(), td.name.clone());
                }

                self.enums.insert(
                    td.name.clone(),
                    EnumInfo {
                        _name: td.name.clone(),
                        params: td.params.clone(),
                        variants: variant_infos,
                    },
                );
            }
            TypeBody::Record(fields) => {
                let field_types: Vec<(std::string::String, Type)> = fields
                    .iter()
                    .map(|f| {
                        let ty = self.resolve_type_expr(&f.ty, &param_vars);
                        (f.name.clone(), ty)
                    })
                    .collect();

                self.records.insert(
                    td.name.clone(),
                    RecordInfo {
                        _name: td.name.clone(),
                        _params: td.params.clone(),
                        fields: field_types,
                    },
                );
            }
        }

        // Auto-derive builtin traits for user-defined types.
        // All enums and records get Equal, Compare, Hash, Display since
        // the runtime supports Eq/Ord/Hash on all Value variants.
        let dummy_span = Span { line: 0, col: 0, offset: 0 };
        for trait_name in &["Equal", "Compare", "Hash", "Display"] {
            self.trait_impls.push(TraitImplInfo {
                trait_name: trait_name.to_string(),
                target_type: td.name.clone(),
                methods: Vec::new(),
                span: dummy_span,
            });
        }
    }

    /// Resolve a TypeExpr AST node to our internal Type representation.
    fn resolve_type_expr(
        &mut self,
        te: &TypeExpr,
        param_vars: &HashMap<std::string::String, Type>,
    ) -> Type {
        match te {
            TypeExpr::Named(name) => {
                // Check if it's a type param variable
                if let Some(tv) = param_vars.get(name) {
                    return tv.clone();
                }
                match name.as_str() {
                    "Int" => Type::Int,
                    "Float" => Type::Float,
                    "Bool" => Type::Bool,
                    "String" => Type::String,
                    "List" => {
                        // List without explicit type param => List(fresh_var)
                        Type::List(Box::new(self.fresh_var()))
                    }
                    "Map" => {
                        // Map without explicit type params => Map(fresh_var, fresh_var)
                        Type::Map(Box::new(self.fresh_var()), Box::new(self.fresh_var()))
                    }
                    _ => {
                        // Could be a record or enum type with no params
                        Type::Generic(name.clone(), vec![])
                    }
                }
            }
            TypeExpr::Generic(name, args) => {
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| self.resolve_type_expr(a, param_vars))
                    .collect();
                match name.as_str() {
                    "List" if resolved_args.is_empty() => {
                        Type::List(Box::new(self.fresh_var()))
                    }
                    "List" if resolved_args.len() == 1 => {
                        Type::List(Box::new(resolved_args.into_iter().next().unwrap()))
                    }
                    "Map" if resolved_args.is_empty() => {
                        Type::Map(Box::new(self.fresh_var()), Box::new(self.fresh_var()))
                    }
                    "Map" if resolved_args.len() == 2 => {
                        let mut iter = resolved_args.into_iter();
                        Type::Map(
                            Box::new(iter.next().unwrap()),
                            Box::new(iter.next().unwrap()),
                        )
                    }
                    _ => Type::Generic(name.clone(), resolved_args),
                }
            }
            TypeExpr::Tuple(elems) => {
                let types: Vec<Type> = elems
                    .iter()
                    .map(|e| self.resolve_type_expr(e, param_vars))
                    .collect();
                Type::Tuple(types)
            }
            TypeExpr::Function(params, ret) => {
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type_expr(p, param_vars))
                    .collect();
                let ret_type = self.resolve_type_expr(ret, param_vars);
                Type::Fun(param_types, Box::new(ret_type))
            }
        }
    }

    // ── Register function declarations ──────────────────────────────

    fn register_fn_decl(&mut self, f: &FnDecl, env: &mut TypeEnv) {
        let param_map = HashMap::new();
        let mut param_types = Vec::new();

        for param in &f.params {
            let ty = if let Some(te) = &param.ty {
                self.resolve_type_expr(te, &param_map)
            } else {
                self.fresh_var()
            };
            param_types.push(ty);
        }

        let ret_type = if let Some(te) = &f.return_type {
            self.resolve_type_expr(te, &param_map)
        } else {
            self.fresh_var()
        };

        let fn_type = Type::Fun(param_types, Box::new(ret_type));
        let scheme = self.generalize(env, &fn_type);
        env.define(f.name.clone(), scheme);

        // Store where clauses mapped to parameter indices
        if !f.where_clauses.is_empty() {
            let mut indexed_clauses = Vec::new();
            for (type_param, trait_name) in &f.where_clauses {
                // Find the parameter index for this type_param name
                for (i, param) in f.params.iter().enumerate() {
                    if let Pattern::Ident(name) = &param.pattern {
                        if name == type_param {
                            indexed_clauses.push((i, trait_name.clone()));
                            break;
                        }
                    }
                }
            }
            if !indexed_clauses.is_empty() {
                self.fn_where_clauses.insert(f.name.clone(), indexed_clauses);
            }
        }
    }

    // ── Register trait declarations ─────────────────────────────────

    fn register_trait_decl(&mut self, t: &TraitDecl) {
        let methods: Vec<(std::string::String, Type)> = t
            .methods
            .iter()
            .map(|m| {
                let param_map = HashMap::new();
                let mut param_types = Vec::new();
                for param in &m.params {
                    let ty = if let Some(te) = &param.ty {
                        self.resolve_type_expr(te, &param_map)
                    } else {
                        self.fresh_var()
                    };
                    param_types.push(ty);
                }
                let ret_type = if let Some(te) = &m.return_type {
                    self.resolve_type_expr(te, &param_map)
                } else {
                    self.fresh_var()
                };
                (m.name.clone(), Type::Fun(param_types, Box::new(ret_type)))
            })
            .collect();

        self.traits.insert(
            t.name.clone(),
            TraitInfo {
                _name: t.name.clone(),
                methods,
            },
        );
    }

    // ── Register trait implementations ──────────────────────────────

    fn register_trait_impl(&mut self, ti: &TraitImpl, env: &mut TypeEnv) {
        let impl_methods: Vec<(std::string::String, usize)> = ti
            .methods
            .iter()
            .map(|m| (m.name.clone(), m.params.len()))
            .collect();
        self.trait_impls.push(TraitImplInfo {
            trait_name: ti.trait_name.clone(),
            target_type: ti.target_type.clone(),
            methods: impl_methods,
            span: ti.span,
        });

        // Register methods as "TypeName.method_name"
        for method in &ti.methods {
            let param_map = HashMap::new();
            let mut param_types = Vec::new();
            for param in &method.params {
                let ty = if let Some(te) = &param.ty {
                    self.resolve_type_expr(te, &param_map)
                } else {
                    self.fresh_var()
                };
                param_types.push(ty);
            }
            let ret_type = if let Some(te) = &method.return_type {
                self.resolve_type_expr(te, &param_map)
            } else {
                self.fresh_var()
            };

            let fn_type = Type::Fun(param_types, Box::new(ret_type));
            let key = format!("{}.{}", ti.target_type, method.name);
            let scheme = self.generalize(env, &fn_type);
            env.define(key, scheme);
        }
    }

    // ── Check function body ─────────────────────────────────────────

    fn check_fn_body(&mut self, f: &mut FnDecl, env: &TypeEnv) {
        let mut local_env = env.child();

        // Validate where clauses
        for (type_param, trait_name) in &f.where_clauses {
            if !self.traits.contains_key(trait_name) {
                self.error(
                    format!("unknown trait '{}' in where clause for '{}'", trait_name, type_param),
                    f.span,
                );
            }
        }

        // Look up the function's registered type and instantiate it
        let fn_scheme = match env.lookup(&f.name) {
            Some(s) => s.clone(),
            None => return, // already reported
        };
        let fn_type = self.instantiate(&fn_scheme);
        let fn_type = self.apply(&fn_type);

        let (param_types, ret_type) = match &fn_type {
            Type::Fun(params, ret) => (params.clone(), *ret.clone()),
            _ => return,
        };

        // Bind parameters
        for (i, param) in f.params.iter().enumerate() {
            if let Some(ty) = param_types.get(i) {
                self.bind_pattern(&param.pattern, ty, &mut local_env);
            }
        }

        // Infer the body and unify with declared return type
        let body_type = self.infer_expr(&mut f.body, &mut local_env);
        self.unify(&body_type, &ret_type, f.body.span);
    }

    // ── Pattern type binding ────────────────────────────────────────

    /// Bind names in a pattern to their types in the environment.
    fn bind_pattern(&mut self, pattern: &Pattern, ty: &Type, env: &mut TypeEnv) {
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Ident(name) => {
                env.define(name.clone(), Scheme::mono(ty.clone()));
            }
            Pattern::Int(_) => {}
            Pattern::Float(_) => {}
            Pattern::Bool(_) => {}
            Pattern::StringLit(_) => {}
            Pattern::Tuple(pats) => {
                let resolved = self.apply(ty);
                match &resolved {
                    Type::Tuple(elems) => {
                        for (p, t) in pats.iter().zip(elems.iter()) {
                            self.bind_pattern(p, t, env);
                        }
                    }
                    _ => {
                        // Create fresh vars for each sub-pattern
                        for p in pats {
                            let tv = self.fresh_var();
                            self.bind_pattern(p, &tv, env);
                        }
                    }
                }
            }
            Pattern::Constructor(name, sub_pats) => {
                // Look up the constructor to find inner types
                let resolved = self.apply(ty);
                if let Some(enum_name) = self.variant_to_enum.get(name).cloned() {
                    if let Some(enum_info) = self.enums.get(&enum_name).cloned() {
                        if let Some(var_info) = enum_info
                            .variants
                            .iter()
                            .find(|v| v.name == *name)
                        {
                            // For parameterized types we need to figure out type args
                            // from the outer type and substitute them in
                            let type_args = match &resolved {
                                Type::Generic(_, args) => args.clone(),
                                _ => enum_info.params.iter().map(|_| self.fresh_var()).collect(),
                            };
                            for (i, sp) in sub_pats.iter().enumerate() {
                                if i < var_info.field_types.len() {
                                    let field_ty = substitute_enum_params(
                                        &var_info.field_types[i],
                                        &enum_info.params,
                                        &type_args,
                                    );
                                    self.bind_pattern(sp, &field_ty, env);
                                } else {
                                    let tv = self.fresh_var();
                                    self.bind_pattern(sp, &tv, env);
                                }
                            }
                            return;
                        }
                    }
                }
                // Fallback: bind sub-patterns with fresh vars
                for sp in sub_pats {
                    let tv = self.fresh_var();
                    self.bind_pattern(sp, &tv, env);
                }
            }
            Pattern::List(pats, rest) => {
                let elem_ty = self.fresh_var();
                let list_ty = Type::List(Box::new(elem_ty.clone()));
                self.unify(ty, &list_ty, Span { line: 0, col: 0, offset: 0 });
                let resolved_elem = self.apply(&elem_ty);
                for p in pats {
                    self.bind_pattern(p, &resolved_elem, env);
                }
                if let Some(rest_pat) = rest {
                    let rest_ty = Type::List(Box::new(resolved_elem));
                    self.bind_pattern(rest_pat, &rest_ty, env);
                }
            }
            Pattern::Record { fields, .. } => {
                let resolved = self.apply(ty);
                if let Type::Record(_, field_types) = &resolved {
                    for (field_name, sub_pat) in fields {
                        if let Some((_, ft)) =
                            field_types.iter().find(|(n, _)| n == field_name)
                        {
                            if let Some(sp) = sub_pat {
                                self.bind_pattern(sp, ft, env);
                            } else {
                                // Shorthand: field name is also the binding
                                env.define(
                                    field_name.clone(),
                                    Scheme::mono(ft.clone()),
                                );
                            }
                        } else {
                            if let Some(sp) = sub_pat {
                                let tv = self.fresh_var();
                                self.bind_pattern(sp, &tv, env);
                            } else {
                                let tv = self.fresh_var();
                                env.define(
                                    field_name.clone(),
                                    Scheme::mono(tv),
                                );
                            }
                        }
                    }
                } else {
                    // Bind with fresh vars
                    for (field_name, sub_pat) in fields {
                        if let Some(sp) = sub_pat {
                            let tv = self.fresh_var();
                            self.bind_pattern(sp, &tv, env);
                        } else {
                            let tv = self.fresh_var();
                            env.define(field_name.clone(), Scheme::mono(tv));
                        }
                    }
                }
            }
            Pattern::Or(alts) => {
                for alt in alts {
                    self.bind_pattern(alt, ty, env);
                }
            }
            Pattern::Range(_, _) => {
                self.unify(ty, &Type::Int, Span { line: 0, col: 0, offset: 0 });
            }
            Pattern::Map(entries) => {
                let val_ty = self.fresh_var();
                let map_ty = Type::Map(Box::new(Type::String), Box::new(val_ty.clone()));
                self.unify(ty, &map_ty, Span { line: 0, col: 0, offset: 0 });
                let resolved_val = self.apply(&val_ty);
                for (_key, pat) in entries {
                    self.bind_pattern(pat, &resolved_val, env);
                }
            }
            Pattern::Pin(name) => {
                // Pin does not introduce a new binding — it checks against an
                // existing variable.  Look it up in the parent (pre-match) scope
                // first, then fall back to the current scope for when/let contexts.
                let found = env.parent.as_ref().and_then(|p| p.lookup(name).cloned())
                    .or_else(|| env.lookup(name).cloned());
                if let Some(scheme) = found {
                    let pinned_ty = self.instantiate(&scheme);
                    self.unify(ty, &pinned_ty, Span { line: 0, col: 0, offset: 0 });
                }
                // If not found, we just skip — the runtime will handle the missing variable.
            }
        }
    }

    // ── Expression type inference ───────────────────────────────────

    fn infer_expr(&mut self, expr: &mut Expr, env: &mut TypeEnv) -> Type {
        let span = expr.span;
        let ty = match &mut expr.kind {
            ExprKind::Int(_) => Type::Int,
            ExprKind::Float(_) => Type::Float,
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::StringLit(_) => Type::String,
            ExprKind::Unit => Type::Unit,

            ExprKind::StringInterp(parts) => {
                // Each part is either a literal or an expression
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        let _t = self.infer_expr(e, env);
                        // In principle, we should check that Display is implemented,
                        // but we keep it lenient for now.
                    }
                }
                Type::String
            }

            ExprKind::List(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::List(Box::new(tv))
                } else {
                    let mut iter = elems.iter_mut();
                    let first_elem = iter.next().unwrap();
                    let first = self.infer_expr(first_elem, env);
                    for elem in iter {
                        let elem_span = elem.span;
                        let t = self.infer_expr(elem, env);
                        self.unify(&first, &t, elem_span);
                    }
                    Type::List(Box::new(first))
                }
            }

            ExprKind::Map(entries) => {
                if entries.is_empty() {
                    let k = self.fresh_var();
                    let v = self.fresh_var();
                    Type::Map(Box::new(k), Box::new(v))
                } else {
                    let mut iter = entries.iter_mut();
                    let first_entry = iter.next().unwrap();
                    let first_k = self.infer_expr(&mut first_entry.0, env);
                    let first_v = self.infer_expr(&mut first_entry.1, env);
                    for (k, v) in iter {
                        let k_span = k.span;
                        let v_span = v.span;
                        let kt = self.infer_expr(k, env);
                        let vt = self.infer_expr(v, env);
                        self.unify(&first_k, &kt, k_span);
                        self.unify(&first_v, &vt, v_span);
                    }
                    Type::Map(Box::new(first_k), Box::new(first_v))
                }
            }

            ExprKind::Tuple(elems) => {
                let types: Vec<Type> = elems
                    .iter_mut()
                    .map(|e| self.infer_expr(e, env))
                    .collect();
                Type::Tuple(types)
            }

            ExprKind::Ident(name) => {
                let name = name.clone();
                if let Some(scheme) = env.lookup(&name) {
                    let scheme = scheme.clone();
                    self.instantiate(&scheme)
                } else {
                    // Unknown variable - could be from an unresolved import
                    // Warn unless it looks like a module-qualified name or `self`
                    if !name.contains('.') && name != "self" {
                        self.warning(
                            format!("possibly undefined variable '{name}'"),
                            span,
                        );
                    }
                    self.fresh_var()
                }
            }

            ExprKind::FieldAccess(obj, field) => {
                let field = field.clone();
                // Capture module name before mutable borrow for inference
                let module_name = if let ExprKind::Ident(n) = &obj.kind { Some(n.clone()) } else { None };

                // Could be record.field, or module.function
                let obj_ty = self.infer_expr(obj, env);
                let obj_ty = self.apply(&obj_ty);

                // Check for module-style access first (e.g., string.split)
                if let Some(ref module_name) = module_name {
                    let qualified = format!("{module_name}.{field}");
                    if let Some(scheme) = env.lookup(&qualified) {
                        let scheme = scheme.clone();
                        let result = self.instantiate(&scheme);
                        let resolved = self.apply(&result);
                        expr.ty = Some(resolved.clone());
                        return resolved;
                    }
                }

                // Record field access
                match &obj_ty {
                    Type::Record(rec_name, fields) => {
                        if let Some((_, ft)) = fields.iter().find(|(n, _)| n == &field) {
                            ft.clone()
                        } else {
                            self.error(
                                format!("record {rec_name} has no field {field}"),
                                span,
                            );
                            Type::Error
                        }
                    }
                    Type::Generic(type_name, _) => {
                        // Check if the type has a record definition
                        if let Some(rec_info) = self.records.get(type_name).cloned() {
                            if let Some((_, ft)) =
                                rec_info.fields.iter().find(|(n, _)| n == &field)
                            {
                                let resolved = self.apply(&ft);
                                expr.ty = Some(resolved.clone());
                                return resolved;
                            }
                        }
                        // Could be a trait method: TypeName.method
                        let key = format!("{type_name}.{field}");
                        if let Some(scheme) = env.lookup(&key) {
                            let scheme = scheme.clone();
                            let result = self.instantiate(&scheme);
                            let resolved = self.apply(&result);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // Lenient: return fresh var
                        self.fresh_var()
                    }
                    _ => {
                        // Try to find a trait method dynamically
                        // For now, be lenient and return a fresh var
                        self.fresh_var()
                    }
                }
            }

            ExprKind::Binary(lhs, op, rhs) => {
                let op = *op;
                let lhs_span = lhs.span;
                let rhs_span = rhs.span;
                let lt = self.infer_expr(lhs, env);
                let rt = self.infer_expr(rhs, env);

                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        self.unify(&lt, &rt, span);
                        lt
                    }
                    BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq => {
                        self.unify(&lt, &rt, span);
                        Type::Bool
                    }
                    BinOp::And | BinOp::Or => {
                        self.unify(&lt, &Type::Bool, lhs_span);
                        self.unify(&rt, &Type::Bool, rhs_span);
                        Type::Bool
                    }
                }
            }

            ExprKind::Unary(op, operand) => {
                let op = *op;
                let operand_span = operand.span;
                let t = self.infer_expr(operand, env);
                match op {
                    UnaryOp::Neg => t,
                    UnaryOp::Not => {
                        self.unify(&t, &Type::Bool, operand_span);
                        Type::Bool
                    }
                }
            }

            ExprKind::Pipe(lhs, rhs) => {
                let lhs_span = lhs.span;
                let arg_type = self.infer_expr(lhs, env);

                // Pipe semantics: a |> f(b) means f(a, b)
                // If the RHS is a Call, we prepend the pipe LHS as the first argument.
                // Check if rhs is a Call before mutable borrow
                let rhs_is_call = matches!(&rhs.kind, ExprKind::Call(..));

                if rhs_is_call {
                    // Destructure rhs.kind mutably to get at callee and call_args
                    if let ExprKind::Call(callee, call_args) = &mut rhs.kind {
                        // Capture callee name for where clause check
                        let callee_fn_name = if let ExprKind::Ident(n) = &callee.kind { Some(n.clone()) } else { None };
                        // Capture arg spans before mutable inference
                        let arg_spans: Vec<Span> = call_args.iter().map(|a| a.span).collect();

                        let callee_ty = self.infer_expr(callee, env);
                        let callee_ty = self.apply(&callee_ty);

                        // Infer types for the explicit call args
                        let explicit_arg_types: Vec<Type> = call_args
                            .iter_mut()
                            .map(|a| self.infer_expr(a, env))
                            .collect();

                        // All args = [pipe_lhs, ...explicit_args]
                        let mut all_arg_types = vec![arg_type];
                        all_arg_types.extend(explicit_arg_types);

                        let result_ty = match &callee_ty {
                            Type::Fun(params, ret) => {
                                let min_len = params.len().min(all_arg_types.len());
                                for i in 0..min_len {
                                    let s = if i == 0 { lhs_span } else { arg_spans[i - 1] };
                                    self.unify(&all_arg_types[i], &params[i], s);
                                }
                                *ret.clone()
                            }
                            Type::Var(_) => {
                                let ret = self.fresh_var();
                                let fn_ty = Type::Fun(all_arg_types.clone(), Box::new(ret.clone()));
                                self.unify(&callee_ty, &fn_ty, span);
                                ret
                            }
                            _ => self.fresh_var(),
                        };

                        // Check where clause constraints for piped calls
                        if let Some(ref fn_name) = callee_fn_name {
                            if let Some(clauses) = self.fn_where_clauses.get(fn_name).cloned() {
                                for (param_idx, trait_name) in &clauses {
                                    if let Some(arg_ty) = all_arg_types.get(*param_idx) {
                                        let resolved = self.apply(arg_ty);
                                        if let Some(type_name) = self.type_name_for_impl(&resolved) {
                                            let has_impl = self.trait_impls.iter().any(|imp| {
                                                imp.trait_name == *trait_name && imp.target_type == type_name
                                            });
                                            if !has_impl {
                                                self.error(
                                                    format!(
                                                        "type '{}' does not implement trait '{}'",
                                                        type_name, trait_name
                                                    ),
                                                    span,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        result_ty
                    } else {
                        unreachable!()
                    }
                } else {
                    // RHS is a plain function/lambda, not a call
                    let fn_type = self.infer_expr(rhs, env);
                    let fn_type = self.apply(&fn_type);

                    match &fn_type {
                        Type::Fun(params, ret) => {
                            if !params.is_empty() {
                                self.unify(&arg_type, &params[0], span);
                            }
                            *ret.clone()
                        }
                        Type::Var(_) => {
                            let ret = self.fresh_var();
                            let fn_ty = Type::Fun(vec![arg_type], Box::new(ret.clone()));
                            self.unify(&fn_type, &fn_ty, span);
                            ret
                        }
                        _ => self.fresh_var(),
                    }
                }
            }

            ExprKind::Range(start, end) => {
                let start_span = start.span;
                let end_span = end.span;
                let st = self.infer_expr(start, env);
                let et = self.infer_expr(end, env);
                self.unify(&st, &Type::Int, start_span);
                self.unify(&et, &Type::Int, end_span);
                Type::List(Box::new(Type::Int))
            }

            ExprKind::QuestionMark(inner) => {
                let inner_ty = self.infer_expr(inner, env);
                let inner_ty = self.apply(&inner_ty);

                // ? operator on Result(a,e) returns a, propagates Err(e)
                // ? operator on Option(a) returns a, propagates None
                match &inner_ty {
                    Type::Generic(name, args) if name == "Result" && args.len() == 2 => {
                        args[0].clone()
                    }
                    Type::Generic(name, args) if name == "Option" && args.len() == 1 => {
                        args[0].clone()
                    }
                    _ => {
                        // Lenient: return fresh var
                        self.fresh_var()
                    }
                }
            }

            ExprKind::Call(callee, args) => {
                // Capture callee name and arg spans before mutable inference
                let callee_fn_name = if let ExprKind::Ident(n) = &callee.kind { Some(n.clone()) } else { None };
                let is_method_call = matches!(&callee.kind, ExprKind::FieldAccess(..));
                let arg_spans: Vec<Span> = args.iter().map(|a| a.span).collect();

                let callee_ty = self.infer_expr(callee, env);
                let callee_ty = self.apply(&callee_ty);

                let arg_types: Vec<Type> = args
                    .iter_mut()
                    .map(|a| self.infer_expr(a, env))
                    .collect();

                let result_ty = match &callee_ty {
                    Type::Fun(params, ret) => {
                        // Unify argument types with parameter types
                        let min_len = params.len().min(arg_types.len());
                        for i in 0..min_len {
                            self.unify(&arg_types[i], &params[i], arg_spans[i]);
                        }
                        // Check arity. For method calls (obj.method(...)),
                        // the type signature includes `self` but the call
                        // site does not, so allow a difference of exactly 1.
                        let arity_ok = if is_method_call {
                            arg_types.len() == params.len()
                                || arg_types.len() + 1 == params.len()
                        } else {
                            arg_types.len() == params.len()
                        };
                        if !arity_ok {
                            self.warning(
                                format!(
                                    "function expects {} argument(s), got {}",
                                    params.len(),
                                    arg_types.len()
                                ),
                                span,
                            );
                        }
                        *ret.clone()
                    }
                    Type::Var(_) => {
                        // The callee is an unresolved type variable - create a function type
                        let ret = self.fresh_var();
                        let fn_ty = Type::Fun(arg_types.clone(), Box::new(ret.clone()));
                        self.unify(&callee_ty, &fn_ty, span);
                        ret
                    }
                    _ => {
                        // Lenient: might be a constructor or something we can't resolve
                        self.fresh_var()
                    }
                };

                // Check where clause constraints
                if let Some(ref fn_name) = callee_fn_name {
                    if let Some(clauses) = self.fn_where_clauses.get(fn_name).cloned() {
                        for (param_idx, trait_name) in &clauses {
                            if let Some(arg_ty) = arg_types.get(*param_idx) {
                                let resolved = self.apply(arg_ty);
                                if let Some(type_name) = self.type_name_for_impl(&resolved) {
                                    let has_impl = self.trait_impls.iter().any(|imp| {
                                        imp.trait_name == *trait_name && imp.target_type == type_name
                                    });
                                    if !has_impl {
                                        self.error(
                                            format!(
                                                "type '{}' does not implement trait '{}'",
                                                type_name, trait_name
                                            ),
                                            span,
                                        );
                                    }
                                }
                                // If type_name_for_impl returns None, the type is
                                // unresolved — skip the check (lenient).
                            }
                        }
                    }
                }

                result_ty
            }

            ExprKind::Lambda { params, body } => {
                let mut local_env = env.child();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        let ty = if let Some(te) = &p.ty {
                            self.resolve_type_expr(te, &HashMap::new())
                        } else {
                            self.fresh_var()
                        };
                        self.bind_pattern(&p.pattern, &ty, &mut local_env);
                        ty
                    })
                    .collect();

                let body_type = self.infer_expr(body, &mut local_env);
                Type::Fun(param_types, Box::new(body_type))
            }

            ExprKind::RecordCreate { name, fields } => {
                let name = name.clone();
                if let Some(rec_info) = self.records.get(&name).cloned() {
                    let field_types: Vec<(std::string::String, Type)> = fields
                        .iter_mut()
                        .map(|(n, e)| {
                            let ty = self.infer_expr(e, env);
                            (n.clone(), ty)
                        })
                        .collect();

                    // Unify with declared field types
                    for (field_name, inferred_ty) in &field_types {
                        if let Some((_, declared_ty)) =
                            rec_info.fields.iter().find(|(n, _)| n == field_name)
                        {
                            self.unify(inferred_ty, declared_ty, span);
                        }
                    }

                    Type::Record(name, rec_info.fields.clone())
                } else {
                    // Unknown record type - infer from fields
                    let field_types: Vec<(std::string::String, Type)> = fields
                        .iter_mut()
                        .map(|(n, e)| {
                            let ty = self.infer_expr(e, env);
                            (n.clone(), ty)
                        })
                        .collect();
                    Type::Record(name, field_types)
                }
            }

            ExprKind::RecordUpdate { expr: base, fields } => {
                let base_ty = self.infer_expr(base, env);
                // Infer the field types but the result is the same as the base
                for (_, field_expr) in fields {
                    let _ft = self.infer_expr(field_expr, env);
                }
                base_ty
            }

            ExprKind::Match { expr: scrutinee, arms } => {
                match scrutinee {
                    Some(scrutinee) => {
                        let scrutinee_span = scrutinee.span;
                        let scrutinee_ty = self.infer_expr(scrutinee, env);
                        let result_ty = self.fresh_var();

                        for arm in arms.iter_mut() {
                            let mut arm_env = env.child();
                            self.check_pattern(&arm.pattern, &scrutinee_ty, &mut arm_env, scrutinee_span);

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, &mut arm_env);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        // Check exhaustiveness after pattern checking, so the
                        // scrutinee type is fully resolved through unification.
                        let resolved_scrutinee_ty = self.apply(&scrutinee_ty);
                        self.check_exhaustiveness(arms, &resolved_scrutinee_ty, scrutinee_span);

                        result_ty
                    }
                    None => {
                        // Guardless match: each arm's guard is a boolean condition
                        let result_ty = self.fresh_var();

                        for arm in arms.iter_mut() {
                            let mut arm_env = env.child();

                            if let Some(ref mut guard) = arm.guard {
                                let guard_span = guard.span;
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard_span);
                            }

                            let body_span = arm.body.span;
                            let arm_ty = self.infer_expr(&mut arm.body, &mut arm_env);
                            self.unify(&result_ty, &arm_ty, body_span);
                        }

                        // No exhaustiveness checking for guardless match
                        result_ty
                    }
                }
            }

            ExprKind::Return(maybe_expr) => {
                if let Some(e) = maybe_expr {
                    self.infer_expr(e, env)
                } else {
                    Type::Unit
                }
            }

            ExprKind::Block(stmts) => {
                let mut last_ty = Type::Unit;
                let mut block_env = env.child();

                for stmt in stmts {
                    last_ty = self.infer_stmt(stmt, &mut block_env);
                }

                last_ty
            }

            ExprKind::Loop { bindings, body } => {
                let mut loop_env = env.child();
                let binding_count = bindings.len();
                for i in 0..binding_count {
                    let ty = self.infer_expr(&mut bindings[i].1, env);
                    let name = bindings[i].0.clone();
                    loop_env.define(name, Scheme::mono(ty));
                }
                let prev_loop = self.loop_binding_count;
                self.loop_binding_count = Some(binding_count);
                let result = self.infer_expr(body, &mut loop_env);
                self.loop_binding_count = prev_loop;
                result
            }

            ExprKind::Recur(args) => {
                let recur_count = args.len();
                for arg in args {
                    let _ty = self.infer_expr(arg, env);
                }
                if let Some(expected) = self.loop_binding_count {
                    if recur_count != expected {
                        self.warning(
                            format!(
                                "loop has {} binding(s), but recur supplies {} argument(s)",
                                expected,
                                recur_count
                            ),
                            span,
                        );
                    }
                }
                self.fresh_var()
            }

        };
        let resolved = self.apply(&ty);
        expr.ty = Some(resolved.clone());
        resolved
    }

    // ── Statement type inference ────────────────────────────────────

    fn infer_stmt(&mut self, stmt: &mut Stmt, env: &mut TypeEnv) -> Type {
        match stmt {
            Stmt::Let { pattern, ty, value } => {
                let value_span = value.span;
                let val_ty = self.infer_expr(value, env);

                if let Some(te) = &ty {
                    let declared = self.resolve_type_expr(te, &HashMap::new());
                    self.unify(&val_ty, &declared, value_span);
                }

                // Generalize for let-polymorphism
                let scheme = self.generalize(env, &val_ty);

                // Bind names in the pattern
                // For let-polymorphism we need to bind with the generalized scheme
                match pattern {
                    Pattern::Ident(name) => {
                        env.define(name.clone(), scheme);
                    }
                    _ => {
                        self.bind_pattern(pattern, &val_ty, env);
                    }
                }

                Type::Unit
            }

            Stmt::When { pattern, expr, else_body } => {
                let expr_ty = self.infer_expr(expr, env);

                // Type check the else body
                let _else_ty = self.infer_expr(else_body, env);

                // Bind the pattern in the current scope (type narrowing)
                self.bind_pattern(pattern, &expr_ty, env);

                // For constructor patterns, narrow the type
                // e.g., when Ok(value) = expr, value has the inner type
                if let Pattern::Constructor(name, sub_pats) = pattern {
                    let expr_ty = self.apply(&expr_ty);
                    if let Some(enum_name) = self.variant_to_enum.get(name).cloned() {
                        if let Some(enum_info) = self.enums.get(&enum_name).cloned() {
                            if let Some(var_info) = enum_info
                                .variants
                                .iter()
                                .find(|v| v.name == *name)
                            {
                                let type_args = match &expr_ty {
                                    Type::Generic(_, args) => args.clone(),
                                    _ => enum_info
                                        .params
                                        .iter()
                                        .map(|_| self.fresh_var())
                                        .collect(),
                                };
                                for (i, sp) in sub_pats.iter().enumerate() {
                                    if i < var_info.field_types.len() {
                                        let field_ty = substitute_enum_params(
                                            &var_info.field_types[i],
                                            &enum_info.params,
                                            &type_args,
                                        );
                                        self.bind_pattern(sp, &field_ty, env);
                                    }
                                }
                            }
                        }
                    }
                }

                Type::Unit
            }

            Stmt::Expr(expr) => self.infer_expr(expr, env),
        }
    }

    // ── Pattern checking (type check, not just bind) ────────────────

    fn check_pattern(
        &mut self,
        pattern: &Pattern,
        expected: &Type,
        env: &mut TypeEnv,
        span: Span,
    ) {
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Ident(name) => {
                env.define(name.clone(), Scheme::mono(expected.clone()));
            }
            Pattern::Int(_) => {
                self.unify(expected, &Type::Int, span);
            }
            Pattern::Float(_) => {
                self.unify(expected, &Type::Float, span);
            }
            Pattern::Bool(_) => {
                self.unify(expected, &Type::Bool, span);
            }
            Pattern::StringLit(_) => {
                self.unify(expected, &Type::String, span);
            }
            Pattern::Tuple(pats) => {
                let elem_types: Vec<Type> =
                    pats.iter().map(|_| self.fresh_var()).collect();
                let tuple_ty = Type::Tuple(elem_types.clone());
                self.unify(expected, &tuple_ty, span);

                for (p, t) in pats.iter().zip(elem_types.iter()) {
                    self.check_pattern(p, t, env, span);
                }
            }
            Pattern::Constructor(name, sub_pats) => {
                // Look up the constructor type
                if let Some(scheme) = env.lookup(name).cloned() {
                    let ctor_ty = self.instantiate(&scheme);
                    let ctor_ty = self.apply(&ctor_ty);

                    match &ctor_ty {
                        Type::Fun(params, ret) => {
                            self.unify(expected, ret, span);
                            for (i, sp) in sub_pats.iter().enumerate() {
                                if i < params.len() {
                                    self.check_pattern(sp, &params[i], env, span);
                                }
                            }
                        }
                        _ => {
                            // Zero-arg constructor
                            if sub_pats.is_empty() {
                                self.unify(expected, &ctor_ty, span);
                            }
                        }
                    }
                } else {
                    // Unknown constructor — bind sub-patterns with fresh vars
                    for sp in sub_pats {
                        let tv = self.fresh_var();
                        self.check_pattern(sp, &tv, env, span);
                    }
                }
            }
            Pattern::List(pats, rest) => {
                let elem_ty = self.fresh_var();
                let list_ty = Type::List(Box::new(elem_ty.clone()));
                self.unify(expected, &list_ty, span);
                let resolved_elem = self.apply(&elem_ty);
                for p in pats {
                    self.check_pattern(p, &resolved_elem, env, span);
                }
                if let Some(rest_pat) = rest {
                    let rest_ty = Type::List(Box::new(resolved_elem));
                    self.check_pattern(rest_pat, &rest_ty, env, span);
                }
            }
            Pattern::Record { name, fields, .. } => {
                if let Some(rec_name) = name {
                    if let Some(rec_info) = self.records.get(rec_name).cloned() {
                        let rec_ty = Type::Record(
                            rec_name.clone(),
                            rec_info.fields.clone(),
                        );
                        self.unify(expected, &rec_ty, span);

                        for (field_name, sub_pat) in fields {
                            if let Some((_, ft)) =
                                rec_info.fields.iter().find(|(n, _)| n == field_name)
                            {
                                if let Some(sp) = sub_pat {
                                    self.check_pattern(sp, ft, env, span);
                                } else {
                                    env.define(
                                        field_name.clone(),
                                        Scheme::mono(ft.clone()),
                                    );
                                }
                            }
                        }
                    }
                } else {
                    // Anonymous record pattern — bind fields
                    for (field_name, sub_pat) in fields {
                        let tv = self.fresh_var();
                        if let Some(sp) = sub_pat {
                            self.check_pattern(sp, &tv, env, span);
                        } else {
                            env.define(field_name.clone(), Scheme::mono(tv));
                        }
                    }
                }
            }
            Pattern::Or(alts) => {
                for alt in alts {
                    self.check_pattern(alt, expected, env, span);
                }
            }
            Pattern::Range(_, _) => {
                self.unify(expected, &Type::Int, span);
            }
            Pattern::Map(entries) => {
                let val_ty = self.fresh_var();
                let map_ty = Type::Map(Box::new(Type::String), Box::new(val_ty.clone()));
                self.unify(expected, &map_ty, span);
                let resolved_val = self.apply(&val_ty);
                for (_key, pat) in entries {
                    self.check_pattern(pat, &resolved_val, env, span);
                }
            }
            Pattern::Pin(name) => {
                // Look up the pinned variable in the parent (pre-match) scope,
                // falling back to current scope for when/let contexts.
                let found = env.parent.as_ref().and_then(|p| p.lookup(name).cloned())
                    .or_else(|| env.lookup(name).cloned());
                if let Some(scheme) = found {
                    let pinned_ty = self.instantiate(&scheme);
                    self.unify(expected, &pinned_ty, span);
                }
            }
        }
    }

    // ── Exhaustiveness checking ─────────────────────────────────────

    fn check_exhaustiveness(
        &mut self,
        arms: &[MatchArm],
        scrutinee_ty: &Type,
        span: Span,
    ) {
        // If any arm has a wildcard or an identifier pattern (which matches anything),
        // the match is exhaustive (for purposes of this analysis).
        let has_catch_all = arms.iter().any(|arm| {
            arm.guard.is_none()
                && matches!(arm.pattern, Pattern::Wildcard | Pattern::Ident(_))
        });
        if has_catch_all {
            return;
        }

        let scrutinee_ty = self.apply(scrutinee_ty);

        // For enum types, check that all variants are covered
        let enum_name = match &scrutinee_ty {
            Type::Generic(name, _) => Some(name.clone()),
            _ => None,
        };

        if let Some(ref enum_name) = enum_name {
            if let Some(enum_info) = self.enums.get(enum_name).cloned() {
                let required_variants: Vec<&str> = enum_info
                    .variants
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect();

                let mut covered_variants: Vec<std::string::String> = Vec::new();

                fn collect_constructors(pat: &Pattern, covered: &mut Vec<std::string::String>) {
                    match pat {
                        Pattern::Constructor(name, _) => {
                            if !covered.contains(name) {
                                covered.push(name.clone());
                            }
                        }
                        Pattern::Or(alts) => {
                            for alt in alts {
                                collect_constructors(alt, covered);
                            }
                        }
                        _ => {}
                    }
                }

                for arm in arms {
                    match &arm.pattern {
                        Pattern::Wildcard | Pattern::Ident(_) => {
                            if arm.guard.is_none() {
                                // catch-all, already handled above
                                return;
                            }
                        }
                        _ => collect_constructors(&arm.pattern, &mut covered_variants),
                    }
                }

                let missing: Vec<&str> = required_variants
                    .iter()
                    .filter(|v| !covered_variants.iter().any(|c| c == *v))
                    .copied()
                    .collect();

                if !missing.is_empty() {
                    self.error(
                        format!(
                            "non-exhaustive match on {enum_name}: missing variant(s) {}",
                            missing.join(", ")
                        ),
                        span,
                    );
                }
            }
        }

        // For Bool type, check true/false coverage
        if matches!(&scrutinee_ty, Type::Bool) {
            let mut has_true = false;
            let mut has_false = false;
            fn collect_bools(pat: &Pattern, has_true: &mut bool, has_false: &mut bool) {
                match pat {
                    Pattern::Bool(true) => *has_true = true,
                    Pattern::Bool(false) => *has_false = true,
                    Pattern::Or(alts) => {
                        for alt in alts {
                            collect_bools(alt, has_true, has_false);
                        }
                    }
                    _ => {}
                }
            }

            for arm in arms {
                match &arm.pattern {
                    Pattern::Wildcard | Pattern::Ident(_) => {
                        if arm.guard.is_none() {
                            return;
                        }
                    }
                    _ => collect_bools(&arm.pattern, &mut has_true, &mut has_false),
                }
            }
            if !has_true || !has_false {
                let mut missing = Vec::new();
                if !has_true {
                    missing.push("true");
                }
                if !has_false {
                    missing.push("false");
                }
                self.error(
                    format!(
                        "non-exhaustive match on Bool: missing {}",
                        missing.join(", ")
                    ),
                    span,
                );
            }
        }

        // For tuple patterns containing constructors, do basic checking
        // For Int/Float/String, we can't easily check exhaustiveness
        // unless there's a wildcard, which we already checked above.
        // Guards make arms potentially non-covering, so we warn if ALL
        // arms have guards and there's no catch-all.
        if arms.iter().all(|a| a.guard.is_some()) {
            self.error(
                "match may be non-exhaustive: all arms have guards".into(),
                span,
            );
        }
    }

    // ── Post-inference type resolution ─────────────────────────────────

    /// After all passes, walk the AST and resolve any remaining type variables
    /// in the `expr.ty` annotations using the final substitution.
    fn resolve_all_types(&self, program: &mut Program) {
        for decl in &mut program.decls {
            match decl {
                Decl::Fn(f) => self.resolve_expr_types(&mut f.body),
                Decl::TraitImpl(ti) => {
                    for m in &mut ti.methods {
                        self.resolve_expr_types(&mut m.body);
                    }
                }
                _ => {}
            }
        }
    }

    fn resolve_expr_types(&self, expr: &mut Expr) {
        if let Some(ty) = &expr.ty {
            expr.ty = Some(self.apply(ty));
        }
        match &mut expr.kind {
            ExprKind::Binary(l, _, r) => {
                self.resolve_expr_types(l);
                self.resolve_expr_types(r);
            }
            ExprKind::Unary(_, e) | ExprKind::QuestionMark(e) | ExprKind::Return(Some(e)) => {
                self.resolve_expr_types(e);
            }
            ExprKind::Call(callee, args) => {
                self.resolve_expr_types(callee);
                for a in args { self.resolve_expr_types(a); }
            }
            ExprKind::List(elems) => {
                for e in elems { self.resolve_expr_types(e); }
            }
            ExprKind::Tuple(elems) => {
                for e in elems { self.resolve_expr_types(e); }
            }
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.resolve_expr_types(k);
                    self.resolve_expr_types(v);
                }
            }
            ExprKind::Lambda { body, .. } => {
                self.resolve_expr_types(body);
            }
            ExprKind::Match { expr: scrutinee, arms } => {
                if let Some(s) = scrutinee { self.resolve_expr_types(s); }
                for arm in arms {
                    if let Some(ref mut guard) = arm.guard {
                        self.resolve_expr_types(guard);
                    }
                    self.resolve_expr_types(&mut arm.body);
                }
            }
            ExprKind::Block(stmts) => {
                for stmt in stmts {
                    match stmt {
                        Stmt::Let { value, .. } => self.resolve_expr_types(value),
                        Stmt::When { expr, else_body, .. } => {
                            self.resolve_expr_types(expr);
                            self.resolve_expr_types(else_body);
                        }
                        Stmt::Expr(e) => self.resolve_expr_types(e),
                    }
                }
            }
            ExprKind::Pipe(l, r) => {
                self.resolve_expr_types(l);
                self.resolve_expr_types(r);
            }
            ExprKind::Range(l, r) => {
                self.resolve_expr_types(l);
                self.resolve_expr_types(r);
            }
            ExprKind::FieldAccess(e, _) => self.resolve_expr_types(e),
            ExprKind::RecordCreate { fields, .. } => {
                for (_, e) in fields { self.resolve_expr_types(e); }
            }
            ExprKind::RecordUpdate { expr, fields } => {
                self.resolve_expr_types(expr);
                for (_, e) in fields { self.resolve_expr_types(e); }
            }
            ExprKind::StringInterp(parts) => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.resolve_expr_types(e);
                    }
                }
            }
            ExprKind::Loop { bindings, body } => {
                for (_, e) in bindings { self.resolve_expr_types(e); }
                self.resolve_expr_types(body);
            }
            ExprKind::Recur(args) => {
                for a in args { self.resolve_expr_types(a); }
            }
            _ => {} // Int, Float, Bool, StringLit, Ident, Unit, Return(None)
        }
    }
}

// ── Helper functions ────────────────────────────────────────────────

/// Check if a type variable occurs in a type (occurs check for unification).
fn occurs_in(var: TyVar, ty: &Type) -> bool {
    match ty {
        Type::Var(v) => *v == var,
        Type::Fun(params, ret) => {
            params.iter().any(|p| occurs_in(var, p)) || occurs_in(var, ret)
        }
        Type::List(inner) => occurs_in(var, inner),
        Type::Tuple(elems) => elems.iter().any(|e| occurs_in(var, e)),
        Type::Record(_, fields) => fields.iter().any(|(_, t)| occurs_in(var, t)),
        Type::Variant(_, args) | Type::Generic(_, args) => {
            args.iter().any(|a| occurs_in(var, a))
        }
        Type::Map(k, v) => occurs_in(var, k) || occurs_in(var, v),
        Type::Int | Type::Float | Type::Bool | Type::String | Type::Unit | Type::Error => false,
    }
}

/// Run the type checker on a program. Returns a list of type errors (warnings).
pub fn check(program: &mut Program) -> Vec<TypeError> {
    let mut checker = TypeChecker::new();
    checker.check_program(program);
    checker.errors
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn parse(input: &str) -> Program {
        let tokens = Lexer::new(input).tokenize().expect("lexer error");
        Parser::new(tokens).parse_program().expect("parse error")
    }

    fn check_errors(input: &str) -> Vec<TypeError> {
        let mut program = parse(input);
        check(&mut program)
    }

    fn check_program(input: &str) -> Vec<TypeError> {
        check_errors(input)
    }

    fn assert_no_errors(input: &str) {
        let errors = check_program(input);
        let hard_errors: Vec<_> = errors.iter().filter(|e| e.severity == Severity::Error).collect();
        if !hard_errors.is_empty() {
            panic!(
                "expected no type errors, got:\n{}",
                hard_errors
                    .iter()
                    .map(|e| format!("  {e}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
    }

    fn assert_has_error(input: &str, expected_substring: &str) {
        let errors = check_program(input);
        assert!(
            errors.iter().any(|e| e.message.contains(expected_substring)),
            "expected an error containing '{}', got: {:?}",
            expected_substring,
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // ── Basic type inference ────────────────────────────────────────

    #[test]
    fn test_int_literal() {
        assert_no_errors(r#"
fn main() {
  let x = 42
  x
}
        "#);
    }

    #[test]
    fn test_float_literal() {
        assert_no_errors(r#"
fn main() {
  let x = 3.14
  x
}
        "#);
    }

    #[test]
    fn test_string_literal() {
        assert_no_errors(r#"
fn main() {
  let x = "hello"
  x
}
        "#);
    }

    #[test]
    fn test_bool_literal() {
        assert_no_errors(r#"
fn main() {
  let x = true
  x
}
        "#);
    }

    #[test]
    fn test_arithmetic() {
        assert_no_errors(r#"
fn main() {
  let x = 1 + 2
  let y = x * 3
  y
}
        "#);
    }

    #[test]
    fn test_comparison() {
        assert_no_errors(r#"
fn main() {
  let x = 1 < 2
  x
}
        "#);
    }

    #[test]
    fn test_function_call() {
        assert_no_errors(r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1, 2)
}
        "#);
    }

    #[test]
    fn test_shadowing() {
        assert_no_errors(r#"
fn main() {
  let x = 1
  let x = x + 1
  let x = x * 3
  x
}
        "#);
    }

    // ── List inference ──────────────────────────────────────────────

    #[test]
    fn test_list_inference() {
        assert_no_errors(r#"
fn main() {
  let xs = [1, 2, 3]
  xs
}
        "#);
    }

    #[test]
    fn test_empty_list() {
        assert_no_errors(r#"
fn main() {
  let xs = []
  xs
}
        "#);
    }

    // ── Tuple inference ─────────────────────────────────────────────

    #[test]
    fn test_tuple_inference() {
        assert_no_errors(r#"
fn main() {
  let pair = (1, "hello")
  pair
}
        "#);
    }

    // ── Lambda inference ────────────────────────────────────────────

    #[test]
    fn test_lambda() {
        assert_no_errors(r#"
fn main() {
  let double = fn(x) { x * 2 }
  double(5)
}
        "#);
    }

    // ── Enum types ──────────────────────────────────────────────────

    #[test]
    fn test_enum_type() {
        assert_no_errors(r#"
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

fn area(shape) {
  match shape {
    Circle(r) -> 3.14159 * r * r
    Rect(w, h) -> w * h
  }
}

fn main() {
  area(Circle(5.0))
}
        "#);
    }

    // ── Record types ────────────────────────────────────────────────

    #[test]
    fn test_record_type() {
        assert_no_errors(r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn main() {
  let u = User { name: "Alice", age: 30, active: true }
  u.name
}
        "#);
    }

    #[test]
    fn test_record_update() {
        assert_no_errors(r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn birthday(user: User) -> User {
  user.{ age: user.age + 1 }
}

fn main() {
  let u = User { name: "Alice", age: 30, active: true }
  let u2 = birthday(u)
  u2.age
}
        "#);
    }

    // ── Match exhaustiveness ────────────────────────────────────────

    #[test]
    fn test_match_exhaustive_with_wildcard() {
        assert_no_errors(r#"
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

fn describe(shape) {
  match shape {
    Circle(r) -> "circle"
    _ -> "other"
  }
}

fn main() {
  describe(Circle(1.0))
}
        "#);
    }

    #[test]
    fn test_match_exhaustive_all_variants() {
        assert_no_errors(r#"
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

fn describe(shape) {
  match shape {
    Circle(r) -> "circle"
    Rect(w, h) -> "rect"
  }
}

fn main() {
  describe(Circle(1.0))
}
        "#);
    }

    #[test]
    fn test_match_non_exhaustive() {
        assert_has_error(
            r#"
type Color {
  Red
  Green
  Blue
}

fn name(c) {
  match c {
    Red -> "red"
    Green -> "green"
  }
}

fn main() {
  name(Red)
}
            "#,
            "non-exhaustive",
        );
    }

    // ── Generic types ───────────────────────────────────────────────

    #[test]
    fn test_option_some_none() {
        assert_no_errors(r#"
fn main() {
  let x = Some(42)
  let y = None
  match x {
    Some(n) -> n
    None -> 0
  }
}
        "#);
    }

    #[test]
    fn test_result_ok_err() {
        assert_no_errors(r#"
fn main() {
  let x = Ok(42)
  match x {
    Ok(n) -> n
    Err(e) -> 0
  }
}
        "#);
    }

    // ── Question mark operator ──────────────────────────────────────

    #[test]
    fn test_question_mark() {
        assert_no_errors(r#"
fn process(x) {
  let val = Ok(x)?
  Ok(val * 2)
}

fn main() {
  match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#);
    }

    // ── When guard (type narrowing) ─────────────────────────────────

    #[test]
    fn test_when_guard() {
        assert_no_errors(r#"
fn process(x) {
  when Ok(value) = Ok(x) else {
    return Err("failed")
  }
  Ok(value * 2)
}

fn main() {
  match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#);
    }

    // ── Pipe operator ───────────────────────────────────────────────

    #[test]
    fn test_pipe_operator() {
        assert_no_errors(r#"
fn main() {
  [1, 2, 3, 4, 5]
  |> filter { x -> x > 2 }
  |> map { x -> x * 10 }
  |> fold(0) { acc, x -> acc + x }
}
        "#);
    }

    // ── String interpolation ────────────────────────────────────────

    #[test]
    fn test_string_interpolation() {
        assert_no_errors(r#"
fn main() {
  let name = "world"
  let n = 42
  "hello {name}, the answer is {n}"
}
        "#);
    }

    // ── Trait implementation ────────────────────────────────────────

    #[test]
    fn test_trait_impl() {
        assert_no_errors(r#"
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle(r={r})"
      Rect(w, h) -> "Rect({w}x{h})"
    }
  }
}

fn main() {
  let s = Circle(5.0)
  s.display()
}
        "#);
    }

    // ── Map literal ─────────────────────────────────────────────────

    #[test]
    fn test_map_literal() {
        assert_no_errors(r#"
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  m
}
        "#);
    }

    // ── Single-expression function ──────────────────────────────────

    #[test]
    fn test_single_expr_fn() {
        assert_no_errors(r#"
fn square(x) = x * x
fn add(a, b) = a + b

fn main() {
  add(square(3), square(4))
}
        "#);
    }

    // ── Integration test programs ───────────────────────────────────

    #[test]
    fn test_fizzbuzz_program() {
        assert_no_errors(r#"
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  let results = [
    fizzbuzz(1),
    fizzbuzz(3),
    fizzbuzz(5),
    fizzbuzz(15),
  ]
  results
}
        "#);
    }

    #[test]
    fn test_closures_and_higher_order() {
        assert_no_errors(r#"
fn make_adder(n) {
  fn(x) { x + n }
}

fn main() {
  let add5 = make_adder(5)
  add5(10)
}
        "#);
    }

    #[test]
    fn test_error_handling_pipeline() {
        assert_no_errors(r#"
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when Some(host_line) = lines |> find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when Some(port_line) = lines |> find { l -> string.contains(l, "port=") } else {
    return Err("missing port in config")
  }

  let host = host_line |> string.replace("host=", "")
  let port_result = port_line |> string.replace("port=", "") |> int.parse()
  when Ok(port) = port_result else {
    return Err("invalid port number")
  }

  Ok("connecting to {host}:{port}")
}

fn main() {
  match parse_config("host=localhost\nport=8080") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }

  match parse_config("host=localhost") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }
}
        "#);
    }

    #[test]
    fn test_match_with_guards() {
        assert_no_errors(r#"
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}

fn main() {
  [classify(-5), classify(0), classify(42)]
}
        "#);
    }

    // ── Let-polymorphism ────────────────────────────────────────────

    #[test]
    fn test_let_polymorphism() {
        assert_no_errors(r#"
fn identity(x) {
  x
}

fn main() {
  let a = identity(42)
  let b = identity("hello")
  a
}
        "#);
    }

    // ── Unification error ───────────────────────────────────────────

    #[test]
    fn test_type_mismatch_in_binary_op() {
        assert_has_error(
            r#"
fn main() {
  let x = 42 + "hello"
  x
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_bool_op_type_mismatch() {
        assert_has_error(
            r#"
fn main() {
  let x = 42 && true
  x
}
            "#,
            "type mismatch",
        );
    }

    // ── Range ───────────────────────────────────────────────────────

    #[test]
    fn test_range_expression() {
        assert_no_errors(r#"
fn main() {
  let r = 1..10
  r
}
        "#);
    }

    // ── Exhaustiveness: guards don't count as covering ──────────────

    #[test]
    fn test_match_guards_with_catch_all() {
        assert_no_errors(r#"
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}

fn main() {
  classify(5)
}
        "#);
    }

    // ── Severity tests ─────────────────────────────────────────────

    #[test]
    fn test_type_error_has_error_severity() {
        // A type mismatch should produce Severity::Error
        let errors = check_errors(r#"
            fn main() {
                let x: Int = "hello"
                x
            }
        "#);
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.severity == Severity::Error));
    }

    #[test]
    fn test_valid_program_no_errors() {
        let errors = check_errors(r#"
            fn main() {
                let x = 42
                x + 1
            }
        "#);
        let hard_errors: Vec<_> = errors.iter().filter(|e| e.severity == Severity::Error).collect();
        assert!(hard_errors.is_empty());
    }

    #[test]
    fn test_trait_impl_validates_methods() {
        // Complete impl should have no errors about missing methods
        let errors = check_program(r#"
            trait Greet {
                fn greet(self) -> String {
                    "hello"
                }
            }
            trait Greet for User {
                fn greet(self) -> String {
                    "hi"
                }
            }
            type User { name: String }
            fn main() { 0 }
        "#);
        let trait_errors: Vec<_> = errors.iter().filter(|e| e.message.contains("missing method")).collect();
        assert!(trait_errors.is_empty(), "unexpected trait errors: {:?}", trait_errors);
    }

    #[test]
    fn test_trait_impl_missing_method() {
        let errors = check_program(r#"
            trait Showable {
                fn show(self) -> String { "default" }
                fn detail(self) -> String { "detail" }
            }
            trait Showable for Item {
                fn show(self) -> String { "item" }
            }
            type Item { name: String }
            fn main() { 0 }
        "#);
        assert!(errors.iter().any(|e| e.message.contains("missing method") && e.message.contains("detail")));
    }

    #[test]
    fn test_trait_impl_unknown_trait() {
        let errors = check_program(r#"
            trait Nonexistent for Thing {
                fn foo(self) -> Int { 0 }
            }
            type Thing { x: Int }
            fn main() { 0 }
        "#);
        assert!(errors.iter().any(|e| e.message.contains("not declared")));
    }

    #[test]
    fn test_builtin_display_trait_exists() {
        // Implementing Display should not produce "trait not declared" error
        let errors = check_program(r#"
            type Color { Red, Blue }
            trait Display for Color {
                fn display(self) -> String {
                    match self {
                        Red -> "red"
                        Blue -> "blue"
                    }
                }
            }
            fn main() { 0 }
        "#);
        let undeclared: Vec<_> = errors.iter().filter(|e| e.message.contains("not declared")).collect();
        assert!(undeclared.is_empty(), "Display should be a built-in trait: {:?}", undeclared);
    }

    #[test]
    fn test_where_unknown_trait_warning() {
        let errors = check_program(r#"
            fn show(x) where x: Nonexistent {
                x
            }
            fn main() { 0 }
        "#);
        assert!(errors.iter().any(|e| e.message.contains("Nonexistent")));
    }

    #[test]
    fn test_where_constraint_satisfied() {
        // Should produce no errors about constraints
        let errors = check_errors(r#"
            trait Showable {
                fn show(self) -> String { "default" }
            }
            type Color { Red, Blue }
            trait Showable for Color {
                fn show(self) -> String { "color" }
            }
            fn display(x) where x: Showable {
                x
            }
            fn main() {
                display(Red)
            }
        "#);
        let constraint_errors: Vec<_> = errors.iter()
            .filter(|e| e.message.contains("does not implement"))
            .collect();
        assert!(constraint_errors.is_empty(), "unexpected: {:?}", constraint_errors);
    }

    #[test]
    fn test_where_constraint_violated() {
        // Should produce an error: Int doesn't implement Showable
        let errors = check_errors(r#"
            trait Showable {
                fn show(self) -> String { "default" }
            }
            fn display(x) where x: Showable {
                x
            }
            fn main() {
                display(42)
            }
        "#);
        assert!(errors.iter().any(|e| e.message.contains("does not implement") || e.message.contains("Showable")),
            "expected constraint violation error, got: {:?}", errors);
    }

    // ── Record types with generic fields (List, Map) ───────────────

    #[test]
    fn test_record_with_list_field() {
        assert_no_errors(r#"
type Bag {
  items: List,
  name: String,
}

fn main() {
  let b = Bag { items: [1, 2, 3], name: "test" }
  b.name
}
        "#);
    }

    #[test]
    fn test_record_with_map_field() {
        assert_no_errors(r#"
type Config {
  data: Map,
}

fn main() {
  let c = Config { data: #{ "key": "value" } }
  c.data
}
        "#);
    }

    #[test]
    fn test_record_with_list_and_map_fields() {
        assert_no_errors(r#"
type Config {
  values: Map,
  errors: List,
}

fn main() {
  let c = Config { values: #{ "a": 1 }, errors: ["err1", "err2"] }
  c.values
}
        "#);
    }

    #[test]
    fn test_record_with_list_field_access() {
        assert_no_errors(r#"
type Bag {
  items: List,
}

fn main() {
  let b = Bag { items: [1, 2, 3] }
  b.items
}
        "#);
    }

    // ── Tests for newly registered builtins ────────────────────────

    #[test]
    fn test_list_module_builtins() {
        assert_no_errors(r#"
fn main() {
  let xs = [1, 2, 3]
  let ys = list.append(xs, 4)
  let zs = list.concat(xs, ys)
  let head = list.head(xs)
  let tail = list.tail(xs)
  let last = list.last(xs)
  let rev = list.reverse(xs)
  let sorted = list.sort(xs)
  let has = list.contains(xs, 2)
  let n = list.length(xs)
  let taken = list.take(xs, 2)
  let dropped = list.drop(xs, 1)
  let got = list.get(xs, 0)
  let pairs = list.enumerate(xs)
  n
}
        "#);
    }

    #[test]
    fn test_string_module_builtins() {
        assert_no_errors(r#"
fn main() {
  let s = "hello world"
  let upper = string.to_upper(s)
  let lower = string.to_lower(s)
  let n = string.length(s)
  let starts = string.starts_with(s, "hello")
  let ends = string.ends_with(s, "world")
  let chars = string.chars(s)
  let repeated = string.repeat(s, 3)
  let idx = string.index_of(s, "world")
  let sliced = string.slice(s, 0, 5)
  let replaced = string.replace(s, "world", "there")
  n
}
        "#);
    }

    #[test]
    fn test_float_module_builtins() {
        assert_no_errors(r#"
fn main() {
  let a = 3.14
  let b = 2.71
  let mn = float.min(a, b)
  let mx = float.max(a, b)
  let parsed = float.parse("3.14")
  let rounded = float.round(a)
  let ceiled = float.ceil(a)
  let floored = float.floor(a)
  let abs = float.abs(a)
  rounded
}
        "#);
    }

    #[test]
    fn test_int_module_builtins() {
        assert_no_errors(r#"
fn main() {
  let a = 5
  let b = 3
  let mn = int.min(a, b)
  let mx = int.max(a, b)
  let f = int.to_float(a)
  f
}
        "#);
    }

    #[test]
    fn test_map_module_builtins() {
        assert_no_errors(r#"
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let got = map.get(m, "a")
  let updated = map.set(m, "c", 3)
  let deleted = map.delete(m, "a")
  let ks = map.keys(m)
  let vs = map.values(m)
  let merged = map.merge(m, #{ "c": 3 })
  ks
}
        "#);
    }

    #[test]
    fn test_io_module_builtins() {
        assert_no_errors(r#"
fn main() {
  let result = io.read_file("test.txt")
  let args = io.args()
  args
}
        "#);
    }

    #[test]
    fn test_option_module_builtins() {
        assert_no_errors(r#"
fn main() {
  let opt = Some(42)
  let is_s = option.is_some(opt)
  let is_n = option.is_none(opt)
  let val = option.unwrap_or(opt, 0)
  let mapped = option.map(opt, fn(x) { x + 1 })
  let res = option.to_result(opt, "no value")
  val
}
        "#);
    }

    #[test]
    fn test_result_module_builtins() {
        assert_no_errors(r#"
fn main() {
  let r = Ok(42)
  let is_ok = result.is_ok(r)
  let is_err = result.is_err(r)
  is_ok
}
        "#);
    }

    #[test]
    fn test_higher_order_builtins() {
        assert_no_errors(r#"
fn main() {
  let xs = [[1, 2], [3, 4], [5]]
  let flat = flatten(xs)
  let zipped = zip([1, 2, 3], ["a", "b", "c"])
  let sorted = sort_by([3, 1, 2], fn(x) { x })
  flat
}
        "#);
    }

    #[test]
    fn test_len_accepts_string_and_map() {
        assert_no_errors(r#"
fn main() {
  let list_len = len([1, 2, 3])
  let str_len = len("hello")
  let map_len = len(#{ "a": 1 })
  list_len + str_len + map_len
}
        "#);
    }

    #[test]
    fn test_assert_ne_builtin() {
        assert_no_errors(r#"
fn main() {
  assert_ne(1, 2)
}
        "#);
    }

    #[test]
    fn test_channel_new_no_type_error() {
        assert_no_errors(r#"
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.close(ch)
  ch
}
        "#);
    }

    #[test]
    fn test_task_spawn_no_type_error() {
        assert_no_errors(r#"
fn main() {
  let h = task.spawn(fn() { 42 })
  let result = task.join(h)
  result
}
        "#);
    }

    #[test]
    fn test_try_no_type_error() {
        assert_no_errors(r#"
fn main() {
  let result = try(fn() { 1 + 2 })
  result
}
        "#);
    }

    #[test]
    fn test_map_length_no_type_error() {
        assert_no_errors(r#"
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let n = map.length(m)
  n
}
        "#);
    }
}
