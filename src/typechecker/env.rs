use super::*;

// ── Type environment ────────────────────────────────────────────────

/// A typing environment mapping names to type schemes.
#[derive(Debug, Clone)]
pub(crate) struct TypeEnv {
    pub(super) bindings: HashMap<Symbol, Scheme>,
    /// The enclosing scope. Shared, not copied: a child scope is made for
    /// every block and lambda, and the outermost scope holds every
    /// builtin name.
    pub(super) parent: Option<Rc<TypeEnv>>,
    /// Whether no scheme of this scope or its parents has a free type
    /// variable: true of the builtin scope, whose schemes are all
    /// generalized, so `free_vars` need not walk it.
    pub(super) closed: bool,
}

impl TypeEnv {
    pub(super) fn new() -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            parent: None,
            closed: false,
        }
    }

    pub(super) fn child(&self) -> Self {
        TypeEnv::child_of(Rc::new(self.clone()))
    }

    /// An empty scope inside `parent`.
    pub(super) fn child_of(parent: Rc<TypeEnv>) -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            parent: Some(parent),
            closed: false,
        }
    }

    pub(super) fn define(&mut self, name: Symbol, scheme: Scheme) {
        self.bindings.insert(name, scheme);
    }

    pub(super) fn lookup(&self, name: Symbol) -> Option<&Scheme> {
        if let Some(s) = self.bindings.get(&name) {
            Some(s)
        } else if let Some(ref parent) = self.parent {
            parent.lookup(name)
        } else {
            None
        }
    }

    /// Collect all in-scope names into `out`. Walks the scope chain from
    /// innermost (self) to outermost (root), inserting each `Symbol`
    /// once. Used by the "did you mean ...?" suggestion path so the type
    /// checker can enumerate candidate names to match against a typo.
    pub(super) fn collect_names(&self, out: &mut BTreeSet<Symbol>) {
        for k in self.bindings.keys() {
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
        for scheme in self.bindings.values() {
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
