//! The recursion check: what a function that calls itself, or a group
//! of functions that call each other, costs in stack.
//!
//! - A group whose every call of a member is a tail call runs in
//!   **constant stack**.
//! - Otherwise each member must descend on one parameter: every call of
//!   a member that keeps a frame passes a strict part of the caller's
//!   parameter in the callee's place, and every tail call passes the
//!   parameter or a part of it. Its **depth follows the data**.
//! - Anything else is refused (`E0345`), unless the function is written
//!   `deep fn`, which turns the check off for its calls. A `deep` the
//!   check does not need is a warning (`W0301`).
//! - A group of tail calls whose member gives `()` writes where it ends:
//!   an ending that is a call giving `()`, or a block that ends in a
//!   `let`, is refused (`E0346`).
//! - A `let`-bound `_name` in scope at a tail call that ends its block
//!   is let go at that call: a warning (`W0302`).
//!
//! The call graph has a node for each top-level function (and top-level
//! `let` of a closure) and for each `let`-bound closure; a name of a
//! node, called or not, is an edge from the innermost node it is written
//! in. Calls through a trait and through a function value are not seen:
//! the check catches the accidents of direct calls, it proves nothing of
//! every program. It runs on a module that type-checked.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::defs::{DefKind, DefTable, Res};
use crate::diagnostic::{Code, Diagnostic};
use crate::intern::{Symbol, resolve};
use crate::source::{FileId, Span};
use crate::types::Type;

/// The calls of the `list` module whose function is called on the
/// elements of the list given first: a call of a member from such a
/// closure, on the element, descends.
const ELEMENT_CALLS: &[&str] = &[
    "each", "map", "flat_map", "fold", "filter", "find", "any", "all",
];

/// How many choices of descending parameters are tried for a group.
const MAX_CHOICES: usize = 20_000;

/// What the check says of a function, as the editor shows it on its
/// name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Every call between the functions of its group is a tail call.
    ConstantStack,
    /// It descends on the parameter named.
    FollowsData(String),
    /// `deep fn`: not checked.
    Deep,
    /// A method that calls a method of its own trait: calls through a
    /// trait are not checked.
    TraitCall,
}

impl Verdict {
    /// The verdict as the editor writes it behind the function's name.
    #[cfg_attr(not(feature = "lsp"), allow(dead_code))]
    pub(crate) fn text(&self) -> String {
        match self {
            Verdict::ConstantStack => "constant stack".to_string(),
            Verdict::FollowsData(param) => format!("depth follows the data (descends on {param})"),
            Verdict::Deep => "deep (not checked)".to_string(),
            Verdict::TraitCall => "not checked: trait call".to_string(),
        }
    }
}

/// What the check found in a module.
#[derive(Default)]
pub(crate) struct Shape {
    /// The errors and warnings, in source order.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// The verdict of each function that calls itself (or a function
    /// that calls it back), by the span of its name; a refused one has
    /// none (its error says why).
    #[cfg_attr(not(feature = "lsp"), allow(dead_code))]
    pub(crate) verdicts: Vec<(Span, Symbol, Verdict)>,
}

/// Check `program`, whose text is `text`. `defs` tells a call of a
/// function of silt's own from one of the program's (only the warning of
/// `W0302` needs it).
pub(crate) fn check(program: &Program, text: &str, defs: &DefTable) -> Shape {
    let mut walk = Walk::new(program, Some(text), Some(defs));
    walk.run(program);
    walk.finish(true)
}

/// What the check of a REPL entry sees of the entries before it: a
/// function defined again with its type takes the old one's place for
/// every caller, so a group can join functions of several entries.
#[cfg(feature = "repl")]
pub(crate) struct Earlier<'a> {
    /// The functions the session binds that the entry does not bind
    /// again, each with the text of its entry and its global slot.
    pub(crate) functions: Vec<(&'a FnDecl, &'a str, u16)>,
    /// The global slot of a definition of an earlier entry.
    pub(crate) slot_of: &'a dyn Fn(crate::defs::DefId) -> Option<u16>,
    /// The slots the entry's functions take, by name.
    pub(crate) takes: HashMap<Symbol, u16>,
}

/// Check the REPL entry `program` as `check` does, with the functions
/// of the entries before it: a group that a function of the entry is
/// in is checked whole; one of earlier functions only was checked with
/// its entry.
#[cfg(feature = "repl")]
pub(crate) fn check_entry<'a>(
    program: &'a Program,
    text: &'a str,
    defs: &DefTable,
    earlier: &Earlier<'a>,
) -> Shape {
    let mut walk = Walk::new(program, Some(text), Some(defs));
    walk.slot_of = Some(earlier.slot_of);
    for &(f, text, slot) in &earlier.functions {
        walk.earlier_texts.insert(f.name_span.file, text);
        walk.slot_node.insert(slot, walk.nodes.len());
        walk.nodes.push(Node {
            name: f.name,
            name_span: f.name_span,
            deep: f.deep,
            top: true,
            value: false,
            params: &f.params,
            body: &f.body,
            earlier: true,
        });
    }
    for (name, slot) in &earlier.takes {
        if let Some(&n) = walk.top.get(name) {
            walk.slot_node.insert(*slot, n);
        }
    }
    walk.run(program);
    walk.finish(true)
}

/// The verdicts of `program`'s recursive functions, for the editor.
#[cfg_attr(not(feature = "lsp"), allow(dead_code))]
pub(crate) fn verdicts(program: &Program) -> Vec<(Span, Symbol, Verdict)> {
    let mut walk = Walk::new(program, None, None);
    walk.run(program);
    walk.finish(false).verdicts
}

// ── The call graph ──────────────────────────────────────────────────

/// A node of the call graph: a top-level function or a `let`-bound
/// closure.
struct Node<'a> {
    name: Symbol,
    name_span: Span,
    /// The `deep` of `deep fn`.
    deep: Option<Span>,
    /// A top-level function (or `let`).
    top: bool,
    /// A top-level `let` of a value that is no closure (`let g = f`):
    /// what it names is named by it.
    value: bool,
    params: &'a [Param],
    body: &'a Expr,
    /// A function of an earlier REPL entry (or a closure in one).
    earlier: bool,
}

/// What a local name is bound to, as far as the check cares.
#[derive(Clone, Copy, Debug)]
enum Origin {
    /// The parameter `.1` of node `.0`.
    Param(usize, usize),
    /// A strict part of the parameter `.1` of node `.0`.
    Part(usize, usize),
    /// The closure of node `.0`.
    Node(usize),
    /// The parameter `.1` of node `.0` under a name a pattern binds to
    /// all of it (the `all` of `match xs { all -> ... }`): what a note
    /// says it is, though it counts as no part of the parameter.
    Whole(usize, usize),
    Other,
}

impl Origin {
    /// What a name is that is `self` in one alternative of an
    /// or-pattern and `other` in another: a strict part only when it is
    /// one in both, the parameter or a part of it when it is that in
    /// both.
    fn or(self, other: Origin) -> Origin {
        match (self, other) {
            (Origin::Part(n, p), Origin::Part(m, q)) if (n, p) == (m, q) => self,
            (
                Origin::Part(n, p) | Origin::Param(n, p),
                Origin::Part(m, q) | Origin::Param(m, q),
            ) if (n, p) == (m, q) => Origin::Param(n, p),
            (Origin::Whole(n, p), Origin::Whole(m, q)) if (n, p) == (m, q) => self,
            _ => Origin::Other,
        }
    }
}

/// What an argument is of the parameters of the node it is written in:
/// the parameter `param` itself, or a strict part of it.
#[derive(Clone, Copy, Debug)]
struct Measure {
    param: usize,
    strict: bool,
}

/// A place where one node names another.
struct Site {
    from: usize,
    to: usize,
    /// The call (or the name, for a value).
    span: Span,
    kind: SiteKind,
    /// The spans of the call's arguments.
    args: Vec<Span>,
    /// For each argument, the parameter of the caller it is made from
    /// (the first one named in it), if any: what an error names.
    made_from: Vec<Option<usize>>,
    /// For each argument, the parameter of the caller it is all of under
    /// a name a pattern binds (`Origin::Whole`), if it is that.
    whole: Vec<Option<usize>>,
    /// The branch of the caller the site is in (see `Branch`).
    branch: usize,
    /// For a member passed as a value to a call: the callee's span.
    passed_to: Option<Span>,
    /// Where the member is named: an alias's name, if it is one.
    named_at: Span,
}

enum SiteKind {
    /// A call in the node's own body, a tail call or not, with what each
    /// argument is.
    Call {
        tail: bool,
        args: Vec<Option<Measure>>,
    },
    /// A call inside a closure that is not a node; `coll` is what the
    /// list is of a closure handed to one of `ELEMENT_CALLS` (none for
    /// any other closure).
    Closure {
        coll: Option<Measure>,
        element: bool,
        args: Vec<Option<Measure>>,
    },
    /// The name passed rather than called; `elem`: as the function of
    /// one of `ELEMENT_CALLS` over a list of what `.0` says, which
    /// passes the element as the argument `.1`.
    Value { elem: Option<(Measure, usize)> },
}

impl Site {
    fn is_tail_call(&self) -> bool {
        matches!(self.kind, SiteKind::Call { tail: true, .. })
    }
}

