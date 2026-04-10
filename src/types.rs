//! Shared type definitions for Silt's type system.
//!
//! This module contains the core type representations used by the type checker,
//! interpreter, and other parts of the compiler pipeline.

use std::collections::HashMap;

use crate::intern::Symbol;
use crate::lexer::Span;

// ── Type representation ─────────────────────────────────────────────

/// A unique identifier for type variables.
pub type TyVar = usize;

/// The core type representation used during inference.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    ExtFloat,
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
    Record(Symbol, Vec<(Symbol, Type)>),
    /// A variant/constructor from an enum type.
    Variant(Symbol, Vec<Type>),
    /// A generic/parameterized type like Result(Int, String).
    Generic(Symbol, Vec<Type>),
    /// Map type: key type -> value type.
    Map(Box<Type>, Box<Type>),
    /// Set type: element type.
    Set(Box<Type>),
    /// Channel type: element type carried through the channel.
    Channel(Box<Type>),
    /// An error type used to allow inference to continue after errors.
    Error,
    /// A bottom type for expressions that never produce a value (return, panic).
    Never,
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Int => write!(f, "Int"),
            Type::Float => write!(f, "Float"),
            Type::ExtFloat => write!(f, "ExtFloat"),
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
            Type::Set(inner) => write!(f, "Set({inner})"),
            Type::Channel(inner) => write!(f, "Channel({inner})"),
            Type::Error => write!(f, "<error>"),
            Type::Never => write!(f, "Never"),
        }
    }
}

// ── Type scheme (polymorphic type) ──────────────────────────────────

/// A type scheme represents a polymorphic type: forall vars . ty
/// The `vars` are the universally quantified type variables.
/// The `constraints` are trait bounds on type variables (from `where` clauses).
#[derive(Debug, Clone)]
pub struct Scheme {
    pub vars: Vec<TyVar>,
    pub ty: Type,
    pub constraints: Vec<(TyVar, Symbol)>,
}

impl Scheme {
    pub fn mono(ty: Type) -> Self {
        Scheme {
            vars: Vec::new(),
            ty,
            constraints: Vec::new(),
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

// ── Free functions on types ─────────────────────────────────────────

/// Count the number of parameters in a function type.
pub fn count_params(ty: &Type) -> usize {
    match ty {
        Type::Fun(params, _) => params.len(),
        _ => 0,
    }
}

/// Collect free type variables in a type.
pub fn free_vars_in(ty: &Type) -> Vec<TyVar> {
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
        Type::Set(inner) => free_vars_in(inner),
        Type::Channel(inner) => free_vars_in(inner),
        Type::Int
        | Type::Float
        | Type::ExtFloat
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Error
        | Type::Never => Vec::new(),
    }
}

/// Substitute type variables according to a mapping.
pub fn substitute_vars(ty: &Type, mapping: &HashMap<TyVar, Type>) -> Type {
    match ty {
        Type::Var(v) => {
            if let Some(replacement) = mapping.get(v) {
                replacement.clone()
            } else {
                ty.clone()
            }
        }
        Type::Fun(params, ret) => {
            let params = params.iter().map(|p| substitute_vars(p, mapping)).collect();
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
                .map(|(n, t)| (*n, substitute_vars(t, mapping)))
                .collect();
            Type::Record(*name, fields)
        }
        Type::Variant(name, args) => {
            let args = args.iter().map(|a| substitute_vars(a, mapping)).collect();
            Type::Variant(*name, args)
        }
        Type::Generic(name, args) => {
            let args = args.iter().map(|a| substitute_vars(a, mapping)).collect();
            Type::Generic(*name, args)
        }
        Type::Map(k, v) => Type::Map(
            Box::new(substitute_vars(k, mapping)),
            Box::new(substitute_vars(v, mapping)),
        ),
        Type::Set(inner) => Type::Set(Box::new(substitute_vars(inner, mapping))),
        Type::Channel(inner) => Type::Channel(Box::new(substitute_vars(inner, mapping))),
        _ => ty.clone(),
    }
}

/// Substitute enum type parameters with concrete type arguments.
/// This is used when we know e.g. Result(Int, String) and want to
/// resolve the type of a variant's field.
pub fn substitute_enum_params(
    field_ty: &Type,
    param_var_ids: &[TyVar],
    type_args: &[Type],
) -> Type {
    match field_ty {
        Type::Var(v) => {
            // Find which parameter position this TyVar corresponds to
            if let Some(pos) = param_var_ids.iter().position(|id| id == v) {
                if pos < type_args.len() {
                    type_args[pos].clone()
                } else {
                    field_ty.clone()
                }
            } else {
                field_ty.clone()
            }
        }
        Type::Fun(params, ret) => {
            let params = params
                .iter()
                .map(|p| substitute_enum_params(p, param_var_ids, type_args))
                .collect();
            let ret = Box::new(substitute_enum_params(ret, param_var_ids, type_args));
            Type::Fun(params, ret)
        }
        Type::List(inner) => Type::List(Box::new(substitute_enum_params(
            inner,
            param_var_ids,
            type_args,
        ))),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| substitute_enum_params(e, param_var_ids, type_args))
                .collect(),
        ),
        Type::Generic(name, args) => {
            let args = args
                .iter()
                .map(|a| substitute_enum_params(a, param_var_ids, type_args))
                .collect();
            Type::Generic(*name, args)
        }
        Type::Channel(inner) => Type::Channel(Box::new(substitute_enum_params(
            inner,
            param_var_ids,
            type_args,
        ))),
        Type::Map(k, v) => Type::Map(
            Box::new(substitute_enum_params(k, param_var_ids, type_args)),
            Box::new(substitute_enum_params(v, param_var_ids, type_args)),
        ),
        Type::Set(t) => Type::Set(Box::new(substitute_enum_params(
            t,
            param_var_ids,
            type_args,
        ))),
        Type::Record(name, fields) => Type::Record(
            *name,
            fields
                .iter()
                .map(|(n, t)| (*n, substitute_enum_params(t, param_var_ids, type_args)))
                .collect(),
        ),
        Type::Variant(name, args) => Type::Variant(
            *name,
            args.iter()
                .map(|a| substitute_enum_params(a, param_var_ids, type_args))
                .collect(),
        ),
        _ => field_ty.clone(),
    }
}
