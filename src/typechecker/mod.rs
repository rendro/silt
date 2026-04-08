//! Hindley-Milner type inference and checking for Silt.
//!
//! This module implements Algorithm W-style type inference with:
//! - Type variables and unification
//! - Let-polymorphism (generalization at let bindings)
//! - Exhaustiveness checking for match expressions
//! - Type narrowing after `when` guard statements
//! - Trait constraint checking

mod builtins;
mod exhaustiveness;
mod inference;
mod resolve;

pub(super) use std::collections::{BTreeSet, HashMap};

pub(super) use crate::ast::*;
pub(super) use crate::lexer::Span;
pub(super) use crate::types::*;

pub use crate::types::{Scheme, Severity, TyVar, Type, TypeError};

// ── Type environment ────────────────────────────────────────────────

/// A typing environment mapping names to type schemes.
#[derive(Debug, Clone)]
pub(super) struct TypeEnv {
    pub(super) bindings: HashMap<std::string::String, Scheme>,
    parent: Option<Box<TypeEnv>>,
}

impl TypeEnv {
    pub(super) fn new() -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            parent: None,
        }
    }

    pub(super) fn child(&self) -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            parent: Some(Box::new(self.clone())),
        }
    }

    pub(super) fn define(&mut self, name: std::string::String, scheme: Scheme) {
        self.bindings.insert(name, scheme);
    }

    pub(super) fn lookup(&self, name: &str) -> Option<&Scheme> {
        if let Some(s) = self.bindings.get(name) {
            Some(s)
        } else if let Some(ref parent) = self.parent {
            parent.lookup(name)
        } else {
            None
        }
    }

    /// Collect all free type variables in the environment.
    pub(super) fn free_vars(&self, checker: &TypeChecker) -> Vec<TyVar> {
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
pub(super) struct EnumInfo {
    pub(super) _name: std::string::String,
    pub(super) params: Vec<std::string::String>,
    pub(super) variants: Vec<VariantInfo>,
}

#[derive(Debug, Clone)]
pub(super) struct VariantInfo {
    pub(super) name: std::string::String,
    pub(super) field_types: Vec<Type>,
}

/// Information about a declared record type.
#[derive(Debug, Clone)]
pub(super) struct RecordInfo {
    pub(super) _name: std::string::String,
    pub(super) _params: Vec<std::string::String>,
    pub(super) fields: Vec<(std::string::String, Type)>,
}

/// Information about a declared trait.
#[derive(Debug, Clone)]
pub(super) struct TraitInfo {
    pub(super) _name: std::string::String,
    pub(super) methods: Vec<(std::string::String, Type)>,
}

/// A registered trait method implementation (new trait system).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(super) struct MethodEntry {
    pub(super) method_type: Type,
    pub(super) span: Span,
    pub(super) is_auto_derived: bool,
}

// ── The type checker ────────────────────────────────────────────────

pub struct TypeChecker {
    /// The substitution: maps type variables to their resolved types.
    pub(super) subst: Vec<Option<Type>>,
    /// Counter for generating fresh type variables.
    pub(super) next_var: TyVar,
    /// Declared enum types (type name -> enum info).
    pub(super) enums: HashMap<std::string::String, EnumInfo>,
    /// Maps variant constructor name -> parent enum type name.
    pub(super) variant_to_enum: HashMap<std::string::String, std::string::String>,
    /// Declared record types (type name -> record info).
    pub(super) records: HashMap<std::string::String, RecordInfo>,
    /// Declared traits.
    pub(super) traits: HashMap<std::string::String, TraitInfo>,
    /// Method table: (type_name, method_name) → method entry.
    pub(super) method_table: HashMap<(std::string::String, std::string::String), MethodEntry>,
    /// Tracks which (trait_name, type_name) pairs have been implemented.
    pub(super) trait_impl_set: std::collections::HashSet<(std::string::String, std::string::String)>,
    /// Maps function names to their where clauses as (param_index, trait_name).
    /// Accumulated type errors.
    pub errors: Vec<TypeError>,
    /// Tracks the number of bindings in the enclosing `loop` (if any),
    /// so that `recur` arity can be validated.
    pub(super) loop_binding_count: Option<usize>,
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
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

    pub(super) fn fresh_var(&mut self) -> Type {
        let v = self.next_var;
        self.next_var += 1;
        self.subst.push(None);
        Type::Var(v)
    }

    // ── Substitution / apply ────────────────────────────────────────