struct Walk<'a, 'd> {
    text: Option<&'a str>,
    /// The texts of the earlier REPL entries whose functions are nodes.
    earlier_texts: HashMap<FileId, &'a str>,
    /// The global slot of a definition of an earlier REPL entry.
    slot_of: Option<&'a dyn Fn(crate::defs::DefId) -> Option<u16>>,
    /// The node of each global slot the nodes of a REPL entry's check
    /// stand for: an earlier entry's function, or the entry's function
    /// that takes its slot.
    slot_node: HashMap<u16, usize>,
    defs: Option<&'d DefTable>,
    nodes: Vec<Node<'a>>,
    /// The top-level nodes by name.
    top: HashMap<Symbol, usize>,
    /// The top-level plain aliases (`let g = f`) by name: the span of
    /// the alias's name and the node it is, through any chain of them.
    top_alias: HashMap<Symbol, (Span, usize)>,
    /// The local names in scope, innermost last.
    scopes: Vec<(Symbol, Origin)>,
    /// The innermost node the walk is in.
    node: Option<usize>,
    /// Inside a closure that is not a node: what the list is of a closure
    /// handed to one of `ELEMENT_CALLS` (`Some(None)` for any other).
    closure: Option<(Option<Measure>, bool)>,
    sites: Vec<Site>,
    /// The `let`-bound `_names` the current function holds on its way to
    /// a tail call.
    held: Vec<(Symbol, Span)>,
    /// The warnings of `W0302` without their help, which waits for the
    /// groups: the caller's node, the callee's node and what to keep.
    let_go: Vec<LetGo>,
    diagnostics: Vec<Diagnostic>,
    /// The methods that call a method of their own trait.
    trait_calls: Vec<(Span, Symbol)>,
    /// The branch the walk is in, and every branch so far.
    branch: usize,
    branches: Vec<Branch>,
    /// How many matches the walk has entered.
    matches: usize,
}

/// A part of a node's body that runs as a whole: its body, an arm of a
/// match, the else block of a `when`, the body of a closure.
struct Branch {
    /// The branch it is written in.
    parent: usize,
    fork: Fork,
    /// Where it ends, when it leaves the function there (an else block,
    /// an arm that ends in `return`): what is written from there on does
    /// not run with it.
    leaves: Option<u32>,
}

/// How a branch runs with the branch it is written in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fork {
    /// The body of a node or of a closure: nothing around it runs with it.
    Body,
    /// An arm of the match numbered `.0`: only one of its arms runs.
    Arm(usize),
    /// The else block of a `when`, which leaves the function.
    Else,
}

