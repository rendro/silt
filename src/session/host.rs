//! Host modules: modules an embedder declares to the session, whose
//! functions are Rust closures (design decision D7).
//!
//! A host module is imported like any module (`import mylib`). Each of
//! its functions is declared by its signature, `fn double(x: Int) ->
//! Int`, which the checker reads as it reads a module's `pub fn`; the
//! closure is what a call runs. A host module can declare record and
//! enum types too, each by its declaration (`type Point { x: Int, y:
//! Int }`): its signatures name them, and a program names them as
//! `mylib.Point`.
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
use std::ops::Range;
use std::sync::Arc;

use crate::ast::{FnDecl, ParamKind, PatternKind, TypeBody, TypeDecl, TypeExpr, TypeExprKind};
use crate::defs::TypeId;
use crate::intern::{Symbol, resolve};
use crate::typechecker::names::type_vars_of;
use crate::value::{
    FromValue, HostCall, HostImpl, HostShape, HostSource, HostStep, HostVar, IntoValue, Value,
};
use crate::vm::VmError;

/// A module of Rust functions, declared to a session in
/// [`super::Config::host`].
#[derive(Clone)]
pub struct HostModule {
    /// The name programs import the module by.
    pub name: String,
    /// The record and enum types it declares, each by its declaration:
    /// `type Point { x: Int, y: Int }`.
    pub types: Vec<String>,
    pub fns: Vec<HostFunction>,
}

/// What a part of the text a host module is checked from declares: the
/// `n`th of its types, or of its functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    Type(usize),
    Fn(usize),
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
            types: Vec::new(),
            fns: Vec::new(),
        }
    }

    /// Add the record or enum type declared by `declaration`, a silt
    /// type declaration: `type Point { x: Int, y: Int }`, `type Shape {
    /// Circle(Float), Square(Float) }`, `type Box(a) { value: a }`. Its
    /// name is the type's name in the module. A function of the module
    /// builds a value of it with [`HostCall::record`] or
    /// [`HostCall::variant`].
    pub fn declare_type(mut self, declaration: impl Into<String>) -> HostModule {
        self.types.push(declaration.into());
        self
    }

    /// Add the function declared by `signature`, called with the
    /// arguments as values (`call.args`).
    ///
    /// A panic inside `call` becomes a runtime error that names the
    /// function; returning `Err` is still the way to fail.
    pub fn function(
        self,
        signature: impl Into<String>,
        call: impl Fn(&HostCall<'_>) -> Result<Value, VmError> + Send + Sync + 'static,
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
        let call = move |_: &HostCall<'_>| f().into_value().map_err(VmError::new);
        self.push(signature.into(), Arc::new(call), Some(0))
    }

    /// Add a function of one argument, converted from and to values.
    pub fn fn1<A: FromValue, R: IntoValue>(
        self,
        signature: impl Into<String>,
        f: impl Fn(A) -> R + Send + Sync + 'static,
    ) -> HostModule {
        let call = move |call: &HostCall<'_>| {
            let a = A::from_value(&call.args[0]).map_err(VmError::new)?;
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
        let call = move |call: &HostCall<'_>| {
            let args = call.args;
            let a = A::from_value(&args[0]).map_err(|e| VmError::new(format!("arg 1: {e}")))?;
            let b = B::from_value(&args[1]).map_err(|e| VmError::new(format!("arg 2: {e}")))?;
            f(a, b).into_value().map_err(VmError::new)
        };
        self.push(signature.into(), Arc::new(call), Some(2))
    }

    /// The text the module is checked from: its type declarations, then
    /// its signatures, each on lines of its own, with the place of each
    /// in the text.
    pub(super) fn text(&self) -> (String, Vec<(Range<usize>, Part)>) {
        let mut text = String::new();
        let mut parts = Vec::new();
        let declared = (self.types.iter().enumerate()).map(|(n, ty)| (ty, Part::Type(n)));
        let signed = (self.fns.iter().enumerate()).map(|(n, f)| (&f.signature, Part::Fn(n)));
        for (written, part) in declared.chain(signed) {
            let start = text.len();
            text.push_str(written);
            parts.push((start..text.len(), part));
            text.push('\n');
        }
        (text, parts)
    }
}

/// A type a host signature names, as the session finds it: its id and,
/// for a type of the host module, its declaration.
pub(super) struct Named<'a> {
    pub id: TypeId,
    pub decl: Option<&'a TypeDecl>,
    /// Whether its values are records or variants (a builtin type such
    /// as `Int` or `tcp.TcpStream` is neither): only those are checked
    /// by the type.
    pub checked: bool,
}