    /// Walk the substitution chain to find the most resolved type.
    pub(super) fn apply(&self, ty: &Type) -> Type {
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
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.apply(e)).collect()),
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
            Type::Map(k, v) => Type::Map(Box::new(self.apply(k)), Box::new(self.apply(v))),
            Type::Set(inner) => Type::Set(Box::new(self.apply(inner))),
            _ => ty.clone(),
        }
    }

    // ── Unification ─────────────────────────────────────────────────

    pub(super) fn unify(&mut self, t1: &Type, t2: &Type, span: Span) {
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
                    self.error(format!("infinite type: ?{v} occurs in {t}"), span);
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
                    self.error(format!("type mismatch: expected {n2}, got {n1}"), span);
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
                    self.error(format!("variant mismatch: expected {n1}, got {n2}"), span);
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
                self.error(format!("type mismatch: expected {t2}, got {t1}"), span);
            }
        }
    }

    // ── Generalization / Instantiation ──────────────────────────────

    /// Generalize a type into a scheme by quantifying over free variables
    /// not present in the environment.
    pub(super) fn generalize(&self, env: &TypeEnv, ty: &Type) -> Scheme {
        let ty = self.apply(ty);
        let env_fvs = env.free_vars(self);
        let ty_fvs = free_vars_in(&ty);
        let vars: Vec<TyVar> = ty_fvs
            .into_iter()
            .filter(|v| !env_fvs.contains(v))
            .collect();
        Scheme {
            vars,
            ty,
            constraints: vec![],
        }
    }

    /// Instantiate a scheme by replacing quantified variables with fresh ones.
    pub(super) fn instantiate(&mut self, scheme: &Scheme) -> Type {
        self.instantiate_with_constraints(scheme).0
    }

    /// Instantiate a scheme and remap its where clause constraints.
    /// Returns (instantiated_type, remapped_constraints).
    pub(super) fn instantiate_with_constraints(&mut self, scheme: &Scheme) -> (Type, Vec<(TyVar, String)>) {
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        for &v in &scheme.vars {
            mapping.insert(v, self.fresh_var());
        }
        let ty = substitute_vars(&scheme.ty, &mapping);
        let constraints = scheme
            .constraints
            .iter()
            .map(|(v, trait_name)| match mapping.get(v) {
                Some(Type::Var(new_v)) => (*new_v, trait_name.clone()),
                _ => (*v, trait_name.clone()),
            })
            .collect();
        (ty, constraints)
    }

    // ── Type name for trait impl matching ────────────────────────────

    /// Convert a resolved Type to a type name string suitable for matching
    /// against `TraitImplInfo.target_type`. Returns `None` if the type is
    /// unresolved (still a type variable) or cannot be mapped to a name.
    pub(super) fn type_name_for_impl(&self, ty: &Type) -> Option<std::string::String> {
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

    pub(super) fn error(&mut self, message: std::string::String, span: Span) {
        self.errors.push(TypeError {
            message,
            span,
            severity: Severity::Error,
        });
    }

    #[allow(dead_code)]
    pub(super) fn warning(&mut self, message: std::string::String, span: Span) {
        self.errors.push(TypeError {
            message,
            span,
            severity: Severity::Warning,
        });
    }

    // ── Check a full program ────────────────────────────────────────

    pub fn check_program(&mut self, program: &mut Program) {
        let mut env = TypeEnv::new();

        // Register builtins in the type environment
        self.register_builtins(&mut env);

        // Register built-in traits
        {
            let display_self = self.fresh_var();
            self.traits.insert(
                "Display".into(),
                TraitInfo {
                    _name: "Display".into(),
                    methods: vec![(
                        "display".into(),
                        Type::Fun(vec![display_self], Box::new(Type::String)),
                    )],
                },
            );
        }
        {
            let compare_a = self.fresh_var();
            let compare_b = self.fresh_var();
            self.traits.insert(
                "Compare".into(),
                TraitInfo {
                    _name: "Compare".into(),
                    methods: vec![(
                        "compare".into(),
                        Type::Fun(vec![compare_a, compare_b], Box::new(Type::Int)),
                    )],
                },
            );
        }
        {
            let equal_a = self.fresh_var();
            let equal_b = self.fresh_var();
            self.traits.insert(
                "Equal".into(),
                TraitInfo {
                    _name: "Equal".into(),
                    methods: vec![(
                        "equal".into(),
                        Type::Fun(vec![equal_a, equal_b], Box::new(Type::Bool)),
                    )],
                },
            );
        }
        {
            let hash_self = self.fresh_var();
            self.traits.insert(
                "Hash".into(),
                TraitInfo {
                    _name: "Hash".into(),
                    methods: vec![(
                        "hash".into(),
                        Type::Fun(vec![hash_self], Box::new(Type::Int)),
                    )],
                },
            );
        }

        // Register builtin trait implementations for primitive types.
        // These allow where clauses like `where a: Equal` to resolve
        // when `a` is Int, String, Bool, etc.
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let primitive_types = ["Int", "Float", "Bool", "String", "()"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
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
            for type_name in &primitive_types {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((trait_name.to_string(), type_name.to_string()));
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
                    self.trait_impl_set
                        .insert((trait_name.to_string(), type_name.to_string()));
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

        // Process imports: register selective/aliased import names in the type environment
        for decl in &program.decls {
            if let Decl::Import(ImportTarget::Items(module, items)) = decl {
                if crate::module::is_builtin_module(module) {
                    for item in items {
                        let qualified = format!("{module}.{item}");
                        if let Some(scheme) = env.lookup(&qualified).cloned() {
                            env.define(item.clone(), scheme);
                        }
                        // Gated constructors (like Monday, GET) are already
                        // registered under their bare name — no alias needed.
                    }
                }
            } else if let Decl::Import(ImportTarget::Alias(module, alias)) = decl
                && crate::module::is_builtin_module(module)
            {
                let functions = crate::module::builtin_module_functions(module);
                for func in functions {
                    let qualified = format!("{module}.{func}");
                    let aliased = format!("{alias}.{func}");
                    if let Some(scheme) = env.lookup(&qualified).cloned() {
                        env.define(aliased, scheme);
                    }
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
            if let Decl::Let {
                ref mut value,
                ref pattern,
                ref ty,
                span,
                ..
            } = program.decls[i]
            {
                let val_ty = self.infer_expr(value, &mut env);
                if let Some(te) = ty {
                    let declared =
                        self.resolve_type_expr(te, &mut std::collections::HashMap::new());
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

        // Fourth pass: detect unresolved type variables on let-binding values
        // where the user did not provide a type annotation.
        self.check_unresolved_let_types(program);

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
                let span = self
                    .method_table
                    .iter()
                    .find(|((t, _), _)| t == type_name)
                    .map(|(_, e)| e.span)
                    .unwrap_or(Span::new(0, 0));
                self.error(format!("trait '{trait_name}' is not declared"), span);
                continue;
            };

            // Skip auto-derived impls (builtin traits on all types).
            let is_auto = trait_info
                .methods
                .first()
                .and_then(|(m, _)| self.method_table.get(&(type_name.clone(), m.clone())))
                .map(|e| e.is_auto_derived)
                .unwrap_or(false);
            if is_auto {
                continue;
            }

            // Check that all required methods are implemented with correct signature.
            for (method_name, trait_method_type) in &trait_info.methods {
                let key = (type_name.clone(), method_name.clone());
                if let Some(entry) = self.method_table.get(&key) {
                    let impl_type = entry.method_type.clone();
                    let impl_span = entry.span;
                    // Instantiate the trait method type with fresh variables so
                    // that unification doesn't permanently bind trait-level vars
                    // (which would break validation of other impls).
                    let fvs = free_vars_in(trait_method_type);
                    let mapping: HashMap<TyVar, Type> = fvs
                        .into_iter()
                        .map(|v| (v, self.fresh_var()))
                        .collect();
                    let expected = substitute_vars(trait_method_type, &mapping);
                    self.unify(&impl_type, &expected, impl_span);
                } else {
                    // Find a span for the error.
                    let span = self
                        .method_table
                        .iter()
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

    /// Helper: create a fresh type variable and return both the Type::Var and
    /// its TyVar id.
    pub(super) fn fresh_tv(&mut self) -> (Type, TyVar) {
        let t = self.fresh_var();
        let v = match &t {
            Type::Var(v) => *v,
            _ => unreachable!(),
        };
        (t, v)
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
                    let type_params: Vec<Type> =
                        td.params.iter().map(|p| param_vars[p].clone()).collect();

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
                env.define(
                    td.name.clone(),
                    Scheme {
                        vars: vec![],
                        ty: record_ty,
                        constraints: vec![],
                    },
                );
            }
        }

        // Auto-derive builtin traits for user-defined types.
        // All enums and records get Equal, Compare, Hash, Display since
        // the runtime supports Eq/Ord/Hash on all Value variants.
        let dummy_span = Span {
            line: 0,
            col: 0,
            offset: 0,
        };
        for trait_name in &["Equal", "Compare", "Hash", "Display"] {
            self.trait_impl_set
                .insert((trait_name.to_string(), td.name.clone()));
        }
        // Register auto-derived method entries
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
    pub(super) fn resolve_type_expr(
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
                    "List" if resolved_args.is_empty() => Type::List(Box::new(self.fresh_var())),
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
                    "Set" if resolved_args.is_empty() => Type::Set(Box::new(self.fresh_var())),
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
            TypeExpr::SelfType => {
                if let Some(ty) = param_vars.get("Self") {
                    ty.clone()
                } else {
                    // Self used outside of a trait context
                    self.fresh_var()
                }
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
        let self_var = self.fresh_var();
        let methods: Vec<(std::string::String, Type)> = t
            .methods
            .iter()
            .map(|m| {
                let mut param_map = HashMap::new();
                param_map.insert("Self".to_string(), self_var.clone());
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

    /// Convert a type name string (like "Int", "Float", "MyRecord") to a Type.
    fn type_from_name(name: &str) -> Type {
        match name {
            "Int" => Type::Int,
            "Float" => Type::Float,
            "Bool" => Type::Bool,
            "String" => Type::String,
            _ => Type::Generic(name.to_string(), vec![]),
        }
    }

    fn register_trait_impl(&mut self, ti: &TraitImpl, env: &mut TypeEnv) {
        let impl_key = (ti.trait_name.clone(), ti.target_type.clone());

        // Coherence check: reject duplicate user-defined impls.
        if self.trait_impl_set.contains(&impl_key) {
            // Allow overriding auto-derived impls.
            let first_method = ti
                .methods
                .first()
                .map(|m| m.name.as_str())
                .unwrap_or("display");
            let is_overriding_auto = self
                .method_table
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

        let self_type = Self::type_from_name(&ti.target_type);

        for method in &ti.methods {
            let mut param_map = HashMap::new();
            param_map.insert("Self".to_string(), self_type.clone());
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



}

// ── Helper functions ────────────────────────────────────────────────

/// Collect the set of variable names bound by a pattern.
pub(super) fn collect_pattern_vars(pat: &Pattern) -> Vec<String> {
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
            alts.first().map(collect_pattern_vars).unwrap_or_default()
        }
        Pattern::Map(entries) => entries
            .iter()
            .flat_map(|(_, p)| collect_pattern_vars(p))
            .collect(),
        Pattern::Wildcard
        | Pattern::Int(_)
        | Pattern::Float(_)
        | Pattern::Bool(_)
        | Pattern::StringLit(_)
        | Pattern::Range(_, _)
        | Pattern::FloatRange(_, _)
        | Pattern::Pin(_) => vec![],
    }
}

/// Check if a type variable occurs in a type (occurs check for unification).
fn occurs_in(var: TyVar, ty: &Type) -> bool {
    match ty {
        Type::Var(v) => *v == var,
        Type::Fun(params, ret) => params.iter().any(|p| occurs_in(var, p)) || occurs_in(var, ret),
        Type::List(inner) => occurs_in(var, inner),
        Type::Tuple(elems) => elems.iter().any(|e| occurs_in(var, e)),
        Type::Record(_, fields) => fields.iter().any(|(_, t)| occurs_in(var, t)),
        Type::Variant(_, args) | Type::Generic(_, args) => args.iter().any(|a| occurs_in(var, a)),
        Type::Map(k, v) => occurs_in(var, k) || occurs_in(var, v),
        Type::Set(inner) => occurs_in(var, inner),
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Error
        | Type::Never => false,
    }
}

/// Run the type checker on a program. Returns a list of type errors (warnings).
pub fn check(program: &mut Program) -> Vec<TypeError> {
    let mut checker = TypeChecker::new();
    checker.check_program(program);
    checker.errors
}

/// Return a map of builtin qualified names to their type signature strings.
/// Used by the LSP to show type info in completions.
pub fn builtin_type_signatures() -> std::collections::HashMap<String, String> {
    let mut checker = TypeChecker::new();
    let mut env = TypeEnv::new();
    checker.register_builtins(&mut env);
    let mut sigs = std::collections::HashMap::new();
    for (name, scheme) in &env.bindings {
        if name.contains('.') {
            let ty = checker.instantiate(scheme);
            sigs.insert(name.clone(), format!("{ty}"));
        }
    }
    sigs
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
        let hard_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
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
            errors
                .iter()
                .any(|e| e.message.contains(expected_substring)),
            "expected an error containing '{}', got: {:?}",
            expected_substring,
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // ── Basic type inference ────────────────────────────────────────

    #[test]
    fn test_int_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = 42
  x
}
        "#,
        );
    }

    #[test]
    fn test_float_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = 3.14
  x
}
        "#,
        );
    }

    #[test]
    fn test_string_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = "hello"
  x
}
        "#,
        );
    }

    #[test]
    fn test_bool_literal() {
        assert_no_errors(
            r#"
fn main() {
  let x = true
  x
}
        "#,
        );
    }

    #[test]
    fn test_arithmetic() {
        assert_no_errors(
            r#"
fn main() {
  let x = 1 + 2
  let y = x * 3
  y
}
        "#,
        );
    }

    #[test]
    fn test_comparison() {
        assert_no_errors(
            r#"
fn main() {
  let x = 1 < 2
  x
}
        "#,
        );
    }

    #[test]
    fn test_function_call() {
        assert_no_errors(
            r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1, 2)
}
        "#,
        );
    }

    #[test]
    fn test_shadowing() {
        assert_no_errors(
            r#"
fn main() {
  let x = 1
  let x = x + 1
  let x = x * 3
  x
}
        "#,
        );
    }

    // ── List inference ──────────────────────────────────────────────

    #[test]
    fn test_list_inference() {
        assert_no_errors(
            r#"
fn main() {
  let xs = [1, 2, 3]
  xs
}
        "#,
        );
    }

    #[test]
    fn test_empty_list() {
        assert_no_errors(
            r#"
fn main() {
  let xs = []
  xs
}
        "#,
        );
    }

    // ── Tuple inference ─────────────────────────────────────────────

    #[test]
    fn test_tuple_inference() {
        assert_no_errors(
            r#"
fn main() {
  let pair = (1, "hello")
  pair
}
        "#,
        );
    }

    // ── Lambda inference ────────────────────────────────────────────

    #[test]
    fn test_lambda() {
        assert_no_errors(
            r#"
fn main() {
  let double = fn(x) { x * 2 }
  double(5)
}
        "#,
        );
    }

    // ── Enum types ──────────────────────────────────────────────────

    #[test]
    fn test_enum_type() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    // ── Record types ────────────────────────────────────────────────

    #[test]
    fn test_record_type() {
        assert_no_errors(
            r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn main() {
  let u = User { name: "Alice", age: 30, active: true }
  u.name
}
        "#,
        );
    }

    #[test]
    fn test_record_update() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    // ── Match exhaustiveness ────────────────────────────────────────

    #[test]
    fn test_match_exhaustive_with_wildcard() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    #[test]
    fn test_match_exhaustive_all_variants() {
        assert_no_errors(
            r#"
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
        "#,
        );
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
        assert_no_errors(
            r#"
fn handle(r) {
  match r {
    Ok(Some(x)) -> x
    Ok(None) -> 0
    Err(e) -> 0
  }
}
fn main() { handle(Ok(Some(1))) }
        "#,
        );
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
        assert_no_errors(
            r#"
fn check(pair) {
  match pair {
    (true, true) -> "both true"
    (true, false) -> "first true"
    (false, _) -> "first false"
  }
}
fn main() { check((true, true)) }
        "#,
        );
    }

    // ── Generic types ───────────────────────────────────────────────

    #[test]
    fn test_option_some_none() {
        assert_no_errors(
            r#"
fn main() {
  let x = Some(42)
  let y = None
  match x {
    Some(n) -> n
    None -> 0
  }
}
        "#,
        );
    }

    #[test]
    fn test_result_ok_err() {
        assert_no_errors(
            r#"
fn main() {
  let x = Ok(42)
  match x {
    Ok(n) -> n
    Err(e) -> 0
  }
}
        "#,
        );
    }

    // ── Question mark operator ──────────────────────────────────────

    #[test]
    fn test_question_mark() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    // ── When guard (type narrowing) ─────────────────────────────────

    #[test]
    fn test_when_guard() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    // ── Boolean when guard ────────────────────────────────────────────

    #[test]
    fn test_when_bool_guard() {
        assert_no_errors(
            r#"
fn check(n) {
  when n > 0 else {
    return "not positive"
  }
  "positive"
}

fn main() {
  check(5)
}
        "#,
        );
    }

    #[test]
    fn test_when_bool_mixed_with_pattern_guard() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    // ── Pipe operator ───────────────────────────────────────────────

    #[test]
    fn test_pipe_operator() {
        assert_no_errors(
            r#"
fn main() {
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
}
        "#,
        );
    }

    // ── String interpolation ────────────────────────────────────────

    #[test]
    fn test_string_interpolation() {
        assert_no_errors(
            r#"
fn main() {
  let name = "world"
  let n = 42
  "hello {name}, the answer is {n}"
}
        "#,
        );
    }

    // ── Trait implementation ────────────────────────────────────────

    #[test]
    fn test_trait_impl() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    // ── Map literal ─────────────────────────────────────────────────

    #[test]
    fn test_map_literal() {
        assert_no_errors(
            r#"
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  m
}
        "#,
        );
    }

    // ── Single-expression function ──────────────────────────────────

    #[test]
    fn test_single_expr_fn() {
        assert_no_errors(
            r#"
fn square(x) = x * x
fn add(a, b) = a + b

fn main() {
  add(square(3), square(4))
}
        "#,
        );
    }

    // ── Integration test programs ───────────────────────────────────

    #[test]
    fn test_fizzbuzz_program() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    #[test]
    fn test_closures_and_higher_order() {
        assert_no_errors(
            r#"
fn make_adder(n) {
  fn(x) { x + n }
}

fn main() {
  let add5 = make_adder(5)
  add5(10)
}
        "#,
        );
    }

    #[test]
    fn test_error_handling_pipeline() {
        assert_no_errors(
            r#"
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when Some(port_line) = lines |> list.find { l -> string.contains(l, "port=") } else {
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
        "#,
        );
    }

    #[test]
    fn test_match_with_guards() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    // ── Let-polymorphism ────────────────────────────────────────────

    #[test]
    fn test_let_polymorphism() {
        assert_no_errors(
            r#"
fn identity(x) {
  x
}

fn main() {
  let a = identity(42)
  let b = identity("hello")
  a
}
        "#,
        );
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
        assert_no_errors(
            r#"
fn main() {
  let r = 1..10
  r
}
        "#,
        );
    }

    // ── Exhaustiveness: guards don't count as covering ──────────────

    #[test]
    fn test_match_guards_with_catch_all() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    // ── Severity tests ─────────────────────────────────────────────

    #[test]
    fn test_type_error_has_error_severity() {
        // A type mismatch should produce Severity::Error
        let errors = check_errors(
            r#"
            fn main() {
                let x: Int = "hello"
                x
            }
        "#,
        );
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.severity == Severity::Error));
    }

    #[test]
    fn test_valid_program_no_errors() {
        let errors = check_errors(
            r#"
            fn main() {
                let x = 42
                x + 1
            }
        "#,
        );
        let hard_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
        assert!(hard_errors.is_empty());
    }

    #[test]
    fn test_trait_impl_validates_methods() {
        // Complete impl should have no errors about missing methods
        let errors = check_program(
            r#"
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
        "#,
        );
        let trait_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("missing method"))
            .collect();
        assert!(
            trait_errors.is_empty(),
            "unexpected trait errors: {:?}",
            trait_errors
        );
    }

    #[test]
    fn test_trait_impl_missing_method() {
        let errors = check_program(
            r#"
            trait Showable {
                fn show(self) -> String { "default" }
                fn detail(self) -> String { "detail" }
            }
            trait Showable for Item {
                fn show(self) -> String { "item" }
            }
            type Item { name: String }
            fn main() { 0 }
        "#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("missing method") && e.message.contains("detail"))
        );
    }

    #[test]
    fn test_trait_impl_unknown_trait() {
        let errors = check_program(
            r#"
            trait Nonexistent for Thing {
                fn foo(self) -> Int { 0 }
            }
            type Thing { x: Int }
            fn main() { 0 }
        "#,
        );
        assert!(errors.iter().any(|e| e.message.contains("not declared")));
    }

    #[test]
    fn test_builtin_display_trait_exists() {
        // Implementing Display should not produce "trait not declared" error
        let errors = check_program(
            r#"
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
        "#,
        );
        let undeclared: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("not declared"))
            .collect();
        assert!(
            undeclared.is_empty(),
            "Display should be a built-in trait: {:?}",
            undeclared
        );
    }

    #[test]
    fn test_where_unknown_trait_warning() {
        let errors = check_program(
            r#"
            fn show(x) where x: Nonexistent {
                x
            }
            fn main() { 0 }
        "#,
        );
        assert!(errors.iter().any(|e| e.message.contains("Nonexistent")));
    }

    #[test]
    fn test_where_constraint_satisfied() {
        // Should produce no errors about constraints
        let errors = check_errors(
            r#"
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
        "#,
        );
        let constraint_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("does not implement"))
            .collect();
        assert!(
            constraint_errors.is_empty(),
            "unexpected: {:?}",
            constraint_errors
        );
    }

    #[test]
    fn test_where_constraint_violated() {
        // Should produce an error: Int doesn't implement Showable
        let errors = check_errors(
            r#"
            trait Showable {
                fn show(self) -> String { "default" }
            }
            fn display(x) where x: Showable {
                x
            }
            fn main() {
                display(42)
            }
        "#,
        );
        assert!(errors.iter().any(|e| e.message.contains("does not implement") || e.message.contains("Showable")),
            "expected constraint violation error, got: {:?}", errors);
    }

    // ── Record types with generic fields (List, Map) ───────────────

    #[test]
    fn test_record_with_list_field() {
        assert_no_errors(
            r#"
type Bag {
  items: List,
  name: String,
}

fn main() {
  let b = Bag { items: [1, 2, 3], name: "test" }
  b.name
}
        "#,
        );
    }

    #[test]
    fn test_record_with_map_field() {
        assert_no_errors(
            r#"
type Config {
  data: Map,
}

fn main() {
  let c = Config { data: #{ "key": "value" } }
  c.data
}
        "#,
        );
    }

    #[test]
    fn test_record_with_list_and_map_fields() {
        assert_no_errors(
            r#"
type Config {
  values: Map,
  errors: List,
}

fn main() {
  let c = Config { values: #{ "a": 1 }, errors: ["err1", "err2"] }
  c.values
}
        "#,
        );
    }

    #[test]
    fn test_record_with_list_field_access() {
        assert_no_errors(
            r#"
type Bag {
  items: List,
}

fn main() {
  let b = Bag { items: [1, 2, 3] }
  b.items
}
        "#,
        );
    }

    // ── Tests for newly registered builtins ────────────────────────

    #[test]
    fn test_list_module_builtins() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    #[test]
    fn test_string_module_builtins() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    #[test]
    fn test_float_module_builtins() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    #[test]
    fn test_int_module_builtins() {
        assert_no_errors(
            r#"
fn main() {
  let a = 5
  let b = 3
  let mn = int.min(a, b)
  let mx = int.max(a, b)
  let f = int.to_float(a)
  f
}
        "#,
        );
    }

    #[test]
    fn test_map_module_builtins() {
        assert_no_errors(
            r#"
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
        "#,
        );
    }

    #[test]
    fn test_io_module_builtins() {
        assert_no_errors(
            r#"
fn main() {
  let result = io.read_file("test.txt")
  let args = io.args()
  args
}
        "#,
        );
    }

    #[test]
    fn test_option_module_builtins() {
        assert_no_errors(
            r#"
fn main() {
  let opt = Some(42)
  let is_s = option.is_some(opt)
  let is_n = option.is_none(opt)
  let val = option.unwrap_or(opt, 0)
  let mapped = option.map(opt, fn(x) { x + 1 })
  let res = option.to_result(opt, "no value")
  val
}
        "#,
        );
    }

    #[test]
    fn test_result_module_builtins() {
        assert_no_errors(
            r#"
fn main() {
  let r = Ok(42)
  let is_ok = result.is_ok(r)
  let is_err = result.is_err(r)
  is_ok
}
        "#,
        );
    }

    #[test]
    fn test_higher_order_builtins() {
        assert_no_errors(
            r#"
fn main() {
  let xs = [[1, 2], [3, 4], [5]]
  let flat = list.flatten(xs)
  let zipped = list.zip([1, 2, 3], ["a", "b", "c"])
  let sorted = list.sort_by([3, 1, 2], fn(x) { x })
  flat
}
        "#,
        );
    }

    #[test]
    fn test_len_accepts_string_and_map() {
        assert_no_errors(
            r#"
fn main() {
  let list_len = list.length([1, 2, 3])
  let str_len = string.length("hello")
  let map_len = map.length(#{ "a": 1 })
  list_len + str_len + map_len
}
        "#,
        );
    }

    #[test]
    fn test_assert_ne_builtin() {
        assert_no_errors(
            r#"
fn main() {
  test.assert_ne(1, 2)
}
        "#,
        );
    }

    #[test]
    fn test_channel_new_no_type_error() {
        assert_no_errors(
            r#"
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.close(ch)
  ch
}
        "#,
        );
    }

    #[test]
    fn test_task_spawn_no_type_error() {
        assert_no_errors(
            r#"
fn main() {
  let h = task.spawn(fn() { 42 })
  let result = task.join(h)
  result
}
        "#,
        );
    }

    #[test]
    fn test_map_length_no_type_error() {
        assert_no_errors(
            r#"
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let n = map.length(m)
  n
}
        "#,
        );
    }

    // ── Type narrowing after when/pattern match ────────────────────

    #[test]
    fn test_when_some_narrows_inner_type() {
        // After `when Some(x) = opt`, x should have the inner type (Int)
        assert_no_errors(
            r#"
fn get_value(opt) {
  when Some(x) = opt else {
    return 0
  }
  x + 1
}

fn main() {
  get_value(Some(42))
}
            "#,
        );
    }

    #[test]
    fn test_when_ok_narrows_inner_type() {
        // After `when Ok(v) = result`, v should have the ok type
        assert_no_errors(
            r#"
fn process(result) {
  when Ok(v) = result else {
    return "error"
  }
  v + 10
}

fn main() {
  process(Ok(5))
}
            "#,
        );
    }

    #[test]
    fn test_when_some_used_in_arithmetic() {
        assert_no_errors(
            r#"
fn double_or_zero(opt) {
  when Some(n) = opt else {
    return 0
  }
  n * 2
}

fn main() {
  double_or_zero(Some(21))
}
            "#,
        );
    }

    // ── Generic type inference ──────────────────────────────────────

    #[test]
    fn test_generic_identity_multiple_types() {
        // A generic function used with multiple types
        assert_no_errors(
            r#"
fn identity(x) {
  x
}

fn main() {
  let a = identity(42)
  let b = identity("hello")
  let c = identity(true)
  a + 1
}
            "#,
        );
    }

    #[test]
    fn test_nested_generic_list_of_options() {
        // List<Option<Int>> — nested generic type
        assert_no_errors(
            r#"
fn main() {
  let xs = [Some(1), Some(2), None]
  xs
}
            "#,
        );
    }

    #[test]
    fn test_generic_function_returning_generic() {
        assert_no_errors(
            r#"
fn wrap(x) {
  Some(x)
}

fn main() {
  let a = wrap(42)
  let b = wrap("hello")
  match a {
    Some(n) -> n
    None -> 0
  }
}
            "#,
        );
    }

    #[test]
    fn test_generic_pair_function() {
        assert_no_errors(
            r#"
fn make_pair(a, b) {
  (a, b)
}

fn main() {
  let p1 = make_pair(1, "hello")
  let p2 = make_pair(true, 3.14)
  p1
}
            "#,
        );
    }

    // ── Recursive functions ─────────────────────────────────────────

    #[test]
    fn test_recursive_function() {
        assert_no_errors(
            r#"
fn factorial(n) {
  match n {
    0 -> 1
    _ -> n * factorial(n - 1)
  }
}

fn main() {
  factorial(5)
}
            "#,
        );
    }

    #[test]
    fn test_recursive_list_function() {
        assert_no_errors(
            r#"
fn sum(xs) {
  match list.head(xs) {
    None -> 0
    Some(h) -> h + sum(list.tail(xs))
  }
}

fn main() {
  sum([1, 2, 3])
}
            "#,
        );
    }

    // ── More exhaustiveness checking ────────────────────────────────

    #[test]
    fn test_match_int_without_wildcard_non_exhaustive() {
        // Matching on Int literal patterns without wildcard should be non-exhaustive
        assert_has_error(
            r#"
fn describe(n) {
  match n {
    0 -> "zero"
    1 -> "one"
  }
}

fn main() {
  describe(2)
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_string_without_wildcard_non_exhaustive() {
        // Matching on String literal patterns without wildcard should be non-exhaustive
        assert_has_error(
            r#"
fn greet(name) {
  match name {
    "alice" -> "hi alice"
    "bob" -> "hi bob"
  }
}

fn main() {
  greet("carol")
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_enum_one_variant_non_exhaustive() {
        // Matching only one variant of a multi-variant enum
        assert_has_error(
            r#"
type Shape {
  Circle(Float)
  Square(Float)
  Triangle(Float, Float)
}

fn area(s) {
  match s {
    Circle(r) -> 3.14 * r * r
  }
}

fn main() {
  area(Circle(5.0))
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_all_guards_non_exhaustive() {
        // Guard arms don't count toward exhaustiveness
        assert_has_error(
            r#"
fn classify(n) {
  match n {
    x when x > 0 -> "positive"
    x when x < 0 -> "negative"
    x when x == 0 -> "zero"
  }
}

fn main() {
  classify(5)
}
            "#,
            "non-exhaustive",
        );
    }

    #[test]
    fn test_match_int_with_wildcard_exhaustive() {
        // Adding a wildcard makes int matching exhaustive
        assert_no_errors(
            r#"
fn describe(n) {
  match n {
    0 -> "zero"
    1 -> "one"
    _ -> "other"
  }
}

fn main() {
  describe(2)
}
            "#,
        );
    }

    #[test]
    fn test_match_string_with_wildcard_exhaustive() {
        assert_no_errors(
            r#"
fn greet(name) {
  match name {
    "alice" -> "hi alice"
    "bob" -> "hi bob"
    _ -> "hi stranger"
  }
}

fn main() {
  greet("carol")
}
            "#,
        );
    }

    // ── Error cases ─────────────────────────────────────────────────

    #[test]
    fn test_wrong_number_of_arguments() {
        assert_has_error(
            r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1, 2, 3)
}
            "#,
            "argument",
        );
    }

    #[test]
    fn test_too_few_arguments() {
        assert_has_error(
            r#"
fn add(a, b) {
  a + b
}

fn main() {
  add(1)
}
            "#,
            "argument",
        );
    }

    #[test]
    fn test_access_nonexistent_record_field() {
        assert_has_error(
            r#"
type Point { x: Int, y: Int }

fn main() {
  let p = Point { x: 1, y: 2 }
  p.z
}
            "#,
            "no field",
        );
    }

    #[test]
    fn test_undefined_variable() {
        assert_has_error(
            r#"
fn main() {
  let x = 1
  y + x
}
            "#,
            "undefined variable",
        );
    }

    #[test]
    fn test_arithmetic_on_string_and_int() {
        // String + Int should produce a type mismatch
        assert_has_error(
            r#"
fn main() {
  "hello" + 42
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_boolean_and_with_non_bool() {
        assert_has_error(
            r#"
fn main() {
  let x = "hello" && true
  x
}
            "#,
            "type mismatch",
        );
    }

    #[test]
    fn test_int_minus_string() {
        assert_has_error(
            r#"
fn main() {
  42 - "hello"
}
            "#,
            "type mismatch",
        );
    }

    // ── Set type inference ──────────────────────────────────────────

    #[test]
    fn test_set_literal_inference() {
        assert_no_errors(
            r#"
fn main() {
  let s = #[1, 2, 3]
  s
}
            "#,
        );
    }

    #[test]
    fn test_empty_set_literal() {
        assert_no_errors(
            r#"
fn main() {
  let s = #[]
  s
}
            "#,
        );
    }

    #[test]
    fn test_set_of_strings() {
        assert_no_errors(
            r#"
fn main() {
  let s = #["hello", "world"]
  s
}
            "#,
        );
    }

    // ── Loop/recur ──────────────────────────────────────────────────

    #[test]
    fn test_loop_basic() {
        assert_no_errors(
            r#"
fn main() {
  loop n = 0 {
    match n > 10 {
      true -> n
      false -> loop(n + 1)
    }
  }
}
            "#,
        );
    }

    #[test]
    fn test_loop_with_accumulator() {
        assert_no_errors(
            r#"
fn main() {
  loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      false -> loop(i + 1, acc + i)
    }
  }
}
            "#,
        );
    }

    #[test]
    fn test_loop_recur_arity_mismatch() {
        // loop has 2 bindings, recur has 1 argument
        let errors = check_program(
            r#"
fn main() {
  loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      false -> loop(i + 1)
    }
  }
}
            "#,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("binding") || e.message.contains("argument")),
            "expected recur arity warning, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // ── Trait system edge cases ─────────────────────────────────────

    #[test]
    fn test_trait_impl_with_wrong_method_signature() {
        // Impl that is missing one of the required methods
        let errors = check_program(
            r#"
trait Describable {
  fn describe(self) -> String { "default" }
  fn summary(self) -> String { "summary" }
}

type Widget { label: String }

trait Describable for Widget {
  fn describe(self) -> String { "widget" }
}

fn main() { 0 }
            "#,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("missing method")),
            "expected missing method error, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_trait_unknown_in_impl() {
        assert_has_error(
            r#"
type Foo { x: Int }

trait DoesNotExist for Foo {
  fn bar(self) -> Int { 0 }
}

fn main() { 0 }
            "#,
            "not declared",
        );
    }

    #[test]
    fn test_where_clause_unknown_trait() {
        let errors = check_program(
            r#"
fn do_thing(x) where x: FakeTrait {
  x
}

fn main() { 0 }
            "#,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("FakeTrait")),
            "expected unknown trait error, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_multiple_trait_impls_for_same_type() {
        // Implementing two different traits for the same type should be fine
        let errors = check_program(
            r#"
trait Printable {
  fn print(self) -> String { "default" }
}

trait Serializable {
  fn serialize(self) -> String { "default" }
}

type Item { name: String }

trait Printable for Item {
  fn print(self) -> String { "item" }
}

trait Serializable for Item {
  fn serialize(self) -> String { "serialized" }
}

fn main() { 0 }
            "#,
        );
        // Should not produce "not declared" or "missing method" errors
        let bad_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("not declared") || e.message.contains("missing method"))
            .collect();
        assert!(
            bad_errors.is_empty(),
            "unexpected trait errors: {:?}",
            bad_errors
        );
    }

    // ── Ascription (as) ─────────────────────────────────────────────

    #[test]
    fn test_valid_ascription() {
        assert_no_errors(
            r#"
fn main() {
  let x = 42 as Int
  x
}
            "#,
        );
    }

    #[test]
    fn test_ascription_string() {
        assert_no_errors(
            r#"
fn main() {
  let s = "hello" as String
  s
}
            "#,
        );
    }

    #[test]
    fn test_ascription_incompatible_type() {
        assert_has_error(
            r#"
fn main() {
  let x = 42 as String
  x
}
            "#,
            "type mismatch",
        );
    }

    // ── Import-dependent type checking ──────────────────────────────

    #[test]
    fn test_string_module_split() {
        assert_no_errors(
            r#"
fn main() {
  let parts = string.split("a,b,c", ",")
  parts
}
            "#,
        );
    }

    #[test]
    fn test_list_map_and_filter() {
        assert_no_errors(
            r#"
fn main() {
  let xs = [1, 2, 3, 4, 5]
  let doubled = list.map(xs, fn(x) { x * 2 })
  let evens = list.filter(xs, fn(x) { x > 2 })
  doubled
}
            "#,
        );
    }

    #[test]
    fn test_map_get_returns_option() {
        assert_no_errors(
            r#"
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let result = map.get(m, "a")
  match result {
    Some(v) -> v
    None -> 0
  }
}
            "#,
        );
    }

    #[test]
    fn test_chained_module_calls() {
        assert_no_errors(
            r#"
fn main() {
  let s = "Hello World"
  let result = s
    |> string.to_lower
    |> string.split(" ")
    |> list.length
  result
}
            "#,
        );
    }

    // ── Additional edge cases ───────────────────────────────────────

    #[test]
    fn test_nested_match_exhaustive() {
        // Nested Result<Option<Int>> fully covered
        assert_no_errors(
            r#"
fn process(r) {
  match r {
    Ok(Some(x)) -> x
    Ok(None) -> -1
    Err(_) -> -2
  }
}

fn main() {
  process(Ok(Some(42)))
}
            "#,
        );
    }

    #[test]
    fn test_enum_match_all_variants_exhaustive() {
        assert_no_errors(
            r#"
type Direction {
  North
  South
  East
  West
}

fn to_string(d) {
  match d {
    North -> "north"
    South -> "south"
    East -> "east"
    West -> "west"
  }
}

fn main() {
  to_string(North)
}
            "#,
        );
    }

    #[test]
    fn test_record_update_type_checks() {
        assert_no_errors(
            r#"
type Config {
  host: String,
  port: Int,
}

fn main() {
  let c = Config { host: "localhost", port: 8080 }
  let c2 = c.{ port: 9090 }
  c2.host
}
            "#,
        );
    }

    #[test]
    fn test_question_mark_on_non_result() {
        // Using ? on a non-Result/Option type should error
        assert_has_error(
            r#"
fn main() -> Result {
  let x = 42?
  x
}
            "#,
            "requires Result or Option",
        );
    }
}
