//! The order a module's top-level `let`s are initialised in.
//!
//! A top-level `let` is initialised after every top-level `let` whose
//! value the code that runs to initialise it can read. Where nothing
//! orders two `let`s, the one written first runs first. A `let` that
//! reaches itself has no order: that is an error, which names the way
//! round.
//!
//! # What can run while a `let` is initialised, and why the reach covers it
//!
//! The reach is a graph over the module's `let`s, functions, impl
//! methods and default methods, and one more node, [`OUTSIDE`], for all
//! code the module does not write. A node has an edge to what its
//! expression *mentions*. The code that runs to initialise `L` is:
//!
//! 1. `L`'s initialiser. Every name in it is mentioned, in closures too.
//! 2. A function, method or closure of the module. To be called it must
//!    be a value at hand: a closure is written inside code that already
//!    runs (its body is part of that code's expression); a function is
//!    named by code that runs; a method is called by code that runs, and
//!    the call's `Selection` says which (one impl's method, or, for a
//!    receiver decided where the code runs, every impl of the method in
//!    the module, and the trait's default); a value stored earlier comes
//!    out of a `let`, and whatever reaches a `let` reaches all that the
//!    `let`'s value mentions. A function value that arrives as an
//!    argument or a field was mentioned by the code that passed or
//!    stored it, which runs too. So every such body is reached by
//!    mentions.
//! 3. Code the module does not write: another module's functions,
//!    methods and default methods, the builtin traits' default methods,
//!    the impls of builtin container types. It cannot name this module's
//!    `let`s. It can call back in two ways only: a function value it is
//!    handed (covered by 2: someone mentioned it), and a method of a
//!    trait it knows on a value it is handed. The traits it knows are
//!    not this module's (a module cannot import one that imports it), so
//!    such a call lands in an impl this module writes for a trait of
//!    another module or a builtin trait. [`OUTSIDE`] has an edge to
//!    every method of every such impl, whatever the values are.
//!    Outside code may run whenever running code mentions a function,
//!    a `let` or a host function of another module (called now, or
//!    stored and called later, or handed to a builtin that calls it);
//!    calls a method of another module's trait or a builtin trait on a
//!    receiver decided where the code runs; or calls a method whose
//!    impl or default method is not this module's. Each of those is an
//!    edge to [`OUTSIDE`]. The builtin functions themselves call only
//!    the function values they are given; the sealed traits (Equal,
//!    Compare, Hash) have no code a program writes.
//!
//! 4. A `Display` impl the formatter calls. Showing a value
//!    (interpolation, a builtin whose signature asks `Display` of its
//!    argument, called or handed on as a value, and `.display()` on a
//!    type with no impl of its own) calls the written `Display` impl of
//!    every part of the value that has one. The parts a value of a type
//!    can have are the type's: its arguments, and, for a record or an
//!    enum of the module, the types of its fields, and theirs. Showing
//!    reaches the module's `Display` impl for each such type; a type of
//!    another module among them is code outside the module; a type
//!    variable among them, or an associated type an impl is yet to
//!    name (`Self::Item`), is any type, so every `Display` impl of the
//!    module and code outside it. (`parts_shown` names every form of
//!    type: one it did not know would be any type too.)
//!
//!    A function with type variables of its own (`fn label(x: a) ->
//!    String where a: Display`) shows values whose types have them.
//!    Its body runs only through a value of the function, and every
//!    such value comes from a place that names the function, at a type:
//!    the function's, with a type for each variable. A value made at
//!    one type is called at that type only, whoever calls it (a `let`
//!    whose type is general is named again, at a type, where it is
//!    used; importers of the module do not run while it is
//!    initialised). So what the function shows of its variables is
//!    reached from each node that names it, with the types the
//!    variables stand for there, and not from the function. A type that
//!    is not known where the function is named, or that has a variable
//!    of the naming function, is any type: this goes one step, not
//!    through a chain of functions.
//!
//! # What a `let` needs
//!
//! A `let` whose value is a plain value (a closure literal; a literal;
//! a name; a list, tuple, record, map, set or variant made of plain
//! values) runs nothing when it is initialised: it needs only the
//! top-level `let`s it names outside closures, whose values it reads.
//! Any other `let` needs every `let` its initialiser reaches.

