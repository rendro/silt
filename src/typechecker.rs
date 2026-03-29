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

// ── Type representation ─────────────────────────────────────────────

/// A unique identifier for type variables.
pub type TyVar = usize;

/// The core type representation used during inference.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Bool,
    String,
    Unit,
    /// A unification variable, to be resolved during inference.
    Var(TyVar),
    /// Function type: param types -> return type.
    Fun(Vec<Type>, Box<Type>),
    /// Homogeneous list type.
    List(Box<Type>),
    /// Tuple type (fixed length, heterogeneous).
    Tuple(Vec<Type>),
    /// A nominal record type: name + field name/type pairs.
    Record(std::string::String, Vec<(std::string::String, Type)>),
    /// A variant/constructor from an enum type.
    Variant(std::string::String, Vec<Type>),
    /// A generic/parameterized type like Result(Int, String).
    Generic(std::string::String, Vec<Type>),
    /// Map type: key type -> value type.
    Map(Box<Type>, Box<Type>),
    /// An error type used to allow inference to continue after errors.
    Error,
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Int => write!(f, "Int"),
            Type::Float => write!(f, "Float"),
            Type::Bool => write!(f, "Bool"),
            Type::String => write!(f, "String"),
            Type::Unit => write!(f, "()"),
            Type::Var(v) => write!(f, "?{v}"),
            Type::Fun(params, ret) => {
                write!(f, "(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ") -> {ret}")
            }
            Type::List(inner) => write!(f, "List({inner})"),
            Type::Tuple(elems) => {
                write!(f, "(")?;
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{e}")?;
                }
                write!(f, ")")
            }
            Type::Record(name, fields) => {
                write!(f, "{name} {{")?;
                for (i, (n, t)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{n}: {t}")?;
                }
                write!(f, "}}")
            }
            Type::Variant(name, args) => {
                write!(f, "{name}")?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{a}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Type::Generic(name, args) => {
                write!(f, "{name}")?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{a}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Type::Map(k, v) => write!(f, "Map({k}, {v})"),
            Type::Error => write!(f, "<error>"),
        }
    }
}

// ── Type scheme (polymorphic type) ──────────────────────────────────

/// A type scheme represents a polymorphic type: forall vars . ty
/// The `vars` are the universally quantified type variables.
#[derive(Debug, Clone)]
pub struct Scheme {
    pub vars: Vec<TyVar>,
    pub ty: Type,
}

impl Scheme {
    fn mono(ty: Type) -> Self {
        Scheme {
            vars: Vec::new(),
            ty,
        }
    }
}

// ── Type errors ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct TypeError {
    pub message: std::string::String,
    pub span: Span,
    pub severity: Severity,
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self.severity {
            Severity::Error => "type error",
            Severity::Warning => "type warning",
        };
        write!(f, "[{}] {}: {}", self.span, label, self.message)
    }
}

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

/// Collect free type variables in a type.
/// Count the number of parameters in a function type.
fn count_params(ty: &Type) -> usize {
    match ty {
        Type::Fun(params, _) => params.len(),
        _ => 0,
    }
}