impl<'a, 'd> Walk<'a, 'd> {
    fn new(program: &'a Program, text: Option<&'a str>, defs: Option<&'d DefTable>) -> Self {
        let mut nodes = Vec::new();
        let mut top = HashMap::new();
        let mut aliases: Vec<(Symbol, Span, Symbol)> = Vec::new();
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) if !f.is_recovery_stub && !f.is_signature_only => {
                    top.insert(f.name, nodes.len());
                    nodes.push(Node {
                        name: f.name,
                        name_span: f.name_span,
                        deep: f.deep,
                        top: true,
                        value: false,
                        params: &f.params,
                        body: &f.body,
                        earlier: false,
                    });
                }
                // A top-level `let` is a node whatever its value: `let g = f`
                // names `f` as `g` does, and naming a function must not
                // switch the check off.
                Decl::Let { pattern, value, .. } => {
                    if let PatternKind::Ident(name) = &pattern.kind {
                        // A plain alias (`let g = f`, `let h = g`) is what it
                        // names: resolved below, no node of its own.
                        if let ExprKind::Ident(target) = &unwrapped(value).kind {
                            aliases.push((*name, pattern.span, *target));
                            continue;
                        }
                        let (params, body, is_value) = match &value.kind {
                            ExprKind::Lambda { params, body } => (&params[..], &**body, false),
                            _ => (&[][..], value, true),
                        };
                        top.insert(*name, nodes.len());
                        nodes.push(Node {
                            name: *name,
                            name_span: pattern.span,
                            deep: None,
                            top: true,
                            value: is_value,
                            params,
                            body,
                            earlier: false,
                        });
                    }
                }
                _ => {}
            }
        }
        // Each alias is what the end of its chain is (a cycle of aliases
        // names no node). Each is resolved once: a walk stops at an alias
        // already resolved, and what it finds is written on every alias
        // it passed, so a long chain of them costs its length, not its
        // length squared.
        let target_of: HashMap<Symbol, Symbol> = aliases
            .iter()
            .map(|&(name, _, target)| (name, target))
            .collect();
        let mut resolved: HashMap<Symbol, Option<usize>> = HashMap::new();
        for &(name, _, _) in &aliases {
            let mut path = Vec::new();
            let mut on_path = std::collections::HashSet::new();
            let mut at = name;
            let end = loop {
                if let Some(&known) = resolved.get(&at) {
                    break known;
                }
                if !on_path.insert(at) {
                    break None;
                }
                match target_of.get(&at) {
                    Some(&next) => {
                        path.push(at);
                        at = next;
                    }
                    None => break top.get(&at).copied(),
                }
            };
            for passed in path {
                resolved.insert(passed, end);
            }
        }
        let top_alias: HashMap<Symbol, (Span, usize)> = (aliases.iter())
            .filter_map(|&(name, span, _)| Some((name, (span, resolved.get(&name).copied()??))))
            .collect();
        Walk {
            text,
            earlier_texts: HashMap::new(),
            slot_of: None,
            slot_node: HashMap::new(),
            defs,
            nodes,
            top,
            top_alias,
            scopes: Vec::new(),
            node: None,
            closure: None,
            sites: Vec::new(),
            held: Vec::new(),
            let_go: Vec::new(),
            diagnostics: Vec::new(),
            trait_calls: Vec::new(),
            branch: 0,
            branches: vec![Branch {
                parent: 0,
                fork: Fork::Body,
                leaves: None,
            }],
            matches: 0,
        }
    }

    /// Enter a new branch; gives the one to go back to.
    fn new_branch(&mut self, fork: Fork, leaves: Option<u32>) -> usize {
        let parent = self.branch;
        self.branches.push(Branch {
            parent,
            fork,
            leaves,
        });
        std::mem::replace(&mut self.branch, self.branches.len() - 1)
    }

    /// Whether the sites `a` and `b` of one node can both run: not in
    /// two arms of one match, and not one in a branch that leaves the
    /// function (an else block, an arm that ends in `return`) and the
    /// other written after it.
    fn run_together(&self, a: &Site, b: &Site) -> bool {
        // (Each branch up to the body it is in.)
        let up = |mut x: usize| {
            let mut path = vec![x];
            while self.branches[x].fork != Fork::Body {
                x = self.branches[x].parent;
                path.push(x);
            }
            path
        };
        let (pa, pb) = (up(a.branch), up(b.branch));
        let Some((ia, ib)) =
            (pa.iter().enumerate()).find_map(|(i, x)| Some((i, pb.iter().position(|y| y == x)?)))
        else {
            return false;
        };
        let fork = |x: usize| self.branches[x].fork;
        if ia > 0
            && ib > 0
            && let (Fork::Arm(m), Fork::Arm(n)) = (fork(pa[ia - 1]), fork(pb[ib - 1]))
            && m == n
        {
            return false;
        }
        let left_before = |path: &[usize], other: &Site| {
            (path.iter()).any(|&x| {
                self.branches[x]
                    .leaves
                    .is_some_and(|end| other.span.start >= end)
            })
        };
        !left_before(&pa[..ia], b) && !left_before(&pb[..ib], a)
    }

    fn run(&mut self, program: &'a Program) {
        for n in 0..self.nodes.len() {
            self.function(n);
        }
        for decl in &program.decls {
            match decl {
                Decl::Let { pattern, value, .. }
                    if !matches!(pattern.kind, PatternKind::Ident(_)) =>
                {
                    self.expr(value, false);
                }
                Decl::TraitImpl(ti) => {
                    let trait_id = match ti.trait_res {
                        Some(Res::Def(id)) => Some(id),
                        _ => None,
                    };
                    for m in &ti.methods {
                        self.method(m, trait_id, ti.trait_name);
                    }
                }
                Decl::Trait(t) => {
                    for m in t.methods.iter().filter(|m| !m.is_signature_only) {
                        self.method(m, None, t.name);
                    }
                }
                _ => {}
            }
        }
    }

    /// Walk the body of node `n`.
    fn function(&mut self, n: usize) {
        let outer_node = self.node.replace(n);
        let outer_closure = self.closure.take();
        let outer_held = std::mem::take(&mut self.held);
        let outer_branch = self.new_branch(Fork::Body, None);
        let depth = self.scopes.len();
        let (params, body) = (self.nodes[n].params, self.nodes[n].body);
        for (i, param) in params.iter().enumerate() {
            match &param.pattern.kind {
                PatternKind::Ident(name) => self.scopes.push((*name, Origin::Param(n, i))),
                _ => self.bind(
                    &param.pattern,
                    Some(Measure {
                        param: i,
                        strict: false,
                    }),
                    false,
                ),
            }
        }
        self.expr(body, true);
        self.scopes.truncate(depth);
        self.node = outer_node;
        self.closure = outer_closure;
        self.held = outer_held;
        self.branch = outer_branch;
    }

    /// Walk a method of an impl of the trait `trait_id` (or of the
    /// trait named `trait_name`): it is no node, and a call of its own
    /// trait in it is said on its name.
    fn method(&mut self, m: &'a FnDecl, trait_id: Option<crate::defs::DefId>, trait_name: Symbol) {
        let calls_own_trait = m.body.any_expr(&mut |e| {
            let ExprKind::FieldAccess(receiver, _, _) = &e.kind else {
                return false;
            };
            let by_selection = match e.sel {
                Some(
                    Selection::Impl { tr, .. }
                    | Selection::Dynamic { tr, .. }
                    | Selection::Native { tr },
                ) => trait_id == Some(tr.0),
                _ => false,
            };
            let by_name = matches!(&receiver.kind, ExprKind::Ident(name) if *name == trait_name);
            by_selection || by_name
        }) || decodes_own_type(m, trait_id);
        if calls_own_trait {
            self.trait_calls.push((m.name_span, m.name));
        }
        let outer_node = self.node.take();
        let outer_held = std::mem::take(&mut self.held);
        let depth = self.scopes.len();
        for param in &m.params {
            self.bind(&param.pattern, None, false);
        }
        self.expr(&m.body, true);
        self.scopes.truncate(depth);
        self.node = outer_node;
        self.held = outer_held;
    }

    // ── Names ───────────────────────────────────────────────────────

    fn lookup(&self, name: Symbol) -> Option<Origin> {
        (self.scopes.iter().rev())
            .find(|(bound, _)| *bound == name)
            .map(|(_, origin)| *origin)
    }

    /// The node `e` names, if it is a name of one.
    fn node_named(&self, e: &Expr) -> Option<usize> {
        let ExprKind::Ident(name) = &e.kind else {
            return None;
        };
        match e.res {
            Some(Res::Local) => match self.lookup(*name) {
                Some(Origin::Node(n)) => Some(n),
                _ => None,
            },
            // (The definition the name names is the node's when it is
            // declared where the node's name is: in a REPL entry a name
            // can still name an earlier entry's definition of it.)
            // (A definition of an earlier REPL entry is the node of its
            // global slot: its function, or the one that took its place.)
            Some(Res::Def(id))
                if let Some(slot_of) = self.slot_of
                    && let Some(&n) = slot_of(id).and_then(|slot| self.slot_node.get(&slot)) =>
            {
                Some(n)
            }
            Some(Res::Def(id)) => {
                let declared_at =
                    |span: Span| self.defs.is_none_or(|defs| defs.get(id).span == span);
                match (self.top.get(name), self.top_alias.get(name)) {
                    (Some(&n), _) => declared_at(self.nodes[n].name_span).then_some(n),
                    (None, Some(&(span, n))) => declared_at(span).then_some(n),
                    (None, None) => None,
                }
            }
            _ => None,
        }
    }

    /// What `e` is of the parameters of the node the walk is in.
    fn measure(&self, e: &Expr) -> Option<Measure> {
        let node = self.node?;
        match &e.kind {
            ExprKind::Ident(name) if e.res == Some(Res::Local) => match self.lookup(*name)? {
                Origin::Param(n, param) if n == node => Some(Measure {
                    param,
                    strict: false,
                }),
                Origin::Part(n, param) if n == node => Some(Measure {
                    param,
                    strict: true,
                }),
                _ => None,
            },
            // A field of the parameter or of a part is a part.
            ExprKind::FieldAccess(inner, _, _) if e.sel == Some(Selection::Field) => {
                self.measure(inner).map(|m| Measure { strict: true, ..m })
            }
            ExprKind::Ascription(inner, _) => self.measure(inner),
            _ => None,
        }
    }

    /// Bind the names of `pattern`, matched against a value that is
    /// `rel` (`below`: strictly inside a pattern that takes it apart).
    fn bind(&mut self, pattern: &Pattern, rel: Option<Measure>, below: bool) {
        let node = self.node;
        let origin = |strict: bool| match (rel, node) {
            (Some(m), Some(n)) if m.strict || strict => Origin::Part(n, m.param),
            (Some(m), Some(n)) => Origin::Whole(n, m.param),
            _ => Origin::Other,
        };
        match &pattern.kind {
            PatternKind::Ident(name) => self.scopes.push((*name, origin(below))),
            PatternKind::Tuple(items) => {
                for item in items {
                    self.bind(item, rel, true);
                }
            }
            PatternKind::Constructor { args, .. } => {
                for arg in args {
                    self.bind(arg, rel, true);
                }
            }
            PatternKind::Record { fields, .. } => {
                for (name, _, sub) in fields {
                    match sub {
                        Some(sub) => self.bind(sub, rel, true),
                        None => self.scopes.push((*name, origin(true))),
                    }
                }
            }
            PatternKind::List(items, rest) => {
                for item in items {
                    self.bind(item, rel, true);
                }
                if let Some(rest) = rest {
                    self.bind(rest, rel, true);
                }
            }
            // (Every alternative binds the same names.)
            // Every alternative binds the same names, and a name is what
            // it is in all of them: the `y` of `Node(y) | y` may be the
            // value itself.
            PatternKind::Or(alternatives) => {
                let start = self.scopes.len();
                let mut merged: Option<Vec<(Symbol, Origin)>> = None;
                for alternative in alternatives {
                    self.bind(alternative, rel, below);
                    let bound: Vec<(Symbol, Origin)> = self.scopes.drain(start..).collect();
                    merged = Some(match merged {
                        None => bound,
                        Some(first) => (first.into_iter())
                            .map(|(name, origin)| {
                                let other = bound.iter().rev().find(|(n, _)| *n == name);
                                (name, other.map_or(Origin::Other, |(_, o)| origin.or(*o)))
                            })
                            .collect(),
                    });
                }
                self.scopes.extend(merged.unwrap_or_default());
            }
            PatternKind::Map(entries) => {
                for (_, value) in entries {
                    self.bind(value, rel, true);
                }
            }
            // The name is the value itself, as a name pattern in its
            // place would be.
            PatternKind::As { pattern, name, .. } => {
                self.bind(pattern, rel, below);
                self.scopes.push((*name, origin(below)));
            }
            PatternKind::Wildcard
            | PatternKind::Int(_)
            | PatternKind::Float(_)
            | PatternKind::Bool(_)
            | PatternKind::StringLit(_)
            | PatternKind::Range(..)
            | PatternKind::FloatRange(..)
            | PatternKind::Pin(_) => {}
        }
    }

    /// Bind the names of `pattern` matched against `value`: a tuple
    /// written out is matched an element at a time.
    fn bind_matched(&mut self, pattern: &Pattern, value: Option<&Expr>) {
        // (`(a, b) as both`: the names behind `as` are the tuple, which
        // is built here and is no part of a parameter.)
        let mut tuple = pattern;
        let mut whole = Vec::new();
        while let PatternKind::As { pattern, name, .. } = &tuple.kind {
            whole.push(*name);
            tuple = pattern;
        }
        if let (
            Some(Expr {
                kind: ExprKind::Tuple(values),
                ..
            }),
            PatternKind::Tuple(items),
        ) = (value, &tuple.kind)
            && values.len() == items.len()
        {
            let measures: Vec<_> = values.iter().map(|v| self.measure(v)).collect();
            for (item, m) in items.iter().zip(measures) {
                self.bind(item, m, false);
            }
            for name in whole {
                self.scopes.push((name, Origin::Other));
            }
            return;
        }
        let m = value.and_then(|v| self.measure(v));
        self.bind(pattern, m, false);
    }

    // ── Expressions ─────────────────────────────────────────────────

    /// Walk `e`; `tail`: it is in tail position of the function (or
    /// closure) it is written in.
    fn expr(&mut self, e: &'a Expr, tail: bool) {
        match &e.kind {
            ExprKind::Ident(_) => {
                if let Some(to) = self.node_named(e) {
                    self.site(
                        to,
                        e.span,
                        SiteKind::Value { elem: None },
                        &[],
                        None,
                        e.span,
                    );
                }
            }
            ExprKind::Call(callee, args) => {
                let args: Vec<&'a Expr> = args.iter().collect();
                self.call(e, callee, args, tail);
            }
            ExprKind::Pipe(left, right) => match &right.kind {
                ExprKind::Call(callee, args) => {
                    let args: Vec<&'a Expr> = std::iter::once(&**left).chain(args).collect();
                    self.call(e, callee, args, tail);
                }
                _ => self.call(e, right, vec![left], tail),
            },
            ExprKind::Lambda { params, body } => self.lambda(params, body, None),
            ExprKind::Block(stmts) => self.block(stmts, tail),
            ExprKind::Match { expr, arms } => {
                if let Some(scrutinee) = expr {
                    self.expr(scrutinee, false);
                }
                self.matches += 1;
                let this = self.matches;
                for arm in arms {
                    let depth = self.scopes.len();
                    let leaves = ends_in_return(&arm.body).then_some(arm.body.span.end);
                    let outer_branch = self.new_branch(Fork::Arm(this), leaves);
                    self.bind_matched(&arm.pattern, expr.as_deref());
                    if let Some(guard) = &arm.guard {
                        self.expr(guard, false);
                    }
                    self.expr(&arm.body, tail);
                    self.scopes.truncate(depth);
                    self.branch = outer_branch;
                }
            }
            ExprKind::Return(Some(value)) => self.expr(value, true),
            _ => e.for_each_child(|child| self.expr(child, false)),
        }
    }

    fn block(&mut self, stmts: &'a [Stmt], tail: bool) {
        let depth = self.scopes.len();
        let held = self.held.len();
        let last = stmts.len().saturating_sub(1);
        for (i, stmt) in stmts.iter().enumerate() {
            match stmt {
                Stmt::Let { pattern, value, .. } => {
                    match (&pattern.kind, &value.kind) {
                        // A closure bound by a `let` is a node; it may
                        // call itself by the name.
                        (PatternKind::Ident(name), ExprKind::Lambda { params, body }) => {
                            let n = self.nodes.len();
                            let earlier = self.node.is_some_and(|m| self.nodes[m].earlier);
                            self.nodes.push(Node {
                                name: *name,
                                name_span: pattern.span,
                                deep: None,
                                top: false,
                                value: false,
                                params,
                                body,
                                earlier,
                            });
                            self.scopes.push((*name, Origin::Node(n)));
                            self.function(n);
                        }
                        // A plain alias of a function (`let h = g`) is that
                        // function for every mention.
                        (PatternKind::Ident(name), _)
                            if self.node_named(unwrapped(value)).is_some() =>
                        {
                            let n = self.node_named(unwrapped(value)).expect("a node");
                            self.scopes.push((*name, Origin::Node(n)));
                        }
                        // A copy of the parameter under another name is the
                        // parameter (a copy of a part is a part, by `bind`).
                        (PatternKind::Ident(name), ExprKind::Ident(_))
                            if matches!(
                                self.measure(value),
                                Some(Measure { strict: false, .. })
                            ) =>
                        {
                            let (Some(n), Some(m)) = (self.node, self.measure(value)) else {
                                unreachable!("a measure is of the node the walk is in")
                            };
                            self.scopes.push((*name, Origin::Param(n, m.param)));
                        }
                        _ => {
                            self.expr(value, false);
                            self.bind_matched(pattern, Some(value));
                        }
                    }
                    if tail {
                        for (name, span) in pattern.binders() {
                            let text = resolve(name);
                            if text.starts_with('_') && text.len() > 1 {
                                self.held.push((name, span));
                            }
                        }
                    }
                }
                Stmt::When {
                    pattern,
                    expr,
                    else_body,
                } => {
                    self.expr(expr, false);
                    // (The else block is a branch of its own: it leaves the
                    // function.)
                    let outer_branch = self.new_branch(Fork::Else, Some(else_body.span.end));
                    self.expr(else_body, false);
                    self.branch = outer_branch;
                    self.bind_matched(pattern, Some(expr));
                }
                Stmt::WhenBool {
                    condition,
                    else_body,
                } => {
                    self.expr(condition, false);
                    let outer_branch = self.new_branch(Fork::Else, Some(else_body.span.end));
                    self.expr(else_body, false);
                    self.branch = outer_branch;
                }
                Stmt::Expr(e) => self.expr(e, tail && i == last),
            }
        }
        self.scopes.truncate(depth);
        self.held.truncate(held);
    }

    /// A closure that is not a node; `element`: handed to one of
    /// `ELEMENT_CALLS` over a list of what `.0` says, whose element is
    /// its parameter `.1`.
    fn lambda(
        &mut self,
        params: &'a [Param],
        body: &'a Expr,
        element: Option<(Option<Measure>, usize)>,
    ) {
        let coll = element.and_then(|(coll, _)| coll);
        let outer_closure = self.closure.replace((coll, element.is_some()));
        let outer_held = std::mem::take(&mut self.held);
        let outer_branch = self.new_branch(Fork::Body, None);
        let depth = self.scopes.len();
        for (i, param) in params.iter().enumerate() {
            match (coll, element) {
                (Some(m), Some((_, at))) if at == i => {
                    self.bind(&param.pattern, Some(Measure { strict: true, ..m }), true)
                }
                _ => self.bind(&param.pattern, None, false),
            }
        }
        self.expr(body, true);
        self.scopes.truncate(depth);
        self.closure = outer_closure;
        self.held = outer_held;
        self.branch = outer_branch;
    }

    /// The call `e` of `callee` with `args` (a pipe's left side first).
    fn call(&mut self, e: &'a Expr, callee: &'a Expr, args: Vec<&'a Expr>, tail: bool) {
        if tail {
            self.let_go(e, callee);
        }
        if let Some(to) = self.node_named(callee) {
            let measures: Vec<Option<Measure>> = args.iter().map(|a| self.measure(a)).collect();
            let kind = match self.closure {
                None => SiteKind::Call {
                    tail,
                    args: measures,
                },
                Some((coll, element)) => SiteKind::Closure {
                    coll,
                    element,
                    args: measures,
                },
            };
            self.site(to, e.span, kind, &args, None, callee.span);
            self.args(callee, &args);
            return;
        }
        if let Some(at) = element_call(callee)
            && args.len() == if at == 1 { 3 } else { 2 }
        {
            let (fun, front) = args.split_last().expect("two arguments or three");
            for arg in front {
                self.expr(arg, false);
            }
            let coll = self.measure(args[0]);
            match &fun.kind {
                ExprKind::Lambda { params, body } => self.lambda(params, body, Some((coll, at))),
                _ => match self.node_named(fun) {
                    Some(to) => {
                        let elem = coll.map(|m| (m, at));
                        let kind = SiteKind::Value { elem };
                        self.site(to, fun.span, kind, &[], Some(callee.span), fun.span);
                    }
                    None => self.expr(fun, false),
                },
            }
            return;
        }
        self.expr(callee, false);
        self.args(callee, &args);
    }

    /// Walk the arguments of a call of `callee`: a node passed is a value
    /// passed to it.
    fn args(&mut self, callee: &'a Expr, args: &[&'a Expr]) {
        for arg in args {
            match self.node_named(arg) {
                Some(to) => {
                    let kind = SiteKind::Value { elem: None };
                    self.site(to, arg.span, kind, &[], Some(callee.span), arg.span);
                }
                None => self.expr(arg, false),
            }
        }
    }

    fn site(
        &mut self,
        to: usize,
        span: Span,
        kind: SiteKind,
        args: &[&Expr],
        passed_to: Option<Span>,
        named_at: Span,
    ) {
        // A top-level `let` of a value (`let routes = [home, help]`) is
        // reached through a call of its name (`let g = f`, `g(n)`): to
        // read it (a table its handlers read) calls nothing.
        if matches!(kind, SiteKind::Value { .. }) && self.nodes[to].value {
            return;
        }
        if let Some(from) = self.node {
            let made_from = args.iter().map(|a| self.made_from(a)).collect();
            let whole = args.iter().map(|a| self.whole_of(a)).collect();
            self.sites.push(Site {
                from,
                to,
                span,
                kind,
                args: args.iter().map(|a| a.span).collect(),
                made_from,
                whole,
                branch: self.branch,
                passed_to,
                named_at,
            });
        }
    }

    /// The parameter of the node the walk is in that `e` is all of under
    /// a name a pattern binds, if it is that.
    fn whole_of(&self, e: &Expr) -> Option<usize> {
        let node = self.node?;
        match &e.kind {
            ExprKind::Ident(name) if e.res == Some(Res::Local) => match self.lookup(*name)? {
                Origin::Whole(n, param) if n == node => Some(param),
                _ => None,
            },
            ExprKind::Ascription(inner, _) => self.whole_of(inner),
            _ => None,
        }
    }

    /// The parameter of the node the walk is in that `e` is made from:
    /// the first one named in it (or a part of it), if any.
    fn made_from(&self, e: &Expr) -> Option<usize> {
        let mut param = None;
        e.any_expr(&mut |x| {
            param = self.measure(x).map(|m| m.param);
            param.is_some()
        });
        param
    }

    /// The tail call `e` of `callee` lets go of what the function holds:
    /// say so of each `_name` it holds (W0302).
    fn let_go(&mut self, e: &Expr, callee: &Expr) {
        if self.held.is_empty() || self.defs.is_none() || !self.leaves_frame(callee) {
            return;
        }
        let call = self.shown(e.span);
        let to = self.node_named(callee);
        for (name, at) in self.held.clone() {
            let warning = Diagnostic::warning(
                Code::LetGoAtTailCall,
                e.span,
                format!("`{name}` is let go at this call"),
            )
            .with_label(at, format!("`{name}` is bound here"))
            .with_note(
                "a call that ends its block is a tail call: the function's values are \
                 let go before the call runs",
            );
            self.let_go.push(LetGo {
                warning,
                name,
                call: call.clone(),
                from: self.node,
                to,
            });
        }
    }

    /// The warnings of `W0302` with their help: a call of the caller's
    /// own group goes on with its loop, and binding its value would keep
    /// a frame each round (`E0345`), unless the caller is `deep`.
    fn let_go_helps(&mut self, groups: &[Vec<usize>]) {
        for LetGo {
            warning,
            name,
            call,
            from,
            to,
        } in std::mem::take(&mut self.let_go)
        {
            if from.is_some_and(|n| self.nodes[n].earlier) {
                continue;
            }
            let looped = match (from, to) {
                (Some(from), Some(to)) => {
                    self.nodes[from].deep.is_none()
                        && (groups.iter()).any(|g| g.contains(&from) && g.contains(&to))
                }
                _ => false,
            };
            let warning = match (looped, from) {
                (true, Some(from)) => {
                    let caller = self.nodes[from].name;
                    warning.with_help(format!(
                        "this call goes on with the loop of `{caller}`, and a call whose value \
                         is bound keeps a frame for every round: to keep `{name}` for the whole \
                         loop, bind it in the function that starts the loop and pass it in"
                    ))
                }
                _ => warning.with_help(format!(
                    "to keep `{name}` until the call returns, bind its value and end with it: \
                     `let result = {call}`, then `result`"
                )),
            };
            self.diagnostics.push(warning);
        }
    }

    /// Whether a tail call of `callee` takes the place of the caller's
    /// frame: a call of the program's own function or of a closure, not
    /// of one of silt's own.
    fn leaves_frame(&self, callee: &Expr) -> bool {
        let program_def = |res: Option<Res>| match (res, self.defs) {
            (Some(Res::Def(id)), Some(defs)) => {
                let def = defs.get(id);
                !def.module.is_builtin() && matches!(def.kind, DefKind::Fn | DefKind::Let)
            }
            _ => false,
        };
        match &callee.kind {
            ExprKind::Ident(_) => callee.res == Some(Res::Local) || program_def(callee.res),
            ExprKind::FieldAccess(..) => match callee.sel {
                Some(Selection::Impl { .. } | Selection::Dynamic { .. } | Selection::FieldCall) => {
                    true
                }
                Some(Selection::Native { .. } | Selection::Field) => false,
                None => program_def(callee.res),
            },
            _ => true,
        }
    }

    // ── Text ────────────────────────────────────────────────────────

    /// The text `span` is in: the program's, or an earlier REPL
    /// entry's.
    fn text_of(&self, span: Span) -> Option<&'a str> {
        self.earlier_texts.get(&span.file).copied().or(self.text)
    }

    /// The text of `span` on one line, cut short when it is long.
    fn shown(&self, span: Span) -> String {
        let Some(text) = self.text_of(span) else {
            return String::new();
        };
        let raw = text
            .get(span.start as usize..span.end as usize)
            .unwrap_or("");
        let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if one_line.chars().count() > 50 {
            let cut: String = one_line.chars().take(47).collect();
            format!("{cut}...")
        } else {
            one_line
        }
    }

    /// How `n`'s parameter `i` is named in a message.
    fn param_name(&self, n: usize, i: usize) -> String {
        match self.nodes[n].params.get(i).map(|p| &p.pattern.kind) {
            Some(PatternKind::Ident(name)) => name.to_string(),
            _ => format!("its argument {}", i + 1),
        }
    }

    // ── The groups ──────────────────────────────────────────────────

    fn finish(mut self, report: bool) -> Shape {
        // (A group of an earlier REPL entry's functions only was checked
        // with it.)
        let mut groups = self.groups();
        groups.retain(|members| members.iter().any(|&m| !self.nodes[m].earlier));
        self.let_go_helps(&groups);
        let mut verdicts = Vec::new();
        let mut grouped = HashSet::new();
        for members in &groups {
            grouped.extend(members.iter().copied());
            self.group(members, report, &mut verdicts);
        }
        if report {
            for n in 0..self.nodes.len() {
                if let Some(deep) = self.nodes[n].deep
                    && !grouped.contains(&n)
                    && !self.nodes[n].earlier
                {
                    let name = self.nodes[n].name;
                    self.diagnostics.push(
                        Diagnostic::warning(
                            Code::NeedlessDeep,
                            deep,
                            format!("`{name}` does not need `deep`: it does not call itself"),
                        )
                        .with_help("remove `deep`"),
                    );
                }
            }
        }
        for (span, name) in std::mem::take(&mut self.trait_calls) {
            verdicts.push((span, name, Verdict::TraitCall));
        }
        verdicts.sort_by_key(|(span, _, _)| span.start);
        self.diagnostics.sort_by_key(|d| d.span.start);
        Shape {
            diagnostics: self.diagnostics,
            verdicts,
        }
    }

    /// The groups of nodes that call each other (or one that calls
    /// itself), each in node order, in the order of their first node.
    fn groups(&self) -> Vec<Vec<usize>> {
        let n = self.nodes.len();
        let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut self_edge = vec![false; n];
        for site in &self.sites {
            edges[site.from].push(site.to);
            if site.from == site.to {
                self_edge[site.from] = true;
            }
        }
        let comps = strongly_connected(&edges);
        let mut groups: Vec<Vec<usize>> = comps
            .into_iter()
            .filter(|c| c.len() > 1 || self_edge[c[0]])
            .map(|mut c| {
                c.sort_unstable();
                c
            })
            .collect();
        groups.sort_by_key(|c| c[0]);
        groups
    }

    fn group(
        &mut self,
        members: &[usize],
        report: bool,
        verdicts: &mut Vec<(Span, Symbol, Verdict)>,
    ) {
        let in_group = |n: usize| members.contains(&n);
        let sites: Vec<usize> = (0..self.sites.len())
            .filter(|&s| in_group(self.sites[s].from) && in_group(self.sites[s].to))
            .collect();
        let checked: Vec<usize> = (sites.iter().copied())
            .filter(|&s| self.nodes[self.sites[s].from].deep.is_none())
            .collect();
        let solved = self.solve(members, &checked);
        for &m in members {
            let node = &self.nodes[m];
            let verdict = match (&solved, node.deep) {
                (_, Some(_)) => Some(Verdict::Deep),
                (Solved::Tail, None) => Some(Verdict::ConstantStack),
                (Solved::Data(d), None) => {
                    let at = members.iter().position(|x| *x == m).expect("a member");
                    d[at].map(|i| Verdict::FollowsData(self.param_name(m, i)))
                }
                (Solved::Refused(..), None) => None,
            };
            if let Some(verdict) = verdict {
                verdicts.push((node.name_span, node.name, verdict));
            }
        }
        if !report {
            return;
        }
        if let Solved::Refused(best, fails, capped) = &solved {
            self.refuse(members, &checked, best, fails, *capped);
        }
        // A `deep` the check would not need.
        if members.iter().any(|&m| self.nodes[m].deep.is_some()) {
            let why = match self.solve(members, &sites) {
                Solved::Tail => Some("it runs in constant stack"),
                Solved::Data(_) => Some("its depth follows the data"),
                Solved::Refused(..) => None,
            };
            if let Some(why) = why {
                for &m in members {
                    if let Some(deep) = self.nodes[m].deep {
                        let name = self.nodes[m].name;
                        self.diagnostics.push(
                            Diagnostic::warning(
                                Code::NeedlessDeep,
                                deep,
                                format!("`{name}` does not need `deep`: {why}"),
                            )
                            .with_help("remove `deep`"),
                        );
                    }
                }
            }
        }
        // A loop of tail calls that gives `()` writes where it ends.
        if sites.iter().all(|&s| self.sites[s].is_tail_call()) {
            let goes_on: HashSet<(u32, u32)> = (sites.iter())
                .map(|&s| (self.sites[s].span.start, self.sites[s].span.end))
                .collect();
            for &m in members {
                let body = self.nodes[m].body;
                if !matches!(body.ty, Some(Type::Unit)) {
                    continue;
                }
                let mut ends = Vec::new();
                endings(body, &goes_on, false, &mut ends);
                for (span, ending, in_arm) in ends {
                    self.unsaid_ending(m, span, ending, in_arm);
                }
            }
        }
    }

    /// Try each choice of one descending parameter per member.
    fn solve(&self, members: &[usize], sites: &[usize]) -> Solved {
        let (params, edges) = self.edges(members, sites);
        solve(&params, &edges, MAX_CHOICES)
    }

    /// Whether `solve` passes the group over `sites` (`Tail` or `Data`),
    /// found without trying every choice before the first that passes.
    fn passes(&self, members: &[usize], sites: &[usize]) -> bool {
        let (params, edges) = self.edges(members, sites);
        passes(&params, &edges, MAX_CHOICES)
    }

    /// How many parameters each member takes, and the `sites` as calls
    /// between the members' places.
    fn edges(&self, members: &[usize], sites: &[usize]) -> (Vec<usize>, Vec<Edge<'_>>) {
        let at = |n: usize| members.iter().position(|m| *m == n).expect("a member");
        let params = (members.iter())
            .map(|&m| self.nodes[m].params.len())
            .collect();
        let edges = (sites.iter())
            .map(|&s| Edge {
                id: s,
                from: at(self.sites[s].from),
                to: at(self.sites[s].to),
                site: &self.sites[s],
            })
            .collect();
        (params, edges)
    }

    /// E0345 for a refused group (`checked`: the calls of members not
    /// `deep`), under the choice of parameters that breaks the rule in
    /// fewest members: one error for each member with a call that breaks
    /// it, at the first such call, unless `deep` on the other members
    /// named is enough. The first of the errors names the group.
    fn refuse(
        &mut self,
        members: &[usize],
        checked: &[usize],
        best: &[Option<usize>],
        fails: &[usize],
        capped: bool,
    ) {
        // A member passed as a value says most, then a call inside a
        // closure, then a call; each the first in the text.
        let key = |s: usize| {
            let rank = match self.sites[s].kind {
                SiteKind::Value { .. } => 0,
                SiteKind::Closure { .. } => 1,
                SiteKind::Call { .. } => 2,
            };
            (rank, self.sites[s].span.start)
        };
        let mut firsts: Vec<usize> = (members.iter())
            .filter_map(|&m| {
                (fails.iter().copied())
                    .filter(|&s| self.sites[s].from == m)
                    .min_by_key(|&s| key(s))
            })
            .collect();
        firsts.sort_by_key(|&s| key(s));
        // A member named need not be when `deep` on the others named lets
        // the group pass (its calls then are all tail calls, say).
        let mut k = 0;
        while k < firsts.len() {
            let others: Vec<usize> = (firsts.iter().enumerate())
                .filter(|&(j, _)| j != k)
                .map(|(_, &s)| self.sites[s].from)
                .collect();
            let left: Vec<usize> = (checked.iter().copied())
                .filter(|&s| !others.contains(&self.sites[s].from))
                .collect();
            if self.passes(members, &left) {
                firsts.remove(k);
            } else {
                k += 1;
            }
        }
        let errors: Vec<Diagnostic> = (firsts.iter().enumerate())
            .map(|(i, &s)| self.refused(members, best, s, i == 0, capped))
            .collect();
        self.diagnostics.extend(errors);
    }

    /// The E0345 error of the call `s` of a refused group; `names_group`:
    /// it says which functions are checked together.
    fn refused(
        &self,
        members: &[usize],
        best: &[Option<usize>],
        s: usize,
        names_group: bool,
        capped: bool,
    ) -> Diagnostic {
        let at = |n: usize| members.iter().position(|m| *m == n).expect("a member");
        let site = &self.sites[s];
        let (from, to) = (&self.nodes[site.from], &self.nodes[site.to]);
        let (from_name, to_name) = (from.name, to.name);
        let d_from = best[at(site.from)];
        let d_to = best[at(site.to)];
        let arg = d_to.and_then(|i| site.args.get(i)).map(|s| self.shown(*s));
        let deep_help = match (from.top, from.value) {
            (true, true) => {
                let to = to_name;
                format!(
                    "if its depth is bounded by something the compiler cannot see, write \
                     `{from_name}` as a top-level `deep fn` that calls `{to}`"
                )
            }
            (true, false) => format!(
                "if its depth is bounded by something the compiler cannot see, write \
                 `deep fn {from_name}`"
            ),
            (false, _) => format!(
                "if its depth is bounded by something the compiler cannot see, make \
                 `{from_name}` a top-level `deep fn`"
            ),
        };
        let mut d = match &site.kind {
            SiteKind::Value { .. } => {
                let message = match site.passed_to {
                    Some(callee) => format!(
                        "`{to_name}` is passed to `{}` as a value, so the compiler cannot see \
                         how it is called",
                        self.shown(callee)
                    ),
                    None => format!(
                        "`{to_name}` is used here as a value, so the compiler cannot see how \
                         it is called"
                    ),
                };
                Diagnostic::error(Code::RecursionShape, site.span, message)
                    .with_help(format!("call `{to_name}` where it is named"))
                    .with_help(deep_help)
            }
            SiteKind::Closure { .. } => Diagnostic::error(
                Code::RecursionShape,
                site.span,
                format!(
                    "this call of `{to_name}` is inside a closure: it takes a frame for the \
                     closure and one for the call"
                ),
            )
            .with_help(
                // (The calls the rule names, by the module's own name)
                "a closure may call it only when it is handed to `list.each`, `list.map`, \
                 `list.flat_map`, `list.fold`, `list.filter`, `list.find`, `list.any` or \
                 `list.all` over a part of a parameter, with the element",
            )
            .with_help(deep_help),
            SiteKind::Call { tail: true, .. } => {
                let arg = arg.unwrap_or_else(|| "nothing".to_string());
                Diagnostic::error(
                    Code::RecursionShape,
                    site.span,
                    format!(
                        "this call of `{to_name}` passes `{arg}`, which is no part of a \
                         parameter of `{from_name}`"
                    ),
                )
                .with_note(
                    "a tail call that does not pass a part lets the other calls of the group \
                     go deeper than the data",
                )
                .with_help(deep_help)
            }
            SiteKind::Call {
                tail: false,
                args: measures,
            } => {
                let measure = d_to.and_then(|i| measures.get(i).copied().flatten());
                let whole = d_to.and_then(|i| site.whole.get(i).copied().flatten());
                // The parameter the argument is made from, or the caller's
                // only one (said when it descends on none).
                let made_from = (d_to.and_then(|i| site.made_from.get(i).copied().flatten()))
                    .or_else(|| (from.params.len() == 1).then_some(0));
                let not_last = format!("not the last thing `{from_name}` does");
                // (The parameter itself only when it is the one descended on;
                // anything else is said against that one.)
                let label = match (&arg, measure, d_from) {
                    (Some(arg), Some(m), _) if !m.strict && d_from.is_none_or(|d| d == m.param) => {
                        match self.param_name(site.from, m.param) {
                            p if p == *arg => {
                                format!(
                                    "{not_last}, and it passes `{arg}` itself, not a part of it"
                                )
                            }
                            p => {
                                format!("{not_last}, and `{arg}` is `{p}` itself, not a part of it")
                            }
                        }
                    }
                    // (`all` of `match xs { all -> ... }` reads as a copy does.)
                    (Some(arg), None, _)
                        if whole.is_some_and(|p| d_from.is_none_or(|d| d == p)) =>
                    {
                        let p = self.param_name(site.from, whole.expect("a parameter"));
                        format!("{not_last}, and `{arg}` is `{p}` itself, not a part of it")
                    }
                    (Some(arg), Some(m), Some(d)) if m.strict => format!(
                        "{not_last}, and `{arg}` is a part of `{}`, not of `{}`",
                        self.param_name(site.from, m.param),
                        self.param_name(site.from, d),
                    ),
                    (Some(arg), _, Some(d)) => format!(
                        "{not_last}, and `{arg}` is not a part of `{}`",
                        self.param_name(site.from, d)
                    ),
                    (Some(arg), _, None) => match made_from {
                        Some(p) => format!(
                            "{not_last}, and `{arg}` is not a part of `{}`",
                            self.param_name(site.from, p)
                        ),
                        None => format!("{not_last}, and `{arg}` is not a part of a parameter"),
                    },
                    (None, _, _) => {
                        format!("{not_last}, and `{to_name}` takes no argument that gets smaller")
                    }
                };
                let d = Diagnostic::error(
                    Code::RecursionShape,
                    site.span,
                    format!("this call of `{to_name}` takes a frame for every round"),
                )
                .with_note(label);
                let counted = d_to
                    .and_then(|i| site.args.get(i))
                    .and_then(|s| self.counted_down(*s));
                let carry = "carry the running value in a parameter, so that the call comes last";
                // Two calls of the group that keep a frame and both run
                // (`fib(n - 1) + fib(n - 2)`, or one in a match's scrutinee
                // and one in its arm) branch out: `deep` is the usual answer,
                // and no fold counts it.
                let branches_out = (self.sites.iter().enumerate()).any(|(j, other)| {
                    j != s
                        && other.from == site.from
                        && members.contains(&other.to)
                        && matches!(other.kind, SiteKind::Call { tail: false, .. })
                        && self.run_together(site, other)
                });
                match (branches_out, counted) {
                    (true, _) => d.with_help(deep_help).with_help(format!("or {carry}")),
                    (false, Some(n)) => d
                        .with_help(format!(
                            // (The fold the rule offers, by the module's own name)
                            "count with the library: `list.fold(1..{n}, start) {{ acc, i -> ... }}`"
                        ))
                        .with_help(format!("or {carry}"))
                        .with_help(deep_help),
                    (false, None) => d.with_help(carry).with_help(deep_help),
                }
            }
        };
        // (Named through an alias: say what the alias is.)
        let written = self.shown(site.named_at);
        if !written.is_empty() && written != to_name.to_string() {
            d = d.with_note(format!(
                "`{written}` is `{to_name}`: an alias is the function it names"
            ));
        }
        if names_group && members.len() > 1 {
            let names: Vec<String> = members
                .iter()
                .map(|&m| format!("`{}`", self.nodes[m].name))
                .collect();
            let names = match names.split_last() {
                Some((last, front)) if !front.is_empty() => {
                    format!("{} and {last}", front.join(", "))
                }
                _ => names.join(""),
            };
            d = d.with_note(format!(
                "{names} call each other, so they are checked together"
            ));
            // One place each other member names one of the group.
            for &m in members.iter().filter(|&&m| m != site.from) {
                if let Some(other) = self
                    .sites
                    .iter()
                    .find(|s| s.from == m && members.contains(&s.to))
                {
                    let (a, b) = (self.nodes[m].name, self.nodes[other.to].name);
                    let does = match other.kind {
                        SiteKind::Value { .. } => "passes",
                        _ => "calls",
                    };
                    d = d.with_label(other.span, format!("`{a}` {does} `{b}` here"));
                }
            }
        }
        // (The notes are about the choice of the first that were tried.)
        if names_group && capped {
            d = d.with_note(format!(
                "the check gave up after trying {} of the ways the functions can each \
                 descend on one parameter: one it did not try may keep the rule",
                thousands(MAX_CHOICES)
            ));
        }
        d
    }

    /// The `n` of an argument `n - 1` (an Int counted down), written as
    /// it is.
    fn counted_down(&self, span: Span) -> Option<String> {
        let text = self
            .text_of(span)?
            .get(span.start as usize..span.end as usize)?;
        let (n, k) = text.split_once('-')?;
        let (n, k) = (n.trim(), k.trim());
        let is_name = n.chars().all(|c| c.is_alphanumeric() || c == '_')
            && n.starts_with(|c: char| c.is_lowercase() || c == '_');
        (is_name && !k.is_empty() && k.chars().all(|c| c.is_ascii_digit())).then(|| n.to_string())
    }

    /// E0346: `m`'s ending at `span` does not say it ends.
    fn unsaid_ending(&mut self, m: usize, span: Span, ending: Ending, in_arm: bool) {
        let name = self.nodes[m].name;
        let (message, label) = match in_arm {
            true => (
                format!("this arm ends `{name}`, but does not say so"),
                format!("`{name}` goes on in other arms; this one stops it"),
            ),
            false => (
                format!("this ends `{name}`, but does not say so"),
                format!("`{name}` goes on elsewhere; this stops it"),
            ),
        };
        let place = if in_arm { "the arm" } else { "it" };
        let go_on = format!("to go on, end {place} with a call of `{name}`");
        let stop = match ending {
            Ending::Call => match self.shown(span) {
                text if text.ends_with("...") => {
                    "to stop here, write `return` before it".to_string()
                }
                text => format!("to stop here, write `return {text}`"),
            },
            Ending::Let => "to stop here, end the block with `()`".to_string(),
        };
        self.diagnostics.push(
            Diagnostic::error(Code::UnsaidEnding, span, message)
                .with_note(label)
                .with_help(go_on)
                .with_help(stop),
        );
    }
}