use super::order::{Mention, references_in_expr, references_in_pattern};
use super::*;
use crate::ast::Selection;
use crate::defs::{DefId, TraitId, TypeId};

/// A top-level `let`, a function, or a method of an impl or a trait;
/// or all code the module does not write ([`OUTSIDE`]).
struct Node<'a> {
    /// As a message names it: `limit`, `area`, `Shape.area`.
    name: String,
    /// The declaration of a `let`, as an index into the module's.
    let_decl: Option<usize>,
    span: Span,
    /// What it runs: an initialiser, a body (and what its parameters'
    /// patterns name). None for [`OUTSIDE`].
    body: Option<&'a Expr>,
    params: &'a [Param],
}

/// The node of all code the module does not write (see the module's
/// documentation): the first node.
const OUTSIDE: usize = 0;

/// Whether a value of the type `ty` is made of builtin types only, all
/// known: a builtin trait's method on it (`[1, 2].display()`) calls no
/// code a program writes.
fn builtin_through(checker: &TypeChecker, ty: &Type) -> bool {
    let all = |parts: &[Type]| parts.iter().all(|t| builtin_through(checker, t));
    match ty {
        Type::Var(_) | Type::Rigid(_) => false,
        Type::Generic(name, args) => {
            checker
                .def(name.id.0)
                .is_some_and(|def| def.module.is_builtin())
                && all(args)
        }
        Type::AnonRecord { fields, tail } => {
            matches!(tail, RowTail::Closed) && fields.values().all(|t| builtin_through(checker, t))
        }
        Type::List(t) | Type::Range(t) | Type::Set(t) | Type::Channel(t) => {
            builtin_through(checker, t)
        }
        Type::Map(k, v) => builtin_through(checker, k) && builtin_through(checker, v),
        Type::Tuple(ts) => all(ts),
        // A type the code does not know here: any type.
        Type::AssocProj { .. } => false,
        // (A function inside a value is not called by a method of the
        // value.)
        Type::Fun(..) => true,
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::String
        | Type::Unit
        | Type::Error
        | Type::Never => true,
    }
}

/// Whether `at` is the type `general` with a type for each of its
/// variables, which `stands` then holds. (Where the two differ in
/// more than that the answer is no: the caller takes it for unknown.)
fn instance(general: &Type, at: &Type, stands: &mut HashMap<TyVar, Type>) -> bool {
    let mut all = |general: &[Type], at: &[Type]| {
        general.len() == at.len() && general.iter().zip(at).all(|(g, a)| instance(g, a, stands))
    };
    match (general, at) {
        (Type::Var(v), _) => match stands.get(v) {
            Some(known) => known == at,
            None => {
                stands.insert(*v, at.clone());
                true
            }
        },
        (Type::Fun(gp, gr), Type::Fun(ap, ar)) => {
            all(gp, ap) && all(std::slice::from_ref(&**gr), std::slice::from_ref(&**ar))
        }
        (Type::List(g), Type::List(a))
        | (Type::Range(g), Type::Range(a))
        | (Type::Set(g), Type::Set(a))
        | (Type::Channel(g), Type::Channel(a)) => instance(g, a, stands),
        (Type::Tuple(g), Type::Tuple(a)) => all(g, a),
        (Type::Generic(gn, g), Type::Generic(an, a)) => gn == an && all(g, a),
        (Type::Map(gk, gv), Type::Map(ak, av)) => {
            instance(gk, ak, stands) && instance(gv, av, stands)
        }
        (
            Type::AnonRecord {
                fields: g,
                tail: RowTail::Closed,
            },
            Type::AnonRecord {
                fields: a,
                tail: RowTail::Closed,
            },
        ) => {
            g.len() == a.len()
                && g.iter()
                    .zip(a)
                    .all(|((gn, g), (an, a))| gn == an && instance(g, a, stands))
        }
        _ => general == at,
    }
}