/// How a host module's text names its types: `lookup(module, name)` is
/// the type `name` (`module.name` with a qualifier).
pub(super) type Lookup<'a> = dyn Fn(Option<Symbol>, Symbol) -> Option<Named<'a>> + 'a;

/// How many types of the host module the shape of one result type
/// opens at most (their fields are checked; past it, a type is checked
/// by its id).
const OPENED_TYPES: usize = 256;

/// What the host function `f` returns, which each result is checked
/// against: the shape of its return type, and the variables of that
/// type, each with the places among the parameters that show it. The
/// types it names are found by `lookup`.
pub(super) fn returns(f: &FnDecl, lookup: &Lookup<'_>) -> (HostShape, Vec<HostVar>) {
    let Some(ret) = &f.return_type else {
        return (HostShape::Any, Vec::new());
    };
    let mut names = Vec::new();
    type_vars_of(ret, &mut names);
    let mut vars: Vec<HostVar> = (names.iter())
        .map(|name| HostVar {
            name: resolve(*name),
            sources: Vec::new(),
        })
        .collect();
    let index = |name: Symbol| names.iter().position(|var| *var == name);
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
        };
        if param.kind == ParamKind::Type {
            if let PatternKind::Ident(var) = param.pattern.kind
                && let Some(var) = index(var)
            {
                source.of_type = true;
                vars[var].sources.push(source);
            }
        } else if let Some(ty) = &param.ty {
            let mut sources = Sources {
                index: &index,
                vars: &mut vars,
                lookup,
                opening: Vec::new(),
            };
            sources.walk(ty, None, &mut source);
        }
    }
    let var = |name: Symbol| match index(name) {
        Some(var) => HostShape::Var(var, resolve(name)),
        None => HostShape::Any,
    };
    let mut shapes = Shapes {
        lookup,
        opening: Vec::new(),
        opened: 0,
    };
    (shapes.shape(ret, &var), vars)
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

/// The type parameters of a host module's type at the arguments a
/// signature writes for them (`a` of `type Box(a)` at `Box(List(b))`),
/// inside the declarations around (`up`).
struct Frame<'t> {
    params: Vec<(Symbol, &'t TypeExpr)>,
    up: Option<&'t Frame<'t>>,
}

/// What finds the places in a host function's parameters that show the
/// variables of its return type: the variables `index` numbers, entered
/// into `vars`; the types of the host module are found by `lookup`, and
/// `opening` are those whose declarations it is in.
struct Sources<'s, 'l, 'a> {
    index: &'s dyn Fn(Symbol) -> Option<usize>,
    vars: &'s mut [HostVar],
    lookup: &'l Lookup<'a>,
    opening: Vec<TypeId>,
}