/// `n` as a message writes it: `20,000`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A warning of `W0302` waiting for its help (`Walk::let_go_helps`).
struct LetGo {
    warning: Diagnostic,
    name: Symbol,
    /// The call, as written.
    call: String,
    /// The node the call is in, and the node it calls.
    from: Option<usize>,
    to: Option<usize>,
}

enum Solved {
    /// Every call is a tail call.
    Tail,
    /// Each member descends on the parameter given (none for one that
    /// takes none).
    Data(Vec<Option<usize>>),
    /// The choice that breaks the rule fewest times, and where it does;
    /// whether the check gave up before it had tried every choice.
    Refused(Vec<Option<usize>>, Vec<usize>, bool),
}

/// Whether `site` keeps the rule when its caller descends on `d_from` and
/// its callee on `d_to`.
fn site_ok(site: &Site, d_from: Option<usize>, d_to: Option<usize>) -> bool {
    let (Some(d_from), Some(d_to)) = (d_from, d_to) else {
        return false;
    };
    let on = |m: Option<Measure>, strict: bool| {
        m.is_some_and(|m| m.param == d_from && (m.strict || !strict))
    };
    match &site.kind {
        SiteKind::Call { tail, args } => on(args.get(d_to).copied().flatten(), !*tail),
        SiteKind::Closure {
            coll,
            element,
            args,
        } => *element && on(*coll, false) && on(args.get(d_to).copied().flatten(), true),
        SiteKind::Value { elem } => {
            elem.is_some_and(|(coll, at)| on(Some(coll), false) && at == d_to)
        }
    }
}

