//! The order a module's top-level `let`s are initialised in.
//!
//! A top-level `let` is initialised after every top-level `let` its
//! initialiser can reach: the ones it names, and the ones the functions
//! and methods it can reach name. The reach is conservative: a function
//! that is mentioned counts as called, and a method call whose impl is
//! chosen where the code runs reaches every impl of the method in the
//! module. Where nothing orders two `let`s, the one written first runs
//! first. A `let` that reaches itself has no order: that is an error,
//! which names the way round.
//!
//! What a `let` holds may be called through it (a closure, a record of
//! functions), so what reaches a `let` reaches what the `let`'s value
//! mentions as well. A `let` whose value is a closure literal runs
//! nothing when it is initialised, so it needs nothing itself.

use super::order::{Mention, references_in_expr, references_in_pattern};
use super::*;
use crate::ast::Selection;
use crate::defs::{DefId, TraitId, TypeId};

/// A top-level `let`, a function, or a method of an impl or a trait.
struct Node<'a> {
    /// As a message names it: `limit`, `area`, `Shape.area`.
    name: String,
    /// The declaration of a `let`, as an index into the module's.
    let_decl: Option<usize>,
    span: Span,
    /// What it runs: an initialiser, a body (and what its parameters'
    /// patterns name).
    body: &'a Expr,
    params: &'a [Param],
}

impl TypeChecker {
    /// The module's top-level `let`s, by the span of each, in the order
    /// they are initialised in. Reports each `let` that reaches itself.
    pub(super) fn init_order(&mut self, decls: &[Decl]) -> Vec<Span> {
        let mut nodes: Vec<Node> = Vec::new();
        let mut by_name: HashMap<Symbol, usize> = HashMap::new();
        // The methods of each impl; of each trait, whatever the impl;
        // and each trait's default methods.
        let mut of_impl: HashMap<(TraitId, TypeId, Symbol), usize> = HashMap::new();
        let mut of_trait: HashMap<(TraitId, Symbol), Vec<usize>> = HashMap::new();
        let mut defaults: HashMap<(TraitId, Symbol), usize> = HashMap::new();
        for (i, decl) in decls.iter().enumerate() {
            match decl {
                Decl::Fn(f) => {
                    by_name.insert(f.name, nodes.len());
                    nodes.push(Node {
                        name: resolve(f.name),
                        let_decl: None,
                        span: f.span,
                        body: &f.body,
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
                        by_name.insert(*name, nodes.len());
                    }
                    nodes.push(Node {
                        name: match binders.first() {
                            Some(name) => resolve(*name),
                            None => "_".to_string(),
                        },
                        let_decl: Some(i),
                        span: *span,
                        body: value,
                        params: &[],
                    });
                }
                Decl::TraitImpl(ti) => {
                    let (Some(tr), Some(ty)) = (self.impl_trait(ti), self.impl_target(ti)) else {
                        continue;
                    };
                    for m in &ti.methods {
                        of_impl.insert((tr.id, ty.id, m.name), nodes.len());
                        of_trait
                            .entry((tr.id, m.name))
                            .or_default()
                            .push(nodes.len());
                        nodes.push(Node {
                            name: format!("{}.{}", ty.name, m.name),
                            let_decl: None,
                            span: m.span,
                            body: &m.body,
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
                            body: &m.body,
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
        if let Some(defs) = &self.defs {
            for id in defs.of_module(self.module) {
                let def = defs.get(*id);
                if matches!(
                    def.kind,
                    crate::defs::DefKind::Fn | crate::defs::DefKind::Let
                ) && let Some(&node) = by_name.get(&def.name)
                {
                    by_def.insert(*id, node);
                }
            }
        }

        // What each node mentions, in the order it does.
        let edges: Vec<Vec<usize>> = nodes
            .iter()
            .map(|node| {
                let mut targets: Vec<usize> = Vec::new();
                let mut note = |mention: Mention| {
                    let mut add = |target: usize| {
                        if !targets.contains(&target) {
                            targets.push(target);
                        }
                    };
                    if let Some(crate::defs::Res::Def(id)) = mention.res
                        && let Some(&target) = by_def.get(&id)
                    {
                        add(target);
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
                    match sel {
                        // One impl's method: the impl's own, or the
                        // trait's default.
                        Some(Selection::Impl { tr, ty }) => {
                            if let Some(&target) = of_impl
                                .get(&(*tr, *ty, *method))
                                .or_else(|| defaults.get(&(*tr, *method)))
                            {
                                add(target);
                            }
                        }
                        Some(Selection::Native { tr } | Selection::Dynamic { tr }) => {
                            for &target in of_trait.get(&(*tr, *method)).into_iter().flatten() {
                                add(target);
                            }
                        }
                        Some(Selection::Field | Selection::FieldCall) => {}
                        // `Type.method`: the methods of that name the
                        // access's trait has.
                        None => {
                            if recv.ty.is_none()
                                && let Some(crate::defs::Res::Def(id)) = res
                                && let Some(tr) = self.trait_key(*id)
                            {
                                for &target in of_trait.get(&(tr.id, *method)).into_iter().flatten()
                                {
                                    add(target);
                                }
                            }
                        }
                    }
                };
                for param in node.params {
                    references_in_pattern(&param.pattern, &mut note);
                }
                references_in_expr(node.body, &mut note);
                targets
            })
            .collect();

        // The `let`s each `let` reaches through functions and methods,
        // with the way to each (the functions between, then the `let`).
        let lets: Vec<usize> = (0..nodes.len())
            .filter(|&n| nodes[n].let_decl.is_some())
            .collect();
        let mut needs: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut ways: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
        let is_closure = |n: usize| matches!(nodes[n].body.kind, ExprKind::Lambda { .. });
        for &start in &lets {
            if is_closure(start) {
                needs.insert(start, Vec::new());
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
                    // the functions its value mentions are reached
                    // too.)
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
            let mut names = vec![nodes[start].name.clone()];
            for pair in ring.windows(2) {
                for &n in &ways[&(pair[0], pair[1])] {
                    names.push(nodes[n].name.clone());
                    // (Each `let` on the way is in the ring.)
                    if nodes[n].let_decl.is_some() {
                        in_ring.insert(n);
                    }
                }
            }
            in_ring.extend(ring);
            self.errors.push(
                Diagnostic::error(
                    Code::InitCycle,
                    nodes[start].span,
                    format!(
                        "the top-level `let` '{}' needs its own value to be initialised: {}",
                        nodes[start].name,
                        names.join(" -> ")
                    ),
                )
                .with_help(
                    "a top-level `let` is initialised after every top-level `let` its value can reach, through the functions it mentions: make one of them a function, or pass the value as an argument",
                ),
            );
        }
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
