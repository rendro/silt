//! Host modules: modules an embedder declares to the session, whose
//! functions are Rust closures (design decision D7).
//!
//! A host module is imported like any module (`import mylib`). Each of
//! its functions is declared by its signature, `fn double(x: Int) ->
//! Int`, which the checker reads as it reads a module's `pub fn`; the
//! closure is what a call runs.
//!
//! ```rust
//! use silt::session::HostModule;
//!
//! let mylib = HostModule::new("mylib")
//!     .fn1("fn double(x: Int) -> Int", |x: i64| x * 2)
//!     .fn2("fn greet(name: String, n: Int) -> String", |name: String, n: i64| {
//!         format!("hello {name} x{n}")
//!     });
//! ```

use std::fmt;
use std::sync::Arc;

use crate::ast::{FnDecl, ParamKind, PatternKind, TypeExpr, TypeExprKind};
use crate::intern::{Symbol, resolve};
use crate::typechecker::names::type_vars_of;
use crate::value::{
    FromValue, HostImpl, HostShape, HostSource, HostStep, HostVar, IntoValue, Value,
};
use crate::vm::VmError;

/// A module of Rust functions, declared to a session in
/// [`super::Config::host`].
#[derive(Clone)]
pub struct HostModule {
    /// The name programs import the module by.
    pub name: String,
    pub fns: Vec<HostFunction>,
}

/// One function of a [`HostModule`].
#[derive(Clone)]
pub struct HostFunction {
    /// The function's signature, a `fn` header with typed parameters and
    /// return type and no body: `fn double(x: Int) -> Int`. Its name is
    /// the function's name in the module.
    pub signature: String,
    /// What a call runs. It is given arguments of the declared types.
    pub call: HostImpl,
    /// The number of arguments `call` takes, when it was built for a
    /// number (`fn0`, `fn1`, `fn2`): the signature must declare as many
    /// parameters.
    pub arity: Option<usize>,
}

impl HostModule {
    /// A module named `name`, with no functions yet.
    pub fn new(name: impl Into<String>) -> HostModule {
        HostModule {
            name: name.into(),
            fns: Vec::new(),
        }
    }

    /// Add the function declared by `signature`, called with the
    /// arguments as values.
    ///
    /// A panic inside `call` becomes a runtime error that names the
    /// function; returning `Err` is still the way to fail.
    pub fn function(
        self,
        signature: impl Into<String>,
        call: impl Fn(&[Value]) -> Result<Value, VmError> + Send + Sync + 'static,
    ) -> HostModule {
        self.push(signature.into(), Arc::new(call), None)
    }

    fn push(mut self, signature: String, call: HostImpl, arity: Option<usize>) -> HostModule {
        self.fns.push(HostFunction {
            signature,
            call,
            arity,
        });
        self
    }

    /// Add a function of no arguments, its result converted to a value.
    pub fn fn0<R: IntoValue>(
        self,
        signature: impl Into<String>,
        f: impl Fn() -> R + Send + Sync + 'static,
    ) -> HostModule {
        let call = move |_: &[Value]| f().into_value().map_err(VmError::new);
        self.push(signature.into(), Arc::new(call), Some(0))
    }

    /// Add a function of one argument, converted from and to values.
    pub fn fn1<A: FromValue, R: IntoValue>(
        self,
        signature: impl Into<String>,
        f: impl Fn(A) -> R + Send + Sync + 'static,
    ) -> HostModule {
        let call = move |args: &[Value]| {
            let a = A::from_value(&args[0]).map_err(VmError::new)?;
            f(a).into_value().map_err(VmError::new)
        };
        self.push(signature.into(), Arc::new(call), Some(1))
    }

    /// Add a function of two arguments, converted from and to values.
    pub fn fn2<A: FromValue, B: FromValue, R: IntoValue>(
        self,
        signature: impl Into<String>,
        f: impl Fn(A, B) -> R + Send + Sync + 'static,
    ) -> HostModule {
        let call = move |args: &[Value]| {
            let a = A::from_value(&args[0]).map_err(|e| VmError::new(format!("arg 1: {e}")))?;
            let b = B::from_value(&args[1]).map_err(|e| VmError::new(format!("arg 2: {e}")))?;
            f(a, b).into_value().map_err(VmError::new)
        };
        self.push(signature.into(), Arc::new(call), Some(2))
    }

    /// The text the module is checked from: one signature per line.
    pub(super) fn signatures(&self) -> String {
        let mut text = String::new();
        for f in &self.fns {
            text.push_str(&f.signature);
            text.push('\n');
        }
        text
    }
}