/// A site of a group as the choices see it: its caller and callee by
/// their place among the members, and the site's number.
struct Edge<'s> {
    id: usize,
    from: usize,
    to: usize,
    site: &'s Site,
}

/// The member at place `k` descends on `pick` of its `params` (none when
/// it takes none).
fn descends(params: &[usize], k: usize, pick: usize) -> Option<usize> {
    (params[k] > 0).then_some(pick)
}

/// The parameters a passing choice can pick for each member (`params`:
/// how many each takes): each one a site of the member cannot keep the
/// rule with, whatever the member at its other end picks of those left,
/// is dropped, until none is. `None` when a member has none left: no
/// choice passes.
fn open_choices(params: &[usize], edges: &[Edge]) -> Option<Vec<Vec<bool>>> {
    let choices: Vec<usize> = params.iter().map(|&p| p.max(1)).collect();
    let ok = |e: &Edge, a: usize, b: usize| {
        site_ok(
            e.site,
            descends(params, e.from, a),
            descends(params, e.to, b),
        )
    };
    let mut open: Vec<Vec<bool>> = choices.iter().map(|&c| vec![true; c]).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for e in edges {
            for a in 0..choices[e.from] {
                if open[e.from][a] {
                    let kept = match e.from == e.to {
                        true => ok(e, a, a),
                        false => (0..choices[e.to]).any(|b| open[e.to][b] && ok(e, a, b)),
                    };
                    if !kept {
                        open[e.from][a] = false;
                        changed = true;
                    }
                }
            }
            for b in 0..choices[e.to] {
                if open[e.to][b]
                    && e.from != e.to
                    && !(0..choices[e.from]).any(|a| open[e.from][a] && ok(e, a, b))
                {
                    open[e.to][b] = false;
                    changed = true;
                }
            }
        }
        if open.iter().any(|o| !o.contains(&true)) {
            return None;
        }
    }
    Some(open)
}

