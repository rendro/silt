//! The order a module's top-level definitions are checked in.
//!
//! A function or a `let` whose declaration does not give its whole type
//! gets it from its body, so the definitions it refers to are checked
//! first. The functions and `let`s of a module form a graph (an edge for
//! each reference, by the resolver's `Res::Def`); its strongly connected
//! components, callees first, are the groups that are inferred together.

use super::*;

/// A group of top-level definitions inferred together, as indices into
/// the module's declarations, in source order.
pub(super) struct Component {
    pub(super) members: Vec<usize>,
    /// Whether a member refers to a member: itself, or through the
    /// others.
    pub(super) cyclic: bool,
}

impl TypeChecker {
    /// The module's functions and top-level `let`s, grouped and ordered:
    /// each group comes after every group it refers to. A function with
    /// a complete signature (`sigs`) has its scheme before any body is
    /// checked, so a reference to it orders nothing; that keeps annotated
    /// polymorphic recursion, and annotated functions that call each
    /// other, out of each other's groups.
    pub(super) fn definition_order(
        &self,
        decls: &[Decl],
        sigs: &[Option<FnSig>],
    ) -> Vec<Component> {
        // The declaration of each definition the module's names resolve
        // to.
        let mut by_name: HashMap<Symbol, usize> = HashMap::new();
        let mut nodes: Vec<usize> = Vec::new();
        for (i, decl) in decls.iter().enumerate() {
            match decl {
                Decl::Fn(f) => {
                    by_name.insert(f.name, i);
                    nodes.push(i);
                }
                Decl::Let { pattern, .. } => {
                    for name in pattern.binder_names() {
                        by_name.insert(name, i);
                    }
                    nodes.push(i);
                }
                _ => {}
            }
        }
        let mut by_def: HashMap<crate::defs::DefId, usize> = HashMap::new();
        if let Some(defs) = &self.defs {
            for id in defs.of_module(self.module) {
                let def = defs.get(*id);
                if matches!(
                    def.kind,
                    crate::defs::DefKind::Fn | crate::defs::DefKind::Let
                ) && let Some(&i) = by_name.get(&def.name)
                {
                    by_def.insert(*id, i);
                }
            }
        }
        let has_scheme = |i: usize| sigs[i].as_ref().is_some_and(|sig| sig.complete);

        // The definitions each one refers to, in the order it does.
        let mut edges: Vec<Vec<usize>> = vec![Vec::new(); decls.len()];
        for &i in &nodes {
            let mut targets: Vec<usize> = Vec::new();
            let mut note = |mention: Mention| {
                if let Some(crate::defs::Res::Def(id)) = mention.res
                    && let Some(&j) = by_def.get(&id)
                    && !has_scheme(j)
                    && !targets.contains(&j)
                {
                    targets.push(j);
                }
            };
            match &decls[i] {
                Decl::Fn(f) => {
                    for param in &f.params {
                        references_in_pattern(&param.pattern, &mut note);
                    }
                    references_in_expr(&f.body, &mut note);
                }
                Decl::Let { value, .. } => references_in_expr(value, &mut note),
                _ => {}
            }
            edges[i] = targets;
        }
        strongly_connected(&nodes, &edges)
    }
}

/// The strongly connected components of the graph over `nodes`, each
/// after every component it has an edge to (Tarjan's algorithm, without
/// recursion: a module may be one long chain of calls).
fn strongly_connected(nodes: &[usize], edges: &[Vec<usize>]) -> Vec<Component> {
    const UNSEEN: usize = usize::MAX;
    let mut index = vec![UNSEEN; edges.len()];
    let mut low = vec![0usize; edges.len()];
    let mut on_stack = vec![false; edges.len()];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0;
    let mut components: Vec<Component> = Vec::new();
    for &root in nodes {
        if index[root] != UNSEEN {
            continue;
        }
        // Each entry: a node and how many of its edges are followed.
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(top) = work.len().checked_sub(1) {
            let (v, followed) = work[top];
            if followed == 0 {
                index[v] = next_index;
                low[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = edges[v].get(followed) {
                work[top].1 += 1;
                if index[w] == UNSEEN {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let mut members = Vec::new();
                loop {
                    let w = stack.pop().expect("the component's root is on the stack");
                    on_stack[w] = false;
                    members.push(w);
                    if w == v {
                        break;
                    }
                }
                members.sort_unstable();
                let cyclic = members.len() > 1 || edges[v].contains(&v);
                components.push(Component { members, cyclic });
            }
        }
    }
    components
}

/// What a definition's body mentions: what a name resolves to, and,
/// for an expression, the expression (a field access says what method
/// it calls).
pub(super) struct Mention<'a> {
    pub(super) res: Option<crate::defs::Res>,
    pub(super) expr: Option<&'a Expr>,
}

/// Call `note` with each expression in `expr` and what each name written
/// in it resolves to.
pub(super) fn references_in_expr<'a>(expr: &'a Expr, note: &mut impl FnMut(Mention<'a>)) {
    note(Mention {
        res: expr.res,
        expr: Some(expr),
    });
    let _ = expr.try_for_each_node(&mut |child| {
        match child {
            Child::Expr(e) => references_in_expr(e, note),
            Child::Pattern(p) => references_in_pattern(p, note),
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
}

/// Call `note` with what each name written in `pattern` resolves to: a
/// pinned name (`^limit`) may be a top-level `let`.
pub(super) fn references_in_pattern<'a>(pattern: &'a Pattern, note: &mut impl FnMut(Mention<'a>)) {
    note(Mention {
        res: pattern.res,
        expr: None,
    });
    pattern.for_each_child(|p| references_in_pattern(p, note));
}
