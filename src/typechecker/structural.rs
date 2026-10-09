//! The structural traits (`Display` without a written impl, and the
//! sealed `Equal`, `Compare` and `Hash`): which of a module's types have
//! them.

use super::*;

impl TypeChecker {
    /// Settle which of the module's types have the structural traits
    /// (`Display`, `Equal`, `Compare`, `Hash`): note the types a
    /// `Display` impl is written for (in the module, or brought in by an
    /// import or an earlier REPL cell), whose `Display` is that impl and
    /// not the structural one, and take the traits away from the types
    /// whose parts lack them (`enforce_structural_gate`). No impl is
    /// made for a structural trait: the VM has each natively, over the
    /// structure of the value.
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

        let own: std::collections::HashSet<TypeRef> = decls
            .iter()
            .filter_map(|decl| match decl {
                Decl::Type(td) if !matches!(td.body, TypeBody::Alias(_)) => {
                    Some(self.own_type(td.name))
                }
                _ => None,
            })
            .collect();
        self.enforce_structural_gate(&own);
    }

    /// Ask the structural judgement of each of the module's types
    /// (`structural_negatives_of`), then un-stamp `trait_impl_set` /
    /// `method_table` for the pairs a field or payload rules out, and
    /// keep the reasons in `structural_negatives`.
    pub(super) fn enforce_structural_gate(
        &mut self,
        user_type_names: &std::collections::HashSet<TypeRef>,
    ) {
        let negatives = self.structural_negatives_of(user_type_names);

        // Un-stamp the negatives: drop the provisional `trait_impl_set`
        // entry (so `where a: Trait` obligations and supertrait checks
        // reject honestly) and the provisional `method_table` entry (so
        // `.compare()` / `.equal()` / `.hash()` calls are rejected
        // instead of reaching the VM's method for a value that has no
        // sound one).
        for (trait_sym, canon) in negatives.keys() {
            self.tables.trait_impl_set.remove(&(*trait_sym, *canon));
            let method_sym = match resolve(trait_sym.name).as_str() {
                "Equal" => intern("equal"),
                "Compare" => intern("compare"),
                "Hash" => intern("hash"),
                "Display" => intern("display"),
                _ => continue,
            };
            // `method_table` is keyed on the declared (un-canonical)
            // type name; for enum/record decls the canonical name is
            // the declared name, but remove under both to be safe.
            self.tables.method_table.remove(&(*canon, method_sym));
            for name in user_type_names {
                if canonical_head(&self.tables.resolver, *name) == *canon {
                    self.tables.method_table.remove(&(*name, method_sym));
                }
            }
        }

        // Clear any stale negatives for the types processed in this
        // run before storing the fresh results (a REPL session or
        // re-check may redefine a type with now-eligible fields; a
        // leftover negative would spuriously reject it).
        let processed: std::collections::HashSet<TypeRef> = user_type_names
            .iter()
            .map(|n| canonical_head(&self.tables.resolver, *n))
            .collect();
        self.tables
            .structural_negatives
            .retain(|(_, canon), _| !processed.contains(canon));
        self.tables.structural_negatives.extend(negatives);
    }

    /// Which of the four structural traits each of the module's types
    /// does not have, with the reason as a message: the judgement
    /// (`structure_gap`) asked of the type at its own parameters, which
    /// hold whatever a use gives them. `Display` is not asked of a type
    /// with a written impl.
    fn structural_negatives_of(
        &self,
        user_type_names: &std::collections::HashSet<TypeRef>,
    ) -> HashMap<(TraitKey, TypeRef), String> {
        let mut names: Vec<TypeRef> = user_type_names.iter().copied().collect();
        names.sort_by_key(|name| resolve(name.name));
        let mut negatives: HashMap<(TraitKey, TypeRef), String> = HashMap::new();
        for name in names {
            let canon = canonical_head(&self.tables.resolver, name);
            let params: Vec<Type> = match self.tables.enums.get(&name) {
                Some(info) => info.param_var_ids.iter().map(|v| Type::Var(*v)).collect(),
                None => self
                    .tables
                    .record_param_var_ids
                    .get(&name)
                    .map(|ids| ids.iter().map(|v| Type::Var(*v)).collect())
                    .unwrap_or_default(),
            };
            let own = Type::Generic(name, params);
            for tr in ["Equal", "Compare", "Hash", "Display"] {
                let tr = TraitKey::builtin(tr);
                if !self.by_structure(tr, canon) {
                    continue;
                }
                let mut walk = super::solve::Walk::default();
                if let Some(gap) = self.structure_gap(tr, &own, &mut walk, 0) {
                    negatives.insert((tr, canon), self.gap_message(tr, &own, &gap));
                }
            }
        }
        negatives
    }
}
