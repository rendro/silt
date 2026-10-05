use super::*;

/// Round 93: human adjective for the gated built-in traits, used by
/// the field-aware auto-derive gate's diagnostics ("... which is not
/// comparable").
fn builtin_trait_adjective(trait_sym: TraitKey) -> &'static str {
    match resolve(trait_sym.name).as_str() {
        "Compare" => "comparable",
        "Equal" => "equatable",
        "Hash" => "hashable",
        _ => "supported",
    }
}

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

    /// Round 93: fixpoint over the user-declared types computing which
    /// `(trait, type)` pairs canNOT satisfy a gated built-in trait
    /// because of an offending field / variant payload. Returns
    /// `(trait, canonical type name) → full diagnostic message`.
    ///
    /// Termination / recursion notes: each pass may only ADD
    /// negatives and the pair space is finite, so the loop is bounded
    /// by `3 × |types|` passes. Recursive and mutually-recursive
    /// types that are otherwise clean are never added — the walk
    /// reads the CURRENT stamp for nominal heads (coinductive: a
    /// reference cycle with no offending field is sound because
    /// runtime values are finite trees), so `type Tree { leaf: Int,
    /// kids: List(Tree) }` keeps all four traits.
    fn compute_auto_derive_field_negatives(
        &self,
        user_type_names: &std::collections::HashSet<TypeRef>,
    ) -> HashMap<(TraitKey, TypeRef), String> {
        let gated_traits = [
            TraitKey::builtin("Equal"),
            TraitKey::builtin("Compare"),
            TraitKey::builtin("Hash"),
        ];

        // Owned snapshot of each user type's resolved body so the
        // fixpoint can walk without re-borrowing `self.tables.enums` /
        // `self.tables.records`. Sorted by name for deterministic results.
        // (Generic-param fields resolve to `Type::Var`s, which the
        // walker treats as supporting — the synthesized impl's
        // `where p: Trait` clause covers them at instantiation.)
        let mut entries: Vec<(TypeRef, TypeRef, TypeBodyKind)> = Vec::new();
        for name in user_type_names {
            let canon = canonical_head(&self.tables.resolver, *name);
            if let Some(info) = self.tables.enums.get(name) {
                entries.push((*name, canon, TypeBodyKind::Enum(info.variants.clone())));
            } else if let Some(info) = self.tables.records.get(name) {
                entries.push((*name, canon, TypeBodyKind::Record(info.fields.clone())));
            }
        }
        entries.sort_by_key(|(name, ..)| resolve(name.name));

        let mut negatives: HashMap<(TraitKey, TypeRef), String> = HashMap::new();
        loop {
            let mut changed = false;
            for (name, canon, body) in &entries {
                for trait_sym in gated_traits {
                    let key = (trait_sym, *canon);
                    if negatives.contains_key(&key) || !self.tables.trait_impl_set.contains(&key) {
                        continue;
                    }
                    let supports =
                        |fty: &Type| self.gate_field_supports_trait(trait_sym, fty, &negatives, 0);
                    let offending: Option<String> = match body {
                        TypeBodyKind::Record(fields) => fields.iter().find_map(|(fname, fty)| {
                            (!supports(fty)).then(|| {
                                format!(
                                    "field '{}' has type '{}'",
                                    resolve(*fname),
                                    self.apply(fty)
                                )
                            })
                        }),
                        TypeBodyKind::Enum(variants) => variants.iter().find_map(|v| {
                            v.field_types.iter().enumerate().find_map(|(i, fty)| {
                                (!supports(fty)).then(|| {
                                    format!(
                                        "variant '{}' payload #{} has type '{}'",
                                        resolve(v.name),
                                        i + 1,
                                        self.apply(fty)
                                    )
                                })
                            })
                        }),
                    };
                    if let Some(field_desc) = offending {
                        negatives.insert(
                            key,
                            format!(
                                "type '{}' cannot derive '{}': {}, which is not {}",
                                resolve(name.name),
                                resolve(trait_sym.name),
                                field_desc,
                                builtin_trait_adjective(trait_sym),
                            ),
                        );
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        negatives
    }

    /// Round 93: recursive, honest "does this FIELD type satisfy the
    /// gated built-in trait?" check used by the field-aware gate and
    /// by the operator-operand instantiation walk.
    ///
    /// Deliberately permissive arms (over-rejection is the failure
    /// mode to avoid):
    ///   - `Var`: a generic param of the enclosing type (the
    ///     synthesized impl's `where p: Trait` clause covers it at
    ///     the instantiation site) or a not-yet-resolved inference
    ///     var — never provably bad here.
    ///   - `AssocProj`: "maybe valid" exactly like `Var` (round-92
    ///     operand parity).
    ///   - depth cap: give up permissively on absurdly deep types
    ///     rather than risk a stack overflow.
    fn gate_field_supports_trait(
        &self,
        trait_sym: TraitKey,
        ty: &Type,
        negatives: &HashMap<(TraitKey, TypeRef), String>,
        depth: usize,
    ) -> bool {
        if depth > 64 {
            return true;
        }
        let ty = self.apply(ty);
        let recurse = |t: &Type| self.gate_field_supports_trait(trait_sym, t, negatives, depth + 1);
        // Stamp lookup for a nominal/container head, honest w.r.t. the
        // in-progress negatives.
        let head_ok = |head: TypeRef| {
            let canon = canonical_head(&self.tables.resolver, head);
            let key = (trait_sym, canon);
            if negatives.contains_key(&key) {
                return false;
            }
            self.tables.trait_impl_set.contains(&key)
        };
        match &ty {
            Type::Error | Type::Never | Type::Var(_) | Type::AssocProj { .. } => true,
            // Functions support none of Equal/Compare/Hash: the
            // Value-level fallbacks are Arc-pointer identity (equal),
            // Arc-pointer ADDRESS ordering (compare — ASLR-
            // nondeterministic) and a constant tag (hash).
            Type::Fun(..) => false,
            // Channels carry identity-based equality (round 82:
            // `Value::Channel(a) == Value::Channel(b)` iff ids match)
            // but no ordering or hashing through the trait surface.
            Type::Channel(_) => trait_sym == TraitKey::builtin("Equal"),
            Type::List(t) | Type::Range(t) | Type::Set(t) => {
                let head = self
                    .type_name_for_impl(&ty)
                    .expect("container head has canonical name");
                head_ok(head) && recurse(t)
            }
            Type::Map(k, v) => head_ok(TypeRef::builtin("Map")) && recurse(k) && recurse(v),
            Type::Tuple(ts) => head_ok(TypeRef::builtin("Tuple")) && ts.iter().all(recurse),
            // Structural records: Value's PartialEq / Ord / Hash all
            // compare them element-wise (round-85 contracts), so the
            // honest answer is the conjunction over the known fields.
            // Open rows are rejected: the hidden tail could carry
            // anything.
            Type::AnonRecord { fields, tail } => {
                matches!(tail, RowTail::Closed) && fields.values().all(recurse)
            }
            // Nominal heads: the stamp (kept honest by the fixpoint
            // for user types, by registration policy for builtins)
            // decides the head; instantiation args / embedded field
            // types are walked so `Box(Fn(Int) -> Int)` is caught even
            // though `Box(a)` itself is conditionally eligible.
            Type::Record(name, fields) => head_ok(*name) && fields.iter().all(|(_, t)| recurse(t)),
            Type::Generic(name, args) => head_ok(*name) && args.iter().all(recurse),
            // Scalars and anything else: defer to the registered
            // stamp, exactly like the one-level synthesis gate.
            _ => self.field_type_supports_trait(trait_sym, &ty),
        }
    }

    /// Round 93: operator-operand violation check for `==`/`!=`
    /// (`is_equality`) and `<`/`>`/`<=`/`>=` on nominal record / enum
    /// operands. Returns the diagnostic to emit when the operand's
    /// type cannot soundly support the Value-level operation.
    pub(super) fn operand_builtin_trait_violation(
        &self,
        ty: &Type,
        is_equality: bool,
    ) -> Option<String> {
        let trait_sym = if is_equality {
            TraitKey::builtin("Equal")
        } else {
            TraitKey::builtin("Compare")
        };
        let resolved =
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        let (name, args) = match &resolved {
            Type::Generic(name, args) => (*name, args.clone()),
            // Nominal records normally flow as `Type::Generic`, but a
            // `Type::Record` form carries its (instantiated) field
            // types inline — walk them directly.
            Type::Record(name, fields) => {
                let canon = canonical_head(&self.tables.resolver, *name);
                if let Some(msg) = self.tables.auto_derive_negatives.get(&(trait_sym, canon)) {
                    return Some(msg.clone());
                }
                let no_negatives = HashMap::new();
                return fields.iter().find_map(|(fname, fty)| {
                    (!self.gate_field_supports_trait(trait_sym, fty, &no_negatives, 0))
                        .then(|| {
                        format!(
                            "type '{}' cannot derive '{}': field '{}' has type '{}', which is not {}",
                            resolve(name.name),
                            resolve(trait_sym.name),
                            resolve(*fname),
                            self.apply(fty),
                            builtin_trait_adjective(trait_sym),
                        )
                    })
                });
            }
            // Round 97: container HEADS (List/Range/Tuple/Map/Set) pass the
            // structural shape gate in `is_valid_compare_operand`, but the
            // Value-level operation recurses into element types — and the VM
            // fallback for a `Fn`-shaped element is Arc-pointer-address
            // ordering (ASLR-nondeterministic), exactly the bug round 3 fixed
            // for bare `Fn` operands and round 93 fixed for nominal fields.
            // Mirror the round-93 field walk: a container is compare/equality
            // -valid IFF every element / component / value type is itself
            // valid. `gate_field_supports_trait` already does this recursion
            // honestly (and bottoms out at `Type::Fun(..) => false`), so we
            // reuse it and surface the whole container type as the reason.
            Type::List(_) | Type::Range(_) | Type::Tuple(_) | Type::Map(..) | Type::Set(_) => {
                let no_negatives = HashMap::new();
                return (!self.gate_field_supports_trait(trait_sym, &resolved, &no_negatives, 0))
                    .then(|| {
                        format!(
                            "type '{resolved}' cannot derive '{}': element type is not {}",
                            resolve(trait_sym.name),
                            builtin_trait_adjective(trait_sym),
                        )
                    });
            }
            _ => return None,
        };
        let canon = canonical_head(&self.tables.resolver, name);
        if let Some(msg) = self.tables.auto_derive_negatives.get(&(trait_sym, canon)) {
            return Some(msg.clone());
        }
        // Instantiation walk: substitute the concrete type args into
        // the declared field / payload types and re-check. This is
        // the use-site flavour of the declaration-level gate — it
        // catches `Box(Fn(Int) -> Int)` where `type Box(a) { v: a }`
        // is conditionally eligible, while leaving phantom params
        // (`type Tag(a) { name: String }`) unpunished because only
        // the types that actually appear in fields are walked.
        let no_negatives = HashMap::new();
        let check = |fty: &Type| self.gate_field_supports_trait(trait_sym, fty, &no_negatives, 0);
        if let Some(info) = self.tables.records.get(&name) {
            let mapping: HashMap<TyVar, Type> = self
                .tables
                .record_param_var_ids
                .get(&name)
                .filter(|ids| ids.len() == args.len())
                .map(|ids| ids.iter().copied().zip(args.iter().cloned()).collect())
                .unwrap_or_default();
            return info.fields.iter().find_map(|(fname, fty)| {
                let concrete = substitute_vars(fty, &mapping);
                (!check(&concrete)).then(|| {
                    format!(
                        "type '{resolved}' cannot derive '{}': field '{}' has type '{}', which is not {}",
                        resolve(trait_sym.name),
                        resolve(*fname),
                        self.apply(&concrete),
                        builtin_trait_adjective(trait_sym),
                    )
                })
            });
        }
        if let Some(info) = self.tables.enums.get(&name) {
            let mapping: HashMap<TyVar, Type> = if info.param_var_ids.len() == args.len() {
                info.param_var_ids
                    .iter()
                    .copied()
                    .zip(args.iter().cloned())
                    .collect()
            } else {
                HashMap::new()
            };
            return info.variants.iter().find_map(|v| {
                v.field_types.iter().enumerate().find_map(|(i, fty)| {
                    let concrete = substitute_vars(fty, &mapping);
                    (!check(&concrete)).then(|| {
                        format!(
                            "type '{resolved}' cannot derive '{}': variant '{}' payload #{} has type '{}', which is not {}",
                            resolve(trait_sym.name),
                            resolve(v.name),
                            i + 1,
                            self.apply(&concrete),
                            builtin_trait_adjective(trait_sym),
                        )
                    })
                })
            });
        }
        None
    }

    /// Round 93: when a `.equal()` / `.compare()` / `.hash()` call
    /// misses the method table because the field-aware gate removed
    /// the provisional auto-derive entry, surface the precise reason
    /// instead of a generic "unknown field or method".
    pub(super) fn method_auto_derive_violation(
        &self,
        type_name: TypeRef,
        method: Symbol,
    ) -> Option<String> {
        let trait_sym = match resolve(method).as_str() {
            "equal" => TraitKey::builtin("Equal"),
            "compare" => TraitKey::builtin("Compare"),
            "hash" => TraitKey::builtin("Hash"),
            _ => return None,
        };
        let canon = canonical_head(&self.tables.resolver, type_name);
        self.tables
            .auto_derive_negatives
            .get(&(trait_sym, canon))
            .cloned()
    }
}
