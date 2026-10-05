use super::*;

// ── Type environment ────────────────────────────────────────────────

/// A typing environment mapping names to type schemes: one top-level
/// scope (a module's, or the builtin one) and a stack of local frames
/// over it, one for each function body, closure, block and match arm
/// being checked. A frame is pushed and popped; no scope is copied.
#[derive(Debug, Clone)]
pub(crate) struct TypeEnv {
    /// The top-level scope's names.
    pub(super) bindings: HashMap<Symbol, Scheme>,
    /// Documentation strings for built-in names (stdlib functions,
    /// constants, type/trait declarations, error variants).
    /// Populated alongside `bindings` via `define_with_doc` /
    /// `attach_doc` from the per-module registration sites under
    /// `src/typechecker/builtins/`. Only the root env carries entries.
    /// Surfaced through LSP hover / completion / signature-help via
    /// `pub fn builtin_docs()`.
    pub(super) builtin_docs: HashMap<Symbol, String>,
    /// The enclosing scope: for a module's scope, the builtin one,
    /// which every check shares.
    pub(super) parent: Option<Rc<TypeEnv>>,
    /// The local names in scope, each with the schemes it is bound to
    /// from the outermost frame in: the last is the one in scope.
    locals: HashMap<Symbol, Vec<Scheme>>,
    /// The local names in the order they were bound.
    log: Vec<Symbol>,
    /// Where in `log` each open frame starts.
    frames: Vec<usize>,
    /// Whether no scheme of this scope or its parents has a free type
    /// variable: true of the builtin scope, whose schemes are all
    /// generalized, so `free_vars` need not walk it.
    pub(super) closed: bool,
}

impl TypeEnv {
    pub(super) fn new() -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            builtin_docs: HashMap::new(),
            parent: None,
            locals: HashMap::new(),
            log: Vec::new(),
            frames: Vec::new(),
            closed: false,
        }
    }

    /// An empty top-level scope inside `parent`.
    pub(super) fn child_of(parent: Rc<TypeEnv>) -> Self {
        TypeEnv {
            parent: Some(parent),
            ..TypeEnv::new()
        }
    }

    /// Open a local frame: what is defined until the matching `pop` is
    /// in scope until then.
    pub(super) fn push(&mut self) {
        self.frames.push(self.log.len());
    }

    /// Close the innermost frame and return what it bound, in the order
    /// it was bound.
    pub(super) fn pop(&mut self) -> Vec<(Symbol, Scheme)> {
        let start = self.frames.pop().expect("a frame is open");
        let mut bound = Vec::with_capacity(self.log.len() - start);
        for name in self.log.drain(start..).rev() {
            let stack = self.locals.get_mut(&name).expect("a bound local");
            let scheme = stack.pop().expect("a bound local");
            if stack.is_empty() {
                self.locals.remove(&name);
            }
            bound.push((name, scheme));
        }
        bound.reverse();
        bound
    }

    /// Bind `name` in the innermost open frame, or in the top-level
    /// scope when none is open.
    pub(super) fn define(&mut self, name: Symbol, scheme: Scheme) {
        if self.frames.is_empty() {
            self.bindings.insert(name, scheme);
        } else {
            self.locals.entry(name).or_default().push(scheme);
            self.log.push(name);
        }
    }

    /// Attach a markdown doc string to an already-defined name. Used
    /// by per-module registration sites under
    /// `src/typechecker/builtins/` (via `attach_module_docs`) so LSP
    /// hover / completion / signature-help can render the same prose
    /// that round 62 phase-2 inlined from the former
    /// `docs/stdlib/*.md` files. No-op if the name
    /// has not been `define`d (the doc would never be reachable);
    /// keeps the parity walker robust against renames in the markdown
    /// without requiring the registration site to also be updated in
    /// lockstep.
    pub(super) fn attach_doc(&mut self, name: Symbol, doc: &str) {
        if self.bindings.contains_key(&name) {
            self.builtin_docs.insert(name, doc.to_string());
        }
    }

    pub(super) fn lookup(&self, name: Symbol) -> Option<&Scheme> {
        if let Some(s) = self.locals.get(&name).and_then(|stack| stack.last()) {
            Some(s)
        } else if let Some(s) = self.bindings.get(&name) {
            Some(s)
        } else if let Some(ref parent) = self.parent {
            parent.lookup(name)
        } else {
            None
        }
    }

    /// Collect all in-scope names into `out`: the local ones, the
    /// top-level scope's and its parents', each `Symbol` once. Used by the "did you mean ...?" suggestion path so the type
    /// checker can enumerate candidate names to match against a typo.
    pub(super) fn collect_names(&self, out: &mut BTreeSet<Symbol>) {
        for k in self.locals.keys().chain(self.bindings.keys()) {
            out.insert(*k);
        }
        if let Some(ref parent) = self.parent {
            parent.collect_names(out);
        }
    }

    /// Collect all free type variables in the environment.
    pub(super) fn free_vars(&self, checker: &TypeChecker) -> Vec<TyVar> {
        let mut fvs = Vec::new();
        if self.closed {
            return fvs;
        }
        for scheme in self.locals.values().flatten().chain(self.bindings.values()) {
            let ty = checker.apply(&scheme.ty);
            let mut ty_fvs = free_vars_in(&ty);
            // Remove the scheme's own quantified variables
            ty_fvs.retain(|v| !scheme.vars.contains(v));
            for v in ty_fvs {
                if !fvs.contains(&v) {
                    fvs.push(v);
                }
            }
        }
        if let Some(ref parent) = self.parent {
            for v in parent.free_vars(checker) {
                if !fvs.contains(&v) {
                    fvs.push(v);
                }
            }
        }
        fvs
    }
}