/// Whether initialising a `let` with the value `expr` runs nothing: the
/// value restriction's syntactic values, and a map or set literal of
/// them. The top-level names it reads (outside closures) are added to
/// `reads`.
fn plain_value(
    checker: &TypeChecker,
    expr: &Expr,
    reads: &mut Vec<Option<crate::defs::Res>>,
) -> bool {
    match &expr.kind {
        ExprKind::Ident(_) | ExprKind::FieldAccess(..) => {
            reads.push(expr.res);
            checker.is_syntactic_value(expr)
        }
        ExprKind::Tuple(elems) | ExprKind::SetLit(elems) => {
            elems.iter().all(|e| plain_value(checker, e, reads))
        }
        ExprKind::List(elems) => elems.iter().all(|e| match e {
            ListElem::Single(e) => plain_value(checker, e, reads),
            ListElem::Spread(_) => false,
        }),
        ExprKind::Map(entries) => entries
            .iter()
            .all(|(k, v)| plain_value(checker, k, reads) && plain_value(checker, v, reads)),
        ExprKind::RecordCreate { fields, .. }
        | ExprKind::AnonRecord {
            spread: None,
            fields,
        } => fields.iter().all(|(_, e)| plain_value(checker, e, reads)),
        ExprKind::Call(callee, args) => {
            checker.is_syntactic_value(expr) && {
                reads.push(callee.res);
                args.iter().all(|e| plain_value(checker, e, reads))
            }
        }
        _ => checker.is_syntactic_value(expr),
    }
}

/// What showing a value of the type `ty` can call: the types `ty` is
/// made of that are another module's or unknown (`outside`), and the
/// types of the module among them (`own`), fields included.
struct Parts {
    own: Vec<TypeRef>,
    outside: bool,
}