fn free_vars_in(ty: &Type) -> Vec<TyVar> {
    match ty {
        Type::Var(v) => vec![*v],
        Type::Fun(params, ret) => {
            let mut fvs = Vec::new();
            for p in params {
                for v in free_vars_in(p) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
            for v in free_vars_in(ret) {
                if !fvs.contains(&v) {
                    fvs.push(v);
                }
            }
            fvs
        }
        Type::List(inner) => free_vars_in(inner),
        Type::Tuple(elems) => {
            let mut fvs = Vec::new();
            for e in elems {
                for v in free_vars_in(e) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
            fvs
        }
        Type::Record(_, fields) => {
            let mut fvs = Vec::new();
            for (_, t) in fields {
                for v in free_vars_in(t) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
            fvs
        }
        Type::Variant(_, args) | Type::Generic(_, args) => {
            let mut fvs = Vec::new();
            for a in args {
                for v in free_vars_in(a) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
            fvs
        }
        Type::Map(k, v) => {
            let mut fvs = free_vars_in(k);
            for fv in free_vars_in(v) {
                if !fvs.contains(&fv) {
                    fvs.push(fv);
                }
            }
            fvs
        }
        Type::Int | Type::Float | Type::Bool | Type::String | Type::Unit | Type::Error => {
            Vec::new()
        }
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

    pub fn check_program(&mut self, program: &Program) {
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

        // Third pass: type check function bodies
        for decl in &program.decls {
            if let Decl::Fn(f) = decl {
                self.check_fn_body(f, &env);
            }
        }

        // Also check trait impl method bodies
        for decl in &program.decls {
            if let Decl::TraitImpl(ti) = decl {
                for method in &ti.methods {
                    self.check_fn_body(method, &env);
                }
            }
        }
    }

    // ── Validate trait implementations ────────────────────────────────

    fn validate_trait_impls(&mut self) {
        // Clone to avoid borrow issues
        let impls = self.trait_impls.clone();
        for impl_info in &impls {
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

    fn register_builtins(&mut self, env: &mut TypeEnv) {
        // print/println: String -> ()
        let str_to_unit = Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Unit),
        ));
        env.define("print".into(), str_to_unit.clone());
        env.define("println".into(), str_to_unit);

        // inspect: a -> String
        {
            let a = self.fresh_var();
            let tv = match &a {
                Type::Var(v) => *v,
                _ => unreachable!(),
            };
            env.define(
                "inspect".into(),
                Scheme {
                    vars: vec![tv],
                    ty: Type::Fun(vec![a], Box::new(Type::String)),
                },
            );
        }

        // panic: String -> a (never returns, but we type it as -> a)
        {
            let a = self.fresh_var();
            let tv = match &a {
                Type::Var(v) => *v,
                _ => unreachable!(),
            };
            env.define(
                "panic".into(),
                Scheme {
                    vars: vec![tv],
                    ty: Type::Fun(vec![Type::String], Box::new(a)),
                },
            );
        }

        // map: (List(a), (a -> b)) -> List(b)
        {
            let a = self.fresh_var();
            let b = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            let bv = match &b { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "map".into(),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(b.clone())),
                        ],
                        Box::new(Type::List(Box::new(b))),
                    ),
                },
            );
        }

        // filter: (List(a), (a -> Bool)) -> List(a)
        {
            let a = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "filter".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                        ],
                        Box::new(Type::List(Box::new(a))),
                    ),
                },
            );
        }

        // fold: (List(a), b, (b, a) -> b) -> b
        {
            let a = self.fresh_var();
            let b = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            let bv = match &b { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "fold".into(),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            b.clone(),
                            Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                        ],
                        Box::new(b),
                    ),
                },
            );
        }

        // each: (List(a), (a -> ())) -> ()
        {
            let a = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "each".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(Type::Unit)),
                        ],
                        Box::new(Type::Unit),
                    ),
                },
            );
        }

        // find: (List(a), (a -> Bool)) -> Option(a) = Generic("Option", [a])
        {
            let a = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "find".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                        ],
                        Box::new(Type::Generic("Option".into(), vec![a])),
                    ),
                },
            );
        }

        // len: List(a) -> Int
        {
            let a = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "len".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a))],
                        Box::new(Type::Int),
                    ),
                },
            );
        }

        // Register builtin variant constructors
        // Ok(a) and Err(e) for Result(a, e)
        {
            let a = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            let e = self.fresh_var();
            let ev = match &e { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "Ok".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic("Result".into(), vec![a, e])),
                    ),
                },
            );
        }
        {
            let a = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            let e = self.fresh_var();
            let ev = match &e { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "Err".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![e.clone()],
                        Box::new(Type::Generic("Result".into(), vec![a, e])),
                    ),
                },
            );
        }
        // Some(a) and None for Option(a)
        {
            let a = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "Some".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic("Option".into(), vec![a])),
                    ),
                },
            );
        }
        {
            let a = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "None".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic("Option".into(), vec![a]),
                },
            );
        }

        // Register builtin enum info for Option and Result
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

        // Register stdlib module functions as "module.function" names
        // string module
        {
            // string.split: (String, String) -> List(String)
            env.define(
                "string.split".into(),
                Scheme::mono(Type::Fun(
                    vec![Type::String, Type::String],
                    Box::new(Type::List(Box::new(Type::String))),
                )),
            );
            // string.contains: (String, String) -> Bool
            env.define(
                "string.contains".into(),
                Scheme::mono(Type::Fun(
                    vec![Type::String, Type::String],
                    Box::new(Type::Bool),
                )),
            );
            // string.replace: (String, String, String) -> String  -- but silt uses replace(str, old) with pipe
            // Actually interpreter has it as: string.replace(s, old, new) -> String, but sometimes 2 args
            // Let's be permissive: accept 2 or 3 args by registering with 3
            env.define(
                "string.replace".into(),
                Scheme::mono(Type::Fun(
                    vec![Type::String, Type::String],
                    Box::new(Type::String),
                )),
            );
            // string.join: (List(String), String) -> String
            env.define(
                "string.join".into(),
                Scheme::mono(Type::Fun(
                    vec![Type::List(Box::new(Type::String)), Type::String],
                    Box::new(Type::String),
                )),
            );
            // string.trim: String -> String
            env.define(
                "string.trim".into(),
                Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
            );
        }

        // int module
        {
            // int.parse: String -> Result(Int, String)
            env.define(
                "int.parse".into(),
                Scheme::mono(Type::Fun(
                    vec![Type::String],
                    Box::new(Type::Generic(
                        "Result".into(),
                        vec![Type::Int, Type::String],
                    )),
                )),
            );
            // int.abs: Int -> Int
            env.define(
                "int.abs".into(),
                Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::Int))),
            );
        }

        // assert_eq: (a, a) -> ()
        {
            let a = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "assert_eq".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a.clone(), a], Box::new(Type::Unit)),
                },
            );
        }

        // assert: Bool -> ()
        env.define(
            "assert".into(),
            Scheme::mono(Type::Fun(vec![Type::Bool], Box::new(Type::Unit))),
        );

        // map_ok: (Result(a,e), (a -> b)) -> Result(b,e)
        {
            let a = self.fresh_var();
            let b = self.fresh_var();
            let e = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            let bv = match &b { Type::Var(v) => *v, _ => unreachable!() };
            let ev = match &e { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "map_ok".into(),
                Scheme {
                    vars: vec![av, bv, ev],
                    ty: Type::Fun(
                        vec![
                            Type::Generic("Result".into(), vec![a, e.clone()]),
                            Type::Fun(vec![Type::Var(av)], Box::new(b.clone())),
                        ],
                        Box::new(Type::Generic("Result".into(), vec![b, e])),
                    ),
                },
            );
        }

        // unwrap_or: (Result(a,e), a) -> a  (or Option version)
        {
            let a = self.fresh_var();
            let e = self.fresh_var();
            let av = match &a { Type::Var(v) => *v, _ => unreachable!() };
            let ev = match &e { Type::Var(v) => *v, _ => unreachable!() };
            env.define(
                "unwrap_or".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![
                            Type::Generic("Result".into(), vec![a.clone(), e]),
                            a.clone(),
                        ],
                        Box::new(a),
                    ),
                },
            );
        }

        // if_nonzero: not a builtin, but user-defined in tests.
        // We don't need it here.
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

    fn check_fn_body(&mut self, f: &FnDecl, env: &TypeEnv) {
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

        let (param_types, _ret_type) = match &fn_type {
            Type::Fun(params, ret) => (params.clone(), *ret.clone()),
            _ => return,
        };

        // Bind parameters
        for (i, param) in f.params.iter().enumerate() {
            if let Some(ty) = param_types.get(i) {
                self.bind_pattern(&param.pattern, ty, &mut local_env);
            }
        }

        // Infer the body
        let _body_type = self.infer_expr(&f.body, &mut local_env);

        // We could unify body_type with ret_type here for stricter checking,
        // but for now we keep it lenient to avoid blocking execution.
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
        }
    }

    // ── Expression type inference ───────────────────────────────────

    fn infer_expr(&mut self, expr: &Expr, env: &mut TypeEnv) -> Type {
        match &expr.kind {
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
                    let first = self.infer_expr(&elems[0], env);
                    for elem in &elems[1..] {
                        let t = self.infer_expr(elem, env);
                        self.unify(&first, &t, elem.span);
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
                    let first_k = self.infer_expr(&entries[0].0, env);
                    let first_v = self.infer_expr(&entries[0].1, env);
                    for (k, v) in &entries[1..] {
                        let kt = self.infer_expr(k, env);
                        let vt = self.infer_expr(v, env);
                        self.unify(&first_k, &kt, k.span);
                        self.unify(&first_v, &vt, v.span);
                    }
                    Type::Map(Box::new(first_k), Box::new(first_v))
                }
            }

            ExprKind::Tuple(elems) => {
                let types: Vec<Type> = elems
                    .iter()
                    .map(|e| self.infer_expr(e, env))
                    .collect();
                Type::Tuple(types)
            }

            ExprKind::Ident(name) => {
                if let Some(scheme) = env.lookup(name) {
                    let scheme = scheme.clone();
                    self.instantiate(&scheme)
                } else {
                    // Unknown variable - could be from an unresolved import
                    // Don't error, just return a fresh variable (lenient mode)
                    self.fresh_var()
                }
            }

            ExprKind::FieldAccess(obj, field) => {
                // Could be record.field, or module.function
                let obj_ty = self.infer_expr(obj, env);
                let obj_ty = self.apply(&obj_ty);

                // Check for module-style access first (e.g., string.split)
                if let ExprKind::Ident(module_name) = &obj.kind {
                    let qualified = format!("{module_name}.{field}");
                    if let Some(scheme) = env.lookup(&qualified) {
                        let scheme = scheme.clone();
                        return self.instantiate(&scheme);
                    }
                }

                // Record field access
                match &obj_ty {
                    Type::Record(rec_name, fields) => {
                        if let Some((_, ft)) = fields.iter().find(|(n, _)| n == field) {
                            ft.clone()
                        } else {
                            self.error(
                                format!("record {rec_name} has no field {field}"),
                                expr.span,
                            );
                            Type::Error
                        }
                    }
                    Type::Generic(type_name, _) => {
                        // Check if the type has a record definition
                        if let Some(rec_info) = self.records.get(type_name).cloned() {
                            if let Some((_, ft)) =
                                rec_info.fields.iter().find(|(n, _)| n == field)
                            {
                                return ft.clone();
                            }
                        }
                        // Could be a trait method: TypeName.method
                        let key = format!("{type_name}.{field}");
                        if let Some(scheme) = env.lookup(&key) {
                            let scheme = scheme.clone();
                            return self.instantiate(&scheme);
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
                let lt = self.infer_expr(lhs, env);
                let rt = self.infer_expr(rhs, env);

                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        self.unify(&lt, &rt, expr.span);
                        lt
                    }
                    BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Leq | BinOp::Geq => {
                        self.unify(&lt, &rt, expr.span);
                        Type::Bool
                    }
                    BinOp::And | BinOp::Or => {
                        self.unify(&lt, &Type::Bool, lhs.span);
                        self.unify(&rt, &Type::Bool, rhs.span);
                        Type::Bool
                    }
                }
            }

            ExprKind::Unary(op, operand) => {
                let t = self.infer_expr(operand, env);
                match op {
                    UnaryOp::Neg => t,
                    UnaryOp::Not => {
                        self.unify(&t, &Type::Bool, operand.span);
                        Type::Bool
                    }
                }
            }

            ExprKind::Pipe(lhs, rhs) => {
                let arg_type = self.infer_expr(lhs, env);

                // Pipe semantics: a |> f(b) means f(a, b)
                // If the RHS is a Call, we prepend the pipe LHS as the first argument.
                match &rhs.kind {
                    ExprKind::Call(callee, call_args) => {
                        let callee_ty = self.infer_expr(callee, env);
                        let callee_ty = self.apply(&callee_ty);

                        // Infer types for the explicit call args
                        let explicit_arg_types: Vec<Type> = call_args
                            .iter()
                            .map(|a| self.infer_expr(a, env))
                            .collect();

                        // All args = [pipe_lhs, ...explicit_args]
                        let mut all_arg_types = vec![arg_type];
                        all_arg_types.extend(explicit_arg_types);

                        let result_ty = match &callee_ty {
                            Type::Fun(params, ret) => {
                                let min_len = params.len().min(all_arg_types.len());
                                for i in 0..min_len {
                                    let span = if i == 0 { lhs.span } else { call_args[i - 1].span };
                                    self.unify(&all_arg_types[i], &params[i], span);
                                }
                                *ret.clone()
                            }
                            Type::Var(_) => {
                                let ret = self.fresh_var();
                                let fn_ty = Type::Fun(all_arg_types.clone(), Box::new(ret.clone()));
                                self.unify(&callee_ty, &fn_ty, expr.span);
                                ret
                            }
                            _ => self.fresh_var(),
                        };

                        // Check where clause constraints for piped calls
                        if let ExprKind::Ident(fn_name) = &callee.kind {
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
                                                    expr.span,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        result_ty
                    }
                    _ => {
                        // RHS is a plain function/lambda, not a call
                        let fn_type = self.infer_expr(rhs, env);
                        let fn_type = self.apply(&fn_type);

                        match &fn_type {
                            Type::Fun(params, ret) => {
                                if !params.is_empty() {
                                    self.unify(&arg_type, &params[0], expr.span);
                                }
                                *ret.clone()
                            }
                            Type::Var(_) => {
                                let ret = self.fresh_var();
                                let fn_ty = Type::Fun(vec![arg_type], Box::new(ret.clone()));
                                self.unify(&fn_type, &fn_ty, expr.span);
                                ret
                            }
                            _ => self.fresh_var(),
                        }
                    }
                }
            }

            ExprKind::Range(start, end) => {
                let st = self.infer_expr(start, env);
                let et = self.infer_expr(end, env);
                self.unify(&st, &Type::Int, start.span);
                self.unify(&et, &Type::Int, end.span);
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
                let callee_ty = self.infer_expr(callee, env);
                let callee_ty = self.apply(&callee_ty);

                let arg_types: Vec<Type> = args
                    .iter()
                    .map(|a| self.infer_expr(a, env))
                    .collect();

                let result_ty = match &callee_ty {
                    Type::Fun(params, ret) => {
                        // Unify argument types with parameter types
                        let min_len = params.len().min(arg_types.len());
                        for i in 0..min_len {
                            self.unify(&arg_types[i], &params[i], args[i].span);
                        }
                        *ret.clone()
                    }
                    Type::Var(_) => {
                        // The callee is an unresolved type variable - create a function type
                        let ret = self.fresh_var();
                        let fn_ty = Type::Fun(arg_types.clone(), Box::new(ret.clone()));
                        self.unify(&callee_ty, &fn_ty, expr.span);
                        ret
                    }
                    _ => {
                        // Lenient: might be a constructor or something we can't resolve
                        self.fresh_var()
                    }
                };

                // Check where clause constraints
                if let ExprKind::Ident(fn_name) = &callee.kind {
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
                                            expr.span,
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
                if let Some(rec_info) = self.records.get(name).cloned() {
                    let field_types: Vec<(std::string::String, Type)> = fields
                        .iter()
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
                            self.unify(inferred_ty, declared_ty, expr.span);
                        }
                    }

                    Type::Record(name.clone(), rec_info.fields.clone())
                } else {
                    // Unknown record type - infer from fields
                    let field_types: Vec<(std::string::String, Type)> = fields
                        .iter()
                        .map(|(n, e)| {
                            let ty = self.infer_expr(e, env);
                            (n.clone(), ty)
                        })
                        .collect();
                    Type::Record(name.clone(), field_types)
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
                        let scrutinee_ty = self.infer_expr(scrutinee, env);
                        let result_ty = self.fresh_var();

                        for arm in arms {
                            let mut arm_env = env.child();
                            self.check_pattern(&arm.pattern, &scrutinee_ty, &mut arm_env, scrutinee.span);

                            if let Some(ref guard) = arm.guard {
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard.span);
                            }

                            let arm_ty = self.infer_expr(&arm.body, &mut arm_env);
                            self.unify(&result_ty, &arm_ty, arm.body.span);
                        }

                        // Check exhaustiveness after pattern checking, so the
                        // scrutinee type is fully resolved through unification.
                        let resolved_scrutinee_ty = self.apply(&scrutinee_ty);
                        self.check_exhaustiveness(arms, &resolved_scrutinee_ty, scrutinee.span);

                        result_ty
                    }
                    None => {
                        // Guardless match: each arm's guard is a boolean condition
                        let result_ty = self.fresh_var();

                        for arm in arms {
                            let mut arm_env = env.child();

                            if let Some(ref guard) = arm.guard {
                                let guard_ty = self.infer_expr(guard, &mut arm_env);
                                self.unify(&guard_ty, &Type::Bool, guard.span);
                            }

                            let arm_ty = self.infer_expr(&arm.body, &mut arm_env);
                            self.unify(&result_ty, &arm_ty, arm.body.span);
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

            ExprKind::Select { arms } => {
                // Infer the type from the first arm's body; all arms should agree
                let mut result_ty = self.fresh_var();
                for arm in arms {
                    self.infer_expr(&arm.channel, env);
                    let body_ty = self.infer_expr(&arm.body, env);
                    self.unify(&result_ty, &body_ty, expr.span);
                    result_ty = body_ty;
                }
                result_ty
            }
        }
    }

    // ── Statement type inference ────────────────────────────────────

    fn infer_stmt(&mut self, stmt: &Stmt, env: &mut TypeEnv) -> Type {
        match stmt {
            Stmt::Let { pattern, ty, value } => {
                let val_ty = self.infer_expr(value, env);

                if let Some(te) = &ty {
                    let declared = self.resolve_type_expr(te, &HashMap::new());
                    self.unify(&val_ty, &declared, value.span);
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

/// Substitute type variables according to a mapping.
fn substitute_vars(ty: &Type, mapping: &HashMap<TyVar, Type>) -> Type {
    match ty {
        Type::Var(v) => {
            if let Some(replacement) = mapping.get(v) {
                replacement.clone()
            } else {
                ty.clone()
            }
        }
        Type::Fun(params, ret) => {
            let params = params
                .iter()
                .map(|p| substitute_vars(p, mapping))
                .collect();
            let ret = Box::new(substitute_vars(ret, mapping));
            Type::Fun(params, ret)
        }
        Type::List(inner) => Type::List(Box::new(substitute_vars(inner, mapping))),
        Type::Tuple(elems) => {
            Type::Tuple(elems.iter().map(|e| substitute_vars(e, mapping)).collect())
        }
        Type::Record(name, fields) => {
            let fields = fields
                .iter()
                .map(|(n, t)| (n.clone(), substitute_vars(t, mapping)))
                .collect();
            Type::Record(name.clone(), fields)
        }
        Type::Variant(name, args) => {
            let args = args.iter().map(|a| substitute_vars(a, mapping)).collect();
            Type::Variant(name.clone(), args)
        }
        Type::Generic(name, args) => {
            let args = args.iter().map(|a| substitute_vars(a, mapping)).collect();
            Type::Generic(name.clone(), args)
        }
        Type::Map(k, v) => Type::Map(
            Box::new(substitute_vars(k, mapping)),
            Box::new(substitute_vars(v, mapping)),
        ),
        _ => ty.clone(),
    }
}

/// Substitute enum type parameters with concrete type arguments.
/// This is used when we know e.g. Result(Int, String) and want to
/// resolve the type of a variant's field.
fn substitute_enum_params(
    field_ty: &Type,
    param_names: &[std::string::String],
    type_args: &[Type],
) -> Type {
    match field_ty {
        Type::Var(v) => {
            // If this Var index corresponds to a param position, substitute
            if (*v) < param_names.len() && (*v) < type_args.len() {
                type_args[*v].clone()
            } else {
                field_ty.clone()
            }
        }
        Type::Fun(params, ret) => {
            let params = params
                .iter()
                .map(|p| substitute_enum_params(p, param_names, type_args))
                .collect();
            let ret = Box::new(substitute_enum_params(ret, param_names, type_args));
            Type::Fun(params, ret)
        }
        Type::List(inner) => {
            Type::List(Box::new(substitute_enum_params(inner, param_names, type_args)))
        }
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| substitute_enum_params(e, param_names, type_args))
                .collect(),
        ),
        Type::Generic(name, args) => {
            let args = args
                .iter()
                .map(|a| substitute_enum_params(a, param_names, type_args))
                .collect();
            Type::Generic(name.clone(), args)
        }
        _ => field_ty.clone(),
    }
}

/// Run the type checker on a program. Returns a list of type errors (warnings).
pub fn check(program: &Program) -> Vec<TypeError> {
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
        let program = parse(input);
        check(&program)
    }

    fn check_program(input: &str) -> Vec<TypeError> {
        check_errors(input)
    }

    fn assert_no_errors(input: &str) {
        let errors = check_program(input);
        if !errors.is_empty() {
            panic!(
                "expected no type errors, got:\n{}",
                errors
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
}
