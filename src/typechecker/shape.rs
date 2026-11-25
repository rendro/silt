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
use crate::source::Span;
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
    params: &'a [Param],
    body: &'a Expr,
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
    Other,
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
    /// For a member passed as a value to a call: the callee's span.
    passed_to: Option<Span>,
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
    defs: Option<&'d DefTable>,
    nodes: Vec<Node<'a>>,
    /// The top-level nodes by name.
    top: HashMap<Symbol, usize>,
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
    diagnostics: Vec<Diagnostic>,
    /// The methods that call a method of their own trait.
    trait_calls: Vec<(Span, Symbol)>,
}

impl<'a, 'd> Walk<'a, 'd> {
    fn new(program: &'a Program, text: Option<&'a str>, defs: Option<&'d DefTable>) -> Self {
        let mut nodes = Vec::new();
        let mut top = HashMap::new();
        for decl in &program.decls {
            match decl {
                Decl::Fn(f) if !f.is_recovery_stub && !f.is_signature_only => {
                    top.insert(f.name, nodes.len());
                    nodes.push(Node {
                        name: f.name,
                        name_span: f.name_span,
                        deep: f.deep,
                        top: true,
                        params: &f.params,
                        body: &f.body,
                    });
                }
                Decl::Let { pattern, value, .. } => {
                    if let (PatternKind::Ident(name), ExprKind::Lambda { params, body }) =
                        (&pattern.kind, &value.kind)
                    {
                        top.insert(*name, nodes.len());
                        nodes.push(Node {
                            name: *name,
                            name_span: pattern.span,
                            deep: None,
                            top: true,
                            params,
                            body,
                        });
                    }
                }
                _ => {}
            }
        }
        Walk {
            text,
            defs,
            nodes,
            top,
            scopes: Vec::new(),
            node: None,
            closure: None,
            sites: Vec::new(),
            held: Vec::new(),
            diagnostics: Vec::new(),
            trait_calls: Vec::new(),
        }
    }

