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
//!    methods and default methods, the impls of builtin container types. It cannot name this module's
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
//!    argument, called or handed on as a value) calls the written
//!    `Display` impl of
//!    every part of the value that has one. The parts a value of a type
//!    can have are the type's: its arguments, and, for a record or an
//!    enum of the module, the types of its fields, and theirs. Showing
//!    reaches the module's `Display` impl for each such type; a type of
//!    another module among them is code outside the module; a type
//!    variable among them is any type, so every `Display` impl of the
//!    module and code outside it. (`parts_shown` names every form of
//!    type: one it did not know would be any type too.)
//!
//!    Writing a value as data is the same with the module's
//!    `json.Encode` impls: a builtin whose signature asks `json.Encode`
//!    of an argument (`json.stringify`), `toml.stringify`, `toml.pretty`
//!    and the parameters of a postgres statement (whose signatures ask
//!    nothing of them, but which write them with the same writer), and
//!    `json.Encode.encode(x)` on a type with no impl of its own. Decoding a document
//!    into a type is the same with the module's `json.Decode` impls of
//!    the type and of the types of its fields: a builtin whose signature
//!    asks `json.Decode` of its type argument (`json.parse`), and
//!    `T.decode(v)` on a type with no impl of its own.
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
/// known: a builtin trait's method on it (`Hash.hash([1, 2])`) calls no
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
        Type::Container(Container::List(t))
        | Type::Container(Container::Set(t))
        | Type::Container(Container::Channel(t)) => builtin_through(checker, t),
        Type::Container(Container::Map(k, v)) => {
            builtin_through(checker, k) && builtin_through(checker, v)
        }
        Type::Container(Container::Tuple(ts)) => all(ts),
        // A type the code does not know here: any type.
        Type::App(..) | Type::Con(..) => false,
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
        ExprKind::RecordCreate { fields, .. } => {
            fields.iter().all(|(_, e)| plain_value(checker, e, reads))
        }
        ExprKind::Call(callee, args) => {
            checker.is_syntactic_value(expr) && {
                reads.push(callee.res);
                args.iter().all(|e| plain_value(checker, e, reads))
            }
        }
        _ => checker.is_syntactic_value(expr),
    }
}

/// The nodes of `nodes` that reach themselves over `next` (each node's
/// successors, by node): those of a strongly connected part of more than
/// one, and one that needs itself.
fn on_a_ring(nodes: &[usize], next: &[Vec<usize>]) -> std::collections::HashSet<usize> {
    (strong_parts(nodes, next).into_iter())
        .filter(|part| part.len() > 1 || next[part[0]].contains(&part[0]))
        .flatten()
        .collect()
}

