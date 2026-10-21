//! The structural traits (`Display`, `json.Encode` and `json.Decode`
//! without a written impl, and the sealed `Equal`, `Compare` and
//! `Hash`). Whether a type has one is the structural judgement's to say
//! (`TypeChecker::structure_gap`; for `json.Decode`, the json decoder's,
//! `json_decode`), for a method call, an operator, a bound and a
//! supertrait alike; what is settled here is which types the module
//! writes an impl of one for, which that judgement does not answer for.

use super::*;

impl TypeChecker {
    /// Note the types the module writes a `Display`, `json.Encode` or
    /// `json.Decode` impl for: their trait is that impl and not the
    /// structural one, also before the impl is entered. Only an impl the
    /// checker accepts counts: one written where it may not be
    /// (`impl_is_here`), or a `json.Decode` impl for a generic type, is
    /// no impl, and the type keeps the trait it had. (An impl of another module or of
    /// an earlier REPL cell is in the session's tables: `by_structure`
    /// reads it there.)
    pub(super) fn settle_structural_traits(&mut self, decls: &[Decl]) {
        let mut written = std::collections::HashSet::new();
        for decl in decls {
            if let Decl::TraitImpl(ti) = decl
                && let Some(tr) = self.named_trait(ti.trait_res, ti.trait_name)
                && WRITABLE_STRUCTURAL_TRAITS
                    .iter()
                    .any(|name| tr.is_builtin(name))
                && let Some(target) = self.impl_target(ti)
                && self.impl_is_here(ti, target)
                && !self.decode_impl_refused(tr, canonical_head(&self.tables.resolver, target))
            {
                written.insert((tr, canonical_head(&self.tables.resolver, target)));
            }
        }
        self.written = written;
    }

    /// Whether the type `head` has the trait `tr`, as far as the type
    /// itself says: by its structure at its own parameters (which hold
    /// whatever a use gives them) for a structural trait of a declared
    /// type, by an impl otherwise.
    pub(super) fn head_has_trait(&self, tr: TraitKey, head: TypeRef) -> bool {
        let declared =
            self.tables.records.contains_key(&head) || self.tables.enums.contains_key(&head);
        if !declared || !self.by_structure(tr, head) {
            return self.tables.trait_impl_set.contains(&(tr, head));
        }
        let params: Vec<Type> = match self.tables.enums.get(&head) {
            Some(info) => info.param_var_ids.iter().map(|v| Type::Var(*v)).collect(),
            None => self
                .tables
                .record_param_var_ids
                .get(&head)
                .map(|ids| ids.iter().map(|v| Type::Var(*v)).collect())
                .unwrap_or_default(),
        };
        let mut walk = super::solve::Walk::default();
        self.structure_gap(tr, &Type::Generic(head, params), &mut walk, 0)
            .is_none()
    }
}