impl TypeChecker {
    /// See [`Parts`]. A type variable inside a declaration (`declared`)
    /// is a parameter: what stands for it is among the type's
    /// arguments where the type is used.
    ///
    /// Every form of type is named here, and none is passed over by a
    /// catch-all: a form that is not known by its name is any type.
    fn parts_shown(&self, ty: &Type, declared: bool, parts: &mut Parts) {
        match ty {
            Type::Var(_) | Type::Rigid(_) => parts.outside |= !declared,
            // The type an impl gives for an associated type, which is
            // not known here: any type.
            Type::AssocProj { .. } => parts.outside = true,
            Type::Generic(name, args) => {
                for arg in args {
                    self.parts_shown(arg, declared, parts);
                }
                let Some(def) = self.def(name.id.0) else {
                    parts.outside = true;
                    return;
                };
                if def.module.is_builtin() {
                    return;
                }
                if def.module != self.module {
                    parts.outside = true;
                    return;
                }
                if parts.own.contains(name) {
                    return;
                }
                parts.own.push(*name);
                let fields: Vec<Type> = match self.tables.records.get(name) {
                    Some(record) => record.fields.iter().map(|(_, t)| t.clone()).collect(),
                    None => self
                        .tables
                        .enums
                        .get(name)
                        .map(|info| {
                            info.variants
                                .iter()
                                .flat_map(|v| v.field_types.iter().cloned())
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                for field in &fields {
                    self.parts_shown(&self.apply(field), true, parts);
                }
            }
            Type::AnonRecord { fields, tail } => {
                parts.outside |= !declared && !matches!(tail, RowTail::Closed);
                for field in fields.values() {
                    self.parts_shown(field, declared, parts);
                }
            }
            Type::List(t) | Type::Range(t) | Type::Set(t) | Type::Channel(t) => {
                self.parts_shown(t, declared, parts)
            }
            Type::Map(k, v) => {
                self.parts_shown(k, declared, parts);
                self.parts_shown(v, declared, parts);
            }
            Type::Tuple(ts) => ts.iter().for_each(|t| self.parts_shown(t, declared, parts)),
            // A function is not shown (a type that holds one has no
            // `Display` but the one written for it, which is the
            // type's own impl, counted with the type).
            Type::Fun(..) => {}
            // No parts, and no impl a program writes. (`Error` and
            // `Never` are of no value that is shown.)
            Type::Int
            | Type::Float
            | Type::Bool
            | Type::String
            | Type::Unit
            | Type::Error
            | Type::Never => {}
        }
    }

    /// The parameters of the builtin function `def` that it shows: the
    /// ones whose type has a variable its signature asks `Display` of.
    fn shown_params(&self, def: &crate::defs::Def) -> Vec<usize> {
        let Some(scheme) = builtin_scheme(def) else {
            return Vec::new();
        };
        let Type::Fun(params, _) = &scheme.ty else {
            return Vec::new();
        };
        let display = TraitKey::builtin("Display");
        let shown: Vec<TyVar> = scheme
            .preds
            .iter()
            .filter_map(|pred| match pred {
                Pred::Trait {
                    tr,
                    subject: Type::Var(v),
                    ..
                } if *tr == display => Some(*v),
                _ => None,
            })
            .collect();
        params
            .iter()
            .enumerate()
            .filter(|(_, param)| free_vars_in(param).iter().any(|v| shown.contains(v)))
            .map(|(i, _)| i)
            .collect()
    }

    /// The module's top-level `let`s, by the span of each, in the order
    /// they are initialised in. Reports each `let` that reaches itself.
    pub(super) fn init_order(&mut self, decls: &[Decl], env: &TypeEnv) -> Vec<Span> {
        let mut nodes: Vec<Node> = vec![Node {
            name: "code outside the module".to_string(),
            let_decl: None,
            span: Span::BUILTIN,
            body: None,
            params: &[],
        }];
        let mut by_name: HashMap<Symbol, usize> = HashMap::new();
        let mut bound_twice: Vec<usize> = Vec::new();
        // The methods of each impl; of each trait, whatever the impl;
        // and each trait's default methods.
        let mut of_impl: HashMap<(TraitId, TypeId, Symbol), usize> = HashMap::new();
        let mut of_trait: HashMap<(TraitId, Symbol), Vec<usize>> = HashMap::new();
        let mut defaults: HashMap<(TraitId, Symbol), usize> = HashMap::new();
        // The methods of the module's impls of traits it does not
        // declare: what code outside the module can call.
        let mut callable_outside: Vec<usize> = Vec::new();
        for (i, decl) in decls.iter().enumerate() {
            match decl {
                Decl::Fn(f) => {
                    by_name.insert(f.name, nodes.len());
                    nodes.push(Node {
                        name: resolve(f.name),
                        let_decl: None,
                        span: f.span,
                        body: Some(&f.body),
                        params: &f.params,
                    });
                }
                Decl::Let {
                    pattern,
                    value,
                    span,
                    ..
                } => {
                    let binders = collect_pattern_vars(pattern);
                    for name in &binders {
                        // (A name bound twice is the parser's error: what
                        // reads it is not ordered by it.)
                        if let Some(first) = by_name.insert(*name, nodes.len()) {
                            bound_twice.extend([first, nodes.len()]);
                        }
                    }
                    nodes.push(Node {
                        name: match binders.first() {
                            Some(name) => resolve(*name),
                            None => "_".to_string(),
                        },
                        let_decl: Some(i),
                        span: *span,
                        body: Some(value),
                        params: &[],
                    });
                }
                Decl::TraitImpl(ti) => {
                    let (Some(tr), Some(ty)) = (self.impl_trait(ti), self.impl_target(ti)) else {
                        continue;
                    };
                    for m in &ti.methods {
                        of_impl.insert((tr.id, ty.id, m.name), nodes.len());
                        if !self.own_traits.values().any(|own| own.id == tr.id) {
                            callable_outside.push(nodes.len());
                        }
                        of_trait
                            .entry((tr.id, m.name))
                            .or_default()
                            .push(nodes.len());
                        nodes.push(Node {
                            name: format!("{}.{}", ty.name, m.name),
                            let_decl: None,
                            span: m.span,
                            body: Some(&m.body),
                            params: &m.params,
                        });
                    }
                }
                Decl::Trait(t) => {
                    let tr = self.own_trait(t.name);
                    for m in t.methods.iter().filter(|m| !m.is_signature_only) {
                        defaults.insert((tr.id, m.name), nodes.len());
                        of_trait
                            .entry((tr.id, m.name))
                            .or_default()
                            .push(nodes.len());
                        nodes.push(Node {
                            name: format!("{}.{}", t.name, m.name),
                            let_decl: None,
                            span: m.span,
                            body: Some(&m.body),
                            params: &m.params,
                        });
                    }
                }
                _ => {}
            }
        }
        if !nodes.iter().any(|node| node.let_decl.is_some()) {
            return Vec::new();
        }
        let mut by_def: HashMap<DefId, usize> = HashMap::new();
        // The functions with type variables of their own, each with its
        // scheme.
        let mut general: HashMap<usize, Scheme> = HashMap::new();
        if let Some(defs) = &self.defs {
            for id in defs.of_module(self.module) {
                let def = defs.get(*id);
                if matches!(
                    def.kind,
                    crate::defs::DefKind::Fn | crate::defs::DefKind::Let
                ) && let Some(&node) = by_name.get(&def.name)
                {
                    by_def.insert(*id, node);
                    if def.kind == crate::defs::DefKind::Fn
                        && let Some(scheme) = env.lookup(def.name)
                        && !scheme.vars.is_empty()
                    {
                        general.insert(node, scheme.clone());
                    }
                }
            }
        }

        // What each node mentions, in the order it does; for a `let`
        // that is mentioned by a name, the name (a `let` may bind
        // several).
        let own_trait = |tr: TraitId| self.own_traits.values().any(|own| own.id == tr);
        let display = TraitKey::builtin("Display");
        let display_method = intern("display");
        let mut named: HashMap<(usize, usize), Symbol> = HashMap::new();
        let mut edges: Vec<Vec<usize>> = Vec::with_capacity(nodes.len());
        // What showing a value of the type `ty` reaches: the `Display`
        // impls of its parts.
        let shows = |ty: &Type, add: &mut dyn FnMut(usize)| {
            let mut parts = Parts {
                own: Vec::new(),
                outside: false,
            };
            self.parts_shown(&self.apply(ty), false, &mut parts);
            for ty in parts.own {
                let ty = canonical_head(&self.tables.resolver, ty);
                if let Some(&target) = of_impl.get(&(display.id, ty.id, display_method)) {
                    add(target);
                }
            }
            if parts.outside {
                for &target in of_trait
                    .get(&(display.id, display_method))
                    .into_iter()
                    .flatten()
                {
                    add(target);
                }
                add(OUTSIDE);
            }
        };
        // A function with type variables of its own shows values whose
        // types have them (`fn label(x: a) -> String where a: Display {
        // "<{x}>" }`). What the variables stand for is known where the
        // function is named: the type it is used at there says so, and
        // a value made there is called at that type only. So such a
        // showing is not the function's (`shown_at`, by function) but
        // of each node that names it (`uses`: the function, and the
        // type it is named at), below.
        let mut shown_at: HashMap<usize, Vec<Type>> = HashMap::new();
        let mut uses: Vec<Vec<(usize, Option<Type>)>> = Vec::with_capacity(nodes.len());
        for (from, node) in nodes.iter().enumerate() {
            let Some(body) = node.body else {
                edges.push(callable_outside.clone());
                uses.push(Vec::new());
                continue;
            };
            let targets: std::cell::RefCell<Vec<usize>> = std::cell::RefCell::new(Vec::new());
            let own_vars: &[TyVar] = general.get(&from).map_or(&[], |scheme| &scheme.vars);
            let deferred: std::cell::RefCell<Vec<Type>> = std::cell::RefCell::new(Vec::new());
            let used: std::cell::RefCell<Vec<(usize, Option<Type>)>> =
                std::cell::RefCell::new(Vec::new());
            let mut note = |mention: Mention| {
                let add = |target: usize| {
                    let mut targets = targets.borrow_mut();
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                };
                if let Some(crate::defs::Res::Def(id)) = mention.res {
                    if let Some(&target) = by_def.get(&id) {
                        add(target);
                        if general.contains_key(&target) {
                            used.borrow_mut()
                                .push((target, mention.expr.and_then(|expr| expr.ty.clone())));
                        }
                        if let Some(def) = self.def(id) {
                            named.entry((from, target)).or_insert(def.name);
                        }
                    } else if self.def(id).is_some_and(|def| {
                        // A function or a `let` of another module: its
                        // code may run.
                        def.module != self.module
                            && !def.module.is_builtin()
                            && matches!(
                                def.kind,
                                crate::defs::DefKind::Fn
                                    | crate::defs::DefKind::Let
                                    | crate::defs::DefKind::Host
                            )
                    }) {
                        add(OUTSIDE);
                    }
                }
                // A value that is shown: what its parts' `Display` impls
                // are. Where its type has variables of the function
                // written here, and no others, the parts they stand for
                // are for who names the function.
                let show = |ty: &Type| {
                    let ty = self.apply(ty);
                    let mut vars: Vec<TyVar> = Vec::new();
                    crate::types::map_rigid(&ty, &mut |r| {
                        vars.push(r.var);
                        Type::Rigid(r)
                    });
                    if vars.is_empty() || !vars.iter().all(|v| own_vars.contains(v)) {
                        return shows(&ty, &mut |target| add(target));
                    }
                    let rest = crate::types::map_rigid(&ty, &mut |_| Type::Unit);
                    shows(&rest, &mut |target| add(target));
                    deferred.borrow_mut().push(ty);
                };
                match mention.expr.map(|expr| (&expr.kind, expr)) {
                    Some((ExprKind::StringInterp(parts), _)) => {
                        for part in parts {
                            if let StringPart::Expr(shown) = part
                                && let Some(ty) = &shown.ty
                            {
                                show(ty);
                            }
                        }
                    }
                    // A builtin that shows an argument, called or not:
                    // the type it is used at says what it is given.
                    Some((ExprKind::Ident(_) | ExprKind::FieldAccess(..), expr)) => {
                        if let Some(crate::defs::Res::Def(id)) = expr.res
                            && let Some(def) = self.def(id)
                            && def.module.is_builtin()
                            && let Some(Type::Fun(params, _)) = &expr.ty
                        {
                            for i in self.shown_params(&def) {
                                if let Some(param) = params.get(i) {
                                    show(param);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                let Some(Expr {
                    kind: ExprKind::FieldAccess(recv, method, _),
                    sel,
                    res,
                    ..
                }) = mention.expr
                else {
                    return;
                };
                // The method of the impl of `tr` for `ty`: the impl's
                // own, the trait's default, or code outside the module
                // (but a builtin trait's method on a value, of the type
                // `of`, that is builtin through and through: that
                // calls nothing a program writes).
                let one_impl = |tr: TraitId, ty: TypeId, of: Option<&Type>| match of_impl
                    .get(&(tr, ty, *method))
                    .or_else(|| defaults.get(&(tr, *method)))
                {
                    Some(&target) => add(target),
                    None => {
                        let inert = self.def(tr.0).is_some_and(|def| def.module.is_builtin())
                            && of.is_some_and(|of| builtin_through(self, &self.apply(of)));
                        if !inert {
                            add(OUTSIDE);
                        }
                    }
                };
                match sel {
                    Some(Selection::Impl { tr, ty }) => one_impl(*tr, *ty, recv.ty.as_ref()),
                    // A receiver decided where the code runs: every
                    // impl of the method in the module, the trait's
                    // default, and, for a trait of another module or a
                    // builtin one, impls and defaults outside it.
                    // (`display` of such a receiver shows it.)
                    Some(Selection::Dynamic { tr })
                        if *tr == display.id && *method == display_method && recv.ty.is_some() =>
                    {
                        if let Some(ty) = &recv.ty {
                            show(ty);
                        }
                    }
                    Some(Selection::Dynamic { tr }) => {
                        for &target in of_trait.get(&(*tr, *method)).into_iter().flatten() {
                            add(target);
                        }
                        if !own_trait(*tr) {
                            add(OUTSIDE);
                        }
                    }
                    // The VM's own method of a builtin trait: `display`
                    // shows the receiver; the others call no code a
                    // program writes.
                    Some(Selection::Native { tr }) => {
                        if *tr == display.id
                            && let Some(ty) = &recv.ty
                        {
                            show(ty);
                        }
                    }
                    Some(Selection::Field | Selection::FieldCall) => {}
                    // `Type.method`: that impl's method.
                    None => {
                        if recv.ty.is_none()
                            && let Some(crate::defs::Res::Def(id)) = res
                            && let Some(tr) = self.trait_key(*id)
                        {
                            let ty = match (&recv.kind, recv.res) {
                                (ExprKind::Ident(name), None) => self.named_type(None, *name),
                                (_, res) => self.res_type(res),
                            };
                            match ty {
                                Some(ty) => {
                                    let ty = canonical_head(&self.tables.resolver, ty);
                                    // (`Int.display`: a type without
                                    // parameters is all of the value's.)
                                    let whole = Type::Generic(ty, Vec::new());
                                    let of = matches!(
                                        builtin_type_name(ty),
                                        Some("Int" | "Float" | "Bool" | "String" | "Unit")
                                    )
                                    .then_some(&whole);
                                    one_impl(tr.id, ty.id, of)
                                }
                                None => {
                                    for &target in
                                        of_trait.get(&(tr.id, *method)).into_iter().flatten()
                                    {
                                        add(target);
                                    }
                                    add(OUTSIDE);
                                }
                            }
                        }
                    }
                }
            };
            for param in node.params {
                references_in_pattern(&param.pattern, &mut note);
            }
            references_in_expr(body, &mut note);
            edges.push(targets.into_inner());
            uses.push(used.into_inner());
            let deferred = deferred.into_inner();
            if !deferred.is_empty() {
                shown_at.insert(from, deferred);
            }
        }
        // What a function shows of its own variables, for each node
        // that names it, at the type it names it at. A type that is not
        // known there, or has a variable left, is any type.
        for (from, used) in uses.iter().enumerate() {
            for (target, at) in used {
                let (Some(shown), Some(scheme)) = (shown_at.get(target), general.get(target))
                else {
                    continue;
                };
                let mut stands: HashMap<TyVar, Type> = HashMap::new();
                let known = at
                    .as_ref()
                    .is_some_and(|at| instance(&scheme.ty, &self.apply(at), &mut stands));
                for ty in shown {
                    let here = match known {
                        true => crate::types::map_rigid(ty, &mut |r| {
                            stands.get(&r.var).cloned().unwrap_or(Type::Rigid(r))
                        }),
                        false => ty.clone(),
                    };
                    // (A function that calls itself, at its own
                    // variables: nothing new.)
                    if from == *target && here == *ty {
                        continue;
                    }
                    shows(&here, &mut |target| {
                        if !edges[from].contains(&target) {
                            edges[from].push(target);
                        }
                    });
                }
            }
        }

        // The `let`s each `let` needs, with the way to each (the
        // functions between, then the `let`).
        let lets: Vec<usize> = (0..nodes.len())
            .filter(|&n| nodes[n].let_decl.is_some())
            .collect();
        let mut needs: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut ways: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
        for &start in &lets {
            // A plain value runs nothing: it needs the `let`s it reads.
            let mut reads = Vec::new();
            if let Some(value) = nodes[start].body
                && plain_value(self, value, &mut reads)
            {
                let mut read: Vec<usize> = Vec::new();
                for res in reads {
                    if let Some(crate::defs::Res::Def(id)) = res
                        && let Some(&target) = by_def.get(&id)
                        && nodes[target].let_decl.is_some()
                        && !read.contains(&target)
                    {
                        ways.insert((start, target), vec![target]);
                        read.push(target);
                    }
                }
                needs.insert(start, read);
                continue;
            }
            // (Breadth first: the way found to a `let` is a shortest.)
            let mut from: HashMap<usize, usize> = HashMap::new();
            let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
            let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
            for &target in &edges[start] {
                if seen.insert(target) {
                    from.insert(target, start);
                    queue.push_back(target);
                }
            }
            let mut reached: Vec<usize> = Vec::new();
            while let Some(node) = queue.pop_front() {
                if nodes[node].let_decl.is_some() {
                    let mut way = vec![node];
                    let mut at = node;
                    while from[&at] != start {
                        at = from[&at];
                        way.push(at);
                    }
                    way.reverse();
                    ways.insert((start, node), way);
                    reached.push(node);
                    // (What the `let` holds may be called through it:
                    // what its value mentions is reached too.)
                }
                for &target in &edges[node] {
                    if seen.insert(target) {
                        from.insert(target, node);
                        queue.push_back(target);
                    }
                }
            }
            needs.insert(start, reached);
        }
        // A `let` that reaches itself, through other `let`s or not, has
        // no place in the order: one error for each such ring, at the
        // first written of its `let`s, with the way round.
        let mut in_ring: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for &start in &lets {
            if in_ring.contains(&start) {
                continue;
            }
            // The shortest way back to `start` over the `let`s.
            let mut from: HashMap<usize, usize> = HashMap::new();
            let mut queue: std::collections::VecDeque<usize> =
                std::collections::VecDeque::from([start]);
            let mut back = false;
            'search: while let Some(at) = queue.pop_front() {
                for &next in &needs[&at] {
                    if next == start {
                        from.insert(start, at);
                        back = true;
                        break 'search;
                    }
                    if let std::collections::hash_map::Entry::Vacant(unseen) = from.entry(next) {
                        unseen.insert(at);
                        queue.push_back(next);
                    }
                }
            }
            if !back {
                continue;
            }
            let mut ring = vec![start];
            let mut at = from[&start];
            while at != start {
                ring.push(at);
                at = from[&at];
            }
            ring.push(start);
            ring.reverse();
            // The way round, each `let` by the name it is read by (a
            // `let` may bind several).
            let mut way: Vec<usize> = vec![start];
            for pair in ring.windows(2) {
                way.extend(&ways[&(pair[0], pair[1])]);
            }
            let mut names: Vec<String> = Vec::with_capacity(way.len());
            for step in way.windows(2) {
                names.push(match named.get(&(step[0], step[1])) {
                    Some(name) if nodes[step[1]].let_decl.is_some() => resolve(*name),
                    _ => nodes[step[1]].name.clone(),
                });
            }
            let own = names.last().cloned().unwrap_or_default();
            names.insert(0, own.clone());
            // (Each `let` on the way is in the ring.)
            in_ring.extend(way.iter().filter(|&&n| nodes[n].let_decl.is_some()));
            if way.iter().any(|n| bound_twice.contains(n)) {
                continue;
            }
            self.errors.push(
                Diagnostic::error(
                    Code::InitCycle,
                    nodes[start].span,
                    format!(
                        "the top-level `let` '{own}' needs its own value to be initialised: {}",
                        names.join(" -> ")
                    ),
                )
                .with_help(
                    "a top-level `let` is initialised after every top-level `let` its value can reach, through the functions it mentions: make one of them a function, or pass the value as an argument",
                ),
            );
        }
        self.let_rings = in_ring.iter().map(|&n| nodes[n].span).collect();
        for reached in needs.values_mut() {
            reached.retain(|n| !in_ring.contains(n));
        }
        // (The `let`s of a ring are reported; they keep their source
        // order among the others.)
        // Each `let` once the ones it needs are in: of those ready, the
        // first written.
        let place: HashMap<usize, usize> = lets.iter().enumerate().map(|(k, &n)| (n, k)).collect();
        let mut waits: Vec<usize> = lets.iter().map(|n| needs[n].len()).collect();
        let mut needed_by: Vec<Vec<usize>> = vec![Vec::new(); lets.len()];
        for (k, n) in lets.iter().enumerate() {
            for need in &needs[n] {
                needed_by[place[need]].push(k);
            }
        }
        let mut ready: std::collections::BinaryHeap<std::cmp::Reverse<usize>> = waits
            .iter()
            .enumerate()
            .filter(|(_, waits)| **waits == 0)
            .map(|(k, _)| std::cmp::Reverse(k))
            .collect();
        let mut order: Vec<Span> = Vec::with_capacity(lets.len());
        while let Some(std::cmp::Reverse(k)) = ready.pop() {
            order.push(nodes[lets[k]].span);
            for &dependent in &needed_by[k] {
                waits[dependent] -= 1;
                if waits[dependent] == 0 {
                    ready.push(std::cmp::Reverse(dependent));
                }
            }
        }
        debug_assert_eq!(order.len(), lets.len(), "the rings are out of the order");
        order
    }
}