/// The strongly connected parts of the graph `next` (each node's
/// successors, by node) that the nodes `nodes` reach, each part after the
/// parts it reaches. Tarjan's search, without recursion.
fn strong_parts(nodes: &[usize], next: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNSEEN: usize = usize::MAX;
    let mut index: Vec<usize> = vec![UNSEEN; next.len()];
    let mut low: Vec<usize> = vec![0; next.len()];
    let mut on_stack: Vec<bool> = vec![false; next.len()];
    let mut stack: Vec<usize> = Vec::new();
    let mut seen = 0;
    let mut parts = Vec::new();
    for &root in nodes {
        if index[root] != UNSEEN {
            continue;
        }
        // (The node, and how many of its successors are looked at.)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&mut (node, ref mut at)) = work.last_mut() {
            if *at == 0 && index[node] == UNSEEN {
                index[node] = seen;
                low[node] = seen;
                seen += 1;
                stack.push(node);
                on_stack[node] = true;
            }
            if let Some(&succ) = next[node].get(*at) {
                *at += 1;
                if index[succ] == UNSEEN {
                    work.push((succ, 0));
                } else if on_stack[succ] {
                    low[node] = low[node].min(index[succ]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == index[node] {
                let mut part = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    part.push(member);
                    if member == node {
                        break;
                    }
                }
                parts.push(part);
            }
        }
    }
    parts
}

/// The `let`s of the node graph `edges` that `start` reaches, in the
/// order a breadth-first search over `edges` finds them: what a `let`
/// whose value is not plain needs.
fn reached_lets(edges: &[Vec<usize>], is_let: &dyn Fn(usize) -> bool, start: usize) -> Vec<usize> {
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for &target in &edges[start] {
        if seen.insert(target) {
            queue.push_back(target);
        }
    }
    let mut reached: Vec<usize> = Vec::new();
    while let Some(node) = queue.pop_front() {
        if is_let(node) {
            reached.push(node);
        }
        for &target in &edges[node] {
            if seen.insert(target) {
                queue.push_back(target);
            }
        }
    }
    reached
}

/// The way from the node `start` to the node `goal` that the search of
/// `init_order` for what `start` needs finds (breadth first over
/// `edges`, in their order): the nodes after `start`, `goal` last. Empty
/// where `goal` is not reached.
fn way_to(edges: &[Vec<usize>], start: usize, goal: usize) -> Vec<usize> {
    let mut from: HashMap<usize, usize> = HashMap::new();
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for &target in &edges[start] {
        if let std::collections::hash_map::Entry::Vacant(unseen) = from.entry(target) {
            unseen.insert(start);
            queue.push_back(target);
        }
    }
    while let Some(node) = queue.pop_front() {
        if node == goal {
            let mut way = vec![node];
            let mut at = node;
            while from[&at] != start {
                at = from[&at];
                way.push(at);
            }
            way.reverse();
            return way;
        }
        for &target in &edges[node] {
            if let std::collections::hash_map::Entry::Vacant(unseen) = from.entry(target) {
                unseen.insert(node);
                queue.push_back(target);
            }
        }
    }
    Vec::new()
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
            Type::Var(_) | Type::Rigid(_) | Type::App(..) | Type::Con(..) => {
                parts.outside |= !declared
            }
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
            Type::Container(Container::List(t))
            | Type::Container(Container::Set(t))
            | Type::Container(Container::Channel(t)) => self.parts_shown(t, declared, parts),
            Type::Container(Container::Map(k, v)) => {
                self.parts_shown(k, declared, parts);
                self.parts_shown(v, declared, parts);
            }
            Type::Container(Container::Tuple(ts)) => {
                ts.iter().for_each(|t| self.parts_shown(t, declared, parts))
            }
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

    /// The parameters of the builtin function `def` that it shows or
    /// writes as data, each with the trait whose written impls of its
    /// parts that calls (`Display`, `json.Encode`): the ones whose type
    /// has a variable its signature asks the trait of; and the
    /// parameters of a postgres statement, which the writer writes as
    /// `json.stringify` does, though its signature asks nothing of them;
    /// and the type
    /// argument of every decoder (toml's and postgres's too, whose
    /// signatures ask nothing of it), which decodes as json's do.
    fn shown_params(&self, def: &crate::defs::Def) -> Vec<(usize, TraitKey)> {
        let Some(scheme) = builtin_scheme(def) else {
            return Vec::new();
        };
        let Type::Fun(params, ret) = &scheme.ty else {
            return Vec::new();
        };
        let encode = TraitKey::builtin("Encode");
        let mut shown: Vec<(TyVar, TraitKey)> = scheme
            .preds
            .iter()
            .filter_map(|pred| match pred {
                Pred::Trait {
                    tr,
                    subject: Type::Var(v),
                    ..
                } if WRITABLE_STRUCTURAL_TRAITS
                    .iter()
                    .any(|name| tr.is_builtin(name)) =>
                {
                    Some((*v, *tr))
                }
                _ => None,
            })
            .collect();
        let row = (def.module.builtin_name())
            .and_then(|module| crate::builtins::registry::registry().module(module))
            .and_then(|module| module.row(&resolve(def.name)));
        let written = match row {
            Some(row) if row.statement => Some(2),
            _ => None,
        };
        if let Some(param) = written.and_then(|i| params.get(i)) {
            shown.extend(free_vars_in(param).into_iter().map(|v| (v, encode)));
        }
        // (A decoder's type argument is what it gives back.)
        if row.is_some_and(|row| row.decoding.is_some()) {
            let decode = TraitKey::builtin("Decode");
            shown.extend(free_vars_in(ret).into_iter().map(|v| (v, decode)));
        }
        let mut found = Vec::new();
        for (i, param) in params.iter().enumerate() {
            for (v, tr) in &shown {
                if free_vars_in(param).contains(v) && !found.contains(&(i, *tr)) {
                    found.push((i, *tr));
                }
            }
        }
        found
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
                    let binders = pattern.binder_names();
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
        let encode = TraitKey::builtin("Encode");
        let decode = TraitKey::builtin("Decode");
        // The method of each trait whose written impls showing, writing
        // or decoding a value calls, at any depth.
        let method_of = |tr: TraitKey| {
            intern(match tr {
                _ if tr == encode => "encode",
                _ if tr == decode => "decode",
                _ => "display",
            })
        };
        let mut named: HashMap<(usize, usize), Symbol> = HashMap::new();
        let mut edges: Vec<Vec<usize>> = Vec::with_capacity(nodes.len());
        // What showing (`Display`) or writing (`json.Encode`) a value of
        // the type `ty` reaches: the impls of `tr` of its parts.
        let shows = |tr: TraitKey, ty: &Type, add: &mut dyn FnMut(usize)| {
            let method = method_of(tr);
            let mut parts = Parts {
                own: Vec::new(),
                outside: false,
            };
            self.parts_shown(&self.apply(ty), false, &mut parts);
            for ty in parts.own {
                let ty = canonical_head(&self.tables.resolver, ty);
                if let Some(&target) = of_impl.get(&(tr.id, ty.id, method)) {
                    add(target);
                }
            }
            if parts.outside {
                for &target in of_trait.get(&(tr.id, method)).into_iter().flatten() {
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
        let mut shown_at: HashMap<usize, Vec<(TraitKey, Type)>> = HashMap::new();
        let mut uses: Vec<Vec<(usize, Option<Type>)>> = Vec::with_capacity(nodes.len());
        for (from, node) in nodes.iter().enumerate() {
            let Some(body) = node.body else {
                edges.push(callable_outside.clone());
                uses.push(Vec::new());
                continue;
            };
            let targets: std::cell::RefCell<Vec<usize>> = std::cell::RefCell::new(Vec::new());
            let own_vars: &[TyVar] = general.get(&from).map_or(&[], |scheme| &scheme.vars);
            let deferred: std::cell::RefCell<Vec<(TraitKey, Type)>> =
                std::cell::RefCell::new(Vec::new());
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
                let show = |tr: TraitKey, ty: &Type| {
                    let ty = self.apply(ty);
                    let mut vars: Vec<TyVar> = Vec::new();
                    crate::types::map_rigid(&ty, &mut |r| {
                        vars.push(r.var);
                        Type::Rigid(r)
                    });
                    if vars.is_empty() || !vars.iter().all(|v| own_vars.contains(v)) {
                        return shows(tr, &ty, &mut |target| add(target));
                    }
                    let rest = crate::types::map_rigid(&ty, &mut |_| Type::Unit);
                    shows(tr, &rest, &mut |target| add(target));
                    deferred.borrow_mut().push((tr, ty));
                };
                match mention.expr.map(|expr| (&expr.kind, expr)) {
                    Some((ExprKind::StringInterp(parts), _)) => {
                        for part in parts {
                            if let StringPart::Expr(shown) = part
                                && let Some(ty) = &shown.ty
                            {
                                show(display, ty);
                            }
                        }
                    }
                    // A builtin that shows or writes an argument, called
                    // or not: the type it is used at says what it is
                    // given.
                    Some((ExprKind::Ident(_) | ExprKind::FieldAccess(..), expr)) => {
                        if let Some(crate::defs::Res::Def(id)) = expr.res
                            && let Some(def) = self.def(id)
                            && def.module.is_builtin()
                            && let Some(Type::Fun(params, _)) = &expr.ty
                        {
                            for (i, tr) in self.shown_params(&def) {
                                if let Some(param) = params.get(i) {
                                    show(tr, param);
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
                    span,
                    ..
                }) = mention.expr
                else {
                    return;
                };
                // The type the method is of: the receiver's, or, called
                // through the trait (`Hash.hash(x)`), the type its
                // argument of type `Self` has.
                let recv_ty: Option<Type> = (recv.ty.clone())
                    .or_else(|| self.trait_call_self.get(span).map(|t| self.apply(t)));
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
                    Some(Selection::Impl { tr, ty }) => one_impl(*tr, *ty, recv_ty.as_ref()),
                    // A receiver decided where the code runs: every
                    // impl of the method in the module, the trait's
                    // default, and, for a trait of another module or a
                    // builtin one, impls and defaults outside it.
                    // (`encode` of such a receiver writes it, `decode` of
                    // a type decodes into it.)
                    Some(Selection::Dynamic { tr, .. })
                        if [encode, decode]
                            .iter()
                            .any(|t| t.id == *tr && *method == method_of(*t))
                            && recv_ty.is_some() =>
                    {
                        if let Some(ty) = &recv_ty
                            && let Some(tr) = [encode, decode].into_iter().find(|t| t.id == *tr)
                        {
                            show(tr, ty);
                        }
                    }
                    Some(Selection::Dynamic { tr, .. }) => {
                        for &target in of_trait.get(&(*tr, *method)).into_iter().flatten() {
                            add(target);
                        }
                        if !own_trait(*tr) {
                            add(OUTSIDE);
                        }
                    }
                    // The VM's own method of a builtin trait: `encode`
                    // writes the receiver, `decode` of a type decodes into
                    // it; the others call no code a program writes.
                    Some(Selection::Native { tr }) => {
                        if let Some(ty) = &recv_ty
                            && let Some(tr) = [encode, decode].into_iter().find(|t| t.id == *tr)
                        {
                            show(tr, ty);
                        }
                    }
                    Some(Selection::Field | Selection::FieldCall) => {}
                    // `Type.method`, a method that makes a value: that
                    // impl's method.
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
                                    // (`Int.empty`: a type without
                                    // parameters is all of the value's.)
                                    let whole = Type::Generic(ty, Vec::new());
                                    let of = matches!(
                                        builtin_type_name(ty),
                                        Some("Int" | "Float" | "Bool" | "String" | "()")
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
                let known = at.as_ref().is_some_and(|at| {
                    crate::types::instance_of(&scheme.ty, &self.apply(at), &mut stands)
                });
                for (tr, ty) in shown {
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
                    shows(*tr, &here, &mut |target| {
                        if !edges[from].contains(&target) {
                            edges[from].push(target);
                        }
                    });
                }
            }
        }

        // The `let`s each `let` needs (`needs_of`): a plain value the
        // `let`s it reads, another every `let` its value reaches, through
        // the functions it mentions and the `let`s on the way (what a
        // `let` holds may be called through it). Those are found whole
        // only where a ring is searched for; elsewhere a `let` is known by
        // the nearest `let`s it reaches, which say the rest (`nearest`): a
        // chain of n `let`s that each read the one before, each needing
        // all before it, took time n squared (3,200 took 40 s).
        let lets: Vec<usize> = (0..nodes.len())
            .filter(|&n| nodes[n].let_decl.is_some())
            .collect();
        // (A node past a plain `let`, below, is numbered after the nodes.)
        let is_let = |n: usize| nodes.get(n).is_some_and(|node| node.let_decl.is_some());
        // What each `let` whose value is plain reads, and the way to each:
        // the `let` read.
        let mut reads_of: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut ways: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
        for &start in &lets {
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
                reads_of.insert(start, read);
            }
        }
        // The graphs below are the node graph itself, with a node past
        // each plain `let` (`past`, numbered after the nodes): what its
        // value mentions reaches, which a `let` that is not plain and
        // reaches the plain one needs, though the plain one needs only
        // what it reads. Over them each `let` reaches what it needs, and
        // each node is looked at once: a search from every `let` through
        // what it reaches (functions, plain `let`s) took time n squared,
        // in a chain of plain `let`s or of functions that many `let`s
        // reach.
        let plain: Vec<usize> = lets
            .iter()
            .copied()
            .filter(|n| reads_of.contains_key(n))
            .collect();
        let mut past_of: Vec<Option<usize>> = vec![None; nodes.len()];
        for (k, &n) in plain.iter().enumerate() {
            past_of[n] = Some(nodes.len() + k);
        }
        let reads = |n: usize| reads_of.get(&n).map_or(&[][..], Vec::as_slice);
        // Who reaches whom over `needs`: a plain `let` its reads, another
        // node what it mentions, a plain `let` it mentions together with
        // the node past it.
        let with_past = |targets: &[usize]| -> Vec<usize> {
            let mut out = Vec::with_capacity(targets.len());
            for &t in targets {
                out.push(t);
                out.extend(past_of[t]);
            }
            out
        };
        let mut near: Vec<Vec<usize>> = (0..nodes.len())
            .map(|n| match past_of[n] {
                Some(_) => reads(n).to_vec(),
                None => with_past(&edges[n]),
            })
            .collect();
        near.extend(plain.iter().map(|&n| with_past(&edges[n])));
        let mut whole: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut needs_of = |at: usize| -> Vec<usize> {
            (whole.entry(at))
                .or_insert_with(|| match reads_of.get(&at) {
                    Some(read) => read.clone(),
                    None => reached_lets(&edges, &is_let, at),
                })
                .clone()
        };
        // A `let` that reaches itself, through other `let`s or not, has
        // no place in the order: one error for each such ring, at the
        // first written of its `let`s, with the way round.
        let mut in_ring: std::collections::HashSet<usize> = std::collections::HashSet::new();
        // (Searched only from a `let` that reaches itself: one search
        // from each `let` of a chain of n, each needing all before it,
        // took time n cubed.)
        let on_a_ring = on_a_ring(&lets, &near);
        drop(near);
        for &start in &lets {
            if in_ring.contains(&start) || !on_a_ring.contains(&start) {
                continue;
            }
            // The shortest way back to `start` over the `let`s.
            let mut from: HashMap<usize, usize> = HashMap::new();
            let mut queue: std::collections::VecDeque<usize> =
                std::collections::VecDeque::from([start]);
            let mut back = false;
            'search: while let Some(at) = queue.pop_front() {
                for next in needs_of(at) {
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
                match ways.get(&(pair[0], pair[1])) {
                    Some(read) => way.extend(read),
                    None => way.extend(way_to(&edges, pair[0], pair[1])),
                }
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
        // What each `let` needs but the `let`s of a ring: the same graph,
        // in which a `let` of a ring is passed through (by a node of its
        // own, `through`, past one that is not plain; by the node past a
        // plain one) and needed by nobody, and a plain `let` needs what
        // it reads outside the rings. The order below is the one of all
        // that each needs, as each placed `let` has what it needs placed
        // before it; a node that is no `let` is passed as soon as what it
        // stands for is placed.
        let ring_lets: Vec<usize> = (lets.iter().copied())
            .filter(|n| in_ring.contains(n) && past_of[*n].is_none())
            .collect();
        let mut through_of: Vec<Option<usize>> = vec![None; nodes.len()];
        for (k, &n) in ring_lets.iter().enumerate() {
            through_of[n] = Some(nodes.len() + plain.len() + k);
        }
        let in_order = |targets: &[usize]| -> Vec<usize> {
            let mut out = Vec::with_capacity(targets.len());
            for &t in targets {
                match (in_ring.contains(&t), past_of[t]) {
                    (true, Some(past)) => out.push(past),
                    (true, None) => out.extend(through_of[t]),
                    (false, past) => {
                        out.push(t);
                        out.extend(past);
                    }
                }
            }
            out
        };
        let mut needs: Vec<Vec<usize>> = (0..nodes.len())
            .map(|n| match past_of[n] {
                Some(_) => (reads(n).iter())
                    .copied()
                    .filter(|t| !in_ring.contains(t))
                    .collect(),
                None => in_order(&edges[n]),
            })
            .collect();
        needs.extend(plain.iter().map(|&n| in_order(&edges[n])));
        needs.extend(ring_lets.iter().map(|&n| in_order(&edges[n])));
        // (The `let`s of a ring are reported; they keep their source
        // order among the others.)
        // Each `let` once the ones it needs are in: of those ready, the
        // first written. Nodes that are no `let` may need each other
        // round (functions that call each other): each strongly connected
        // part is one step, and no `let` outside a ring is in a part with
        // another node (it would reach itself).
        let place: HashMap<usize, usize> = lets.iter().enumerate().map(|(k, &n)| (n, k)).collect();
        let ids: Vec<usize> = (0..needs.len()).collect();
        let parts = strong_parts(&ids, &needs);
        let mut part_of: Vec<usize> = vec![0; needs.len()];
        for (p, part) in parts.iter().enumerate() {
            for &n in part {
                part_of[n] = p;
            }
        }
        let mut waits: Vec<usize> = vec![0; parts.len()];
        let mut needed_by: Vec<Vec<usize>> = vec![Vec::new(); parts.len()];
        // (The part each part's needs were last counted for.)
        let mut counted: Vec<usize> = vec![usize::MAX; parts.len()];
        for (p, part) in parts.iter().enumerate() {
            for &n in part {
                for &need in &needs[n] {
                    let q = part_of[need];
                    if q != p && counted[q] != p {
                        counted[q] = p;
                        needed_by[q].push(p);
                        waits[p] += 1;
                    }
                }
            }
        }
        // The `let` of a part, if it is one: by its place in the source.
        let let_of = |p: usize| match parts[p].as_slice() {
            [n] if is_let(*n) => Some(place[n]),
            _ => None,
        };
        let mut ready: std::collections::BinaryHeap<std::cmp::Reverse<(usize, usize)>> =
            std::collections::BinaryHeap::new();
        let mut passed: Vec<usize> = Vec::new();
        let is_ready =
            |p: usize,
             ready: &mut std::collections::BinaryHeap<std::cmp::Reverse<(usize, usize)>>,
             passed: &mut Vec<usize>| {
                match let_of(p) {
                    Some(k) => ready.push(std::cmp::Reverse((k, p))),
                    None => passed.push(p),
                }
            };
        for (p, _) in waits.iter().enumerate().filter(|(_, waits)| **waits == 0) {
            is_ready(p, &mut ready, &mut passed);
        }
        let mut order: Vec<Span> = Vec::with_capacity(lets.len());
        loop {
            let p = match passed.pop() {
                Some(p) => p,
                None => match ready.pop() {
                    Some(std::cmp::Reverse((k, p))) => {
                        order.push(nodes[lets[k]].span);
                        p
                    }
                    None => break,
                },
            };
            for &dependent in &needed_by[p] {
                waits[dependent] -= 1;
                if waits[dependent] == 0 {
                    is_ready(dependent, &mut ready, &mut passed);
                }
            }
        }
        debug_assert_eq!(order.len(), lets.len(), "the rings are out of the order");
        order
    }
}