impl Sources<'_, '_, '_> {
    /// Enter the places of the variables in the type `ty` of a
    /// parameter, at `source`; `frame` gives the type parameters of the
    /// declarations `ty` is written in (none: it is the signature's).
    fn walk(&mut self, ty: &TypeExpr, frame: Option<&Frame<'_>>, source: &mut HostSource) {
        // `TypeOf(a)`, the type `type a` gives its parameter, written out:
        // the type a call gives there, as of `type a`, at its place
        // (`t: TypeOf(a)`, `o: Option(TypeOf(a))`).
        if frame.is_none()
            && let Some(var) = type_of_variable(ty)
        {
            if let Some(var) = (self.index)(var) {
                let mut at = source.clone();
                at.of_type = true;
                self.vars[var].sources.push(at);
            }
            return;
        }
        let step = |this: &mut Self, step: HostStep, ty: &TypeExpr, source: &mut HostSource| {
            source.path.push(step);
            this.walk(ty, frame, source);
            source.path.pop();
        };
        match &ty.kind {
            TypeExprKind::Named {
                module: None, name, ..
            } if is_variable(*name) => match frame {
                // (A type parameter of a declaration is what the
                // signature writes for it.)
                Some(frame) => {
                    if let Some((_, arg)) = frame.params.iter().find(|(param, _)| param == name) {
                        self.walk(arg, frame.up, source);
                    }
                }
                None => {
                    if let Some(var) = (self.index)(*name) {
                        self.vars[var].sources.push(source.clone());
                    }
                }
            },
            TypeExprKind::Generic {
                module: None,
                name,
                args,
                ..
            } if matches!(
                (resolve(*name).as_str(), args.len()),
                ("List" | "Set" | "Option", 1) | ("Map" | "Result", 2)
            ) =>
            {
                match (resolve(*name).as_str(), args.as_slice()) {
                    ("List" | "Set", [item]) => step(self, HostStep::Element, item, source),
                    ("Map", [k, v]) => {
                        step(self, HostStep::Key, k, source);
                        step(self, HostStep::Value, v, source);
                    }
                    ("Option", [item]) => step(self, HostStep::Some, item, source),
                    ("Result", [ok, err]) => {
                        step(self, HostStep::Ok, ok, source);
                        step(self, HostStep::Err, err, source);
                    }
                    _ => {}
                }
            }
            // A generic type of the host module: its fields, at the
            // arguments written here (`value: a` of `Box(a)`).
            TypeExprKind::Generic {
                module: None,
                name,
                args,
                ..
            } => {
                let Some(decl) = (self.lookup)(None, *name).and_then(|named| {
                    let open = self.opening.contains(&named.id);
                    named.decl.filter(|_| !open).map(|decl| (named.id, decl))
                }) else {
                    return;
                };
                let (id, decl) = decl;
                let inner = Frame {
                    params: decl.params.iter().copied().zip(args).collect(),
                    up: frame,
                };
                self.opening.push(id);
                match &decl.body {
                    TypeBody::Record(fields) => {
                        for (n, field) in fields.iter().enumerate() {
                            source.path.push(HostStep::Field(n));
                            self.walk(&field.ty, Some(&inner), source);
                            source.path.pop();
                        }
                    }
                    TypeBody::Enum(variants) => {
                        for (ordinal, variant) in variants.iter().enumerate() {
                            for (n, ty) in variant.fields.iter().enumerate() {
                                let ordinal = u16::try_from(ordinal).unwrap_or(u16::MAX);
                                source.path.push(HostStep::Variant(ordinal, n));
                                self.walk(ty, Some(&inner), source);
                                source.path.pop();
                            }
                        }
                    }
                    TypeBody::Alias(_) => {}
                }
                self.opening.pop();
            }
            TypeExprKind::Tuple(parts) => {
                for (n, part) in parts.iter().enumerate() {
                    step(self, HostStep::Part(n), part, source);
                }
            }
            _ => {}
        }
    }
}

/// What builds the shapes of a host function's result type: the types
/// of the host module whose declarations it is in (`opening`, each with
/// the shapes of its type arguments), and how many it has opened.
struct Shapes<'l, 'a> {
    lookup: &'l Lookup<'a>,
    opening: Vec<(TypeId, Vec<HostShape>)>,
    opened: usize,
}

impl Shapes<'_, '_> {
    /// The shape of the values of the type `ty`, which a host function's
    /// result is checked against, each type variable's shape `var` of
    /// it.
    fn shape(&mut self, ty: &TypeExpr, var: &dyn Fn(Symbol) -> HostShape) -> HostShape {
        let mut boxed = |ty: &TypeExpr| Box::new(self.shape(ty, var));
        match &ty.kind {
            TypeExprKind::Named {
                module: None, name, ..
            } if is_variable(*name) => var(*name),
            TypeExprKind::Named {
                module: None, name, ..
            } if matches!(
                resolve(*name).as_str(),
                "Int" | "Float" | "Bool" | "String" | "Bytes" | "Never"
            ) =>
            {
                match resolve(*name).as_str() {
                    "Int" => HostShape::Int,
                    "Float" => HostShape::Float,
                    "Bool" => HostShape::Bool,
                    "String" => HostShape::String,
                    "Bytes" => HostShape::Bytes,
                    _ => HostShape::Never,
                }
            }
            TypeExprKind::Generic {
                module: None,
                name,
                args,
                ..
            } if matches!(
                (resolve(*name).as_str(), args.len()),
                ("List" | "Set" | "Option" | "Channel", 1) | ("Map" | "Result", 2)
            ) =>
            {
                match (resolve(*name).as_str(), args.as_slice()) {
                    ("List", [item]) => HostShape::List(boxed(item)),
                    ("Set", [item]) => HostShape::Set(boxed(item)),
                    ("Map", [k, v]) => HostShape::Map(boxed(k), boxed(v)),
                    ("Option", [item]) => HostShape::Option(boxed(item)),
                    ("Result", [ok, err]) => HostShape::Result(boxed(ok), boxed(err)),
                    ("Channel", [item]) => HostShape::Channel(boxed(item)),
                    _ => HostShape::Any,
                }
            }
            TypeExprKind::Named { module, name, .. } => self.named(ty, *module, *name, &[], var),
            TypeExprKind::Generic {
                module, name, args, ..
            } => self.named(ty, *module, *name, args, var),
            TypeExprKind::Tuple(items) if items.is_empty() => HostShape::Unit,
            TypeExprKind::Tuple(items) => {
                HostShape::Tuple(items.iter().map(|item| self.shape(item, var)).collect())
            }
            _ => HostShape::Any,
        }
    }