/// Try each choice of one descending parameter per member (`params`: how
/// many each takes) among those `open_choices` leaves, the first
/// member's choice counted fastest, at most `cap` of them. A group it
/// refuses is refused under the choice of all that breaks the rule
/// fewest times (among the first `cap`).
fn solve(params: &[usize], edges: &[Edge], cap: usize) -> Solved {
    if edges.iter().all(|e| e.site.is_tail_call()) {
        return Solved::Tail;
    }
    let fails_under = |d: &[Option<usize>]| -> Vec<usize> {
        (0..edges.len())
            .filter(|&i| !site_ok(edges[i].site, d[edges[i].from], d[edges[i].to]))
            .collect()
    };
    // (Whether the choices left were more than `cap`.)
    let mut capped = false;
    if let Some(open) = open_choices(params, edges) {
        let left: Vec<Vec<usize>> = (open.iter())
            .map(|o| (0..o.len()).filter(|&a| o[a]).collect())
            .collect();
        let mut pick = vec![0usize; params.len()];
        capped = 'choices: {
            for _ in 0..cap {
                let d: Vec<Option<usize>> = (pick.iter().enumerate())
                    .map(|(k, &i)| descends(params, k, left[k][i]))
                    .collect();
                if fails_under(&d).is_empty() {
                    return Solved::Data(d);
                }
                if !next_choice(&mut pick, |k| left[k].len()) {
                    break 'choices false;
                }
            }
            true
        };
    }
    let mut pick = vec![0usize; params.len()];
    // (The members a choice breaks the rule in, then the calls: each
    // member broken is a `deep` to write.)
    // (`fails` by place in `edges`.)
    let cost = |fails: &[usize]| {
        let broken: HashSet<usize> = fails.iter().map(|&i| edges[i].from).collect();
        (broken.len(), fails.len())
    };
    let mut best: Option<(Vec<Option<usize>>, Vec<usize>)> = None;
    let mut best_cost = (usize::MAX, usize::MAX);
    for _ in 0..cap {
        let d: Vec<Option<usize>> = (pick.iter().enumerate())
            .map(|(k, &i)| descends(params, k, i))
            .collect();
        let fails = fails_under(&d);
        let this = cost(&fails);
        if this < best_cost {
            best_cost = this;
            best = Some((d, fails));
        }
        if !next_choice(&mut pick, |k| params[k].max(1)) {
            break;
        }
    }
    let (d, fails) = best.expect("one choice tried");
    Solved::Refused(d, fails.iter().map(|&i| edges[i].id).collect(), capped)
}

