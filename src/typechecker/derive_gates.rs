use super::*;

impl TypeChecker {
    /// Conservative check: is the field type `ty` known to satisfy
    /// `trait_name` as recorded in `trait_impl_set`? Returns false on
    /// unresolved tyvars or unknown nominal heads. Used by
    /// `synthesize_auto_derive_impls` to decide whether a record / enum
    /// can have a sound auto-derived impl.
    pub(super) fn field_type_supports_trait(&self, trait_name: TraitKey, ty: &Type) -> bool {
        let Some(type_name) = self.type_name_for_impl(ty) else {
            return false;
        };
        let canonical = canonical_head(&self.tables.resolver, type_name);
        self.tables
            .trait_impl_set
            .contains(&(trait_name, canonical))
    }

    /// Round 93: compute honest field-aware eligibility for the three
    /// gated built-in traits (Equal / Compare / Hash) over every
    /// user-declared type, then un-stamp `trait_impl_set` /
    /// `method_table` for the ineligible pairs and store the reasons
    /// in `auto_derive_negatives`. See the call site in
    /// `synthesize_auto_derive_impls` for the full rationale.
    pub(super) fn enforce_auto_derive_field_gate(
        &mut self,
        user_type_names: &std::collections::HashSet<TypeRef>,
    ) {
        let negatives = self.compute_auto_derive_field_negatives(user_type_names);

        // Un-stamp the negatives: drop the provisional `trait_impl_set`
        // entry (so `where a: Trait` obligations and supertrait checks
        // reject honestly) and the provisional auto-derived
        // `method_table` entry (so `.compare()` / `.equal()` / `.hash()`
        // calls are rejected instead of falling through to
        // `dispatch_trait_method`'s Value-level behaviour at runtime).
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
            .auto_derive_negatives
            .retain(|(_, canon), _| !processed.contains(canon));
        self.tables.auto_derive_negatives.extend(negatives);
    }

    /// Which of the four structural traits each of the module's types
    /// does not have, with the reason as a message: the judgement
    /// (`structure_gap`) asked of the type at its own parameters, which
    /// hold whatever a use gives them. `Display` is not asked of a type
    /// with a written impl.
    fn compute_auto_derive_field_negatives(
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
