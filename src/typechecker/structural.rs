//! The structural traits (`Display` without a written impl, and the
//! sealed `Equal`, `Compare` and `Hash`). Whether a type has one is the
//! structural judgement's to say (`TypeChecker::structure_gap`), for a
//! method call, an operator, a bound and a supertrait alike; what is
//! settled here is which types have a `Display` impl written for them,
//! which that judgement does not answer for.

use super::*;

impl TypeChecker {
    /// Note the types a `Display` impl is written for (in the module,
    /// or brought in by an import or an earlier REPL cell): their
    /// `Display` is that impl and not the structural one.
    pub(super) fn settle_structural_traits(&mut self, decls: &[Decl]) {
        let display = TraitKey::builtin("Display");
        let mut written: std::collections::HashSet<TypeRef> = std::collections::HashSet::new();
        for decl in decls {
            if let Decl::TraitImpl(ti) = decl
                && self.named_trait(ti.trait_res, ti.trait_name) == Some(display)
                && let Some(target) = self.impl_target(ti)
            {
                written.insert(target);
            }
        }
        let display_method = intern("display");
        for ((type_name, method), entry) in &self.tables.method_table {
            if *method == display_method && !entry.structural && entry.trait_name == Some(display) {
                written.insert(*type_name);
            }
        }
        self.display_written = written
            .iter()
            .map(|ty| canonical_head(&self.tables.resolver, *ty))
            .collect();
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