/// Step `pick` to the next choice, the first member's counted fastest,
/// each member `k` choosing among `count(k)`: false when it was the last.
fn next_choice(pick: &mut [usize], count: impl Fn(usize) -> usize) -> bool {
    for (k, p) in pick.iter_mut().enumerate() {
        *p += 1;
        if *p < count(k) {
            return true;
        }
        *p = 0;
    }
    false
}

/// Whether `solve` with the same arguments passes, without trying the
/// choices one by one: a choice passes when every site keeps the rule,
/// and `solve` finds one when one is among the first `cap` it tries of
/// those `open_choices` leaves. So: look for a passing choice among
/// them in `solve`'s order, the last member's choice first, and give up
/// where the choices left all come after the first `cap`.
fn passes(params: &[usize], edges: &[Edge], cap: usize) -> bool {
    if edges.iter().all(|e| e.site.is_tail_call()) {
        return true;
    }
    let n = params.len();
    let ok = |e: &Edge, a: usize, b: usize| {
        site_ok(
            e.site,
            descends(params, e.from, a),
            descends(params, e.to, b),
        )
    };
    let Some(open) = open_choices(params, edges) else {
        return false;
    };
    // The parameters left of each member, in order: `solve` counts
    // through these.
    let left: Vec<Vec<usize>> = (open.iter())
        .map(|o| (0..o.len()).filter(|&a| o[a]).collect())
        .collect();
    // How far apart in `solve`'s order two choices of each member are
    // (as far as `cap` counts).
    let mut weight = vec![1usize; n];
    for k in 1..n {
        weight[k] = weight[k - 1].saturating_mul(left[k - 1].len()).min(cap);
    }
    // The sites between each member and the members after it, and its own.
    let mut back: Vec<Vec<&Edge>> = vec![Vec::new(); n];
    for e in edges {
        back[e.from.min(e.to)].push(e);
    }
    // Choose from the last member down; `pick[k]` is the place in
    // `left[k]` of the next parameter to try for member `k`, `index`
    // where the choices so far stand in `solve`'s order.
    let mut pick = vec![0usize; n];
    let mut index = vec![0usize; n + 1];
    let chosen = |pick: &[usize], k: usize| left[k][pick[k] - 1];
    let mut k = n;
    loop {
        // Member `k - 1` is to choose: its next parameter left, still
        // among the first `cap` choices, that keeps the rule with the
        // members after it.
        let m = k - 1;
        let mut found = false;
        while pick[m] < left[m].len() {
            let place = pick[m];
            let a = left[m][place];
            pick[m] += 1;
            let at = index[k].saturating_add(weight[m].saturating_mul(place));
            if at >= cap {
                pick[m] = left[m].len();
                break;
            }
            let fits = back[m].iter().all(|e| {
                let (x, y) = match (e.from == m, e.to == m) {
                    (true, true) => (a, a),
                    (true, false) => (a, chosen(&pick, e.to)),
                    (false, _) => (chosen(&pick, e.from), a),
                };
                ok(e, x, y)
            });
            if fits {
                index[m] = at;
                found = true;
                break;
            }
        }
        match found {
            true if m == 0 => return true,
            true => k -= 1,
            false => {
                // Back to the member after it.
                pick[m] = 0;
                if k == n {
                    return false;
                }
                k += 1;
            }
        }
    }
}

/// How a member of a `()` loop ends without saying so.
#[derive(Clone, Copy)]
enum Ending {
    /// A call that gives `()`.
    Call,
    /// A block whose last statement is a `let` (or a `when`).
    Let,
}