    /// The shape of the record or enum type `module.name` applied to
    /// `args`, written `ty`: of a type of the host module, with the
    /// shapes of its fields at those arguments.
    fn named(
        &mut self,
        ty: &TypeExpr,
        module: Option<crate::ast::Qualifier>,
        name: Symbol,
        args: &[TypeExpr],
        var: &dyn Fn(Symbol) -> HostShape,
    ) -> HostShape {
        let Some(found) = (self.lookup)(module.map(|m| m.name), name) else {
            return HostShape::Any;
        };
        if !found.checked {
            return HostShape::Any;
        }
        let written = crate::typechecker::typeexpr::type_expr_text(ty);
        let head = |parts| HostShape::Named {
            id: found.id,
            name: written.clone(),
            parts,
        };
        let Some(decl) = found.decl else {
            return head(Vec::new());
        };
        let given: Vec<(Symbol, HostShape)> = (decl.params.iter().zip(args))
            .map(|(param, arg)| (*param, self.shape(arg, var)))
            .collect();
        let at: Vec<HostShape> = given.iter().map(|(_, shape)| shape.clone()).collect();
        // (A type met again inside itself is checked as the one around
        // it, at the same arguments; at others, and past the bound, by
        // its id.)
        if let Some((_, around)) = self.opening.iter().rev().find(|(id, _)| *id == found.id) {
            return match *around == at {
                true => HostShape::Again(found.id, written),
                false => head(Vec::new()),
            };
        }
        if self.opened >= OPENED_TYPES {
            return head(Vec::new());
        }
        self.opened += 1;
        let of_param = |name: Symbol| {
            (given.iter())
                .find(|(param, _)| *param == name)
                .map_or(HostShape::Any, |(_, shape)| shape.clone())
        };
        self.opening.push((found.id, at));
        let parts = match &decl.body {
            TypeBody::Record(fields) => vec![
                (fields.iter())
                    .map(|field| self.shape(&field.ty, &of_param))
                    .collect(),
            ],
            TypeBody::Enum(variants) => (variants.iter())
                .map(|variant| {
                    (variant.fields.iter())
                        .map(|field| self.shape(field, &of_param))
                        .collect()
                })
                .collect(),
            TypeBody::Alias(_) => Vec::new(),
        };
        self.opening.pop();
        head(parts)
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
        TypeExprKind::Named { .. } | TypeExprKind::SelfType => false,
    }
}

/// The first `TypeOf(..)` in the parameter type `ty` around anything
/// but a bare type variable (`TypeOf(List(a))`, `TypeOf(Int)`): a type
/// given so shows no type for its variables that a result could be
/// checked against, and says nothing `type a` cannot.
pub(super) fn type_of_not_a_variable(ty: &TypeExpr) -> Option<&TypeExpr> {
    match &ty.kind {
        TypeExprKind::Generic {
            module: None, name, ..
        } if resolve(*name) == crate::defs::TYPE_OF && type_of_variable(ty).is_none() => Some(ty),
        TypeExprKind::Generic { args, .. } | TypeExprKind::Tuple(args) => {
            args.iter().find_map(type_of_not_a_variable)
        }
        TypeExprKind::Function(..) | TypeExprKind::Named { .. } | TypeExprKind::SelfType => None,
    }
}

/// The variables of the return type of the host function `f` that no
/// parameter introduces (by its type, or as a `type a` parameter).
/// Nothing could check that the function's result is of the type each
/// call asks for of one, as a silt function's body is checked.
pub(super) fn return_only_variables(f: &FnDecl) -> Vec<Symbol> {
    let mut given = Vec::new();
    for param in &f.params {
        if param.kind == ParamKind::Type
            && let PatternKind::Ident(name) = param.pattern.kind
        {
            given.push(name);
        }
        if let Some(ty) = &param.ty {
            type_vars_of(ty, &mut given);
        }
    }
    let mut written = Vec::new();
    if let Some(ty) = &f.return_type {
        type_vars_of(ty, &mut written);
    }
    // (A name that starts with `_` is no variable: the checker says it
    // is an unknown type, wherever it is written.)
    (written.into_iter())
        .filter(|v| !given.contains(v) && !resolve(*v).starts_with('_'))
        .collect()
}

impl fmt::Debug for HostModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostModule")
            .field("name", &self.name)
            .field("types", &self.types)
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