/// What the host function `f` returns, which each result is checked
/// against: the shape of its return type, and the variables of that
/// type, each with the places among the parameters that show it.
pub(super) fn returns(f: &FnDecl) -> (HostShape, Vec<HostVar>) {
    let Some(ret) = &f.return_type else {
        return (HostShape::Any, Vec::new());
    };
    let (mut names, mut rows) = (Vec::new(), Vec::new());
    type_vars_of(ret, &mut names);
    row_vars_of(ret, &mut rows);
    let order: Vec<(Symbol, bool)> = (names.iter().map(|name| (*name, false)))
        .chain(rows.iter().map(|row| (*row, true)))
        .collect();
    let mut vars: Vec<HostVar> = (order.iter())
        .map(|(name, _)| HostVar {
            name: resolve(*name),
            sources: Vec::new(),
        })
        .collect();
    let index = |name: Symbol, row: bool| order.iter().position(|var| *var == (name, row));
    for (at, param) in f.params.iter().enumerate() {
        let name = match param.pattern.kind {
            PatternKind::Ident(name) => Some(resolve(name)),
            _ => None,
        };
        let mut source = HostSource {
            param: at,
            name,
            path: Vec::new(),
            of_type: false,
            written: Vec::new(),
        };
        if param.kind == ParamKind::Type {
            if let PatternKind::Ident(var) = param.pattern.kind
                && let Some(var) = index(var, false)
            {
                source.of_type = true;
                vars[var].sources.push(source);
            }
        } else if let Some(var) = param.ty.as_ref().and_then(type_of_variable) {
            // `x: TypeOf(a)`, the type `type a` gives its parameter,
            // written out: the type argument of the call, as of `type a`.
            if let Some(var) = index(var, false) {
                source.of_type = true;
                vars[var].sources.push(source);
            }
        } else if let Some(ty) = &param.ty {
            sources_in(ty, &mut source, &index, &mut vars);
        }
    }
    (shape(ret, &index), vars)
}

/// The variable `a` of the type `TypeOf(a)`, which a `type a` parameter
/// has.
fn type_of_variable(ty: &TypeExpr) -> Option<Symbol> {
    let TypeExprKind::Generic {
        module: None,
        name,
        args,
        ..
    } = &ty.kind
    else {
        return None;
    };
    match args.as_slice() {
        [
            TypeExpr {
                kind:
                    TypeExprKind::Named {
                        module: None,
                        name: var,
                        ..
                    },
                ..
            },
        ] if resolve(*name) == crate::defs::TYPE_OF && is_variable(*var) => Some(*var),
        _ => None,
    }
}

/// Whether the type `name` written alone is a type variable.
fn is_variable(name: Symbol) -> bool {
    resolve(name).starts_with(|c: char| c.is_lowercase() || c == '_')
}

/// The places in the parameter's type `ty`, at `source`, of the
/// variables `index` numbers, entered into `vars`.
fn sources_in(
    ty: &TypeExpr,
    source: &mut HostSource,
    index: &dyn Fn(Symbol, bool) -> Option<usize>,
    vars: &mut [HostVar],
) {
    let mut step = |step: HostStep, ty: &TypeExpr, source: &mut HostSource| {
        source.path.push(step);
        sources_in(ty, source, index, vars);
        source.path.pop();
    };
    match &ty.kind {
        TypeExprKind::Named {
            module: None, name, ..
        } if is_variable(*name) => {
            if let Some(var) = index(*name, false) {
                vars[var].sources.push(source.clone());
            }
        }
        TypeExprKind::Generic {
            module: None,
            name,
            args,
            ..
        } => match (resolve(*name).as_str(), args.as_slice()) {
            ("List" | "Set", [item]) => step(HostStep::Element, item, source),
            ("Map", [k, v]) => {
                step(HostStep::Key, k, source);
                step(HostStep::Value, v, source);
            }
            ("Option", [item]) => step(HostStep::Some, item, source),
            ("Result", [ok, err]) => {
                step(HostStep::Ok, ok, source);
                step(HostStep::Err, err, source);
            }
            _ => {}
        },
        TypeExprKind::Tuple(parts) => {
            for (n, part) in parts.iter().enumerate() {
                step(HostStep::Part(n), part, source);
            }
        }
        TypeExprKind::AnonRecord { fields, tail } => {
            for (name, field) in fields {
                step(HostStep::Field(resolve(*name)), field, source);
            }
            if let Some((row, _)) = tail
                && let Some(var) = index(*row, true)
            {
                let mut at = source.clone();
                at.written = fields.iter().map(|(name, _)| resolve(*name)).collect();
                vars[var].sources.push(at);
            }
        }
        _ => {}
    }
}