/// The endings of `e` in tail position that do not say they end: a
/// call (not in `goes_on`) that gives `()`, a block that ends in a
/// statement that is no expression. `in_arm`: inside a match arm.
fn endings(
    e: &Expr,
    goes_on: &HashSet<(u32, u32)>,
    in_arm: bool,
    out: &mut Vec<(Span, Ending, bool)>,
) {
    match &e.kind {
        ExprKind::Block(stmts) => match stmts.last() {
            None => {}
            Some(Stmt::Expr(last)) => endings(last, goes_on, in_arm, out),
            Some(Stmt::Let { pattern, value, .. }) => {
                out.push((pattern.span.to(value.span), Ending::Let, in_arm))
            }
            Some(Stmt::When {
                pattern, else_body, ..
            }) => out.push((pattern.span.to(else_body.span), Ending::Let, in_arm)),
            Some(Stmt::WhenBool {
                condition,
                else_body,
            }) => out.push((condition.span.to(else_body.span), Ending::Let, in_arm)),
        },
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                endings(&arm.body, goes_on, true, out);
            }
        }
        ExprKind::Call(..) | ExprKind::Pipe(..)
            if !goes_on.contains(&(e.span.start, e.span.end))
                && matches!(e.ty, Some(Type::Unit)) =>
        {
            out.push((e.span, Ending::Call, in_arm));
        }
        _ => {}
    }
}

/// Whether `e` leaves the function when it ends: it ends in `return`
/// (in a block, in every arm of a match).
fn ends_in_return(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Return(_) => true,
        ExprKind::Block(stmts) => {
            matches!(stmts.last(), Some(Stmt::Expr(last)) if ends_in_return(last))
        }
        ExprKind::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|a| ends_in_return(&a.body))
        }
        _ => false,
    }
}

/// Whether `m` is the `decode` of a `json.Decode` impl that decodes
/// a value of its own type (`json.parse(text, User)` in the impl for
/// `User`): the builtin calls the impl back, a call of its own trait.
fn decodes_own_type(m: &FnDecl, trait_id: Option<crate::defs::DefId>) -> bool {
    let decode = (trait_id.and_then(super::names::builtin_def)).is_some_and(|def| {
        def.module.builtin_name() == Some("json") && resolve(def.name) == "Decode"
    });
    // (Its type is `Result(Self, json.JsonError)`.)
    let own = match &m.body.ty {
        Some(Type::Generic(_, args)) if decode => args.first().and_then(Type::type_ref),
        _ => None,
    };
    let Some(own) = own else {
        return false;
    };
    m.body.any_expr(&mut |e| match &e.ty {
        Some(Type::Generic(name, args)) if name.is_builtin(crate::defs::TYPE_OF) => args
            .first()
            .and_then(Type::type_ref)
            .is_some_and(|t| t.id == own.id),
        _ => false,
    })
}

/// `e` without the type a `as` gives it.
fn unwrapped(e: &Expr) -> &Expr {
    match &e.kind {
        ExprKind::Ascription(inner, _) => unwrapped(inner),
        _ => e,
    }
}

/// The `list` call `callee` names, if it is one of `ELEMENT_CALLS`: the
/// place of the element among its function's parameters.
fn element_call(callee: &Expr) -> Option<usize> {
    // `list.fold`, through an alias of the module or a name imported
    // from it (`import list.{ fold }`).
    let (m, name) = match (&callee.kind, callee.res) {
        (ExprKind::FieldAccess(module, name, _), _) => match module.res {
            Some(Res::Module(m)) => (m, *name),
            _ => return None,
        },
        (ExprKind::Ident(_), Some(Res::Def(id))) => {
            let def = super::names::builtin_def(id)?;
            (def.module, def.name)
        }
        _ => return None,
    };
    if m.builtin_name() != Some("list") {
        return None;
    }
    let name = resolve(name);
    ELEMENT_CALLS
        .contains(&name.as_str())
        .then_some(if name == "fold" { 1 } else { 0 })
}

/// The strongly connected parts of the graph `edges` (Tarjan's).
fn strongly_connected(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct State {
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        next: usize,
        out: Vec<Vec<usize>>,
    }
    let n = edges.len();
    let mut s = State {
        index: vec![None; n],
        low: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };
    // (Iterative: a call chain can be long.)
    for root in 0..n {
        if s.index[root].is_some() {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        s.index[root] = Some(s.next);
        s.low[root] = s.next;
        s.next += 1;
        s.stack.push(root);
        s.on_stack[root] = true;
        while let Some(&mut (v, ref mut i)) = work.last_mut() {
            if *i < edges[v].len() {
                let w = edges[v][*i];
                *i += 1;
                match s.index[w] {
                    None => {
                        s.index[w] = Some(s.next);
                        s.low[w] = s.next;
                        s.next += 1;
                        s.stack.push(w);
                        s.on_stack[w] = true;
                        work.push((w, 0));
                    }
                    Some(iw) if s.on_stack[w] => s.low[v] = s.low[v].min(iw),
                    Some(_) => {}
                }
            } else {
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    s.low[parent] = s.low[parent].min(s.low[v]);
                }
                if Some(s.low[v]) == s.index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = s.stack.pop() {
                        s.on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    s.out.push(comp);
                }
            }
        }
    }
    s.out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small random number source (no seed from outside: the same runs
    /// every time).
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) % n as u64) as usize
        }
    }

    fn site(rng: &mut Rng, from: usize, to: usize, params: &[usize]) -> Site {
        let measure = |rng: &mut Rng| match rng.below(4) {
            0 => None,
            _ if params[from] == 0 => None,
            _ => Some(Measure {
                param: rng.below(params[from]),
                strict: rng.below(2) == 0,
            }),
        };
        let args = (0..params[to]).map(|_| measure(rng)).collect();
        let kind = match rng.below(8) {
            0 => SiteKind::Closure {
                coll: measure(rng),
                element: rng.below(2) == 0,
                args,
            },
            1 => SiteKind::Value {
                elem: measure(rng).map(|m| (m, rng.below(params[to].max(1)))),
            },
            _ => SiteKind::Call {
                tail: rng.below(3) == 0,
                args,
            },
        };
        Site {
            from,
            to,
            span: Span::BUILTIN,
            kind,
            args: Vec::new(),
            made_from: Vec::new(),
            whole: Vec::new(),
            branch: 0,
            passed_to: None,
            named_at: Span::BUILTIN,
        }
    }

    /// A ring of seven functions of five parameters each, each calling
    /// the next on a part of its last: the one choice that passes comes
    /// last of the 78,125, past the cap, and is found.
    #[test]
    fn the_one_choice_left_is_found_past_the_cap() {
        let params = vec![5; 7];
        let sites: Vec<Site> = (0..7)
            .map(|from| {
                let mut args = vec![None; 5];
                args[4] = Some(Measure {
                    param: 4,
                    strict: true,
                });
                Site {
                    from,
                    to: (from + 1) % 7,
                    span: Span::BUILTIN,
                    kind: SiteKind::Call { tail: false, args },
                    args: Vec::new(),
                    made_from: Vec::new(),
                    whole: Vec::new(),
                    branch: 0,
                    passed_to: None,
                    named_at: Span::BUILTIN,
                }
            })
            .collect();
        let edges: Vec<Edge> = (sites.iter().enumerate())
            .map(|(id, s)| Edge {
                id,
                from: s.from,
                to: s.to,
                site: s,
            })
            .collect();
        assert!(matches!(
            solve(&params, &edges, MAX_CHOICES),
            Solved::Data(d) if d == vec![Some(4); 7]
        ));
        assert!(passes(&params, &edges, MAX_CHOICES));
    }

    /// `thousands` writes a count as a message does.
    #[test]
    fn thousands_are_marked() {
        assert_eq!(thousands(20_000), "20,000");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    /// `passes` says what `solve` finds, also where `solve` stops at its
    /// cap before the first choice that passes.
    #[test]
    fn passes_agrees_with_solve() {
        let mut rng = Rng(7);
        let mut passed = 0;
        let mut capped = 0;
        for _ in 0..20_000 {
            let n = 1 + rng.below(6);
            let params: Vec<usize> = (0..n).map(|_| rng.below(4)).collect();
            let count = 1 + rng.below(3 * n);
            let sites: Vec<Site> = (0..count)
                .map(|_| {
                    let (from, to) = (rng.below(n), rng.below(n));
                    site(&mut rng, from, to, &params)
                })
                .collect();
            let edges: Vec<Edge> = (sites.iter().enumerate())
                .map(|(id, s)| Edge {
                    id,
                    from: s.from,
                    to: s.to,
                    site: s,
                })
                .collect();
            let cap = match rng.below(3) {
                0 => 1 + rng.below(40),
                _ => MAX_CHOICES,
            };
            let solves = |cap: usize| !matches!(solve(&params, &edges, cap), Solved::Refused(..));
            let solved = solves(cap);
            passed += usize::from(solved);
            capped += usize::from(!solved && solves(MAX_CHOICES));
            // (A refusal says it gave up where a choice not tried passes.)
            if let Solved::Refused(_, _, gave_up) = solve(&params, &edges, cap)
                && solves(MAX_CHOICES)
            {
                assert!(gave_up, "params {params:?}, cap {cap}: gave up unsaid");
            }
            assert_eq!(
                passes(&params, &edges, cap),
                solved,
                "params {params:?}, cap {cap}, sites {:?}",
                (sites.iter()).map(|s| (s.from, s.to)).collect::<Vec<_>>()
            );
        }
        // (Both answers are seen often, and a cap that hides a choice.)
        assert!(passed > 1_000 && passed < 19_000, "{passed} pass");
        assert!(capped > 0, "no cap seen");
    }
}