    fn run(&mut self, program: &'a Program) {
        for n in 0..self.nodes.len() {
            self.function(n);
        }
        for decl in &program.decls {
            match decl {
                Decl::Let { value, .. } if !matches!(value.kind, ExprKind::Lambda { .. }) => {
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
        });
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
            Some(Res::Def(id)) => self.top.get(name).copied().filter(|&n| {
                self.defs
                    .is_none_or(|defs| defs.get(id).span == self.nodes[n].name_span)
            }),
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
            PatternKind::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    let below = below
                        || alternatives
                            .iter()
                            .all(|a| !matches!(a.kind, PatternKind::Ident(_)));
                    self.bind(first, rel, below);
                }
            }
            PatternKind::Map(entries) => {
                for (_, value) in entries {
                    self.bind(value, rel, true);
                }
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
        if let (
            Some(Expr {
                kind: ExprKind::Tuple(values),
                ..
            }),
            PatternKind::Tuple(items),
        ) = (value, &pattern.kind)
            && values.len() == items.len()
        {
            let measures: Vec<_> = values.iter().map(|v| self.measure(v)).collect();
            for (item, m) in items.iter().zip(measures) {
                self.bind(item, m, false);
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
                    self.site(to, e.span, SiteKind::Value { elem: None }, Vec::new(), None);
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
                for arm in arms {
                    let depth = self.scopes.len();
                    self.bind_matched(&arm.pattern, expr.as_deref());
                    if let Some(guard) = &arm.guard {
                        self.expr(guard, false);
                    }
                    self.expr(&arm.body, tail);
                    self.scopes.truncate(depth);
                }
            }
            ExprKind::Return(Some(value)) => self.expr(value, true),
            ExprKind::Loop { bindings, body } => {
                for (_, _, init) in bindings {
                    self.expr(init, false);
                }
                let depth = self.scopes.len();
                for (name, _, _) in bindings {
                    self.scopes.push((*name, Origin::Other));
                }
                self.expr(body, tail);
                self.scopes.truncate(depth);
            }
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
                            self.nodes.push(Node {
                                name: *name,
                                name_span: pattern.span,
                                deep: None,
                                top: false,
                                params,
                                body,
                            });
                            self.scopes.push((*name, Origin::Node(n)));
                            self.function(n);
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
                    self.expr(else_body, false);
                    self.bind_matched(pattern, Some(expr));
                }
                Stmt::WhenBool {
                    condition,
                    else_body,
                } => {
                    self.expr(condition, false);
                    self.expr(else_body, false);
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
            let spans = args.iter().map(|a| a.span).collect();
            self.site(to, e.span, kind, spans, None);
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
                        self.site(to, fun.span, kind, Vec::new(), Some(callee.span));
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
                    self.site(to, arg.span, kind, Vec::new(), Some(callee.span));
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
        args: Vec<Span>,
        passed_to: Option<Span>,
    ) {
        if let Some(from) = self.node {
            self.sites.push(Site {
                from,
                to,
                span,
                kind,
                args,
                passed_to,
            });
        }
    }

    /// The tail call `e` of `callee` lets go of what the function holds:
    /// say so of each `_name` it holds (W0302).
    fn let_go(&mut self, e: &Expr, callee: &Expr) {
        if self.held.is_empty() || self.defs.is_none() || !self.leaves_frame(callee) {
            return;
        }
        let call = self.shown(e.span);
        for (name, at) in self.held.clone() {
            self.diagnostics.push(
                Diagnostic::warning(
                    Code::LetGoAtTailCall,
                    e.span,
                    format!("`{name}` is let go at this call"),
                )
                .with_label(at, format!("`{name}` is bound here"))
                .with_note(
                    "a call that ends its block is a tail call: the function's values are \
                     let go before the call runs",
                )
                .with_help(format!(
                    "to keep `{name}` until the call returns, bind its value and end with it: \
                     `let result = {call}`, then `result`"
                )),
            );
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

    /// The text of `span` on one line, cut short when it is long.
    fn shown(&self, span: Span) -> String {
        let Some(text) = self.text else {
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

    /// `n` called with its own parameters: what "go on" writes.
    fn call_text(&self, n: usize) -> String {
        let node = &self.nodes[n];
        let params: Vec<String> = (node.params.iter())
            .map(|p| match &p.pattern.kind {
                PatternKind::Ident(name) => name.to_string(),
                _ => self.shown(p.pattern.span),
            })
            .collect();
        format!("{}({})", node.name, params.join(", "))
    }

    // ── The groups ──────────────────────────────────────────────────

    fn finish(mut self, report: bool) -> Shape {
        let groups = self.groups();
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
        if let Solved::Refused(best, fails) = &solved {
            self.refuse(members, best, fails);
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
        if sites.iter().all(|&s| self.sites[s].is_tail_call()) {
            return Solved::Tail;
        }
        let choices: Vec<usize> = (members.iter())
            .map(|&m| self.nodes[m].params.len().max(1))
            .collect();
        let at = |n: usize| members.iter().position(|m| *m == n).expect("a member");
        let mut pick = vec![0usize; members.len()];
        let mut best: Option<(Vec<Option<usize>>, Vec<usize>)> = None;
        for _ in 0..MAX_CHOICES {
            let d: Vec<Option<usize>> = (members.iter().zip(&pick))
                .map(|(&m, &i)| (!self.nodes[m].params.is_empty()).then_some(i))
                .collect();
            let fails: Vec<usize> = (sites.iter().copied())
                .filter(|&s| {
                    let site = &self.sites[s];
                    !site_ok(site, d[at(site.from)], d[at(site.to)])
                })
                .collect();
            if fails.is_empty() {
                return Solved::Data(d);
            }
            if best.as_ref().is_none_or(|(_, b)| fails.len() < b.len()) {
                best = Some((d, fails));
            }
            // The next choice.
            let mut k = 0;
            loop {
                if k == pick.len() {
                    let (d, fails) = best.expect("one choice tried");
                    return Solved::Refused(d, fails);
                }
                pick[k] += 1;
                if pick[k] < choices[k] {
                    break;
                }
                pick[k] = 0;
                k += 1;
            }
        }
        let (d, fails) = best.expect("one choice tried");
        Solved::Refused(d, fails)
    }

    /// E0345: the first call of a refused group that breaks the rule
    /// under the choice of parameters that breaks it fewest times.
    fn refuse(&mut self, members: &[usize], best: &[Option<usize>], fails: &[usize]) {
        // A member passed as a value says most, then a call inside a
        // closure, then a call; each the first in the text.
        let rank = |s: usize| match self.sites[s].kind {
            SiteKind::Value { .. } => 0,
            SiteKind::Closure { .. } => 1,
            SiteKind::Call { .. } => 2,
        };
        let Some(&first) = fails
            .iter()
            .min_by_key(|&&s| (rank(s), self.sites[s].span.start))
        else {
            return;
        };
        let at = |n: usize| members.iter().position(|m| *m == n).expect("a member");
        let site = &self.sites[first];
        let (from, to) = (&self.nodes[site.from], &self.nodes[site.to]);
        let (from_name, to_name) = (from.name, to.name);
        let d_from = best[at(site.from)];
        let d_to = best[at(site.to)];
        let arg = d_to.and_then(|i| site.args.get(i)).map(|s| self.shown(*s));
        let deep_help = match from.top {
            true => format!(
                "if its depth is bounded by something the compiler cannot see, write \
                 `deep fn {from_name}`"
            ),
            false => format!(
                "if its depth is bounded by something the compiler cannot see, make \
                 `{from_name}` a top-level `deep fn`"
            ),
        };
        let descends_on = |n: usize, d: Option<usize>| match d {
            Some(i) => self.param_name(n, i),
            None => "nothing".to_string(),
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
                        "this call of `{to_name}` passes `{arg}` where `{to_name}` descends \
                         on `{}`",
                        descends_on(site.to, d_to)
                    ),
                )
                .with_note(
                    "a tail call that does not pass a part lets the other calls of the group \
                     go deeper than the data",
                )
                .with_help(deep_help)
            }
            SiteKind::Call { tail: false, .. } => {
                let label = match (&arg, d_from) {
                    (Some(arg), Some(i)) => format!(
                        "not the last thing `{from_name}` does, and `{arg}` is not a part of \
                         `{}`",
                        self.param_name(site.from, i)
                    ),
                    (Some(arg), None) => format!(
                        "not the last thing `{from_name}` does, and `{arg}` is not a part of \
                         a parameter"
                    ),
                    (None, _) => format!(
                        "not the last thing `{from_name}` does, and `{to_name}` takes no \
                         argument that gets smaller"
                    ),
                };
                let mut d = Diagnostic::error(
                    Code::RecursionShape,
                    site.span,
                    format!("this call of `{to_name}` takes a frame for every round"),
                )
                .with_note(label);
                let counted = d_to
                    .and_then(|i| site.args.get(i))
                    .and_then(|s| self.counted_down(*s));
                let carry = "carry the running value in a parameter, so that the call comes last";
                d = match counted {
                    Some(n) => d
                        .with_help(format!(
                            // (The fold the rule offers, by the module's own name)
                            "count with the library: `list.fold(1..{n}, start) {{ acc, i -> ... }}`"
                        ))
                        .with_help(format!("or {carry}")),
                    None => d.with_help(carry),
                };
                d.with_help(deep_help)
            }
        };
        if members.len() > 1 {
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
        self.diagnostics.push(d);
    }

    /// The `n` of an argument `n - 1` (an Int counted down), written as
    /// it is.
    fn counted_down(&self, span: Span) -> Option<String> {
        let text = self.text?.get(span.start as usize..span.end as usize)?;
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
        let go_on = format!("to go on, end {place} with `{}`", self.call_text(m));
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

enum Solved {
    /// Every call is a tail call.
    Tail,
    /// Each member descends on the parameter given (none for one that
    /// takes none).
    Data(Vec<Option<usize>>),
    /// The choice that breaks the rule fewest times, and where it does.
    Refused(Vec<Option<usize>>, Vec<usize>),
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
        ExprKind::Loop { body, .. } => endings(body, goes_on, in_arm, out),
        ExprKind::Call(..) | ExprKind::Pipe(..)
            if !goes_on.contains(&(e.span.start, e.span.end))
                && matches!(e.ty, Some(Type::Unit)) =>
        {
            out.push((e.span, Ending::Call, in_arm));
        }
        _ => {}
    }
}

/// The `list` call `callee` names, if it is one of `ELEMENT_CALLS`: the
/// place of the element among its function's parameters.
fn element_call(callee: &Expr) -> Option<usize> {
    let ExprKind::FieldAccess(module, name, _) = &callee.kind else {
        return None;
    };
    let Some(Res::Module(m)) = module.res else {
        return None;
    };
    if m.builtin_name() != Some("list") {
        return None;
    }
    let name = resolve(*name);
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
