//! Hindley-Milner type inference and checking for Silt.
//!
//! This module implements Algorithm W-style type inference with:
//! - Type variables and unification
//! - Let-polymorphism (generalization at let bindings)
//! - Exhaustiveness checking for match expressions
//! - Type narrowing after `when` guard statements
//! - Trait constraint checking

use std::collections::{BTreeSet, HashMap};

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

/// A registered trait method implementation (new trait system).
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct MethodEntry {
    method_type: Type,
    span: Span,
    is_auto_derived: bool,
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
    /// Method table: (type_name, method_name) → method entry.
    method_table: HashMap<(std::string::String, std::string::String), MethodEntry>,
    /// Tracks which (trait_name, type_name) pairs have been implemented.
    trait_impl_set: std::collections::HashSet<(std::string::String, std::string::String)>,
    /// Maps function names to their where clauses as (param_index, trait_name).
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
            method_table: HashMap::new(),
            trait_impl_set: std::collections::HashSet::new(),
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
            Type::Set(inner) => Type::Set(Box::new(self.apply(inner))),
            _ => ty.clone(),
        }
    }

    // ── Unification ─────────────────────────────────────────────────

    fn unify(&mut self, t1: &Type, t2: &Type, span: Span) {
        let t1 = self.apply(t1);
        let t2 = self.apply(t2);

        match (&t1, &t2) {
            (Type::Error, _) | (_, Type::Error) | (Type::Never, _) | (_, Type::Never) => {}
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

            (Type::Set(a), Type::Set(b)) => {
                self.unify(a, b, span);
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
                        format!("record type mismatch: expected {n2}, got {n1}"),
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
                        format!("type mismatch: expected {n2}, got {n1}"),
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
                    format!("type mismatch: expected {t2}, got {t1}"),
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
        Scheme { vars, ty, constraints: vec![] }
    }

    /// Instantiate a scheme by replacing quantified variables with fresh ones.
    fn instantiate(&mut self, scheme: &Scheme) -> Type {
        self.instantiate_with_constraints(scheme).0
    }

    /// Instantiate a scheme and remap its where clause constraints.
    /// Returns (instantiated_type, remapped_constraints).
    fn instantiate_with_constraints(&mut self, scheme: &Scheme) -> (Type, Vec<(TyVar, String)>) {
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for &v in &scheme.vars {
            mapping.insert(v, self.fresh_var());
        }
        let ty = substitute_vars(&scheme.ty, &mapping);
        let constraints = scheme.constraints.iter().map(|(v, trait_name)| {
            match mapping.get(v) {
                Some(Type::Var(new_v)) => (*new_v, trait_name.clone()),
                _ => (*v, trait_name.clone()),
            }
        }).collect();
        (ty, constraints)
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
            let trait_methods: &[(&str, Type)] = &[
                ("display", Type::Fun(vec![self.fresh_var()], Box::new(Type::String))),
                ("equal", Type::Fun(vec![self.fresh_var(), self.fresh_var()], Box::new(Type::Bool))),
                ("compare", Type::Fun(vec![self.fresh_var(), self.fresh_var()], Box::new(Type::Int))),
                ("hash", Type::Fun(vec![self.fresh_var()], Box::new(Type::Int))),
            ];
            for type_name in &primitive_types {
                for trait_name in &all_traits {
                    self.trait_impl_set.insert((trait_name.to_string(), type_name.to_string()));
                }
                // Register method entries for each builtin trait method
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (type_name.to_string(), method_name.to_string()),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                        },
                    );
                }
            }
            for type_name in &["List", "Tuple", "Map", "Set"] {
                for trait_name in &all_traits {
                    self.trait_impl_set.insert((trait_name.to_string(), type_name.to_string()));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (type_name.to_string(), method_name.to_string()),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                        },
                    );
                }
            }
        }

        // First pass: register all type declarations
        for decl in &program.decls {
            if let Decl::Type(td) = decl {
                self.register_type_decl(td, &mut env);
            }
        }

        // Second pass: register all function signatures, trait impls, and top-level lets
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

        // Process top-level let bindings (after functions are registered so
        // the value expression can call functions, and before function body
        // checking so functions can reference the constants).
        for i in 0..program.decls.len() {
            if let Decl::Let { ref mut value, ref pattern, ref ty, span, .. } = program.decls[i] {
                let val_ty = self.infer_expr(value, &mut env);
                if let Some(te) = ty {
                    let declared = self.resolve_type_expr(te, &mut std::collections::HashMap::new());
                    self.unify(&val_ty, &declared, span);
                }
                let scheme = self.generalize(&env, &val_ty);
                if let Pattern::Ident(name) = pattern {
                    env.define(name.clone(), scheme);
                } else {
                    self.bind_pattern(pattern, &val_ty, &mut env);
                }
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
        // Validate using method_table + trait_impl_set (the new system).
        let impl_pairs: Vec<(std::string::String, std::string::String)> =
            self.trait_impl_set.iter().cloned().collect();
        for (trait_name, type_name) in &impl_pairs {
            // Check that the trait exists first.
            let Some(trait_info) = self.traits.get(trait_name).cloned() else {
                let span = self.method_table.iter()
                    .find(|((t, _), _)| t == type_name)
                    .map(|(_, e)| e.span)
                    .unwrap_or(Span::new(0, 0));
                self.error(
                    format!("trait '{trait_name}' is not declared"),
                    span,
                );
                continue;
            };

            // Skip auto-derived impls (builtin traits on all types).
            let is_auto = trait_info.methods.first()
                .and_then(|(m, _)| self.method_table.get(&(type_name.clone(), m.clone())))
                .map(|e| e.is_auto_derived)
                .unwrap_or(false);
            if is_auto {
                continue;
            }

            // Check that all required methods are implemented with correct arity.
            for (method_name, trait_method_type) in &trait_info.methods {
                let key = (type_name.clone(), method_name.clone());
                if let Some(entry) = self.method_table.get(&key) {
                    let expected_arity = count_params(trait_method_type);
                    let actual_arity = count_params(&entry.method_type);
                    if actual_arity != expected_arity {
                        self.error(
                            format!(
                                "method '{}' in trait impl '{}' for '{}' has wrong arity: expected {}, got {}",
                                method_name, trait_name, type_name, expected_arity, actual_arity
                            ),
                            entry.span,
                        );
                    }
                } else {
                    // Find a span for the error.
                    let span = self.method_table.iter()
                        .find(|((t, _), _)| t == type_name)
                        .map(|(_, e)| e.span)
                        .unwrap_or(Span::new(0, 0));
                    self.error(
                        format!(
                            "trait impl '{}' for '{}' is missing method '{}'",
                            trait_name, type_name, method_name
                        ),
                        span,
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
        // ── print / println: (a) -> () ─────────────────────────────────
        // Accept any type (the runtime uses Display for formatting).
        {
            let (a, av) = self.fresh_tv();
            env.define("print".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone()], Box::new(Type::Unit)),
                constraints: vec![],
            });
        }
        {
            let (a, av) = self.fresh_tv();
            env.define("println".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone()], Box::new(Type::Unit)),
                constraints: vec![],
            });
        }

        // ── panic: String -> a ─────────────────────────────────────────
        {
            let (a, av) = self.fresh_tv();
            env.define("panic".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![Type::String], Box::new(a)),
                constraints: vec![],
            });
        }

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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
            });
        }
        // None : Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("None".into(), Scheme {
                vars: vec![av],
                ty: Type::Generic("Option".into(), vec![a]),
                constraints: vec![],
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

        // Step enum: Stop(a) / Continue(a) — for list.fold_until
        self.enums.insert(
            "Step".into(),
            EnumInfo {
                _name: "Step".into(),
                params: vec!["a".into()],
                variants: vec![
                    VariantInfo { name: "Stop".into(), field_types: vec![Type::Var(0)] },
                    VariantInfo { name: "Continue".into(), field_types: vec![Type::Var(0)] },
                ],
            },
        );
        self.variant_to_enum.insert("Stop".into(), "Step".into());
        self.variant_to_enum.insert("Continue".into(), "Step".into());
        {
            let (a, av) = self.fresh_tv();
            env.define("Stop".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![a.clone()],
                    Box::new(Type::Generic("Step".into(), vec![a])),
                ),
                constraints: vec![],
            });
        }
        {
            let (a, av) = self.fresh_tv();
            env.define("Continue".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![a.clone()],
                    Box::new(Type::Generic("Step".into(), vec![a])),
                ),
                constraints: vec![],
            });
        }

        // ChannelResult enum: Message(a) / Closed — for channel.receive
        self.enums.insert(
            "ChannelResult".into(),
            EnumInfo {
                _name: "ChannelResult".into(),
                params: vec!["a".into()],
                variants: vec![
                    VariantInfo { name: "Message".into(), field_types: vec![Type::Var(0)] },
                    VariantInfo { name: "Closed".into(), field_types: vec![] },
                ],
            },
        );
        self.variant_to_enum.insert("Message".into(), "ChannelResult".into());
        self.variant_to_enum.insert("Closed".into(), "ChannelResult".into());
        // Also register Empty as a standalone (used in try_receive alongside Message/Closed)
        self.variant_to_enum.insert("Empty".into(), "ChannelResult".into());
        {
            let (a, av) = self.fresh_tv();
            env.define("Message".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![a.clone()],
                    Box::new(Type::Generic("ChannelResult".into(), vec![a])),
                ),
                constraints: vec![],
            });
        }
        {
            let (a, av) = self.fresh_tv();
            env.define("Closed".into(), Scheme {
                vars: vec![av],
                ty: Type::Generic("ChannelResult".into(), vec![a]),
                constraints: vec![],
            });
        }
        {
            let (a, av) = self.fresh_tv();
            env.define("Empty".into(), Scheme {
                vars: vec![av],
                ty: Type::Generic("ChannelResult".into(), vec![a]),
                constraints: vec![],
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
                constraints: vec![],
            });
        }

        // task.join: (Handle) -> a
        {
            let (h, hv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("task.join".into(), Scheme {
                vars: vec![hv, av],
                ty: Type::Fun(vec![h], Box::new(a)),
                constraints: vec![],
            });
        }

        // task.cancel: (Handle) -> Unit
        {
            let (h, hv) = self.fresh_tv();
            env.define("task.cancel".into(), Scheme {
                vars: vec![hv],
                ty: Type::Fun(vec![h], Box::new(Type::Unit)),
                constraints: vec![],
            });
        }

        // ── regex module ────────────────────────────────────────────────

        // regex.is_match: (String, String) -> Bool
        env.define("regex.is_match".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Bool),
        )));

        // regex.find: (String, String) -> Option(String)
        env.define("regex.find".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic("Option".into(), vec![Type::String])),
        )));

        // regex.find_all: (String, String) -> List(String)
        env.define("regex.find_all".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::List(Box::new(Type::String))),
        )));

        // regex.split: (String, String) -> List(String)
        env.define("regex.split".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::List(Box::new(Type::String))),
        )));

        // regex.replace: (String, String, String) -> String
        env.define("regex.replace".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String, Type::String],
            Box::new(Type::String),
        )));

        // regex.replace_all: (String, String, String) -> String
        env.define("regex.replace_all".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String, Type::String],
            Box::new(Type::String),
        )));

        // regex.replace_all_with: (String, String, (String) -> String) -> String
        env.define("regex.replace_all_with".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String, Type::Fun(vec![Type::String], Box::new(Type::String))],
            Box::new(Type::String),
        )));

        // regex.captures: (String, String) -> Option(List(String))
        env.define("regex.captures".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic("Option".into(), vec![Type::List(Box::new(Type::String))])),
        )));

        // regex.captures_all: (String, String) -> List(List(String))
        env.define("regex.captures_all".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::List(Box::new(Type::List(Box::new(Type::String))))),
        )));

        // ── json module ─────────────────────────────────────────────────

        // json.parse: (T, String) -> Result(T, String)
        // The first arg is a type descriptor; the same type flows into the Result.
        {
            let (a, av) = self.fresh_tv();
            let result_ty = Type::Generic("Result".into(), vec![a.clone(), Type::String]);
            env.define("json.parse".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![a, Type::String],
                    Box::new(result_ty),
                ),
                constraints: vec![],
            });
        }

        // json.parse_list: (T, String) -> Result(List(T), String)
        {
            let (a, av) = self.fresh_tv();
            let result_ty = Type::Generic("Result".into(), vec![
                Type::List(Box::new(a.clone())),
                Type::String,
            ]);
            env.define("json.parse_list".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![a, Type::String],
                    Box::new(result_ty),
                ),
                constraints: vec![],
            });
        }

        // json.parse_map: (V, String) -> Result(Map(String, V), String)
        {
            let (a, av) = self.fresh_tv();
            let result_ty = Type::Generic("Result".into(), vec![
                Type::Map(Box::new(Type::String), Box::new(a.clone())),
                Type::String,
            ]);
            env.define("json.parse_map".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![a, Type::String],
                    Box::new(result_ty),
                ),
                constraints: vec![],
            });
        }

        // json.stringify: (a) -> String
        {
            let (a, av) = self.fresh_tv();
            env.define("json.stringify".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![a],
                    Box::new(Type::String),
                ),
                constraints: vec![],
            });
        }

        // json.pretty: (a) -> String
        {
            let (a, av) = self.fresh_tv();
            env.define("json.pretty".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![a],
                    Box::new(Type::String),
                ),
                constraints: vec![],
            });
        }

        // ── Primitive type descriptors (for json.parse_map etc.) ──────
        // These carry the actual type so json.parse can propagate it
        // into the return type.
        for name in &["Int", "Float", "String", "Bool"] {
            let ty = match *name {
                "Int" => Type::Int,
                "Float" => Type::Float,
                "String" => Type::String,
                "Bool" => Type::Bool,
                _ => unreachable!(),
            };
            env.define(name.to_string(), Scheme {
                vars: vec![],
                ty,
                constraints: vec![],
            });
        }

        // ── Per-module registrations ───────────────────────────────────
        self.register_list_builtins(env);
        self.register_string_builtins(env);
        self.register_int_builtins(env);
        self.register_float_builtins(env);
        self.register_map_builtins(env);
        self.register_set_builtins(env);
        self.register_result_builtins(env);
        self.register_option_builtins(env);
        self.register_io_builtins(env);
        self.register_fs_builtins(env);
        self.register_test_builtins(env);
        self.register_math_builtins(env);
        self.register_channel_builtins(env);
        self.register_time_builtins(env);
    }

    fn register_list_builtins(&mut self, env: &mut TypeEnv) {
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
            });
        }

        // list.filter_map: (List(a), (a -> Option(b))) -> List(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("list.filter_map".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(
                            Type::Generic("Option".into(), vec![b.clone()])
                        )),
                    ],
                    Box::new(Type::List(Box::new(b))),
                ),
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
            });
        }

        // list.fold_until: (List(a), b, (b, a) -> Step(b)) -> b
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("list.fold_until".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(vec![b.clone(), a], Box::new(
                            Type::Generic("Step".into(), vec![b.clone()])
                        )),
                    ],
                    Box::new(b),
                ),
                constraints: vec![],
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
                constraints: vec![],
            });
        }

        // list.append: (List(a), a) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.append".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), a.clone()],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
            });
        }

        // list.prepend: (List(a), a) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.prepend".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), a.clone()],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
            });
        }

        // list.set: (List(a), Int, a) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.set".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone())), Type::Int, a.clone()],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
            });
        }

        // list.unique: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("list.unique".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
            });
        }

        // list.group_by: (List(a), (a -> k)) -> Map(k, List(a))
        {
            let (a, av) = self.fresh_tv();
            let (k, kv) = self.fresh_tv();
            env.define("list.group_by".into(), Scheme {
                vars: vec![av, kv],
                ty: Type::Fun(
                    vec![
                        Type::List(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(k.clone())),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(Type::List(Box::new(a))))),
                ),
                constraints: vec![],
            });
        }
    }

    fn register_string_builtins(&mut self, env: &mut TypeEnv) {
        // string.from: (a) -> String
        {
            let (a, av) = self.fresh_tv();
            env.define("string.from".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a], Box::new(Type::String)),
                constraints: vec![],
            });
        }

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

        // string.trim_start: (String) -> String
        env.define("string.trim_start".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::String),
        )));

        // string.trim_end: (String) -> String
        env.define("string.trim_end".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::String),
        )));

        // string.char_code: (String) -> Int
        env.define("string.char_code".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Int),
        )));

        // string.from_char_code: (Int) -> String
        env.define("string.from_char_code".into(), Scheme::mono(Type::Fun(
            vec![Type::Int],
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

        // string.is_empty: (String) -> Bool
        env.define("string.is_empty".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Bool),
        )));

        // string.is_alpha: (String) -> Bool
        env.define("string.is_alpha".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Bool),
        )));

        // string.is_digit: (String) -> Bool
        env.define("string.is_digit".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Bool),
        )));

        // string.is_upper: (String) -> Bool
        env.define("string.is_upper".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Bool),
        )));

        // string.is_lower: (String) -> Bool
        env.define("string.is_lower".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Bool),
        )));

        // string.is_alnum: (String) -> Bool
        env.define("string.is_alnum".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Bool),
        )));

        // string.is_whitespace: (String) -> Bool
        env.define("string.is_whitespace".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Bool),
        )));
    }

    fn register_int_builtins(&mut self, env: &mut TypeEnv) {
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
    }

    fn register_float_builtins(&mut self, env: &mut TypeEnv) {
        // float.parse: (String) -> Result(Float, String)
        env.define("float.parse".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic("Result".into(), vec![Type::Float, Type::String])),
        )));

        // float.round: (Float) -> Float
        env.define("float.round".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::Float),
        )));

        // float.ceil: (Float) -> Float
        env.define("float.ceil".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::Float),
        )));

        // float.floor: (Float) -> Float
        env.define("float.floor".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::Float),
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

        // float.to_string: (Float, Int) -> String
        // The second argument (decimal places) is optional at runtime;
        // registering the 2-arg form lets the typechecker validate both
        // arguments.  The 1-arg call still passes the arity check because
        // module-qualified calls go through FieldAccess which permits ±1.
        env.define("float.to_string".into(), Scheme::mono(Type::Fun(
            vec![Type::Float, Type::Int],
            Box::new(Type::String),
        )));

        // float.to_int: (Float) -> Int
        env.define("float.to_int".into(), Scheme::mono(Type::Fun(
            vec![Type::Float],
            Box::new(Type::Int),
        )));
    }

    fn register_map_builtins(&mut self, env: &mut TypeEnv) {
        // map.get: (Map(k, v), k) -> Option(v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.get".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        k,
                    ],
                    Box::new(Type::Generic("Option".into(), vec![v])),
                ),
                constraints: vec![(kv, "Hash".into())],
            });
        }

        // map.set: (Map(k, v), k, v) -> Map(k, v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.set".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        k.clone(),
                        v.clone(),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![(kv, "Hash".into())],
            });
        }

        // map.delete: (Map(k, v), k) -> Map(k, v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.delete".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        k.clone(),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![(kv, "Hash".into())],
            });
        }

        // map.contains: (Map(k, v), k) -> Bool  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.contains".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v)),
                        k,
                    ],
                    Box::new(Type::Bool),
                ),
                constraints: vec![(kv, "Hash".into())],
            });
        }

        // map.keys: (Map(k, v)) -> List(k)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.keys".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k.clone()), Box::new(v))],
                    Box::new(Type::List(Box::new(k))),
                ),
                constraints: vec![(kv, "Hash".into())],
            });
        }

        // map.values: (Map(k, v)) -> List(v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.values".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k), Box::new(v.clone()))],
                    Box::new(Type::List(Box::new(v))),
                ),
                constraints: vec![(kv, "Hash".into())],
            });
        }

        // map.merge: (Map(k, v), Map(k, v)) -> Map(k, v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.merge".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![(kv, "Hash".into())],
            });
        }

        // map.length: (Map(k, v)) -> Int  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.length".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k), Box::new(v))],
                    Box::new(Type::Int),
                ),
                constraints: vec![(kv, "Hash".into())],
            });
        }

        // map.filter: (Map(k, v), (k, v) -> Bool) -> Map(k, v)
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.filter".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        Type::Fun(vec![k.clone(), v.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![],
            });
        }

        // map.map: (Map(k, v), (k, v) -> (k2, v2)) -> Map(k2, v2)
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            let (k2, k2v) = self.fresh_tv();
            let (v2, v2v) = self.fresh_tv();
            env.define("map.map".into(), Scheme {
                vars: vec![kv, vv, k2v, v2v],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        Type::Fun(vec![k, v], Box::new(Type::Tuple(vec![k2.clone(), v2.clone()]))),
                    ],
                    Box::new(Type::Map(Box::new(k2), Box::new(v2))),
                ),
                constraints: vec![],
            });
        }

        // map.entries: (Map(k, v)) -> List((k, v))
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.entries".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::Map(Box::new(k.clone()), Box::new(v.clone()))],
                    Box::new(Type::List(Box::new(Type::Tuple(vec![k, v])))),
                ),
                constraints: vec![],
            });
        }

        // map.from_entries: (List((k, v))) -> Map(k, v)
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.from_entries".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![Type::List(Box::new(Type::Tuple(vec![k.clone(), v.clone()])))],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![],
            });
        }

        // map.each: (Map(k, v), (k, v) -> ()) -> ()
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.each".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        Type::Fun(vec![k, v], Box::new(Type::Unit)),
                    ],
                    Box::new(Type::Unit),
                ),
                constraints: vec![],
            });
        }

        // map.update: (Map(k, v), k, v, (v) -> v) -> Map(k, v)
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define("map.update".into(), Scheme {
                vars: vec![kv, vv],
                ty: Type::Fun(
                    vec![
                        Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        k.clone(),
                        v.clone(),
                        Type::Fun(vec![v.clone()], Box::new(v.clone())),
                    ],
                    Box::new(Type::Map(Box::new(k), Box::new(v))),
                ),
                constraints: vec![],
            });
        }
    }

    fn register_set_builtins(&mut self, env: &mut TypeEnv) {
        // set.new: () -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("set.new".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![], Box::new(Type::Set(Box::new(a)))),
                constraints: vec![],
            });
        }

        // set.from_list: (List(a)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("set.from_list".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::List(Box::new(a.clone()))],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            });
        }

        // set.to_list: (Set(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("set.to_list".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Set(Box::new(a.clone()))],
                    Box::new(Type::List(Box::new(a))),
                ),
                constraints: vec![],
            });
        }

        // set.contains: (Set(a), a) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define("set.contains".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Set(Box::new(a.clone())), a],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
            });
        }

        // set.insert: (Set(a), a) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("set.insert".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Set(Box::new(a.clone())), a.clone()],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            });
        }

        // set.remove: (Set(a), a) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("set.remove".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Set(Box::new(a.clone())), a.clone()],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            });
        }

        // set.length: (Set(a)) -> Int
        {
            let (a, av) = self.fresh_tv();
            env.define("set.length".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![Type::Set(Box::new(a))],
                    Box::new(Type::Int),
                ),
                constraints: vec![],
            });
        }

        // set.union: (Set(a), Set(a)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("set.union".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Set(Box::new(a.clone())),
                    ],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            });
        }

        // set.intersection: (Set(a), Set(a)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("set.intersection".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Set(Box::new(a.clone())),
                    ],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            });
        }

        // set.difference: (Set(a), Set(a)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("set.difference".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Set(Box::new(a.clone())),
                    ],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            });
        }

        // set.is_subset: (Set(a), Set(a)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define("set.is_subset".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Set(Box::new(a.clone())),
                    ],
                    Box::new(Type::Bool),
                ),
                constraints: vec![],
            });
        }

        // set.map: (Set(a), (a -> b)) -> Set(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("set.map".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(b.clone())),
                    ],
                    Box::new(Type::Set(Box::new(b))),
                ),
                constraints: vec![],
            });
        }

        // set.filter: (Set(a), (a -> Bool)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define("set.filter".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                    ],
                    Box::new(Type::Set(Box::new(a))),
                ),
                constraints: vec![],
            });
        }

        // set.each: (Set(a), (a -> ())) -> ()
        {
            let (a, av) = self.fresh_tv();
            env.define("set.each".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        Type::Fun(vec![a], Box::new(Type::Unit)),
                    ],
                    Box::new(Type::Unit),
                ),
                constraints: vec![],
            });
        }

        // set.fold: (Set(a), b, (b, a) -> b) -> b
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("set.fold".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Set(Box::new(a.clone())),
                        b.clone(),
                        Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                    ],
                    Box::new(b),
                ),
                constraints: vec![],
            });
        }
    }

    fn register_result_builtins(&mut self, env: &mut TypeEnv) {
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
                constraints: vec![],
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
                constraints: vec![],
            });
        }

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
                constraints: vec![],
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
                constraints: vec![],
            });
        }

        // result.flat_map: (Result(a, e), (a) -> Result(b, e)) -> Result(b, e)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define("result.flat_map".into(), Scheme {
                vars: vec![av, bv, ev],
                ty: Type::Fun(
                    vec![
                        Type::Generic("Result".into(), vec![a.clone(), e.clone()]),
                        Type::Fun(vec![a], Box::new(Type::Generic("Result".into(), vec![b.clone(), e.clone()]))),
                    ],
                    Box::new(Type::Generic("Result".into(), vec![b, e])),
                ),
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
            });
        }
    }

    fn register_option_builtins(&mut self, env: &mut TypeEnv) {
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
                constraints: vec![],
            });
        }

        // option.flat_map: (Option(a), (a -> Option(b))) -> Option(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("option.flat_map".into(), Scheme {
                vars: vec![av, bv],
                ty: Type::Fun(
                    vec![
                        Type::Generic("Option".into(), vec![a.clone()]),
                        Type::Fun(vec![a], Box::new(Type::Generic("Option".into(), vec![b.clone()]))),
                    ],
                    Box::new(Type::Generic("Option".into(), vec![b])),
                ),
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
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
                constraints: vec![],
            });
        }
    }

    fn register_io_builtins(&mut self, env: &mut TypeEnv) {
        // io.inspect: a -> String
        {
            let (a, av) = self.fresh_tv();
            env.define("io.inspect".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a], Box::new(Type::String)),
                constraints: vec![],
            });
        }

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
    }

    fn register_fs_builtins(&mut self, env: &mut TypeEnv) {
        // fs.exists: (String) -> Bool
        env.define("fs.exists".into(), Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Bool),
        )));
    }

    fn register_test_builtins(&mut self, env: &mut TypeEnv) {
        // test.assert: (Bool, String) -> ()
        // The message parameter is optional at runtime; registering the full
        // arity lets the typechecker validate the message type while the
        // is_method_call arity tolerance still allows the 1-arg form.
        env.define(
            "test.assert".into(),
            Scheme::mono(Type::Fun(vec![Type::Bool, Type::String], Box::new(Type::Unit))),
        );

        // test.assert_eq: (a, a, String) -> ()
        // The message parameter is optional at runtime.
        {
            let (a, av) = self.fresh_tv();
            env.define("test.assert_eq".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone(), a, Type::String], Box::new(Type::Unit)),
                constraints: vec![],
            });
        }

        // test.assert_ne: (a, a, String) -> ()
        // The message parameter is optional at runtime.
        {
            let (a, av) = self.fresh_tv();
            env.define("test.assert_ne".into(), Scheme {
                vars: vec![av],
                ty: Type::Fun(vec![a.clone(), a, Type::String], Box::new(Type::Unit)),
                constraints: vec![],
            });
        }
    }

    fn register_math_builtins(&mut self, env: &mut TypeEnv) {
        // math.sqrt, math.log, math.log10, math.sin, math.cos, math.tan,
        // math.asin, math.acos, math.atan: (Float) -> Float
        {
            let float_to_float = Scheme::mono(Type::Fun(
                vec![Type::Float],
                Box::new(Type::Float),
            ));
            for name in &[
                "math.sqrt", "math.log", "math.log10",
                "math.sin", "math.cos", "math.tan",
                "math.asin", "math.acos", "math.atan",
            ] {
                env.define(name.to_string(), float_to_float.clone());
            }
        }

        // math.pow, math.atan2: (Float, Float) -> Float
        {
            let ff_to_f = Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Float],
                Box::new(Type::Float),
            ));
            env.define("math.pow".into(), ff_to_f.clone());
            env.define("math.atan2".into(), ff_to_f);
        }

        // math.pi, math.e: Float (constants — typed as zero-arg functions)
        env.define("math.pi".into(), Scheme::mono(Type::Float));
        env.define("math.e".into(), Scheme::mono(Type::Float));
    }

    fn register_channel_builtins(&mut self, env: &mut TypeEnv) {
        // channel.new: (Int) -> Channel  (opaque; use fresh var)
        {
            let (ch, chv) = self.fresh_tv();
            env.define("channel.new".into(), Scheme {
                vars: vec![chv],
                ty: Type::Fun(vec![Type::Int], Box::new(ch)),
                constraints: vec![],
            });
        }

        // channel.send: (Channel, a) -> Unit
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("channel.send".into(), Scheme {
                vars: vec![chv, av],
                ty: Type::Fun(vec![ch, a], Box::new(Type::Unit)),
                constraints: vec![],
            });
        }

        // channel.receive: (Channel) -> ChannelResult(a)
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("channel.receive".into(), Scheme {
                vars: vec![chv, av],
                ty: Type::Fun(
                    vec![ch],
                    Box::new(Type::Generic("ChannelResult".into(), vec![a])),
                ),
                constraints: vec![],
            });
        }

        // channel.close: (Channel) -> Unit
        {
            let (ch, chv) = self.fresh_tv();
            env.define("channel.close".into(), Scheme {
                vars: vec![chv],
                ty: Type::Fun(vec![ch], Box::new(Type::Unit)),
                constraints: vec![],
            });
        }

        // channel.try_send: (Channel, a) -> Bool
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("channel.try_send".into(), Scheme {
                vars: vec![chv, av],
                ty: Type::Fun(vec![ch, a], Box::new(Type::Bool)),
                constraints: vec![],
            });
        }

        // channel.try_receive: (Channel) -> ChannelResult(a)
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define("channel.try_receive".into(), Scheme {
                vars: vec![chv, av],
                ty: Type::Fun(
                    vec![ch],
                    Box::new(Type::Generic("ChannelResult".into(), vec![a])),
                ),
                constraints: vec![],
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
                constraints: vec![],
            });
        }

        // channel.each: (Channel(a), Fn(a) -> b) -> Unit
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define("channel.each".into(), Scheme {
                vars: vec![chv, av, bv],
                ty: Type::Fun(
                    vec![ch, Type::Fun(vec![a], Box::new(b))],
                    Box::new(Type::Unit),
                ),
                constraints: vec![],
            });
        }
    }

    fn register_time_builtins(&mut self, env: &mut TypeEnv) {
        // ── Time module type definitions ──────────────────────────────

        let instant_ty = Type::Record("Instant".into(), vec![
            ("epoch_ns".into(), Type::Int),
        ]);
        let date_ty = Type::Record("Date".into(), vec![
            ("year".into(), Type::Int),
            ("month".into(), Type::Int),
            ("day".into(), Type::Int),
        ]);
        let time_of_day_ty = Type::Record("Time".into(), vec![
            ("hour".into(), Type::Int),
            ("minute".into(), Type::Int),
            ("second".into(), Type::Int),
            ("ns".into(), Type::Int),
        ]);
        let datetime_ty = Type::Record("DateTime".into(), vec![
            ("date".into(), date_ty.clone()),
            ("time".into(), time_of_day_ty.clone()),
        ]);
        let duration_ty = Type::Record("Duration".into(), vec![
            ("ns".into(), Type::Int),
        ]);
        let weekday_ty = Type::Generic("Weekday".into(), vec![]);

        // Register record types so field access type-checks
        self.records.insert("Instant".into(), RecordInfo {
            _name: "Instant".into(),
            _params: vec![],
            fields: vec![("epoch_ns".into(), Type::Int)],
        });
        self.records.insert("Date".into(), RecordInfo {
            _name: "Date".into(),
            _params: vec![],
            fields: vec![
                ("year".into(), Type::Int),
                ("month".into(), Type::Int),
                ("day".into(), Type::Int),
            ],
        });
        self.records.insert("Time".into(), RecordInfo {
            _name: "Time".into(),
            _params: vec![],
            fields: vec![
                ("hour".into(), Type::Int),
                ("minute".into(), Type::Int),
                ("second".into(), Type::Int),
                ("ns".into(), Type::Int),
            ],
        });
        self.records.insert("DateTime".into(), RecordInfo {
            _name: "DateTime".into(),
            _params: vec![],
            fields: vec![
                ("date".into(), date_ty.clone()),
                ("time".into(), time_of_day_ty.clone()),
            ],
        });
        self.records.insert("Duration".into(), RecordInfo {
            _name: "Duration".into(),
            _params: vec![],
            fields: vec![("ns".into(), Type::Int)],
        });

        // Register Weekday enum
        self.enums.insert("Weekday".into(), EnumInfo {
            _name: "Weekday".into(),
            params: vec![],
            variants: vec![
                VariantInfo { name: "Monday".into(), field_types: vec![] },
                VariantInfo { name: "Tuesday".into(), field_types: vec![] },
                VariantInfo { name: "Wednesday".into(), field_types: vec![] },
                VariantInfo { name: "Thursday".into(), field_types: vec![] },
                VariantInfo { name: "Friday".into(), field_types: vec![] },
                VariantInfo { name: "Saturday".into(), field_types: vec![] },
                VariantInfo { name: "Sunday".into(), field_types: vec![] },
            ],
        });
        for day in ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"] {
            self.variant_to_enum.insert(day.to_string(), "Weekday".into());
            env.define(day.to_string(), Scheme::mono(weekday_ty.clone()));
        }

        // ── Function signatures ──────────────────────────────────────

        // time.now: () -> Instant
        env.define("time.now".into(), Scheme::mono(Type::Fun(
            vec![], Box::new(instant_ty.clone()),
        )));

        // time.today: () -> Date
        env.define("time.today".into(), Scheme::mono(Type::Fun(
            vec![], Box::new(date_ty.clone()),
        )));

        // time.date: (Int, Int, Int) -> Result(Date, String)
        env.define("time.date".into(), Scheme::mono(Type::Fun(
            vec![Type::Int, Type::Int, Type::Int],
            Box::new(Type::Generic("Result".into(), vec![date_ty.clone(), Type::String])),
        )));

        // time.time: (Int, Int, Int) -> Result(Time, String)
        env.define("time.time".into(), Scheme::mono(Type::Fun(
            vec![Type::Int, Type::Int, Type::Int],
            Box::new(Type::Generic("Result".into(), vec![time_of_day_ty.clone(), Type::String])),
        )));

        // time.datetime: (Date, Time) -> DateTime
        env.define("time.datetime".into(), Scheme::mono(Type::Fun(
            vec![date_ty.clone(), time_of_day_ty.clone()],
            Box::new(datetime_ty.clone()),
        )));

        // time.to_datetime: (Instant, Int) -> DateTime
        env.define("time.to_datetime".into(), Scheme::mono(Type::Fun(
            vec![instant_ty.clone(), Type::Int],
            Box::new(datetime_ty.clone()),
        )));

        // time.to_instant: (DateTime, Int) -> Instant
        env.define("time.to_instant".into(), Scheme::mono(Type::Fun(
            vec![datetime_ty.clone(), Type::Int],
            Box::new(instant_ty.clone()),
        )));

        // time.to_utc: (Instant) -> DateTime
        env.define("time.to_utc".into(), Scheme::mono(Type::Fun(
            vec![instant_ty.clone()],
            Box::new(datetime_ty.clone()),
        )));

        // time.from_utc: (DateTime) -> Instant
        env.define("time.from_utc".into(), Scheme::mono(Type::Fun(
            vec![datetime_ty.clone()],
            Box::new(instant_ty.clone()),
        )));

        // time.format: (DateTime, String) -> String
        env.define("time.format".into(), Scheme::mono(Type::Fun(
            vec![datetime_ty.clone(), Type::String],
            Box::new(Type::String),
        )));

        // time.format_date: (Date, String) -> String
        env.define("time.format_date".into(), Scheme::mono(Type::Fun(
            vec![date_ty.clone(), Type::String],
            Box::new(Type::String),
        )));

        // time.parse: (String, String) -> Result(DateTime, String)
        env.define("time.parse".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic("Result".into(), vec![datetime_ty.clone(), Type::String])),
        )));

        // time.parse_date: (String, String) -> Result(Date, String)
        env.define("time.parse_date".into(), Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic("Result".into(), vec![date_ty.clone(), Type::String])),
        )));

        // time.add_days: (Date, Int) -> Date
        env.define("time.add_days".into(), Scheme::mono(Type::Fun(
            vec![date_ty.clone(), Type::Int],
            Box::new(date_ty.clone()),
        )));

        // time.add_months: (Date, Int) -> Date
        env.define("time.add_months".into(), Scheme::mono(Type::Fun(
            vec![date_ty.clone(), Type::Int],
            Box::new(date_ty.clone()),
        )));

        // time.add: (Instant, Duration) -> Instant
        env.define("time.add".into(), Scheme::mono(Type::Fun(
            vec![instant_ty.clone(), duration_ty.clone()],
            Box::new(instant_ty.clone()),
        )));

        // time.since: (Instant, Instant) -> Duration
        env.define("time.since".into(), Scheme::mono(Type::Fun(
            vec![instant_ty.clone(), instant_ty.clone()],
            Box::new(duration_ty.clone()),
        )));

        // time.hours: (Int) -> Duration
        env.define("time.hours".into(), Scheme::mono(Type::Fun(
            vec![Type::Int], Box::new(duration_ty.clone()),
        )));

        // time.minutes: (Int) -> Duration
        env.define("time.minutes".into(), Scheme::mono(Type::Fun(
            vec![Type::Int], Box::new(duration_ty.clone()),
        )));

        // time.seconds: (Int) -> Duration
        env.define("time.seconds".into(), Scheme::mono(Type::Fun(
            vec![Type::Int], Box::new(duration_ty.clone()),
        )));

        // time.ms: (Int) -> Duration
        env.define("time.ms".into(), Scheme::mono(Type::Fun(
            vec![Type::Int], Box::new(duration_ty.clone()),
        )));

        // time.weekday: (Date) -> Weekday
        env.define("time.weekday".into(), Scheme::mono(Type::Fun(
            vec![date_ty.clone()], Box::new(weekday_ty),
        )));

        // time.days_between: (Date, Date) -> Int
        env.define("time.days_between".into(), Scheme::mono(Type::Fun(
            vec![date_ty.clone(), date_ty.clone()],
            Box::new(Type::Int),
        )));

        // time.days_in_month: (Int, Int) -> Int
        env.define("time.days_in_month".into(), Scheme::mono(Type::Fun(
            vec![Type::Int, Type::Int],
            Box::new(Type::Int),
        )));

        // time.is_leap_year: (Int) -> Bool
        env.define("time.is_leap_year".into(), Scheme::mono(Type::Fun(
            vec![Type::Int],
            Box::new(Type::Bool),
        )));

        // time.sleep: (Duration) -> Unit
        env.define("time.sleep".into(), Scheme::mono(Type::Fun(
            vec![duration_ty],
            Box::new(Type::Unit),
        )));
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
                        .map(|te| self.resolve_type_expr(te, &mut param_vars))
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
                                constraints: vec![],
                            },
                        );
                    } else {
                        // Constructor function
                        env.define(
                            variant.name.clone(),
                            Scheme {
                                vars: var_ids,
                                ty: Type::Fun(field_types, Box::new(result_type)),
                                constraints: vec![],
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
                        let ty = self.resolve_type_expr(&f.ty, &mut param_vars);
                        (f.name.clone(), ty)
                    })
                    .collect();

                self.records.insert(
                    td.name.clone(),
                    RecordInfo {
                        _name: td.name.clone(),
                        _params: td.params.clone(),
                        fields: field_types.clone(),
                    },
                );

                // Register the record type name as a value so it can be
                // passed to json.parse: `json.parse(Employee, str)`
                // The type is the record type itself, so json.parse can
                // propagate it into the return type.
                let record_ty = Type::Record(td.name.clone(), field_types);
                env.define(td.name.clone(), Scheme {
                    vars: vec![],
                    ty: record_ty,
                    constraints: vec![],
                });
            }
        }

        // Auto-derive builtin traits for user-defined types.
        // All enums and records get Equal, Compare, Hash, Display since
        // the runtime supports Eq/Ord/Hash on all Value variants.
        let dummy_span = Span { line: 0, col: 0, offset: 0 };
        for trait_name in &["Equal", "Compare", "Hash", "Display"] {
            self.trait_impl_set.insert((trait_name.to_string(), td.name.clone()));
        }
        // Register auto-derived method entries
        let builtin_methods: &[(&str, Type)] = &[
            ("display", Type::Fun(vec![self.fresh_var()], Box::new(Type::String))),
            ("equal", Type::Fun(vec![self.fresh_var(), self.fresh_var()], Box::new(Type::Bool))),
            ("compare", Type::Fun(vec![self.fresh_var(), self.fresh_var()], Box::new(Type::Int))),
            ("hash", Type::Fun(vec![self.fresh_var()], Box::new(Type::Int))),
        ];
        for (method_name, method_type) in builtin_methods {
            self.method_table.insert(
                (td.name.clone(), method_name.to_string()),
                MethodEntry {
                    method_type: method_type.clone(),
                    span: dummy_span,
                    is_auto_derived: true,
                },
            );
        }
    }

    /// Resolve a TypeExpr AST node to our internal Type representation.
    fn resolve_type_expr(
        &mut self,
        te: &TypeExpr,
        param_vars: &mut HashMap<std::string::String, Type>,
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
                    "Set" => {
                        // Set without explicit type param => Set(fresh_var)
                        Type::Set(Box::new(self.fresh_var()))
                    }
                    _ => {
                        // Lowercase names in type annotations are type variables
                        // (e.g., `a` in `List(a)` or `fn foo(x: a) -> a`)
                        let first_char = name.chars().next().unwrap_or('A');
                        if first_char.is_lowercase() {
                            let tv = self.fresh_var();
                            param_vars.insert(name.clone(), tv.clone());
                            tv
                        } else {
                            // Uppercase: a record or enum type with no params
                            Type::Generic(name.clone(), vec![])
                        }
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
                    "Set" if resolved_args.is_empty() => {
                        Type::Set(Box::new(self.fresh_var()))
                    }
                    "Set" if resolved_args.len() == 1 => {
                        Type::Set(Box::new(resolved_args.into_iter().next().unwrap()))
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
        let mut param_map = HashMap::new();
        let mut param_types = Vec::new();

        for param in &f.params {
            let ty = if let Some(te) = &param.ty {
                self.resolve_type_expr(te, &mut param_map)
            } else {
                self.fresh_var()
            };
            param_types.push(ty);
        }

        let ret_type = if let Some(te) = &f.return_type {
            self.resolve_type_expr(te, &mut param_map)
        } else {
            self.fresh_var()
        };

        let fn_type = Type::Fun(param_types.clone(), Box::new(ret_type));
        let mut scheme = self.generalize(env, &fn_type);

        // Resolve where clauses to (TyVar, trait_name) using param_map.
        // Type variables must be introduced via explicit type annotations in the signature.
        for (type_param, trait_name) in &f.where_clauses {
            if let Some(ty) = param_map.get(type_param) {
                let resolved = self.apply(ty);
                if let Type::Var(tv) = resolved {
                    scheme.constraints.push((tv, trait_name.clone()));
                }
            } else {
                self.error(
                    format!(
                        "type variable '{}' in where clause is not introduced in the function signature; \
                         use an explicit type annotation, e.g.: fn {}({}: {}) where {}: {}",
                        type_param, f.name,
                        f.params.first().map(|p| match &p.pattern {
                            Pattern::Ident(n) => n.as_str(),
                            _ => "_",
                        }).unwrap_or("_"),
                        type_param, type_param, trait_name
                    ),
                    f.span,
                );
            }
        }

        env.define(f.name.clone(), scheme);
    }

    // ── Register trait declarations ─────────────────────────────────

    fn register_trait_decl(&mut self, t: &TraitDecl) {
        let methods: Vec<(std::string::String, Type)> = t
            .methods
            .iter()
            .map(|m| {
                let mut param_map = HashMap::new();
                let mut param_types = Vec::new();
                for param in &m.params {
                    let ty = if let Some(te) = &param.ty {
                        self.resolve_type_expr(te, &mut param_map)
                    } else {
                        self.fresh_var()
                    };
                    param_types.push(ty);
                }
                let ret_type = if let Some(te) = &m.return_type {
                    self.resolve_type_expr(te, &mut param_map)
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
        let impl_key = (ti.trait_name.clone(), ti.target_type.clone());

        // Coherence check: reject duplicate user-defined impls.
        if self.trait_impl_set.contains(&impl_key) {
            // Allow overriding auto-derived impls.
            let first_method = ti.methods.first().map(|m| m.name.as_str()).unwrap_or("display");
            let is_overriding_auto = self.method_table
                .get(&(ti.target_type.clone(), first_method.to_string()))
                .map(|e| e.is_auto_derived)
                .unwrap_or(true);
            if !is_overriding_auto {
                self.error(
                    format!(
                        "duplicate implementation of trait '{}' for type '{}'",
                        ti.trait_name, ti.target_type
                    ),
                    ti.span,
                );
                return;
            }
        }

        self.trait_impl_set.insert(impl_key);

        for method in &ti.methods {
            let mut param_map = HashMap::new();
            let mut param_types = Vec::new();
            for param in &method.params {
                let ty = if let Some(te) = &param.ty {
                    self.resolve_type_expr(te, &mut param_map)
                } else {
                    self.fresh_var()
                };
                param_types.push(ty);
            }
            let ret_type = if let Some(te) = &method.return_type {
                self.resolve_type_expr(te, &mut param_map)
            } else {
                self.fresh_var()
            };

            let fn_type = Type::Fun(param_types, Box::new(ret_type));

            // New: populate method_table.
            self.method_table.insert(
                (ti.target_type.clone(), method.name.clone()),
                MethodEntry {
                    method_type: fn_type.clone(),
                    span: ti.span,
                    is_auto_derived: false,
                },
            );

            // Legacy: register in TypeEnv as "TypeName.method_name".
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
            Pattern::FloatRange(_, _) => {
                self.unify(ty, &Type::Float, Span { line: 0, col: 0, offset: 0 });
            }
            Pattern::Map(entries) => {
                let key_ty = self.fresh_var();
                let val_ty = self.fresh_var();
                let map_ty = Type::Map(Box::new(key_ty), Box::new(val_ty.clone()));
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
                    let elem_type = self.fresh_var();
                    for elem in elems.iter_mut() {
                        match elem {
                            ListElem::Single(e) => {
                                let t = self.infer_expr(e, env);
                                self.unify(&elem_type, &t, e.span);
                            }
                            ListElem::Spread(e) => {
                                let t = self.infer_expr(e, env);
                                let expected = Type::List(Box::new(elem_type.clone()));
                                self.unify(&expected, &t, e.span);
                            }
                        }
                    }
                    Type::List(Box::new(elem_type))
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

            ExprKind::SetLit(elems) => {
                if elems.is_empty() {
                    let tv = self.fresh_var();
                    Type::Set(Box::new(tv))
                } else {
                    let elem_type = self.fresh_var();
                    for e in elems.iter_mut() {
                        let t = self.infer_expr(e, env);
                        self.unify(&elem_type, &t, e.span);
                    }
                    Type::Set(Box::new(elem_type))
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

                // Check for module-style access first (e.g., string.split)
                // Do this BEFORE inferring obj to avoid false "possibly undefined variable" warnings
                // for stdlib module names like list, string, map, io, etc.
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

                // Could be record.field — infer the object type
                let obj_ty = self.infer_expr(obj, env);
                let obj_ty = self.apply(&obj_ty);

                // Field / method access
                match &obj_ty {
                    Type::Record(rec_name, fields) => {
                        // Direct field access first
                        if let Some((_, ft)) = fields.iter().find(|(n, _)| n == &field) {
                            ft.clone()
                        } else if let Some(entry) = self.method_table.get(&(rec_name.clone(), field.clone())).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        } else {
                            self.error(
                                format!("record {rec_name} has no field or method '{field}'"),
                                span,
                            );
                            Type::Error
                        }
                    }
                    Type::Generic(type_name, _) => {
                        // Check record field definitions
                        if let Some(rec_info) = self.records.get(type_name).cloned() {
                            if let Some((_, ft)) =
                                rec_info.fields.iter().find(|(n, _)| n == &field)
                            {
                                let resolved = self.apply(&ft);
                                expr.ty = Some(resolved.clone());
                                return resolved;
                            }
                        }
                        // Check method table (trait methods)
                        if let Some(entry) = self.method_table.get(&(type_name.clone(), field.clone())).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        // Legacy fallback: check TypeEnv for "TypeName.method"
                        let key = format!("{type_name}.{field}");
                        if let Some(scheme) = env.lookup(&key) {
                            let scheme = scheme.clone();
                            let result = self.instantiate(&scheme);
                            let resolved = self.apply(&result);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format!("unknown field or method '{field}' on type {type_name}"),
                            span,
                        );
                        Type::Error
                    }
                    // Primitive types — check method table for trait methods
                    Type::Int | Type::Float | Type::Bool | Type::String | Type::Unit => {
                        let type_name = match &obj_ty {
                            Type::Int => "Int", Type::Float => "Float",
                            Type::Bool => "Bool", Type::String => "String",
                            Type::Unit => "()", _ => unreachable!(),
                        };
                        if let Some(entry) = self.method_table.get(&(type_name.to_string(), field.clone())).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format!("unknown method '{field}' on type {type_name}"),
                            span,
                        );
                        Type::Error
                    }
                    // Collection types
                    Type::List(_) => {
                        if let Some(entry) = self.method_table.get(&("List".to_string(), field.clone())).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(format!("unknown method '{field}' on List"), span);
                        Type::Error
                    }
                    Type::Tuple(_) => {
                        if let Some(entry) = self.method_table.get(&("Tuple".to_string(), field.clone())).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(format!("unknown method '{field}' on Tuple"), span);
                        Type::Error
                    }
                    Type::Map(_, _) => {
                        if let Some(entry) = self.method_table.get(&("Map".to_string(), field.clone())).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(format!("unknown method '{field}' on Map"), span);
                        Type::Error
                    }
                    Type::Set(_) => {
                        if let Some(entry) = self.method_table.get(&("Set".to_string(), field.clone())).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(format!("unknown method '{field}' on Set"), span);
                        Type::Error
                    }
                    // Variant types — look up parent enum
                    Type::Variant(variant_name, _) => {
                        let parent = self.variant_to_enum.get(variant_name).cloned()
                            .unwrap_or_else(|| variant_name.clone());
                        if let Some(entry) = self.method_table.get(&(parent.clone(), field.clone())).cloned() {
                            let resolved = self.apply(&entry.method_type);
                            expr.ty = Some(resolved.clone());
                            return resolved;
                        }
                        self.error(
                            format!("unknown method '{field}' on type {parent}"),
                            span,
                        );
                        Type::Error
                    }
                    Type::Var(_) | Type::Error => {
                        // Unresolved type variable or prior error — stay lenient
                        self.fresh_var()
                    }
                    _ => {
                        self.error(
                            format!("unknown field or method '{field}' on type {obj_ty}"),
                            span,
                        );
                        Type::Error
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

                        // If callee is a named function, use instantiate_with_constraints
                        let (callee_ty, where_constraints) = if let Some(ref name) = callee_fn_name {
                            if let Some(scheme) = env.lookup(name).cloned() {
                                let (ty, constraints) = self.instantiate_with_constraints(&scheme);
                                (self.apply(&ty), constraints)
                            } else {
                                let ty = self.infer_expr(callee, env);
                                (self.apply(&ty), vec![])
                            }
                        } else {
                            let ty = self.infer_expr(callee, env);
                            (self.apply(&ty), vec![])
                        };

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

                        // Check where clause constraints using instantiated TyVars
                        for (tyvar, trait_name) in &where_constraints {
                            let resolved = self.apply(&Type::Var(*tyvar));
                            if let Some(type_name) = self.type_name_for_impl(&resolved) {
                                if !self.trait_impl_set.contains(&(trait_name.clone(), type_name.clone())) {
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

                // If callee is a named function, use instantiate_with_constraints
                // to get where clause constraints with remapped type variables.
                let (callee_ty, where_constraints) = if let Some(ref name) = callee_fn_name {
                    if let Some(scheme) = env.lookup(name).cloned() {
                        let (ty, constraints) = self.instantiate_with_constraints(&scheme);
                        (self.apply(&ty), constraints)
                    } else {
                        let ty = self.infer_expr(callee, env);
                        (self.apply(&ty), vec![])
                    }
                } else {
                    let ty = self.infer_expr(callee, env);
                    (self.apply(&ty), vec![])
                };

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
                                || arg_types.len() == params.len() + 1
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

                // Check where clause constraints using instantiated TyVars
                for (tyvar, trait_name) in &where_constraints {
                    let resolved = self.apply(&Type::Var(*tyvar));
                    if let Some(type_name) = self.type_name_for_impl(&resolved) {
                        if !self.trait_impl_set.contains(&(trait_name.clone(), type_name.clone())) {
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

                result_ty
            }

            ExprKind::Lambda { params, body } => {
                let mut local_env = env.child();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        let ty = if let Some(te) = &p.ty {
                            self.resolve_type_expr(te, &mut HashMap::new())
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
                    self.infer_expr(e, env);
                }
                Type::Never
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
                    let declared = self.resolve_type_expr(te, &mut HashMap::new());
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

            Stmt::WhenBool { condition, else_body } => {
                let cond_ty = self.infer_expr(condition, env);
                self.unify(&cond_ty, &Type::Bool, condition.span);

                // Type check the else body
                let _else_ty = self.infer_expr(else_body, env);

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
                // Validate that all alternatives bind the same set of variables.
                if alts.len() >= 2 {
                    let first_vars: BTreeSet<String> =
                        collect_pattern_vars(&alts[0]).into_iter().collect();
                    for (i, alt) in alts.iter().enumerate().skip(1) {
                        let alt_vars: BTreeSet<String> =
                            collect_pattern_vars(alt).into_iter().collect();
                        if first_vars != alt_vars {
                            self.error(
                                format!(
                                    "or-pattern alternatives must bind the same variables; \
                                     first alternative binds {:?}, alternative {} binds {:?}",
                                    first_vars,
                                    i + 1,
                                    alt_vars
                                ),
                                span,
                            );
                        }
                    }
                }
                for alt in alts {
                    self.check_pattern(alt, expected, env, span);
                }
            }
            Pattern::Range(_, _) => {
                self.unify(expected, &Type::Int, span);
            }
            Pattern::FloatRange(_, _) => {
                self.unify(expected, &Type::Float, span);
            }
            Pattern::Map(entries) => {
                let key_ty = self.fresh_var();
                let val_ty = self.fresh_var();
                let map_ty = Type::Map(Box::new(key_ty), Box::new(val_ty.clone()));
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

    // ── Exhaustiveness checking (Maranget-style usefulness) ──────────
    //
    // Based on "Warnings for pattern matching" (Maranget, JFP 2007).
    // A match is exhaustive iff the wildcard pattern is NOT useful after
    // all arms have been processed.

    fn check_exhaustiveness(
        &mut self,
        arms: &[MatchArm],
        scrutinee_ty: &Type,
        span: Span,
    ) {
        // Collect patterns from arms without guards (guarded arms don't
        // guarantee coverage since the guard may be false).
        let patterns: Vec<&Pattern> = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .map(|a| &a.pattern)
            .collect();

        let scrutinee_ty = self.apply(scrutinee_ty);

        if self.is_useful(&patterns, &Pattern::Wildcard, &scrutinee_ty, 0) {
            let msg = self.missing_description(&patterns, &scrutinee_ty);
            self.error(format!("non-exhaustive match: {msg}"), span);
        }

        // Warn if ALL arms have guards.
        if !arms.is_empty() && arms.iter().all(|a| a.guard.is_some()) {
            self.error(
                "match may be non-exhaustive: all arms have guards".into(),
                span,
            );
        }
    }

    /// Check if `query` is useful with respect to existing patterns.
    /// Returns true if there exists a value matching `query` not matched by `matrix`.
    /// `depth` tracks recursion depth to prevent infinite expansion of recursive types.
    fn is_useful(&self, matrix: &[&Pattern], query: &Pattern, ty: &Type, depth: usize) -> bool {
        // Guard against infinite recursion on recursive types (e.g. type Expr { Num(Int), Add(Expr, Expr) }).
        // Beyond a reasonable depth, conservatively assume exhaustive (not useful).
        const MAX_EXHAUSTIVENESS_DEPTH: usize = 20;
        if depth > MAX_EXHAUSTIVENESS_DEPTH {
            return false;
        }

        if matrix.is_empty() {
            return true;
        }

        // Expand or-patterns in the query.
        if let Pattern::Or(alts) = query {
            return alts.iter().any(|alt| self.is_useful(matrix, alt, ty, depth));
        }

        // Expand or-patterns in the matrix.
        let expanded: Vec<&Pattern> = matrix.iter().flat_map(|p| Self::expand_or(p)).collect();
        let matrix = &expanded[..];

        if matches!(query, Pattern::Wildcard | Pattern::Ident(_)) {
            return self.is_wildcard_useful(matrix, ty, depth);
        }

        self.is_constructor_useful(matrix, query, ty, depth)
    }

    fn expand_or(pat: &Pattern) -> Vec<&Pattern> {
        match pat {
            Pattern::Or(alts) => alts.iter().flat_map(Self::expand_or).collect(),
            _ => vec![pat],
        }
    }

    /// Check if a wildcard is useful: enumerate constructors of the type
    /// and see if they're all covered.
    fn is_wildcard_useful(&self, matrix: &[&Pattern], ty: &Type, depth: usize) -> bool {
        match ty {
            Type::Bool => {
                let true_pat = Pattern::Bool(true);
                let false_pat = Pattern::Bool(false);
                self.is_useful(matrix, &true_pat, ty, depth + 1)
                    || self.is_useful(matrix, &false_pat, ty, depth + 1)
            }
            Type::Generic(name, _) => {
                if let Some(enum_info) = self.enums.get(name).cloned() {
                    for variant in &enum_info.variants {
                        let sub_pats: Vec<Pattern> = (0..variant.field_types.len())
                            .map(|_| Pattern::Wildcard)
                            .collect();
                        let ctor = Pattern::Constructor(variant.name.clone(), sub_pats.clone());
                        if self.is_useful(matrix, &ctor, ty, depth + 1) {
                            return true;
                        }
                    }
                    false
                } else {
                    false
                }
            }
            Type::Tuple(elem_tys) => {
                // Single constructor: the tuple itself.
                let sub_pats: Vec<Pattern> = elem_tys.iter().map(|_| Pattern::Wildcard).collect();
                let tuple_q = Pattern::Tuple(sub_pats);
                self.is_useful(matrix, &tuple_q, ty, depth + 1)
            }
            // Record types have a single constructor — a wildcard is NOT useful
            // if any row already matches (record pattern, wildcard, or ident).
            Type::Record(..) => {
                !matrix.iter().any(|p| matches!(p,
                    Pattern::Wildcard | Pattern::Ident(_) | Pattern::Record { .. }))
            }
            // Lists have two constructors: [] (empty) and [_, ..rest] (non-empty).
            Type::List(_elem_ty) => {
                let empty = Pattern::List(vec![], None);
                let non_empty = Pattern::List(
                    vec![Pattern::Wildcard],
                    Some(Box::new(Pattern::Wildcard)),
                );
                self.is_useful(matrix, &empty, ty, depth + 1)
                    || self.is_useful(matrix, &non_empty, ty, depth + 1)
            }
            // Infinite types: wildcard is useful iff no wildcard/ident in matrix.
            _ => {
                !matrix.iter().any(|p| matches!(p, Pattern::Wildcard | Pattern::Ident(_)))
            }
        }
    }

    /// Check if a specific constructor pattern is useful.
    fn is_constructor_useful(&self, matrix: &[&Pattern], query: &Pattern, ty: &Type, depth: usize) -> bool {
        match query {
            Pattern::Bool(b) => {
                let specialized: Vec<&Pattern> = matrix.iter().filter(|p| {
                    matches!(p, Pattern::Bool(pb) if pb == b)
                        || matches!(p, Pattern::Wildcard | Pattern::Ident(_))
                }).copied().collect();
                specialized.is_empty()
            }
            Pattern::Constructor(name, sub_pats) => {
                let specialized = self.specialize_constructor(matrix, name, sub_pats.len());
                if sub_pats.is_empty() {
                    specialized.is_empty()
                } else {
                    let sub_ty = self.sub_type_for_constructor(name, ty);
                    let sub_query = if sub_pats.len() == 1 {
                        sub_pats[0].clone()
                    } else {
                        Pattern::Tuple(sub_pats.clone())
                    };
                    let sub_refs: Vec<&Pattern> = specialized.iter().collect();
                    self.is_useful(&sub_refs, &sub_query, &sub_ty, depth + 1)
                }
            }
            Pattern::Tuple(sub_pats) => {
                let arity = sub_pats.len();
                // Specialize: keep rows with matching tuple arity, extract sub-patterns.
                // Wildcards expand to N wildcards.
                let specialized = self.specialize_tuple(matrix, arity);
                let spec_refs: Vec<&Pattern> = specialized.iter().collect();
                if arity == 0 {
                    specialized.is_empty()
                } else if arity == 1 {
                    let elem_ty = match ty {
                        Type::Tuple(ts) if !ts.is_empty() => ts[0].clone(),
                        _ => Type::Error,
                    };
                    // Unwrap the single element from each specialized tuple.
                    let unwrapped: Vec<Pattern> = specialized.iter().map(|p| {
                        match p {
                            Pattern::Tuple(ps) if !ps.is_empty() => ps[0].clone(),
                            _ => p.clone(),
                        }
                    }).collect();
                    let unwrapped_refs: Vec<&Pattern> = unwrapped.iter().collect();
                    self.is_useful(&unwrapped_refs, &sub_pats[0], &elem_ty, depth + 1)
                } else {
                    // Multi-element tuple: decompose column-by-column on the
                    // specialized matrix.
                    self.is_tuple_useful_recursive(&spec_refs, sub_pats, ty, depth)
                }
            }
            // List patterns: [] is the "empty" constructor, [h, ..t] is the "cons" constructor.
            Pattern::List(elems, rest) => {
                let is_empty = elems.is_empty() && rest.is_none();
                if is_empty {
                    // Empty list pattern: useful if no empty list or wildcard in matrix
                    let specialized: Vec<&Pattern> = matrix.iter().filter(|p| {
                        matches!(p, Pattern::Wildcard | Pattern::Ident(_))
                            || matches!(p, Pattern::List(e, r) if e.is_empty() && r.is_none())
                    }).copied().collect();
                    specialized.is_empty()
                } else {
                    // Non-empty list pattern: useful if no non-empty list or wildcard covers it
                    let specialized: Vec<&Pattern> = matrix.iter().filter(|p| {
                        matches!(p, Pattern::Wildcard | Pattern::Ident(_))
                            || matches!(p, Pattern::List(e, _) if !e.is_empty())
                    }).copied().collect();
                    specialized.is_empty()
                }
            }
            // Literal patterns — useful iff no wildcard covers them.
            Pattern::Int(_) | Pattern::Float(_) | Pattern::StringLit(_)
            | Pattern::Range(..) | Pattern::FloatRange(..) | Pattern::Pin(_) => {
                !matrix.iter().any(|p| matches!(p, Pattern::Wildcard | Pattern::Ident(_)))
            }
            _ => false,
        }
    }

    /// Check multi-element tuple usefulness by specializing on the first column.
    /// This implements the proper Maranget column decomposition.
    fn is_tuple_useful_recursive(&self, matrix: &[&Pattern], sub_pats: &[Pattern], ty: &Type, depth: usize) -> bool {
        let arity = sub_pats.len();
        if arity == 0 {
            return matrix.is_empty();
        }
        if arity == 1 {
            let col_ty = match ty {
                Type::Tuple(ts) if !ts.is_empty() => ts[0].clone(),
                _ => Type::Error,
            };
            let col_pats: Vec<&Pattern> = matrix.iter().filter_map(|p| {
                match p {
                    Pattern::Tuple(ps) if ps.len() == 1 => Some(&ps[0]),
                    Pattern::Wildcard | Pattern::Ident(_) => Some(*p),
                    _ => None,
                }
            }).collect();
            return self.is_useful(&col_pats, &sub_pats[0], &col_ty, depth + 1);
        }

        // Multi-column: specialize on first column, then recurse on rest.
        let first_ty = match ty {
            Type::Tuple(ts) if !ts.is_empty() => ts[0].clone(),
            _ => Type::Error,
        };
        let rest_ty = match ty {
            Type::Tuple(ts) if ts.len() > 1 => Type::Tuple(ts[1..].to_vec()),
            _ => Type::Error,
        };

        // Get the constructors to check from the first column of the query.
        let query_first = &sub_pats[0];
        let query_rest = Pattern::Tuple(sub_pats[1..].to_vec());

        // For each constructor that query_first could be, specialize the matrix
        // on that constructor in the first column and check if query_rest is useful.
        let first_constructors = self.constructors_for_query(query_first, &first_ty);

        for ctor in &first_constructors {
            // Specialize: keep rows whose first column matches this constructor,
            // replace with the remaining columns.
            let mut specialized_rest: Vec<Pattern> = Vec::new();
            for pat in matrix {
                match pat {
                    Pattern::Tuple(ps) if ps.len() == arity => {
                        if Self::first_col_matches(&ps[0], ctor) {
                            specialized_rest.push(Pattern::Tuple(ps[1..].to_vec()));
                        }
                    }
                    Pattern::Wildcard | Pattern::Ident(_) => {
                        let wilds: Vec<Pattern> = (0..arity - 1).map(|_| Pattern::Wildcard).collect();
                        specialized_rest.push(Pattern::Tuple(wilds));
                    }
                    _ => {}
                }
            }
            let rest_refs: Vec<&Pattern> = specialized_rest.iter().collect();
            if self.is_useful(&rest_refs, &query_rest, &rest_ty, depth + 1) {
                return true;
            }
        }
        false
    }

    /// Get the set of constructors to check for a query pattern against a type.
    fn constructors_for_query(&self, query: &Pattern, ty: &Type) -> Vec<Pattern> {
        match query {
            Pattern::Wildcard | Pattern::Ident(_) => {
                // Need to enumerate all constructors of the type.
                match ty {
                    Type::Bool => vec![Pattern::Bool(true), Pattern::Bool(false)],
                    Type::Generic(name, _) => {
                        if let Some(info) = self.enums.get(name) {
                            info.variants.iter().map(|v| {
                                let sub_pats: Vec<Pattern> = (0..v.field_types.len())
                                    .map(|_| Pattern::Wildcard).collect();
                                Pattern::Constructor(v.name.clone(), sub_pats)
                            }).collect()
                        } else {
                            vec![Pattern::Wildcard]
                        }
                    }
                    _ => vec![Pattern::Wildcard],
                }
            }
            // Specific constructor: just check itself.
            _ => vec![query.clone()],
        }
    }

    /// Check if a pattern in the first column matches a specific constructor.
    fn first_col_matches(pat: &Pattern, ctor: &Pattern) -> bool {
        match (pat, ctor) {
            // Wildcards/idents match anything.
            (Pattern::Wildcard | Pattern::Ident(_), _) => true,
            // A wildcard constructor means "anything" — all patterns match.
            (_, Pattern::Wildcard | Pattern::Ident(_)) => true,
            (Pattern::Bool(a), Pattern::Bool(b)) => a == b,
            (Pattern::Constructor(a, _), Pattern::Constructor(b, _)) => a == b,
            (Pattern::Int(a), Pattern::Int(b)) => a == b,
            (Pattern::StringLit(a), Pattern::StringLit(b)) => a == b,
            _ => false,
        }
    }

    /// Specialize the matrix for a specific enum constructor.
    fn specialize_constructor(&self, matrix: &[&Pattern], ctor_name: &str, arity: usize) -> Vec<Pattern> {
        let mut result = Vec::new();
        for pat in matrix {
            match pat {
                Pattern::Constructor(name, sub_pats) if name == ctor_name => {
                    if arity <= 1 {
                        result.push(sub_pats.first().cloned().unwrap_or(Pattern::Wildcard));
                    } else {
                        result.push(Pattern::Tuple(sub_pats.clone()));
                    }
                }
                Pattern::Wildcard | Pattern::Ident(_) => {
                    if arity <= 1 {
                        result.push(Pattern::Wildcard);
                    } else {
                        let wilds = (0..arity).map(|_| Pattern::Wildcard).collect();
                        result.push(Pattern::Tuple(wilds));
                    }
                }
                _ => {}
            }
        }
        result
    }

    /// Specialize the matrix for a tuple constructor with the given arity.
    fn specialize_tuple(&self, matrix: &[&Pattern], arity: usize) -> Vec<Pattern> {
        let mut result = Vec::new();
        for pat in matrix {
            match pat {
                Pattern::Tuple(sub_pats) if sub_pats.len() == arity => {
                    result.push(Pattern::Tuple(sub_pats.clone()));
                }
                Pattern::Wildcard | Pattern::Ident(_) => {
                    let wilds = (0..arity).map(|_| Pattern::Wildcard).collect();
                    result.push(Pattern::Tuple(wilds));
                }
                _ => {}
            }
        }
        result
    }

    /// Get the sub-type for a constructor's fields.
    fn sub_type_for_constructor(&self, ctor_name: &str, parent_ty: &Type) -> Type {
        if let Some(enum_name) = self.variant_to_enum.get(ctor_name) {
            if let Some(enum_info) = self.enums.get(enum_name) {
                if let Some(variant) = enum_info.variants.iter().find(|v| v.name == ctor_name) {
                    if variant.field_types.len() == 1 {
                        if let Type::Generic(_, type_args) = parent_ty {
                            return substitute_enum_params(
                                &variant.field_types[0],
                                &enum_info.params,
                                type_args,
                            );
                        }
                        return variant.field_types[0].clone();
                    } else if variant.field_types.len() > 1 {
                        let field_types: Vec<Type> = if let Type::Generic(_, type_args) = parent_ty {
                            variant.field_types.iter()
                                .map(|ft| substitute_enum_params(ft, &enum_info.params, type_args))
                                .collect()
                        } else {
                            variant.field_types.clone()
                        };
                        return Type::Tuple(field_types);
                    }
                }
            }
        }
        Type::Error
    }

    /// Generate a human-readable description of what's missing.
    fn missing_description(&self, patterns: &[&Pattern], ty: &Type) -> std::string::String {
        match ty {
            Type::Bool => {
                let has_true = patterns.iter().any(|p| Self::covers_bool(p, true));
                let has_false = patterns.iter().any(|p| Self::covers_bool(p, false));
                let mut missing = Vec::new();
                if !has_true { missing.push("true"); }
                if !has_false { missing.push("false"); }
                if missing.is_empty() {
                    "not all patterns are covered".into()
                } else {
                    format!("missing {}", missing.join(", "))
                }
            }
            Type::Generic(name, _) => {
                if let Some(enum_info) = self.enums.get(name).cloned() {
                    let mut missing = Vec::new();
                    for variant in &enum_info.variants {
                        let sub_pats: Vec<Pattern> = (0..variant.field_types.len())
                            .map(|_| Pattern::Wildcard)
                            .collect();
                        let ctor = Pattern::Constructor(variant.name.clone(), sub_pats);
                        if self.is_useful(patterns, &ctor, ty, 0) {
                            missing.push(variant.name.clone());
                        }
                    }
                    if missing.is_empty() {
                        "not all patterns are covered".into()
                    } else {
                        format!("missing variant(s) {}", missing.join(", "))
                    }
                } else {
                    "not all patterns are covered".into()
                }
            }
            _ => "not all patterns are covered".into(),
        }
    }

    fn covers_bool(pat: &Pattern, val: bool) -> bool {
        match pat {
            Pattern::Bool(b) => *b == val,
            Pattern::Wildcard | Pattern::Ident(_) => true,
            Pattern::Or(alts) => alts.iter().any(|a| Self::covers_bool(a, val)),
            _ => false,
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
                for elem in elems {
                    match elem {
                        ListElem::Single(e) => self.resolve_expr_types(e),
                        ListElem::Spread(e) => self.resolve_expr_types(e),
                    }
                }
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
            ExprKind::SetLit(elems) => {
                for e in elems { self.resolve_expr_types(e); }
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
                        Stmt::WhenBool { condition, else_body } => {
                            self.resolve_expr_types(condition);
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

/// Collect the set of variable names bound by a pattern.
fn collect_pattern_vars(pat: &Pattern) -> Vec<String> {
    match pat {
        Pattern::Ident(name) => vec![name.clone()],
        Pattern::Tuple(pats) => pats.iter().flat_map(collect_pattern_vars).collect(),
        Pattern::List(pats, rest) => {
            let mut vars: Vec<String> = pats.iter().flat_map(collect_pattern_vars).collect();
            if let Some(rest_pat) = rest {
                vars.extend(collect_pattern_vars(rest_pat));
            }
            vars
        }
        Pattern::Constructor(_, pats) => pats.iter().flat_map(collect_pattern_vars).collect(),
        Pattern::Record { fields, .. } => {
            let mut vars: Vec<String> = Vec::new();
            for (field_name, sub_pat) in fields {
                if let Some(p) = sub_pat {
                    vars.extend(collect_pattern_vars(p));
                } else {
                    // Shorthand field `{ x }` binds `x`
                    vars.push(field_name.clone());
                }
            }
            vars
        }
        Pattern::Or(alts) => {
            // Return vars from first alt (they should all be the same after validation)
            alts.first().map(|a| collect_pattern_vars(a)).unwrap_or_default()
        }
        Pattern::Map(entries) => {
            entries.iter().flat_map(|(_, p)| collect_pattern_vars(p)).collect()
        }
        Pattern::Wildcard | Pattern::Int(_) | Pattern::Float(_)
        | Pattern::Bool(_) | Pattern::StringLit(_)
        | Pattern::Range(_, _) | Pattern::FloatRange(_, _) | Pattern::Pin(_) => vec![],
    }
}

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
        Type::Set(inner) => occurs_in(var, inner),
        Type::Int | Type::Float | Type::Bool | Type::String | Type::Unit | Type::Error | Type::Never => false,
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

    #[test]
    fn test_match_non_exhaustive_nested_option() {
        // The new Maranget algorithm catches nested patterns.
        // Matching Ok(Some(x)) and Err(e) misses Ok(None).
        assert_has_error(
            r#"
fn handle(r) {
  match r {
    Ok(Some(x)) -> x
    Err(e) -> 0
  }
}
fn main() { handle(Ok(Some(1))) }
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_exhaustive_nested_option() {
        // Full coverage of nested Option inside Result.
        assert_no_errors(r#"
fn handle(r) {
  match r {
    Ok(Some(x)) -> x
    Ok(None) -> 0
    Err(e) -> 0
  }
}
fn main() { handle(Ok(Some(1))) }
        "#);
    }

    #[test]
    fn test_match_non_exhaustive_bool_in_tuple() {
        // Tuple of bools: (true, true) and (false, false) misses mixed cases.
        assert_has_error(
            r#"
fn check(pair) {
  match pair {
    (true, true) -> "both"
    (false, false) -> "neither"
  }
}
fn main() { check((true, true)) }
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_exhaustive_bool_tuple() {
        assert_no_errors(r#"
fn check(pair) {
  match pair {
    (true, true) -> "both true"
    (true, false) -> "first true"
    (false, _) -> "first false"
  }
}
fn main() { check((true, true)) }
        "#);
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

    // ── Boolean when guard ────────────────────────────────────────────

    #[test]
    fn test_when_bool_guard() {
        assert_no_errors(r#"
fn check(n) {
  when n > 0 else {
    return "not positive"
  }
  "positive"
}

fn main() {
  check(5)
}
        "#);
    }

    #[test]
    fn test_when_bool_mixed_with_pattern_guard() {
        assert_no_errors(r#"
fn process(x) {
  when Ok(value) = Ok(x) else {
    return Err("failed")
  }
  when value > 0 else {
    return Err("must be positive")
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
