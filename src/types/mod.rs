//! Shared type definitions for Silt's type system.
//!
//! This module contains the core type representations used by the type checker,
//! interpreter, and other parts of the compiler pipeline.

pub mod builtins;
pub mod canonical;

use std::collections::{BTreeMap, HashMap};

use crate::intern::Symbol;
use crate::lexer::Span;

// ── Type representation ─────────────────────────────────────────────

/// A unique identifier for type variables.
pub type TyVar = usize;

/// Tail of a row (record) type. Either closed (no extra fields) or
/// open with a unification variable that may bind to a record holding
/// the remaining fields. See `Type::AnonRecord`.
#[derive(Debug, Clone, PartialEq)]
pub enum RowTail {
    /// The record is closed: it has exactly the listed fields and no more.
    Closed,
    /// The record may have additional fields. The TyVar is a row variable
    /// that unification can bind to a record carrying the leftover fields.
    Var(TyVar),
}

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
    /// Inclusive integer-range type produced by `a..b`. Nominally distinct
    /// from `List(T)` so annotations like `let r: Range(Int) = 1..10`
    /// succeed, but unifies bidirectionally with `List(T)` — Range is a
    /// zero-cost alias whose runtime representation is the same `Vec<Value>`
    /// as a List. Laziness is future work (tracked in docs/language/operators.md).
    Range(Box<Type>),
    /// Tuple type (fixed length, heterogeneous).
    Tuple(Vec<Type>),
    /// A nominal record type: name + field name/type pairs.
    Record(Symbol, Vec<(Symbol, Type)>),
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
    /// Associated-type projection: `<receiver as trait_name>::assoc_name`.
    ///
    /// Two states:
    ///   - **Concrete receiver**: the canonicaliser reduces this to the
    ///     impl's binding for `assoc_name`. The reduction is the dispatch
    ///     oracle's only behaviour for projections.
    ///   - **Abstract receiver** (still a type variable, or another
    ///     unreduced AssocProj): stays as `AssocProj` and propagates
    ///     through inference. Two abstract `AssocProj`s unify iff they
    ///     have the same receiver, trait_name, and assoc_name.
    AssocProj {
        receiver: Box<Type>,
        trait_name: Symbol,
        assoc_name: Symbol,
    },
    /// An anonymous structural record type (row-polymorphic capable).
    /// `{name: String, age: Int}` is closed; `{name: String, ...r}` has
    /// a row-tail variable. Field order is irrelevant for equality —
    /// `BTreeMap` gives stable ordering for canonicalisation/rendering.
    AnonRecord {
        fields: BTreeMap<Symbol, Type>,
        tail: RowTail,
    },
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
            // Type variables have no user-facing name at this point in
            // inference — rendering `?17` leaks an internal id. The
            // underscore matches silt's own "I don't care about this
            // type" convention in patterns and reads as "unknown type"
            // in diagnostics.
            Type::Var(_) => write!(f, "_"),
            Type::Fun(params, ret) => {
                // Match the parser's surface syntax `Fn(A, B) -> C` so
                // diagnostics render fn types in the same form users
                // wrote in annotations. Without the `Fn` prefix, the
                // render `(A, B) -> C` visually collides with silt's
                // tuple-type syntax `(A, B)`.
                write!(f, "Fn(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ") -> {ret}")
            }
            Type::List(inner) => write!(f, "List({inner})"),
            Type::Range(inner) => write!(f, "Range({inner})"),
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
            Type::Generic(name, args) => {
                // `TypeOf(a)` is the internal lowering of a `type a`
                // parameter. Render it as `type a` so diagnostics use the
                // surface syntax the user wrote — never leak `TypeOf`.
                if crate::intern::resolve(*name) == "TypeOf" && args.len() == 1 {
                    return write!(f, "type {}", args[0]);
                }
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
            // A type that reached `Type::Error` already triggered a
            // prior diagnostic; rendering `<error>` on cascading
            // messages reads as double-reporting. An empty placeholder
            // (`_`) keeps downstream messages readable without
            // suggesting a second, distinct failure.
            Type::Error => write!(f, "_"),
            Type::Never => write!(f, "Never"),
            Type::AssocProj {
                receiver,
                trait_name,
                assoc_name,
            } => {
                // Render the qualified form `<recv as Trait>::Name` for
                // diagnostics so the receiver/trait/assoc-name triple is
                // unambiguous regardless of context.
                write!(f, "<{receiver} as {trait_name}>::{assoc_name}")
            }
            Type::AnonRecord { fields, tail } => {
                write!(f, "{{")?;
                let mut first = true;
                for (n, t) in fields.iter() {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{n}: {t}")?;
                }
                if matches!(tail, RowTail::Var(_)) {
                    if !first {
                        write!(f, ", ")?;
                    }
                    // Row variables render as `..` to indicate "more
                    // fields possible". Don't leak the internal id.
                    write!(f, "...")?;
                }
                write!(f, "}}")
            }
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
        Type::Range(inner) => free_vars_in(inner),
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
        Type::Generic(_, args) => {
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
        Type::AssocProj { receiver, .. } => free_vars_in(receiver),
        Type::AnonRecord { fields, tail } => {
            let mut fvs = Vec::new();
            for t in fields.values() {
                for v in free_vars_in(t) {
                    if !fvs.contains(&v) {
                        fvs.push(v);
                    }
                }
            }
            if let RowTail::Var(v) = tail
                && !fvs.contains(v)
            {
                fvs.push(*v);
            }
            fvs
        }
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
        Type::Range(inner) => Type::Range(Box::new(substitute_vars(inner, mapping))),
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
        Type::AssocProj {
            receiver,
            trait_name,
            assoc_name,
        } => Type::AssocProj {
            receiver: Box::new(substitute_vars(receiver, mapping)),
            trait_name: *trait_name,
            assoc_name: *assoc_name,
        },
        Type::AnonRecord { fields, tail } => {
            let new_fields: BTreeMap<Symbol, Type> = fields
                .iter()
                .map(|(n, t)| (*n, substitute_vars(t, mapping)))
                .collect();
            let new_tail = match tail {
                RowTail::Closed => RowTail::Closed,
                RowTail::Var(v) => match mapping.get(v) {
                    // If the row var resolved to a record, merge its
                    // fields and propagate its tail.
                    Some(Type::AnonRecord {
                        fields: extra_fields,
                        tail: extra_tail,
                    }) => {
                        let mut merged = new_fields.clone();
                        for (n, t) in extra_fields.iter() {
                            merged.insert(*n, substitute_vars(t, mapping));
                        }
                        return Type::AnonRecord {
                            fields: merged,
                            tail: extra_tail.clone(),
                        };
                    }
                    Some(_other) => RowTail::Var(*v), // can't merge sensibly
                    None => RowTail::Var(*v),
                },
            };
            Type::AnonRecord {
                fields: new_fields,
                tail: new_tail,
            }
        }
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
        Type::Range(inner) => Type::Range(Box::new(substitute_enum_params(
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
        Type::AssocProj {
            receiver,
            trait_name,
            assoc_name,
        } => Type::AssocProj {
            receiver: Box::new(substitute_enum_params(receiver, param_var_ids, type_args)),
            trait_name: *trait_name,
            assoc_name: *assoc_name,
        },
        Type::AnonRecord { fields, tail } => Type::AnonRecord {
            fields: fields
                .iter()
                .map(|(n, t)| (*n, substitute_enum_params(t, param_var_ids, type_args)))
                .collect(),
            tail: tail.clone(),
        },
        _ => field_ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Regression: substitute_enum_params recurses into Map/Set ───────
    // Locks in 3a4edd6 B3: prior to the fix these branches fell through
    // to `_ => field_ty.clone()`, so a variant field typed
    // `Map(String, a)` (or `Set(a)`) carrying the enum's parameter
    // variable was returned unchanged, leaking the enum's internal
    // TyVar into downstream inference instead of being replaced with
    // the concrete instantiation.

    #[test]
    fn substitute_enum_params_recurses_into_map_value() {
        // Simulate `type Box(a) { Carry(Map(String, a)) }` instantiated
        // as `Box(Int)`: enum param var is TyVar 0, type_args is [Int].
        let param_var_ids = vec![0usize];
        let type_args = vec![Type::Int];
        let field = Type::Map(Box::new(Type::String), Box::new(Type::Var(0)));
        let result = substitute_enum_params(&field, &param_var_ids, &type_args);
        assert_eq!(
            result,
            Type::Map(Box::new(Type::String), Box::new(Type::Int)),
            "Map value type variable must be substituted"
        );
    }

    #[test]
    fn substitute_enum_params_recurses_into_map_key() {
        // A pathological but legal shape: `Map(a, Int)`.
        let param_var_ids = vec![0usize];
        let type_args = vec![Type::String];
        let field = Type::Map(Box::new(Type::Var(0)), Box::new(Type::Int));
        let result = substitute_enum_params(&field, &param_var_ids, &type_args);
        assert_eq!(
            result,
            Type::Map(Box::new(Type::String), Box::new(Type::Int)),
            "Map key type variable must be substituted"
        );
    }

    #[test]
    fn substitute_enum_params_recurses_into_set() {
        // Simulate `type Bag(a) { Contents(Set(a)) }` as `Bag(Int)`.
        let param_var_ids = vec![0usize];
        let type_args = vec![Type::Int];
        let field = Type::Set(Box::new(Type::Var(0)));
        let result = substitute_enum_params(&field, &param_var_ids, &type_args);
        assert_eq!(
            result,
            Type::Set(Box::new(Type::Int)),
            "Set element type variable must be substituted"
        );
    }

    #[test]
    fn substitute_enum_params_handles_nested_map_of_set() {
        // Map(String, Set(a)) — catches a regression where only the
        // outermost container is substituted.
        let param_var_ids = vec![0usize];
        let type_args = vec![Type::Int];
        let field = Type::Map(
            Box::new(Type::String),
            Box::new(Type::Set(Box::new(Type::Var(0)))),
        );
        let result = substitute_enum_params(&field, &param_var_ids, &type_args);
        assert_eq!(
            result,
            Type::Map(
                Box::new(Type::String),
                Box::new(Type::Set(Box::new(Type::Int))),
            ),
            "nested Set inside Map must be substituted"
        );
    }

    // ── Regression: Type::Fun Display matches parser `Fn(...)` surface ──
    // The parser at src/parser.rs:838 reads function-type annotations as
    // `Fn(A, B) -> C`. Without the `Fn` prefix in Display, diagnostics
    // render fn types as `(A, B) -> C`, which visually collides with
    // silt's tuple-type syntax `(A, B)` and doesn't match anything a
    // user could write in an annotation.

    #[test]
    fn display_fun_uses_fn_prefix_multi_arg() {
        let ty = Type::Fun(vec![Type::Int, Type::String], Box::new(Type::Int));
        assert_eq!(format!("{ty}"), "Fn(Int, String) -> Int");
    }

    #[test]
    fn display_fun_uses_fn_prefix_single_arg() {
        let ty = Type::Fun(vec![Type::Int], Box::new(Type::Bool));
        assert_eq!(format!("{ty}"), "Fn(Int) -> Bool");
    }

    #[test]
    fn display_fun_distinguishes_tuple_arg_from_multi_arg() {
        // `Fn((Int, String)) -> Int` is a 1-arg fn taking a tuple.
        // `Fn(Int, String) -> Int` is a 2-arg fn. These must render
        // distinctly so diagnostics don't conflate arity with tupling.
        let tuple_arg = Type::Fun(
            vec![Type::Tuple(vec![Type::Int, Type::String])],
            Box::new(Type::Int),
        );
        let two_arg = Type::Fun(vec![Type::Int, Type::String], Box::new(Type::Int));
        assert_eq!(format!("{tuple_arg}"), "Fn((Int, String)) -> Int");
        assert_eq!(format!("{two_arg}"), "Fn(Int, String) -> Int");
        assert_ne!(format!("{tuple_arg}"), format!("{two_arg}"));
    }

    #[test]
    fn display_fun_zero_arg() {
        let ty = Type::Fun(vec![], Box::new(Type::Unit));
        assert_eq!(format!("{ty}"), "Fn() -> ()");
    }
}