/// The shape of the values of the type `ty`, which a host function's
/// result is checked against, its variables numbered by `index`.
fn shape(ty: &TypeExpr, index: &dyn Fn(Symbol, bool) -> Option<usize>) -> HostShape {
    let boxed = |ty: &TypeExpr| Box::new(shape(ty, index));
    match &ty.kind {
        TypeExprKind::Named {
            module: None, name, ..
        } if is_variable(*name) => match index(*name, false) {
            Some(var) => HostShape::Var(var, resolve(*name)),
            None => HostShape::Any,
        },
        TypeExprKind::Named {
            module: None, name, ..
        } => match resolve(*name).as_str() {
            "Int" => HostShape::Int,
            "Float" => HostShape::Float,
            "Bool" => HostShape::Bool,
            "String" => HostShape::String,
            "Bytes" => HostShape::Bytes,
            "Never" => HostShape::Never,
            _ => HostShape::Any,
        },
        TypeExprKind::Generic {
            module: None,
            name,
            args,
            ..
        } => match (resolve(*name).as_str(), args.as_slice()) {
            ("List", [item]) => HostShape::List(boxed(item)),
            ("Set", [item]) => HostShape::Set(boxed(item)),
            ("Map", [k, v]) => HostShape::Map(boxed(k), boxed(v)),
            ("Option", [item]) => HostShape::Option(boxed(item)),
            ("Result", [ok, err]) => HostShape::Result(boxed(ok), boxed(err)),
            ("Channel", [item]) => HostShape::Channel(boxed(item)),
            _ => HostShape::Any,
        },
        TypeExprKind::Tuple(items) if items.is_empty() => HostShape::Unit,
        TypeExprKind::Tuple(items) => {
            HostShape::Tuple(items.iter().map(|item| shape(item, index)).collect())
        }
        TypeExprKind::AnonRecord { fields, tail } => {
            let row = match tail {
                Some((row, _)) => match index(*row, true) {
                    Some(var) => Some((var, resolve(*row))),
                    // (No parameter shows the row: the checker refuses
                    // that signature.)
                    None => return HostShape::Any,
                },
                None => None,
            };
            HostShape::Record {
                fields: (fields.iter())
                    .map(|(name, ty)| (resolve(*name), shape(ty, index)))
                    .collect(),
                row,
            }
        }
        _ => HostShape::Any,
    }
}

/// Whether the type `ty` is or holds a function type. A host function
/// cannot call a function it is given (it has no interpreter to call it
/// on), nor make one to give back.
pub(super) fn mentions_function(ty: &TypeExpr) -> bool {
    match &ty.kind {
        TypeExprKind::Function(..) => true,
        TypeExprKind::Generic { args, .. } | TypeExprKind::Tuple(args) => {
            args.iter().any(mentions_function)
        }
        TypeExprKind::AnonRecord { fields, .. } => {
            fields.iter().any(|(_, ty)| mentions_function(ty))
        }
        TypeExprKind::Named { .. } | TypeExprKind::SelfType => false,
    }
}

/// The variables of the return type of the host function `f` that no
/// parameter introduces (by its type, or as a `type a` parameter), each
/// with whether it is a row variable (`{name: String, ...r}`). Nothing
/// could check that the function's result is of the type each call
/// asks for of one, as a silt function's body is checked.
pub(super) fn return_only_variables(f: &FnDecl) -> Vec<(Symbol, bool)> {
    let mut given = Vec::new();
    let mut given_rows = Vec::new();
    for param in &f.params {
        if param.kind == ParamKind::Type
            && let PatternKind::Ident(name) = param.pattern.kind
        {
            given.push(name);
        }
        if let Some(ty) = &param.ty {
            type_vars_of(ty, &mut given);
            row_vars_of(ty, &mut given_rows);
        }
    }
    let (mut written, mut rows) = (Vec::new(), Vec::new());
    if let Some(ty) = &f.return_type {
        type_vars_of(ty, &mut written);
        row_vars_of(ty, &mut rows);
    }
    // (A name that starts with `_` is no variable: the checker says it
    // is an unknown type, wherever it is written.)
    let vars =
        (written.into_iter()).filter(|v| !given.contains(v) && !resolve(*v).starts_with('_'));
    let rows = rows.into_iter().filter(|r| !given_rows.contains(r));
    vars.map(|v| (v, false))
        .chain(rows.map(|r| (r, true)))
        .collect()
}

/// The row variables of the open records `ty` writes (`...r`), into
/// `out`.
fn row_vars_of(ty: &TypeExpr, out: &mut Vec<Symbol>) {
    match &ty.kind {
        TypeExprKind::AnonRecord { fields, tail } => {
            if let Some((name, _)) = tail
                && !out.contains(name)
            {
                out.push(*name);
            }
            fields.iter().for_each(|(_, ty)| row_vars_of(ty, out));
        }
        TypeExprKind::Generic { args, .. } | TypeExprKind::Tuple(args) => {
            args.iter().for_each(|ty| row_vars_of(ty, out));
        }
        TypeExprKind::Function(params, ret) => {
            params.iter().for_each(|ty| row_vars_of(ty, out));
            row_vars_of(ret, out);
        }
        TypeExprKind::Named { .. } | TypeExprKind::SelfType => {}
    }
}

impl fmt::Debug for HostModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostModule")
            .field("name", &self.name)
            .field(
                "fns",
                &self.fns.iter().map(|f| &f.signature).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl fmt::Debug for HostFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HostFunction({})", self.signature)
    }
}
