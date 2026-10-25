use super::inference::*;
use super::*;

/// A predicate a use owes (`TypeChecker::want`), until it is solved:
/// checked against the impls once its subject is known, or against the
/// declared bounds of an annotation variable; or taken into the scheme
/// of the definition that is generalised over its subject.
#[derive(Debug, Clone)]
pub(crate) struct Wanted {
    pub(super) goal: Goal,
    pub(super) origin: Origin,
    /// Whether it is checked already.
    pub(super) solved: bool,
    /// Whether a scheme has it: its subject is a variable a definition
    /// is general in (`generalize`), so each use of that owes it.
    pub(super) in_scheme: bool,
    /// For a call of a trait method (`Pred::Via`): the types the call
    /// gives the method, each at the argument that gives it.
    pub(super) places: super::builtin_needs::Places,
    /// For the call of a trait method itself (`Tr.m(x, ..)`, `x.m(..)`):
    /// each parameter of the method's type, the argument that gives it.
    pub(super) arguments: super::builtin_needs::Arguments,
    /// Its node in the module's evidence (`evidence::Ledger`).
    pub(super) ev: super::evidence::EvRef,
}

/// What waits for a type to be known.
#[derive(Debug, Clone)]
pub(crate) enum Goal {
    /// A predicate a use owes.
    Pred(Pred),
    /// `recv.name(args)`, with the result `result`, where the receiver's
    /// type was unknown at the call: the function-typed field it calls
    /// is checked when the receiver's type is known (`select_call`), and
    /// when the definition is generalised if it still is not.
    Select {
        recv: Type,
        name: Symbol,
        args: Vec<Type>,
        result: Type,
    },
    /// `base.{ field: value }` where the type of `base` was unknown at
    /// the update: the field is checked when it is known. It never
    /// enters a scheme: an update of a record nothing decides needs an
    /// annotation.
    Update {
        base: Type,
        field: Symbol,
        value: Type,
    },
    /// `operand?`, with the result `ok`, in a function that returns `ret`
    /// (none outside any), where the operand's type was unknown. Like an
    /// update, it never enters a scheme.
    Try {
        operand: Type,
        ok: Type,
        ret: Option<Type>,
    },
    /// `result` is the anonymous record made from the record `base`:
    /// its fields but those named `without`, and the fields `with`
    /// (a spread `{...base, f: e}`; the rest binder of a record pattern,
    /// `{f, ...rest}`). What it is waits until the type of `base` is
    /// decided: a declared record gives its fields, an anonymous one
    /// its own. A base that is still unknown, or an open row, when its
    /// scope closes is a record over a row that no declared record may
    /// stand for (`Pred::Anon`).
    Rebuild {
        base: Type,
        without: Vec<Symbol>,
        with: Vec<(Symbol, Type)>,
        result: Type,
        /// Whether it is a pattern's rest (the pattern has reported a
        /// scrutinee that is no record).
        rest: bool,
    },
    /// A use of a scheme writes the record type `{fields, ...row}` and
    /// another with the same row: if a declared record stands for the
    /// row, each of `fields` is that record's field, at its type.
    Listed {
        fields: std::collections::BTreeMap<Symbol, Type>,
        row: Type,
    },
}

impl Goal {
    /// The type the goal waits for.
    pub(super) fn waits_on(&self) -> &Type {
        match self {
            Goal::Pred(pred) => pred.subject(),
            Goal::Select { recv, .. } => recv,
            Goal::Update { base, .. } => base,
            Goal::Try { operand, .. } => operand,
            Goal::Listed { row, .. } => row,
            // (Or the row of an open record: `rebuild_waits`.)
            Goal::Rebuild { base, .. } => base,
        }
    }
}

/// How many types deep the structural judgement looks (see
/// `structure_gap`).
const STRUCTURE_DEPTH: usize = 100;

const TOO_DEEP: &str = "it is nested too deeply, or its definition recurs at ever larger types";

/// What one structural judgement has gathered so far.
#[derive(Default)]
pub(super) struct Walk {
    /// The nominal types looked at, each at its arguments: a type that
    /// comes up again is not looked at twice.
    seen: Vec<(TypeRef, Vec<Type>)>,
    /// The parts whose type is unknown, or an annotation variable.
    pub(super) open: Vec<Type>,
    /// The parts that have a written `Display` impl.
    pub(super) written: Vec<Type>,
}

/// The part of a type that lacks a structural trait.
pub(super) struct Gap {
    /// The way to the part from the type: `field 'f'`, `element`.
    path: Vec<String>,
    /// The head of the part's type (`Fn`), and the type.
    head: String,
    full: String,
    /// Why, when it is not simply that the part's type has no such
    /// trait.
    why: Option<&'static str>,
}

impl Gap {
    /// Whether it is the type itself that lacks the trait, not a part
    /// of it.
    pub(super) fn is_whole(&self) -> bool {
        self.path.is_empty() && self.why.is_none()
    }
}

/// The `op` of the `Origin` of what a pin pattern (`^x`) owes: it
/// compares as `==` does.
pub(super) const PIN: &str = "^";

/// Where a predicate is owed, and what asked for it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Origin {
    /// The use: a call, a name used as a value.
    pub(super) span: Span,
    /// The name of what is used, for a message (`hello`, `list.sort`).
    pub(super) callee: Option<Symbol>,
    /// The operator that owes it, as a message names it (`'+'`), when
    /// the use is an operator's operand.
    pub(super) op: Option<&'static str>,
}

impl TypeChecker {
    /// A use owes `pred`. A subject that is known is checked now; an
    /// annotation variable must have the bound declared. A subject still
    /// unknown waits (`solve_wanted`): by the time the definitions being
    /// checked are done it is known, or it is generalised and the
    /// predicate is the scheme's (`generalize`), or it belongs to an
    /// outer binding and waits on.
    pub(super) fn want(&mut self, pred: Pred, origin: Origin) -> Option<super::evidence::EvId> {
        let pred = match pred {
            Pred::Given {
                subject,
                given,
                written,
                passer,
                body,
                ..
            } => {
                let passer = self.passer_at(passer, &origin);
                self.unplaced.push(self.given_waiting.len());
                self.given_waiting.push(super::builtin_needs::GivenUse {
                    given,
                    subject,
                    written,
                    passer,
                    body,
                    in_body: self.body_owner,
                    origin,
                    placed: false,
                    use_id: self.use_id,
                    taken: false,
                });
                return None;
            }
            Pred::Builtin {
                check,
                subject,
                passer: Some(passer),
            } => Pred::Builtin {
                check,
                subject,
                passer: Some(self.passer_at(passer, &origin)),
            },
            Pred::Via {
                tr,
                method,
                receiver,
                method_ty,
                passer: Some(passer),
                at,
            } => Pred::Via {
                tr,
                method,
                receiver,
                method_ty,
                passer: Some(self.passer_at(passer, &origin)),
                at,
            },
            other => other,
        };
        let decoder = match &pred {
            Pred::Builtin {
                check: BuiltinCheck::Decodes { .. },
                ..
            } => true,
            Pred::Trait { tr, .. } => Self::is_decode(*tr),
            _ => false,
        };
        let k = self.wanted.len();
        let ev = self.want_goal(Goal::Pred(pred), origin);
        if decoder && !self.wanted[k].solved {
            self.waiting_decoders.push(k);
        }
        if let Goal::Pred(Pred::Via { .. }) = self.wanted[k].goal {
            self.waiting_vias.push(k);
        }
        Some(ev)
    }

    /// `goal` waits for its type, or is decided now if the type is known.
    /// Its node in the module's evidence.
    pub(super) fn want_goal(&mut self, goal: Goal, origin: Origin) -> super::evidence::EvId {
        let ev = self.ev_new(super::evidence::EvNode::Pending);
        self.wanted.push(Wanted {
            goal,
            origin,
            solved: false,
            in_scheme: false,
            places: Vec::new(),
            arguments: Vec::new(),
            ev: self.ev_here(ev),
        });
        self.solve_wanted(self.wanted.len() - 1);
        ev
    }

    /// Check each predicate owed since `from` whose subject is known by
    /// now. What the impl that answers one asks of the subject's parts
    /// (`trait Greet for Box(a) where a: Greet`) is owed in turn, by the
    /// same use: checked here if the part is known, waiting like any
    /// predicate if it is not.
    /// The key of the trait arguments `args` of an impl (`ImplArgs`):
    /// their canonical text, aliases expanded, each type variable
    /// numbered as it first appears.
    pub(super) fn impl_args_key(&self, args: &[Type]) -> ImplArgs {
        if args.is_empty() {
            return ImplArgs::none();
        }
        let mut vars: Vec<TyVar> = Vec::new();
        let parts: Vec<String> = (args.iter())
            .map(|arg| {
                let arg =
                    crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(arg));
                key_text(&arg, &mut vars)
            })
            .collect();
        ImplArgs::of(&parts.join(","))
    }

    /// Whether the types `a` and `b` could be one: unified each with a
    /// substitution of its own (no variable of the session is bound),
    /// aliases expanded. The substitution, if they could.
    pub(super) fn could_unify(&self, a: &[Type], b: &[Type]) -> Option<HashMap<TyVar, Type>> {
        if a.len() != b.len() {
            return None;
        }
        let canon = |ty: &Type| {
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty))
        };
        let mut subst: HashMap<TyVar, Type> = HashMap::new();
        (a.iter().zip(b))
            .all(|(a, b)| probe_unify(&canon(a), &canon(b), &mut subst))
            .then_some(subst)
    }

    /// The impls of `tr` for the type `head` whose trait arguments could
    /// be the use's `args`.
    pub(super) fn impls_fitting(&self, tr: TraitKey, head: TypeRef, args: &[Type]) -> Vec<ImplKey> {
        let impls = self.tables.impl_args_of(tr, head);
        if args.is_empty() {
            return impls.iter().map(|key| (tr, head, *key)).collect();
        }
        let args: Vec<Type> = (args.iter())
            .map(|arg| {
                crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(arg))
            })
            .collect();
        let index = self.impl_index(tr, head);
        // (Only the impls whose first trait argument has the use's outer
        // type, or is a variable, are unified with the use's.)
        let heads: Vec<Option<OuterHead>> = args.iter().map(outer_head).collect();
        let mut at: Vec<usize> = match heads.first().copied().flatten() {
            Some(first) => (index.by_first.get(&Some(first)).into_iter().flatten())
                .chain(index.by_first.get(&None).into_iter().flatten())
                .copied()
                .collect(),
            None => (0..index.impls.len()).collect(),
        };
        at.sort_unstable();
        (at.into_iter())
            .map(|i| &index.impls[i])
            .filter(|(_, written)| {
                written.is_empty() || {
                    let apart = (written.iter().zip(&heads)).any(|(written, head)| {
                        head.is_some_and(|head| outer_head(written).is_some_and(|w| w != head))
                    });
                    let mut subst: HashMap<TyVar, Type> = HashMap::new();
                    !apart
                        && written.len() == args.len()
                        && (args.iter().zip(written.iter()))
                            .all(|(arg, written)| probe_unify(arg, written, &mut subst))
                }
            })
            .map(|(key, _)| *key)
            .collect()
    }

    /// The impls of `tr` for the type `head`, as `impls_fitting` reads
    /// them (`ImplIndex`), kept for the module's other uses while no impl
    /// is added: not while an impl's trait arguments are not known yet.
    fn impl_index(&self, tr: TraitKey, head: TypeRef) -> std::rc::Rc<ImplIndex> {
        let impls = self.tables.impl_args_of(tr, head);
        if let Some(found) = self.impl_index.borrow().get(&(tr, head))
            && found.impls.len() == impls.len()
        {
            return found.clone();
        }
        let mut index = ImplIndex::default();
        let mut known = true;
        for (i, args) in impls.iter().enumerate() {
            let key = (tr, head, *args);
            let canon: std::rc::Rc<[Type]> = match self.tables.impl_trait_args.get(&key) {
                Some(written) => (written.iter())
                    .map(|arg| {
                        crate::types::canonical::canonicalize(
                            &self.tables.resolver,
                            &self.apply(arg),
                        )
                    })
                    .collect(),
                None => {
                    known = false;
                    std::rc::Rc::from([])
                }
            };
            let first = canon.first().and_then(outer_head);
            index.by_first.entry(first).or_default().push(i);
            index.impls.push((key, canon));
        }
        let index = std::rc::Rc::new(index);
        if known {
            self.impl_index
                .borrow_mut()
                .insert((tr, head), index.clone());
        }
        index
    }

    /// The impl of `tr` for the type `head` a use at the trait arguments
    /// `args` reaches: the one impl whose trait arguments could be the
    /// use's. None where none could, or several still could.
    pub(super) fn impl_reached(
        &self,
        tr: TraitKey,
        head: TypeRef,
        args: &[Type],
    ) -> Option<ImplKey> {
        match self.impls_fitting(tr, head, args).as_slice() {
            [key] => Some(*key),
            _ => None,
        }
    }

    /// Whether a trait argument of a use is a type variable nothing has
    /// decided yet.
    pub(super) fn trait_args_open(&self, args: &[Type]) -> bool {
        (args.iter()).any(|arg| matches!(self.apply(arg), Type::Var(_)))
    }

    /// Whether `subject: tr(args)` waits for its trait arguments: the
    /// impls of the trait for the subject's type that could be the use's
    /// are several, or one that would decide a part the use leaves open
    /// (`From(List(Int))` for a use at `From(List(_))`), or one while an
    /// argument is still a type variable nothing decided; so which impl
    /// the use reaches is not known yet. (Its trait arguments choose the
    /// impl, never its being the only one: a second impl never changes
    /// what a use that compiled means.)
    pub(super) fn waits_for_trait_args(&self, tr: TraitKey, subject: &Type, args: &[Type]) -> bool {
        if args.is_empty() || (args.iter()).any(|arg| matches!(self.apply(arg), Type::Error)) {
            return false;
        }
        // (Trait arguments with no variable left fit one impl at most,
        // and leave it nothing to decide.)
        let open = self.trait_args_open(args);
        if !open && (args.iter()).all(|arg| free_vars_in(&self.apply(arg)).is_empty()) {
            return false;
        }
        let subject =
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(subject));
        let Some(head) = self.type_name_for_impl(&subject) else {
            return false;
        };
        if self.tables.impl_args_of(tr, head).is_empty() {
            return false;
        }
        if open {
            return true;
        }
        match self.impls_fitting(tr, head, args).as_slice() {
            [] => false,
            [key] => {
                let written = (self.tables.impl_trait_args.get(key)).map_or(&[][..], Vec::as_slice);
                !self.fixes_nothing(written, args)
            }
            _ => true,
        }
    }

    /// Whether the impl's trait arguments `written` are the use's `args`
    /// with nothing the use leaves open decided by them: a type variable
    /// of the use meets only one of the impl's, which stands for it.
    fn fixes_nothing(&self, written: &[Type], args: &[Type]) -> bool {
        let canon = |ty: &Type| {
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty))
        };
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        written.len() == args.len()
            && (written.iter().zip(args))
                .all(|(written, arg)| match_one_way(&canon(written), &canon(arg), &mut mapping))
    }

    pub(super) fn solve_wanted(&mut self, from: usize) {
        let mut i = from;
        while i < self.wanted.len() {
            // (What still waits is passed over as it is: a trait method's
            // call waits for its scope's end, a use for its trait
            // arguments.)
            let waits = match &self.wanted[i].goal {
                _ if self.wanted[i].solved => true,
                Goal::Pred(Pred::Via { .. }) => true,
                Goal::Pred(Pred::Trait {
                    tr, args, subject, ..
                }) => {
                    let subject = self.apply(subject);
                    matches!(subject, Type::Var(_))
                        || self.waits_for_trait_args(*tr, &subject, args)
                }
                _ => false,
            };
            if !waits {
                let origin = self.wanted[i].origin;
                match self.wanted[i].goal.clone() {
                    Goal::Pred(Pred::Trait {
                        tr,
                        args,
                        subject,
                        asker,
                    }) => {
                        // (Decided: it waited for its subject and trait
                        // arguments above.)
                        let subject = self.apply(&subject);
                        self.wanted[i].solved = true;
                        // One error for one type at one place: a use that
                        // owes two traits its type has neither of
                        // (`test.assert_eq(f, f)`) says the first.
                        let reported = self.errors.len();
                        let ev = Some(self.wanted[i].ev);
                        self.verify_trait_obligation(tr, &args, &subject, origin, asker, ev);
                        if self.errors.len() > reported {
                            let key = (origin.span, self.show_type(&subject));
                            if !self.lacking.insert(key) {
                                self.errors.truncate(reported);
                            }
                        }
                    }
                    Goal::Pred(Pred::Anon { row, given }) => match self.apply(&row) {
                        Type::Var(_) => {}
                        // (The row of an open record is the rest of
                        // the row; the fields it has by now are looked
                        // at when the signature's body is known.)
                        Type::AnonRecord {
                            fields,
                            tail: RowTail::Var(rest),
                        } => {
                            if let Some(given) = given {
                                let seen = Type::AnonRecord {
                                    fields,
                                    tail: RowTail::Closed,
                                };
                                self.anon_waiting.push((given, seen, origin));
                            }
                            self.wanted[i].goal = Goal::Pred(Pred::Anon {
                                row: Type::Var(rest),
                                given,
                            });
                        }
                        row => {
                            self.wanted[i].solved = true;
                            match given {
                                Some(given) => self.anon_waiting.push((given, row, origin)),
                                None => self.row_is_anon(&row, origin),
                            }
                        }
                    },
                    Goal::Rebuild {
                        base,
                        without,
                        with,
                        result,
                        rest,
                    } => {
                        if self.rebuild_waits(&base).is_none() {
                            self.wanted[i].solved = true;
                            self.rebuild(&base, &without, &with, &result, rest, origin);
                        }
                    }
                    Goal::Listed { fields, row } => match self.apply(&row) {
                        Type::Var(_) => {}
                        Type::AnonRecord {
                            tail: RowTail::Var(rest),
                            ..
                        } => {
                            self.wanted[i].goal = Goal::Listed {
                                fields,
                                row: Type::Var(rest),
                            };
                        }
                        row => {
                            self.wanted[i].solved = true;
                            self.listed_fields_are(&fields, &row, origin);
                        }
                    },
                    Goal::Select {
                        recv,
                        name,
                        args,
                        result,
                    } => {
                        if !matches!(self.apply(&recv), Type::Var(_)) {
                            self.wanted[i].solved = true;
                            self.select_call(&recv, name, args, result, origin.span);
                        }
                    }
                    Goal::Try { operand, ok, ret } => {
                        let operand = self.apply(&operand);
                        if !matches!(operand, Type::Var(_)) {
                            self.wanted[i].solved = true;
                            self.try_operand(&operand, &ok, ret, origin.span);
                        }
                    }
                    Goal::Pred(Pred::Builtin {
                        check,
                        subject,
                        passer,
                    }) => {
                        let subject = self.apply(&subject);
                        if !matches!(subject, Type::Var(_)) {
                            self.wanted[i].solved = true;
                            self.check_builtin_argument(check, &subject, passer, origin);
                        }
                    }
                    // A trait method's call waits for its scope's end: a
                    // closure the call is in may be general in the types
                    // it gives (`generalize`); `settle_bounds` decides the
                    // rest.
                    Goal::Pred(Pred::Via { .. }) => {}
                    // (Never wanted: `given_waiting`.)
                    Goal::Pred(Pred::Given { .. }) => self.wanted[i].solved = true,
                    Goal::Pred(Pred::Lacks { row, field }) => {
                        let row = self.apply(&row);
                        if !matches!(row, Type::Var(_)) {
                            self.wanted[i].solved = true;
                            self.row_lacks(&row, field, origin);
                        }
                    }
                    Goal::Update { base, field, value } => {
                        let base = self.apply(&base);
                        if !matches!(base, Type::Var(_)) {
                            self.wanted[i].solved = true;
                            self.update_field(&base, field, &value, origin.span);
                        }
                    }
                }
            }
            i += 1;
        }
    }

    /// Decide what `recv.field(args)` calls, now that the receiver's type
    /// is known: the function a field of the record holds.
    fn select_call(
        &mut self,
        recv: &Type,
        field: Symbol,
        args: Vec<Type>,
        result: Type,
        span: Span,
    ) {
        let result_ty = Type::Fun(args, Box::new(result));
        let resolved =
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(recv));
        match &resolved {
            Type::Error | Type::Never => {}
            Type::Var(_) => unreachable!("a selection waits while its receiver is unknown"),
            Type::AnonRecord { fields: af, tail } => {
                if let Some(field_ty) = af.get(&field) {
                    let ft = field_ty.clone();
                    self.unify(&result_ty, &ft, span);
                } else if let RowTail::Var(_) = tail {
                    let extended = Type::AnonRecord {
                        fields: std::collections::BTreeMap::from([(field, result_ty.clone())]),
                        tail: RowTail::Var(self.fresh_tyvar_id()),
                    };
                    self.unify(&resolved, &extended, span);
                } else {
                    let candidates: Vec<(Symbol, Type)> =
                        af.iter().map(|(k, v)| (*k, v.clone())).collect();
                    self.no_field(&resolved, field, None, &candidates, span);
                }
            }
            Type::Generic(type_name, type_args) => {
                // User-declared records with or without type parameters
                // are represented as Type::Generic(name, args). Look up
                // the record definition and validate the field.
                let type_name = *type_name;
                if self.tables.records.contains_key(&type_name)
                    && let Some((_, ft)) = self
                        .instantiate_record_fields_with_args(type_name, type_args)
                        .into_iter()
                        .find(|(n, _)| *n == field)
                {
                    let field_ty = self.apply(&ft);
                    self.unify(&result_ty, &field_ty, span);
                    return;
                }
                let fields = (self.tables.records.get(&type_name))
                    .map(|rec| rec.fields.clone())
                    .unwrap_or_default();
                self.no_field(&resolved, field, None, &fields, span);
            }
            // Any other type has no fields.
            _ => self.no_field(&resolved, field, None, &[], span),
        }
    }

    /// Check `operand?` now that the operand's type is known: it is a
    /// Result or an Option, whose payload is `result_ty`, and the
    /// function it is in returns the same wrapper.
    pub(super) fn try_operand(
        &mut self,
        resolved: &Type,
        result_ty: &Type,
        expected_ret: Option<Type>,
        span: Span,
    ) {
        let (head, args) = match &resolved {
            Type::Error | Type::Never => return,
            Type::Generic(name, args) if name.is_builtin("Result") && args.len() == 2 => {
                ("Result", args.clone())
            }
            Type::Generic(name, args) if name.is_builtin("Option") && args.len() == 1 => {
                ("Option", args.clone())
            }
            other => {
                self.error(
                    Code::InvalidQuestion,
                    format!(
                        "'?' operator requires Result or Option type, got '{}'",
                        self.show_type(other)
                    ),
                    span,
                );
                return;
            }
        };
        // The unwrapped Ok/Some payload is what flowed into the
        // surrounding expression (t1 = got, t2 = expected).
        self.unify(&args[0], result_ty, span);
        let Some(ret) = expected_ret else {
            self.error(
                Code::InvalidQuestion,
                "? operator can only be used inside a function that returns Result or Option"
                    .to_string(),
                span,
            );
            return;
        };
        let ret_resolved = self.apply(&ret);
        let expected_wrapper = if head == "Result" {
            let fresh_ok = self.fresh_var();
            Type::builtin("Result", vec![fresh_ok, args[1].clone()])
        } else {
            let fresh_inner = self.fresh_var();
            Type::option(fresh_inner)
        };
        match &ret_resolved {
            Type::Error | Type::Never => {}
            // Return type still open (or already the right wrapper):
            // constrain it, exactly like the inline concrete path.
            Type::Var(_) => {
                self.unify(&ret_resolved, &expected_wrapper, span);
                self.qmark_decided.insert(span, resolved.clone());
            }
            // A Result or an Option, of either wrapper: a `?` of the
            // other one is the mismatch the inline path reports.
            Type::Generic(n, _) if n.is_builtin("Result") || n.is_builtin("Option") => {
                let before = self.errors.len();
                self.unify(&ret_resolved, &expected_wrapper, span);
                // An Option's `?` in a function that returns a Result:
                // the None has no error to return.
                if head == "Option" && n.is_builtin("Result") {
                    for d in &mut self.errors[before..] {
                        d.help.push(
                            "turn the Option into a Result first: `|> option.to_result(error)?`"
                                .to_string(),
                        );
                    }
                }
            }
            // Concrete non-matching return type: this is the unsound
            // lambda repro (`{ x -> x? + 1 }` whose return type
            // resolved to Int). Curated message, same class as the
            // inline no-context error.
            other => {
                self.errors.push(
                    Diagnostic::error(
                        Code::InvalidQuestion,
                        span,
                        "? operator can only be used inside a function that returns Result or Option",
                    )
                    .with_note(format!(
                        "the ?-ed expression is {}, but the enclosing function returns {}",
                        self.show_type(resolved),
                        self.show_type(other)
                    )),
                );
            }
        }
    }

    /// Report that the record a spread extends with `field` has the
    /// field already: the one wording.
    pub(super) fn extends_existing(&mut self, field: Symbol, span: Span) {
        if !self.lacking.insert((span, format!("field {field}"))) {
            return;
        }
        self.errors.push(
            Diagnostic::error(
                Code::TypeMismatch,
                span,
                format!("cannot extend the record with field '{field}': it has one already"),
            )
            .with_help(format!(
                "a record is extended, never overwritten: update the field with `r.{{ {field}: ... }}`"
            )),
        );
    }

    /// Report that the declared record `record` is given for a row a
    /// definition makes an anonymous record from: once for one record at
    /// one place.
    fn spread_through(&mut self, record: &Type, origin: Origin) {
        let shown = self.show_type(record);
        if !self.lacking.insert((origin.span, shown.clone())) {
            return;
        }
        let through = match origin.callee {
            Some(callee) => format!("'{callee}'"),
            None => "this function".to_string(),
        };
        self.errors.push(Diagnostic::error(
            Code::TypeMismatch,
            origin.span,
            format!(
                "cannot spread a `{shown}` through {through}: convert it where its type is known, `{{...p}}`"
            ),
        ));
    }

    /// Check that no declared record stands for `row`, known now
    /// (`Pred::Anon`).
    fn row_is_anon(&mut self, row: &Type, origin: Origin) {
        match row {
            Type::AnonRecord {
                tail: RowTail::Rigid(r),
                ..
            }
            | Type::Rigid(r) => self.anon_rigid(*r, origin),
            Type::Generic(name, _) if self.tables.records.contains_key(name) => {
                self.spread_through(row, origin)
            }
            _ => {}
        }
    }

    /// Whether a body makes an anonymous record from the row variable
    /// `var` of its function's signature.
    pub(super) fn is_anon_row(&self, var: TyVar) -> bool {
        self.anon_rows.contains(&var) || self.anon_rows.contains(&self.rigid_rep_var(var))
    }

    /// Whether `var` is a row variable of a function's signature.
    fn is_sig_row(&self, var: TyVar) -> bool {
        self.sig_rows.contains(&var) || self.sig_rows.contains(&self.rigid_rep_var(var))
    }

    /// The fields a body adds over the row variable `var` of its
    /// function's signature.
    pub(super) fn adds_of(&self, var: TyVar) -> Vec<Symbol> {
        let mut adds = self.row_adds.get(&var).cloned().unwrap_or_default();
        if let Some(more) = self.row_adds.get(&self.rigid_rep_var(var)) {
            adds.extend(
                more.iter()
                    .filter(|f| !adds.contains(f))
                    .copied()
                    .collect::<Vec<_>>(),
            );
        }
        adds
    }

    /// A body makes an anonymous record from a record whose row is the
    /// annotation variable `r`, with the fields `adds` added. For a row
    /// variable of a function's signature, the function's uses owe that
    /// no declared record stands for it and that what does has none of
    /// the fields; no other annotation can ask that. Whether it is news.
    fn mark_sig_row(&mut self, var: TyVar, adds: &[Symbol]) -> bool {
        let rep = self.rigid_rep_var(var);
        let mut news = self.anon_rows.insert(var);
        news |= self.anon_rows.insert(rep);
        for key in [var, rep] {
            let known = self.row_adds.entry(key).or_default();
            for field in adds {
                if !known.contains(field) {
                    known.push(*field);
                    news = true;
                }
            }
        }
        news
    }

    /// See `mark_sig_row`: reports the row of another annotation.
    pub(super) fn anon_rigid(&mut self, r: RigidId, origin: Origin) {
        if self.is_sig_row(r.var) {
            self.mark_sig_row(r.var, &[]);
            return;
        }
        if !self
            .lacking
            .insert((origin.span, format!("row {}", r.name)))
        {
            return;
        }
        self.errors.push(
            Diagnostic::error(
                Code::TypeMismatch,
                origin.span,
                format!(
                    "cannot spread a record of the row `{}` here: a declared record may stand for the row, and a spread makes an anonymous record",
                    r.name
                ),
            )
            .with_help(
                "only a function's own signature keeps declared records from a row its body spreads: do the spread in a function",
            ),
        );
    }

    /// Decide what waited for the bodies of the module's functions:
    /// whether what stands for a row variable of a signature is spread
    /// by the body, and with which fields added.
    pub(super) fn settle_rows(&mut self) {
        let waiting = std::mem::take(&mut self.anon_waiting);
        // What a body does to its row it does to the row of a signature
        // that is given for it.
        loop {
            let mut news = false;
            for (given, row, _) in &waiting {
                if !self.is_anon_row(*given) {
                    continue;
                }
                let (fields, to) = match row {
                    Type::AnonRecord {
                        fields,
                        tail: RowTail::Rigid(r),
                    } => (fields.keys().copied().collect(), r.var),
                    Type::Rigid(r) => (Vec::new(), r.var),
                    _ => continue,
                };
                if self.is_sig_row(to) {
                    let adds: Vec<Symbol> = self
                        .adds_of(*given)
                        .into_iter()
                        .filter(|f| !fields.contains(f))
                        .collect();
                    news |= self.mark_sig_row(to, &adds);
                }
            }
            if !news {
                break;
            }
        }
        for (given, row, origin) in waiting {
            if !self.is_anon_row(given) {
                continue;
            }
            self.row_is_anon(&row, origin);
            if let Type::AnonRecord { fields, .. } = &row {
                for field in self.adds_of(given) {
                    if fields.contains_key(&field) {
                        self.extends_existing(field, origin.span);
                    }
                }
            }
        }
    }

    /// The variable a `Goal::Rebuild` over `base` waits for: the type
    /// of the base, or the row of the open record it is.
    pub(super) fn rebuild_waits(&self, base: &Type) -> Option<TyVar> {
        match self.apply(base) {
            Type::Var(v) => Some(v),
            Type::AnonRecord {
                tail: RowTail::Var(v),
                ..
            } => Some(v),
            _ => None,
        }
    }

    /// Decide `Goal::Rebuild`: the type of `base` is decided, or its
    /// scope closes.
    fn rebuild(
        &mut self,
        base: &Type,
        without: &[Symbol],
        with: &[(Symbol, Type)],
        result: &Type,
        rest: bool,
        origin: Origin,
    ) {
        use std::collections::BTreeMap;
        let span = origin.span;
        let mut base_ty =
            crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(base));
        // A base nothing has decided is a record with a row of its own.
        if matches!(base_ty, Type::Var(_)) {
            let open = Type::AnonRecord {
                fields: BTreeMap::new(),
                tail: RowTail::Var(self.fresh_tyvar_id()),
            };
            self.unify(&base_ty, &open, span);
            base_ty = self.apply(&open);
        }
        let (mut fields, tail): (BTreeMap<Symbol, Type>, RowTail) = match &base_ty {
            // A declared record gives its fields.
            Type::Generic(name, args) if self.tables.records.contains_key(name) => (
                self.instantiate_record_fields_with_args(*name, args)
                    .into_iter()
                    .collect(),
                RowTail::Closed,
            ),
            Type::AnonRecord { fields, tail } => (fields.clone(), tail.clone()),
            other => {
                if !rest && !matches!(other, Type::Error | Type::Never) {
                    self.error(
                        Code::TypeMismatch,
                        format!(
                            "spread requires a record base, but '{}' is not a record type",
                            self.show_type(other)
                        ),
                        span,
                    );
                }
                let _ = self.unify_types(result, &Type::Error);
                return;
            }
        };
        for name in without {
            fields.remove(name);
        }
        // A record is extended, never overwritten.
        for (name, ty) in with {
            if fields.insert(*name, ty.clone()).is_some() {
                self.extends_existing(*name, span);
            }
        }
        // What is made is anonymous whatever the row turns out to be: a
        // place that wants a declared record is not given one.
        let wanted = self.apply(result);
        if let Type::Generic(name, _) = &wanted
            && self.tables.records.contains_key(name)
            && matches!(tail, RowTail::Var(_))
        {
            let made = Type::AnonRecord { fields, tail };
            let shown = self.written_type(*name);
            self.errors.push(
                Diagnostic::error(
                    Code::TypeMismatch,
                    span,
                    format!(
                        "type mismatch: expected {}, got {}",
                        self.show_type(&wanted),
                        self.show_type(&made)
                    ),
                )
                .with_help(format!(
                    "an anonymous record is not a `{shown}`: write `{shown} {{ ... }}`"
                )),
            );
            return;
        }
        match &tail {
            RowTail::Closed => {}
            RowTail::Rigid(r) => {
                self.anon_rigid(*r, origin);
                for (name, _) in with {
                    self.lacks_rigid(*r, *name);
                }
            }
            RowTail::Var(row) => {
                for (name, _) in with {
                    self.want(
                        Pred::Lacks {
                            row: Type::Var(*row),
                            field: *name,
                        },
                        origin,
                    );
                }
                self.want(
                    Pred::Anon {
                        row: Type::Var(*row),
                        given: None,
                    },
                    origin,
                );
            }
        }
        self.unify(&Type::AnonRecord { fields, tail }, result, span);
    }

    /// Give each `Goal::Rebuild` of the scope just left whose base is
    /// still undecided, and the scope's to generalise, a row of its
    /// own, in the order they were written (a rest binder before the
    /// spread over it).
    pub(super) fn default_rebuilds(&mut self) {
        let mut i = self.closed_mark;
        while i < self.wanted.len() {
            if !self.wanted[i].solved
                && let Goal::Rebuild {
                    base,
                    without,
                    with,
                    result,
                    rest,
                } = self.wanted[i].goal.clone()
                && self
                    .rebuild_waits(&base)
                    .is_none_or(|v| self.tables.vars.is_generalizable(v))
            {
                let origin = self.wanted[i].origin;
                self.wanted[i].solved = true;
                // The variables this makes are the scope's.
                self.reopen_level();
                self.rebuild(&base, &without, &with, &result, rest, origin);
                self.solve_wanted(self.closed_mark);
                self.close_level();
            }
            i += 1;
        }
    }

    /// Check a record type a scheme's use writes over a row a declared
    /// record stands for (`Goal::Listed`).
    fn listed_fields_are(
        &mut self,
        fields: &std::collections::BTreeMap<Symbol, Type>,
        row: &Type,
        origin: Origin,
    ) {
        let Type::Generic(name, args) = row else {
            return;
        };
        if !self.tables.records.contains_key(name) {
            return;
        }
        let declared = self.instantiate_record_fields_with_args(*name, args);
        for (field, ty) in fields {
            let Some((_, declared_ty)) = declared.iter().find(|(n, _)| n == field) else {
                continue;
            };
            if self.unify_types(ty, declared_ty).is_err() {
                let shown = self.show_type(row);
                // (One error for one record at one place: it may be
                // spread there as well.)
                if !self.lacking.insert((origin.span, shown.clone())) {
                    return;
                }
                self.error(
                    Code::TypeMismatch,
                    format!(
                        "a `{shown}` cannot be the record here: its field '{field}' is of type {}, and this use also has the record with a field '{field}' of type {}",
                        self.show_type(&self.apply(declared_ty)),
                        self.show_type(&self.apply(ty)),
                    ),
                    origin.span,
                );
            }
        }
    }

    /// The record over the annotation variable `r` is extended with
    /// `field`: for a row variable of a function's signature, the
    /// function's uses owe that what stands for it has no such field.
    /// (For another annotation's, the spread itself is the error:
    /// `anon_rigid`.)
    fn lacks_rigid(&mut self, r: RigidId, field: Symbol) {
        if self.is_sig_row(r.var) {
            self.mark_sig_row(r.var, &[field]);
        }
    }

    /// Check that the row `row`, known now, has no field `field`
    /// (`Pred::Lacks`). A declared record cannot stand for a row that
    /// is extended at all.
    fn row_lacks(&mut self, row: &Type, field: Symbol, origin: Origin) {
        match row {
            Type::AnonRecord { fields, tail } => {
                if fields.contains_key(&field) {
                    self.extends_existing(field, origin.span);
                    return;
                }
                match tail {
                    RowTail::Var(rest) => {
                        self.want(
                            Pred::Lacks {
                                row: Type::Var(*rest),
                                field,
                            },
                            origin,
                        );
                    }
                    RowTail::Rigid(r) => self.lacks_rigid(*r, field),
                    RowTail::Closed => {}
                }
            }
            Type::Rigid(r) => self.lacks_rigid(*r, field),
            Type::Generic(name, _) if self.tables.records.contains_key(name) => {
                self.spread_through(row, origin);
            }
            _ => {}
        }
    }

    /// Check `base.{ field: value }` now that the type of `base` is
    /// known.
    fn update_field(&mut self, base: &Type, field: Symbol, value: &Type, span: Span) {
        let declared: Vec<(Symbol, Type)> = match base {
            Type::Error | Type::Never => return,
            Type::AnonRecord { fields, tail } => {
                if !fields.contains_key(&field) && matches!(tail, RowTail::Var(_)) {
                    let extended = Type::AnonRecord {
                        fields: std::collections::BTreeMap::from([(field, value.clone())]),
                        tail: RowTail::Var(self.fresh_tyvar_id()),
                    };
                    self.unify(base, &extended, span);
                    return;
                }
                fields.iter().map(|(k, v)| (*k, v.clone())).collect()
            }
            Type::Generic(name, args) if self.tables.records.contains_key(name) => {
                self.instantiate_record_fields_with_args(*name, args)
            }
            other => {
                self.error(
                    Code::TypeMismatch,
                    format!(
                        "record update requires a record base, but '{}' is not a record type",
                        self.show_type(other)
                    ),
                    span,
                );
                return;
            }
        };
        match declared.iter().find(|(n, _)| *n == field) {
            Some((_, declared_ty)) => self.unify(value, declared_ty, span),
            None => {
                let base = format!("unknown field '{field}' in {}", self.show_type(base));
                self.error_help(
                    Code::UnknownField,
                    format_record_field_suggestion(base, field, &declared),
                    span,
                );
            }
        }
    }

    /// Decide each `e?` of the scope just left whose operand is still
    /// unknown and is the scope's to generalise, where the return type
    /// of the function it is in is known by now: `?` unwraps a Result
    /// with the function's error type, or an Option.
    pub(super) fn decide_tries(&mut self) {
        let mine: Vec<usize> = (self.closed_mark..self.wanted.len())
            .filter(|&i| {
                !self.wanted[i].solved
                    && matches!(self.wanted[i].goal, Goal::Try { .. })
                    && matches!(
                        self.apply(self.wanted[i].goal.waits_on()),
                        Type::Var(var) if self.tables.vars.is_generalizable(var)
                    )
            })
            .collect();
        self.reopen_level();
        for i in mine {
            let Goal::Try {
                operand,
                ret: Some(ret),
                ..
            } = self.wanted[i].goal.clone()
            else {
                continue;
            };
            let wrapper = match self.apply(&ret) {
                Type::Generic(name, args) if name.is_builtin("Result") && args.len() == 2 => {
                    Type::builtin("Result", vec![self.fresh_var(), args[1].clone()])
                }
                Type::Generic(name, args) if name.is_builtin("Option") && args.len() == 1 => {
                    Type::option(self.fresh_var())
                }
                _ => continue,
            };
            let _ = self.unify_types(&operand, &wrapper);
        }
        self.solve_wanted(self.closed_mark);
        self.close_level();
    }

    /// Decide each call `x.f(args)` of the scope just left whose
    /// receiver is still unknown and is the scope's to generalise: `x` is
    /// a record with a function in its field `m`.
    pub(super) fn default_selects(&mut self) {
        let mine: Vec<usize> = (self.closed_mark..self.wanted.len())
            .filter(|&i| {
                !self.wanted[i].solved
                    && matches!(self.wanted[i].goal, Goal::Select { .. })
                    && matches!(
                        self.apply(self.wanted[i].goal.waits_on()),
                        Type::Var(var) if self.tables.vars.is_generalizable(var)
                    )
            })
            .collect();
        // The variables this makes are the scope's.
        self.reopen_level();
        for i in mine {
            let origin = self.wanted[i].origin;
            let Goal::Select {
                recv,
                name,
                args,
                result,
            } = self.wanted[i].goal.clone()
            else {
                continue;
            };
            // (An earlier one of these may have decided this receiver.)
            if matches!(self.apply(&recv), Type::Var(_)) {
                self.wanted[i].solved = true;
                let call_ty = Type::Fun(args, Box::new(result));
                self.default_select(&recv, name, call_ty, origin.span);
            }
        }
        self.solve_wanted(self.closed_mark);
        self.close_level();
    }

    /// `x.name(..)` whose receiver nothing decided: `x` is a record with
    /// a function of the call's type in its field `name`.
    fn default_select(&mut self, recv: &Type, name: Symbol, call_ty: Type, span: Span) {
        let row = Type::AnonRecord {
            fields: std::collections::BTreeMap::from([(name, call_ty)]),
            tail: RowTail::Var(self.fresh_tyvar_id()),
        };
        self.unify(recv, &row, span);
    }

    /// Report that `method`, called on `on` (`type 'Int'`, `a value
    /// whose type is not known`), is a method of each of `traits`: the
    /// one wording, the traits by their names in order, each as a
    /// program writes it (`json.Encode`, `Display`).
    pub(super) fn ambiguous_method(
        &mut self,
        method: Symbol,
        on: &str,
        traits: &[TraitKey],
        span: Span,
    ) {
        let mut shown: Vec<String> = traits.iter().map(|t| self.show_trait(*t)).collect();
        shown.sort();
        self.errors.push(
            Diagnostic::error(
                Code::AmbiguousMethod,
                span,
                format!(
                    "ambiguous method '{method}' on {on}: provided by traits {}",
                    shown.join(", ")
                ),
            )
            .with_help(
                "say which is meant: annotate the receiver's type, or bound it with a `where` clause",
            ),
        );
    }

    /// Report that `method`, called on `on` (a value of type `a`, the
    /// type `a` of a `type a` parameter), whose type is the annotation
    /// variable `r`, is a method of each of `traits`, which bound it:
    /// being bounded already, the call says which it means by being in a
    /// function bounded by only one of them. The help names the bounds
    /// that give the method as the program wrote them: a bound whose
    /// supertrait gives it (`where a: S` with `trait S: Ar`), not the
    /// supertrait, unless one bound gives it all.
    pub(super) fn ambiguous_bound_method(
        &mut self,
        method: Symbol,
        on: &str,
        r: RigidId,
        traits: &[TraitKey],
        span: Span,
    ) {
        let in_scope = self.bounds.get(&r.var).cloned().unwrap_or_default();
        let above = |t: TraitKey| self.supertraits_of(t);
        // The bounds no other one has as a supertrait at its arguments:
        // what the `where` clause says, the supertraits aside
        // (`where a: Sub(Int) + Conv(String)` with `trait Sub(b):
        // Conv(b)` says `Conv(String)`, and `Conv(Int)` comes of `Sub`).
        let said: Vec<&(TraitKey, Vec<Type>)> = (in_scope.iter())
            .filter(|bound| {
                !(in_scope.iter())
                    .any(|other| other != *bound && self.supers_at(other).contains(bound))
            })
            .collect();
        let giving: Vec<&(TraitKey, Vec<Type>)> = (said.iter().copied())
            .filter(|(t, _)| traits.iter().any(|g| g == t || above(*t).contains(g)))
            .collect();
        let named: Vec<&(TraitKey, Vec<Type>)> = match giving.len() {
            0 | 1 => (in_scope.iter())
                .filter(|(t, _)| traits.contains(t))
                .collect(),
            _ => giving,
        };
        let shown_args = |this: &Self, args: &[Type]| -> Vec<String> {
            (args.iter())
                .map(|a| this.show_type(&this.apply(&substitute_vars(a, &this.rigid_of))))
                .collect()
        };
        // Two bounds of one trait at different arguments
        // (`where a: Conv(Int) + Conv(String)`): the bounds give the
        // method, by their arguments.
        let mut of_one_trait = traits.to_vec();
        super::inference::distinct(&mut of_one_trait);
        match of_one_trait.len() < traits.len() {
            true => {
                let mut given: Vec<String> = (in_scope.iter())
                    .filter(|(t, _)| traits.contains(t))
                    .map(|(t, args)| match shown_args(self, args) {
                        args if args.is_empty() => self.show_trait(*t),
                        args => format!("{}({})", self.show_trait(*t), args.join(", ")),
                    })
                    .collect();
                dedup_in_order(&mut given);
                self.errors.push(
                    Diagnostic::error(
                        Code::AmbiguousMethod,
                        span,
                        format!(
                            "ambiguous method '{method}' on {on}: provided by bounds {}",
                            given.join(", ")
                        ),
                    )
                    .with_help(
                        "say which is meant: annotate the receiver's type, or bound it with a `where` clause",
                    ),
                );
            }
            // (The traits in the order the bounds are written.)
            false => {
                let mut written: Vec<String> = (in_scope.iter())
                    .filter(|(t, _)| traits.contains(t))
                    .map(|(t, _)| self.show_trait(*t))
                    .collect();
                dedup_in_order(&mut written);
                self.errors.push(
                    Diagnostic::error(
                        Code::AmbiguousMethod,
                        span,
                        format!(
                            "ambiguous method '{method}' on {on}: provided by traits {}",
                            written.join(", ")
                        ),
                    )
                    .with_help(
                        "say which is meant: annotate the receiver's type, or bound it with a \
                         `where` clause",
                    ),
                );
            }
        }
        let mut bounds: Vec<String> = (named.into_iter())
            .map(|(t, args)| {
                let args = shown_args(self, args);
                match args.is_empty() {
                    true => format!("`{}`", self.written_trait(*t)),
                    false => format!("`{}({})`", self.written_trait(*t), args.join(", ")),
                }
            })
            .collect();
        dedup_in_order(&mut bounds);
        let (all, both) = match bounds.split_last() {
            Some((last, [one])) => (format!("{one} and {last}"), "both"),
            Some((last, rest)) if !rest.is_empty() => {
                (format!("{} and {last}", rest.join(", ")), "all")
            }
            _ => return,
        };
        if let Some(last) = self.errors.last_mut() {
            last.help = vec![format!(
                "the bounds {all} {both} give `{method}`: call it from a function bounded by \
                 only one of them"
            )];
        }
    }

    /// The supertraits the bound `bound` brings into scope, at the
    /// arguments it gives them, theirs too: what `declare_bound` adds
    /// with it.
    fn supers_at(&self, bound: &(TraitKey, Vec<Type>)) -> Vec<(TraitKey, Vec<Type>)> {
        let mut found: Vec<(TraitKey, Vec<Type>)> = Vec::new();
        let mut next = vec![bound.clone()];
        while let Some((t, args)) = next.pop() {
            let Some(info) = self.tables.traits.get(&t) else {
                continue;
            };
            for (i, super_name) in info.supertraits.iter().enumerate() {
                let super_args: Vec<Type> = match info.supertrait_args.get(i) {
                    Some(exprs) if !exprs.is_empty() => (exprs.iter())
                        .map(|te| super::inference::resolve_supertrait_arg(te, info, &args))
                        .collect(),
                    _ => Vec::new(),
                };
                let reached = (*super_name, super_args);
                if !found.contains(&reached) {
                    found.push(reached.clone());
                    next.push(reached);
                }
            }
        }
        found
    }

    /// The supertraits of the trait `t`, theirs too.
    pub(super) fn supertraits_of(&self, t: TraitKey) -> Vec<TraitKey> {
        let mut found: Vec<TraitKey> = Vec::new();
        let mut next = vec![t];
        while let Some(t) = next.pop() {
            for s in (self.tables.traits.get(&t)).map_or(&[][..], |info| &info.supertraits) {
                if !found.contains(s) {
                    found.push(*s);
                    next.push(*s);
                }
            }
        }
        found
    }

    /// The bound `tr(args)` as a message shows it: `Greet`, `Conv(Int)`.
    pub(super) fn show_bound(&self, tr: TraitKey, args: &[Type]) -> String {
        let name = self.show_trait(tr);
        if args.is_empty() {
            return name;
        }
        let args: Vec<String> = args
            .iter()
            .map(|t| self.show_type(&self.apply(t)))
            .collect();
        format!("{name}({})", args.join(", "))
    }

    /// Where the scheme being instantiated is used: the use that named
    /// itself (`named_use`: a call, a name), else the expression being
    /// checked.
    pub(super) fn use_origin(&self) -> Origin {
        self.named_use.unwrap_or(Origin {
            span: self.at,
            callee: None,
            op: None,
        })
    }

    // ── Type name for trait impl matching ────────────────────────────

    /// The type a resolved Type's impls are keyed by. Returns `None` if
    /// the type is unresolved (still a type variable) or has no head.
    ///
    /// Routes through `crate::types::canonical::canonicalize` so that an
    /// alias collapses to the type it stands for before name lookup.
    /// This is the single source of truth for the dispatch-name oracle
    /// on the typechecker side.
    pub(super) fn type_name_for_impl(&self, ty: &Type) -> Option<TypeRef> {
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, ty);
        match &ty {
            // Anonymous (structural) records are CONCRETE types — they
            // carry a definite shape — but no user impl can ever target
            // them (impls bind to nominal heads only).
            // Returning `None` here would let `verify_trait_obligation` and
            // its callers in inference.rs treat AnonRecord receivers as
            // "still polymorphic, defer", which silently bypasses
            // user-trait `where` constraints — a soundness hole.
            // Returning the builtin `<anon>` type makes the existing
            // `trait_impl_set.contains(...)` check fire the "does not
            // implement trait 'X'" diagnostic (which writes the record's
            // type); no program can name that type, so no impl targets
            // it.
            Type::AnonRecord { .. } => Some(TypeRef::builtin(crate::defs::ANON_RECORD)),
            // Function values resolve to `Fn`, and `Unit` is `Unit`, so
            // `where a: Trait` constraints route into the same impl
            // table the compiler keys impl methods by and
            // `dispatch_type_for_value` returns at runtime.
            _ => head_of(&ty),
        }
    }

    /// Return the positional type arguments of a (concrete) type. Mirrors
    /// the inverse of `register_trait_impl`'s self_type construction:
    /// `Type::Generic(_, args)` yields `args`; the parameterized builtin
    /// containers (List, Set, Channel, Map) yield their element types in
    /// declaration order; tuples yield their elements and functions their
    /// params-then-return. Anything else (Int, String, Record without type
    /// params, etc.) has no positional args. Used by `verify_trait_obligation`
    /// to walk into an impl's where-clause obligations.
    pub(super) fn type_args_of(&self, ty: &Type) -> Vec<Type> {
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, ty);
        match &ty {
            Type::Generic(_, args) => args.clone(),
            Type::List(inner) | Type::Set(inner) | Type::Channel(inner) => {
                vec![(**inner).clone()]
            }
            Type::Map(k, v) => vec![(**k).clone(), (**v).clone()],
            // Tuple-/Fn-shaped alias impls (`type P2 = (Int, Int)`;
            // `type IntOp = Fn(Int) -> Int`) register under the synthetic
            // heads `"Tuple"`/`"Fn"` with the expanded structural self
            // type. Their positional args are the element types (params
            // plus return for `Fn`) so `verify_trait_obligation`'s
            // self-type-args comparison sees them. Falling through to
            // `Vec::new()`, obligated-vs-impl args would compare as
            // empty-vs-empty and ANY tuple/function would satisfy
            // a bound whose only impl targeted a concrete alias shape.
            // A subject of another length than such an impl's
            // self type is refused by the caller; the impl for every
            // tuple (`trait T for Tuple`) has a variable for its self
            // type and no positional args at all.
            Type::Tuple(elems) => elems.clone(),
            Type::Fun(params, ret) => {
                let mut args = params.clone();
                args.push((**ret).clone());
                args
            }
            _ => Vec::new(),
        }
    }

    /// Check that `ty`, whose head is known, implements `trait_name` at
    /// `bound_trait_args`, for the use `origin`: the one answer to "does
    /// this type implement this trait". Reports
    /// `type 'X' does not implement trait 'Y'` if not.
    ///
    /// A trait has at most one impl for a type's head, so the impl that
    /// answers is known from the head alone, and what it says decides
    /// what is still unknown of the subject and of the bound: the impl's
    /// self type and trait arguments, with new variables for the impl's
    /// own, are unified with the subject's and the bound's (`s(3, y)`
    /// for `where a: Conv(b)` and `trait Conv(String) for Int` makes `y`
    /// a `String`). What the impl's header asks of its variables is owed
    /// by the same use (`want`): a part of the subject still unknown
    /// waits, and is generalised with the definition or decided later.
    pub(super) fn verify_trait_obligation(
        &mut self,
        trait_name: TraitKey,
        bound_trait_args: &[Type],
        ty: &Type,
        origin: Origin,
        asker: Option<Asker>,
        ev: Option<super::evidence::EvRef>,
    ) {
        let span = origin.span;
        let resolved = self.apply(ty);
        if matches!(resolved, Type::Error | Type::Never) {
            return;
        }
        // An annotation variable implements what its bounds say.
        if let Type::Rigid(r) = resolved {
            if Self::is_decode(trait_name) {
                self.note_bound_asker(r, asker);
            }
            self.ev_set(
                ev,
                super::evidence::EvNode::Bound {
                    var: r.var,
                    tr: trait_name,
                    args: bound_trait_args.to_vec(),
                },
            );
            self.require_declared_bound(r, trait_name, bound_trait_args, origin);
            return;
        }
        let Some(type_name) = self.type_name_for_impl(&resolved) else {
            // Still unknown: the caller lets it wait (`want`).
            return;
        };
        // Equal, Compare and Hash, and Display where no impl is written,
        // are what the type's structure says.
        if self.by_structure(trait_name, type_name) {
            // `json.Decode` is what the json decoder decodes: its own
            // judgement, which names the decoder.
            self.ev_set(ev, super::evidence::EvNode::Structural(trait_name));
            if Self::is_decode(trait_name) {
                self.want_decode(&resolved, origin, asker);
                return;
            }
            let mut walk = Walk::default();
            match self.structure_gap(trait_name, &resolved, &mut walk, 0) {
                Some(gap) => {
                    // An operator's own operand, of a type the operator
                    // is not for: the operator's message.
                    if let Some(op) = origin.op
                        && gap.is_whole()
                    {
                        let shown = self.show_type(&resolved);
                        let message = match (op, origin.callee) {
                            // (A pin compares as `==` does.)
                            (PIN, Some(pinned)) => format!(
                                "the pin '^{pinned}' requires a comparable type, got '{shown}'"
                            ),
                            _ => operator_message(&resolve(trait_name.name), op, &resolved, &shown),
                        };
                        self.error(Code::UnsupportedOperation, message, span);
                        return;
                    }
                    let message = match gap.is_whole() {
                        true => self.type_value_message(trait_name, &[], &resolved),
                        false => None,
                    };
                    let message =
                        message.unwrap_or_else(|| self.gap_message(trait_name, &resolved, &gap));
                    self.error(Code::MissingTraitImpl, message, span);
                }
                None => {
                    // A part still unknown, or an annotation variable,
                    // owes the trait in turn; a part with a written
                    // `Display` impl is that impl's to answer.
                    for part in walk.open {
                        self.want(
                            Pred::Trait {
                                tr: trait_name,
                                args: Vec::new(),
                                subject: part,
                                asker: None,
                            },
                            origin,
                        );
                    }
                    for part in walk.written {
                        self.verify_impl(trait_name, &[], &part, origin, None);
                    }
                }
            }
            return;
        }
        self.verify_impl(trait_name, bound_trait_args, &resolved, origin, ev);
    }

    /// Whether `trait_name` holds of a type with the head `head` by the
    /// type's structure: the sealed `Equal`, `Compare` and `Hash`
    /// always; `Display` and `json.Encode` unless an impl of it is
    /// written for the head.
    pub(super) fn by_structure(&self, trait_name: TraitKey, head: TypeRef) -> bool {
        if ["Equal", "Compare", "Hash"]
            .iter()
            .any(|name| trait_name.is_builtin(name))
        {
            return true;
        }
        // An impl is written for the head: in this module (`written`,
        // known before its impls are entered), in another one or in an
        // earlier REPL cell, or by silt itself (a builtin error enum's
        // `Display`): its method is a row of the impls' methods. A
        // method `display` of another trait is no `Display` impl.
        let Some((_, method)) = STRUCTURAL_METHODS.iter().find(|(name, _)| {
            WRITABLE_STRUCTURAL_TRAITS.contains(name) && trait_name.is_builtin(name)
        }) else {
            return false;
        };
        !self.written.contains(&(trait_name, head))
            && self
                .tables
                .impl_methods
                .get(head, intern(method), trait_name)
                .is_none()
    }

    /// What a message says of `ty` that lacks the trait `tr` (at
    /// `args`) if it is the type of a type given as a value
    /// (`println(json.Value)`), which has no trait: the type as the
    /// program writes it, and what cannot be done with it. `None` for
    /// any other type.
    pub(super) fn type_value_message(
        &self,
        tr: TraitKey,
        args: &[Type],
        ty: &Type,
    ) -> Option<String> {
        let Type::Generic(head, given) = ty else {
            return None;
        };
        let [given] = given.as_slice() else {
            return None;
        };
        if !head.is_builtin(crate::defs::TYPE_OF) {
            return None;
        }
        // A type whose arguments the value leaves open (`List`) by its
        // name alone.
        let given = self.apply(given);
        let shown = match &given {
            _ if free_vars_in(&given).is_empty() => self.show_type(&given),
            Type::List(_) => "List".to_string(),
            Type::Set(_) => "Set".to_string(),
            Type::Map(..) => "Map".to_string(),
            Type::Channel(_) => "Channel".to_string(),
            Type::Generic(r, _) => self.show_type(&Type::Generic(*r, Vec::new())),
            other => self.show_type(other),
        };
        let what = match () {
            _ if tr.is_builtin("Display") => "cannot be shown".to_string(),
            _ if tr.is_builtin("Equal") => "cannot be compared with `==`".to_string(),
            _ if tr.is_builtin("Compare") => "cannot be ordered".to_string(),
            _ if tr.is_builtin("Hash") => "cannot be hashed".to_string(),
            _ if tr.is_builtin("Encode") => "cannot be encoded as json".to_string(),
            _ => format!("does not implement trait '{}'", self.show_bound(tr, args)),
        };
        Some(format!(
            "`{shown}` is a type, and a type given as a value {what}"
        ))
    }

    /// What a message says of a type that lacks the structural trait
    /// `tr`: the type, and the way down to the part that lacks it
    /// (`type 'Box(H)' does not implement trait 'Display': variant 'Box'
    /// payload #1 > field 'f' is of type 'Fn(Int) -> Int', which does
    /// not`).
    pub(super) fn gap_message(&self, tr: TraitKey, ty: &Type, gap: &Gap) -> String {
        let tr = self.show_trait(tr);
        if gap.why == Some(TOO_DEEP) {
            return format!(
                "type '{}' cannot be shown to implement trait '{tr}': {TOO_DEEP}",
                self.show_type(&self.apply(ty))
            );
        }
        match (gap.path.is_empty(), gap.why) {
            (true, None) => format!("type '{}' does not implement trait '{tr}'", gap.head),
            (true, Some(why)) => {
                format!("type '{}' does not implement trait '{tr}': {why}", gap.full)
            }
            (false, why) => format!(
                "type '{}' does not implement trait '{tr}': {} is of type '{}', {}",
                self.show_type(&self.apply(ty)),
                gap.path.join(" > "),
                gap.full,
                why.unwrap_or("which does not")
            ),
        }
    }

    /// Why `ty` does not have the structural trait `tr`, if it does not:
    /// a function has none of them, a channel only `Equal`; a container,
    /// a tuple, a record or an enum has one when each of its parts does
    /// (and its head has it at all: a map has no `Compare`). The parts
    /// whose type is still unknown, or an annotation variable, are added
    /// to `walk.open`: the trait holds if it holds of them. A type being
    /// looked at is taken to hold where it comes up again inside itself
    /// (`walk.seen`), so a recursive type holds if its other parts do. A
    /// type that recurs at ever larger arguments
    /// (`type N(a) { Z, S(N(List(a))) }`) cannot be decided this way:
    /// past `STRUCTURE_DEPTH` levels it is reported as not verified.
    pub(super) fn structure_gap(
        &self,
        tr: TraitKey,
        ty: &Type,
        walk: &mut Walk,
        depth: usize,
    ) -> Option<Gap> {
        let ty = crate::types::canonical::canonicalize(&self.tables.resolver, &self.apply(ty));
        let leaf = |this: &Self, head: &str, why: Option<&'static str>| {
            Some(Gap {
                path: Vec::new(),
                head: head.to_string(),
                full: this.show_type(&ty),
                why,
            })
        };
        if depth > STRUCTURE_DEPTH {
            return leaf(self, "", Some(TOO_DEEP));
        }
        let stamped = |this: &Self, head: TypeRef| {
            let canon = canonical_head(&this.tables.resolver, head);
            this.tables.trait_impl_set.contains(&(tr, canon))
        };
        // The first part that lacks the trait, with the way to it.
        let first = |this: &Self, walk: &mut Walk, parts: Vec<(String, Type)>| {
            parts.into_iter().find_map(|(what, part)| {
                this.structure_gap(tr, &part, walk, depth + 1)
                    .map(|mut gap| {
                        if gap.why != Some(TOO_DEEP) {
                            gap.path.insert(0, what);
                        }
                        gap
                    })
            })
        };
        match &ty {
            Type::Error | Type::Never | Type::AssocProj { .. } => None,
            Type::Var(_) | Type::Rigid(_) => {
                if !walk.open.contains(&ty) {
                    walk.open.push(ty.clone());
                }
                None
            }
            Type::Fun(..) => leaf(self, "Fn", None),
            Type::Channel(_) if tr.is_builtin("Equal") => None,
            Type::List(_)
            | Type::Set(_)
            | Type::Map(..)
            | Type::Tuple(_)
            | Type::Channel(_)
            | Type::Int
            | Type::Float
            | Type::Bool
            | Type::String
            | Type::Unit => {
                let head = self.type_name_for_impl(&ty).expect("the type has a head");
                if !stamped(self, head) {
                    return leaf(self, &resolve(head.name), None);
                }
                let parts: Vec<(String, Type)> = match &ty {
                    Type::List(t) | Type::Set(t) => {
                        vec![("element".to_string(), (**t).clone())]
                    }
                    // The writer writes a key as its text: of a map,
                    // `json.Encode` asks its values only.
                    Type::Map(_, v) if tr.is_builtin("Encode") => {
                        vec![("value".to_string(), (**v).clone())]
                    }
                    Type::Map(k, v) => vec![
                        ("key".to_string(), (**k).clone()),
                        ("value".to_string(), (**v).clone()),
                    ],
                    Type::Tuple(ts) => ts
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (format!("part #{}", i + 1), t.clone()))
                        .collect(),
                    _ => Vec::new(),
                };
                first(self, walk, parts)
            }
            Type::AnonRecord { fields, tail } => {
                if tr.is_builtin("Compare") {
                    return leaf(self, &self.show_type(&ty), None);
                }
                // The fields behind an open row have the trait when the
                // row does: a row variable owes it like a type variable
                // (and stands for the record's other fields, or for the
                // nominal record itself, at a use).
                let rest = match tail {
                    RowTail::Closed => None,
                    RowTail::Var(v) => Some(Type::Var(*v)),
                    RowTail::Rigid(r) => Some(Type::Rigid(*r)),
                };
                if let Some(rest) = rest
                    && !walk.open.contains(&rest)
                {
                    walk.open.push(rest);
                }
                let parts = fields
                    .iter()
                    .map(|(n, t)| (format!("field '{n}'"), t.clone()))
                    .collect();
                first(self, walk, parts)
            }
            Type::Generic(name, _) => {
                let head = canonical_head(&self.tables.resolver, *name);
                // A written `Display` or `json.Encode` impl answers for
                // its type.
                if !self.by_structure(tr, head) {
                    walk.written.push(ty.clone());
                    return None;
                }
                // A type with no fields or variants to look at (a
                // handle, `Bytes`) has what the builtins say it has.
                let opaque = !self.tables.records.contains_key(name)
                    && !self.tables.enums.contains_key(name);
                if opaque {
                    return match stamped(self, head) {
                        true => None,
                        false => leaf(self, &self.show_type(&Type::Generic(*name, vec![])), None),
                    };
                }
                let args = self.type_args_of(&ty);
                if walk.seen.iter().any(|(h, a)| *h == head && *a == args) {
                    return None;
                }
                walk.seen.push((head, args.clone()));
                // The type's parts, at its arguments.
                let parts: Vec<(String, Type)> = if let Some(info) = self.tables.records.get(name) {
                    let mapping: HashMap<TyVar, Type> = self
                        .tables
                        .record_param_var_ids
                        .get(name)
                        .filter(|ids| ids.len() == args.len())
                        .map(|ids| ids.iter().copied().zip(args.iter().cloned()).collect())
                        .unwrap_or_default();
                    info.fields
                        .iter()
                        .map(|(n, t)| (format!("field '{n}'"), substitute_vars(t, &mapping)))
                        .collect()
                } else if let Some(info) = self.tables.enums.get(name) {
                    let mapping: HashMap<TyVar, Type> = if info.param_var_ids.len() == args.len() {
                        info.param_var_ids
                            .iter()
                            .copied()
                            .zip(args.iter().cloned())
                            .collect()
                    } else {
                        HashMap::new()
                    };
                    info.variants
                        .iter()
                        .flat_map(|v| {
                            v.field_types.iter().enumerate().map(|(i, t)| {
                                (
                                    format!("variant '{}' payload #{}", v.name, i + 1),
                                    substitute_vars(t, &mapping),
                                )
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                first(self, walk, parts)
            }
        }
    }

    /// Check that `ty` implements `trait_name` by an impl: the one the
    /// type's head has.
    fn verify_impl(
        &mut self,
        trait_name: TraitKey,
        bound_trait_args: &[Type],
        resolved: &Type,
        origin: Origin,
        ev: Option<super::evidence::EvRef>,
    ) {
        let span = origin.span;
        let resolved = resolved.clone();
        let Some(type_name) = self.type_name_for_impl(&resolved) else {
            return;
        };
        if !self
            .tables
            .trait_impl_set
            .contains(&(trait_name, type_name))
        {
            // The impl for every type was reported.
            if self.blanket_traits.contains(&trait_name) {
                return;
            }
            // An operator's own operand: the operator's message.
            if let Some(op) = origin.op {
                self.error(
                    Code::UnsupportedOperation,
                    operator_message(
                        &resolve(trait_name.name),
                        op,
                        &resolved,
                        &self.show_type(&resolved),
                    ),
                    span,
                );
                return;
            }
            // (An anonymous record has no name: the message writes its
            // type, and says why it has no impl.)
            let message = match &resolved {
                Type::AnonRecord { .. } => format!(
                    "type '{}' does not implement trait '{}': an anonymous record has the traits its fields give it, and no impl is written for one",
                    self.show_type(&resolved),
                    self.show_bound(trait_name, bound_trait_args)
                ),
                // The `()` is no type the program wrote: it is what a
                // statement's call was given (`fix_statement_calls`).
                Type::Unit if self.in_fixed_statement(span) => format!(
                    "trait '{}' is asked of a type that nothing decides: the call is a \
                     statement, so the type is `()`, which does not implement the trait",
                    self.show_bound(trait_name, bound_trait_args)
                ),
                _ => (self.type_value_message(trait_name, bound_trait_args, &resolved))
                    .unwrap_or_else(|| {
                        format!(
                            "type '{}' does not implement trait '{}'",
                            self.show_type(&Type::Generic(type_name, vec![])),
                            self.show_bound(trait_name, bound_trait_args)
                        )
                    }),
            };
            self.error(Code::MissingTraitImpl, message, span);
            return;
        }
        // Of an impl the module wrote twice, the duplicate's error is
        // the one: what the use gives the trait is nothing more to
        // decide (`Error`).
        if self.duplicated_impls.contains(&(trait_name, type_name)) {
            for arg in bound_trait_args {
                for v in free_vars_in(&self.apply(arg)) {
                    let _ = self.unify_types(&Type::Var(v), &Type::Error);
                }
            }
            return;
        }
        // The impl of the trait arguments: the one whose own could be the
        // use's. (A use waits where several could: `waits_for_trait_args`.)
        let fitting = self.impls_fitting(trait_name, type_name, bound_trait_args);
        // (A trait argument that is a type variable is some type: only an
        // impl for every argument at its place is for it.)
        let rigid = (bound_trait_args.iter()).any(|arg| matches!(self.apply(arg), Type::Rigid(_)));
        let key = match fitting.as_slice() {
            [key, ..] => Some(*key),
            // Of a trait argument that is a type variable, no impl.
            [] if rigid => None,
            // One impl, at other trait arguments: its own are reported
            // below, as the matched impl.
            [] => self.tables.one_impl(trait_name, type_name),
        };
        if key.is_none() && !self.tables.impl_args_of(trait_name, type_name).is_empty() {
            let message = format!(
                "type '{}' does not implement trait '{}'",
                self.show_type(&Type::Generic(type_name, vec![])),
                self.show_bound(trait_name, bound_trait_args)
            );
            self.error(Code::MissingTraitImpl, message, span);
            return;
        }
        let impl_args = key.map_or_else(ImplArgs::none, |key| key.2);
        // The impl, with new variables for its own: its self type, its
        // trait arguments and what its header asks.
        let lookup = key.unwrap_or((trait_name, type_name, ImplArgs::none()));
        let impl_self = self.tables.impl_self_types.get(&lookup).cloned();
        let impl_trait_args = (self.tables.impl_trait_args.get(&lookup))
            .cloned()
            .unwrap_or_default();
        let header = (self.tables.impl_preds.get(&lookup))
            .cloned()
            .unwrap_or_default();
        let mut own: Vec<TyVar> = impl_self.iter().flat_map(free_vars_in).collect();
        own.extend(impl_trait_args.iter().flat_map(free_vars_in));
        for pred in &header {
            own.extend(
                pred.args()
                    .iter()
                    .chain([pred.subject()])
                    .flat_map(free_vars_in),
            );
        }
        let mut fresh: HashMap<TyVar, Type> = HashMap::new();
        for v in own {
            fresh.entry(v).or_insert_with(|| self.fresh_var());
        }
        // Head-key membership alone is not enough: an alias-expanded impl
        // can carry CONCRETE self-type args (`type Bytes2 = List(Int)`;
        // `trait Total for Bytes2` registers under head "List" with
        // self_type `List(Int)`). The subject's positional args are
        // compared with the impl self type's: a concrete mismatch is
        // reported; a repeated binder of a NON-LINEAR impl self type
        // (`type Pair(a) = (a, a)`) must see equal types
        // (`impl_self_args_consistent`). A length mismatch between two
        // tuples or two functions is a mismatch; otherwise the impl's
        // self type says nothing of the subject's parts (the impl for
        // every tuple or function, whose self type is a variable):
        // skipped. Impls without a stored self type (the stamps of the
        // structural traits) skip the check.
        let obligated_args = self.type_args_of(&resolved);
        // Whether the impl's variables are the subject's parts by now.
        let mut linked = false;
        if let Some(impl_self) = &impl_self {
            let impl_self = substitute_vars(impl_self, &fresh);
            let impl_args = self.type_args_of(&impl_self);
            if obligated_args.len() == impl_args.len() {
                if !self.impl_self_args_consistent(&obligated_args, &impl_args) {
                    let (obligated, only) = self.show_apart(&resolved, &impl_self);
                    self.error(
                        Code::MissingTraitImpl,
                        format!(
                            "type '{}' does not implement trait '{}': the only impl is for '{}'",
                            obligated,
                            self.show_bound(trait_name, bound_trait_args),
                            only
                        ),
                        span,
                    );
                    return;
                }
                // The only impl is this one: what it says of the
                // subject's parts holds.
                for (ob, im) in obligated_args.iter().zip(&impl_args) {
                    let _ = self.unify_types(ob, im);
                }
                linked = true;
            } else if matches!(
                (&resolved, &impl_self),
                (Type::Tuple(_), Type::Tuple(_)) | (Type::Fun(..), Type::Fun(..))
            ) {
                // An impl for one tuple or function type (through an
                // alias: `type P2 = (Int, Int)`) is no impl for a tuple
                // of another length or a function of other parameters.
                // (The impl for every tuple has a variable for its self
                // type, and says nothing of the subject's parts.)
                let (obligated, only) = self.show_apart(&resolved, &impl_self);
                self.error(
                    Code::MissingTraitImpl,
                    format!(
                        "type '{}' does not implement trait '{}': the only impl is for '{}'",
                        obligated,
                        self.show_bound(trait_name, bound_trait_args),
                        only
                    ),
                    span,
                );
                return;
            }
        }
        // The bound's trait arguments are the impl's
        // (`where a: TryInto(Int)` against `trait TryInto(Float) for
        // String` is a mismatch). A bare bound (supertrait chains) says
        // nothing of them.
        if !bound_trait_args.is_empty() && impl_trait_args.len() == bound_trait_args.len() {
            let impl_trait_args: Vec<Type> = impl_trait_args
                .iter()
                .map(|t| substitute_vars(t, &fresh))
                .collect();
            // (An impl that fits the use was found so; a record is
            // compared again, by its fields and its row as written.)
            let fitted = !fitting.is_empty();
            for (bound_arg, impl_arg) in bound_trait_args.iter().zip(&impl_trait_args) {
                let b = self.apply(bound_arg);
                let i = self.apply(impl_arg);
                if (!fitted || holds_record(&b) || holds_record(&i))
                    && !self.trait_arg_compatible(&b, &i)
                {
                    self.error(
                        Code::MissingTraitImpl,
                        format!(
                            "type '{}' does not implement trait '{}': the matched impl is '{}'",
                            self.show_type(&Type::Generic(type_name, vec![])),
                            self.show_bound(trait_name, bound_trait_args),
                            self.show_bound(trait_name, &impl_trait_args),
                        ),
                        span,
                    );
                    return;
                }
            }
            for (bound_arg, impl_arg) in bound_trait_args.iter().zip(&impl_trait_args) {
                let _ = self.unify_types(bound_arg, impl_arg);
            }
        }
        // What the impl's header asks of its variables, which are the
        // subject's parts now, is owed by the same use. (The impl for
        // every tuple or function says nothing of the parts.)
        let mut subs = Vec::with_capacity(header.len());
        for pred in header {
            let dictionary = super::evidence::passes_dictionary(&pred);
            let ev = match linked {
                true => self.want(pred.substitute(&fresh), origin),
                false => None,
            };
            if dictionary {
                subs.push(ev);
            }
        }
        let subs = (subs.into_iter())
            .map(|sub| sub.unwrap_or_else(|| self.ev_new(super::evidence::EvNode::Pending)))
            .collect();
        self.ev_set(
            ev,
            super::evidence::EvNode::Impl {
                tr: trait_name,
                ty: type_name,
                args: impl_args,
                subs,
            },
        );
    }

    /// Check, where an impl is declared, that `ty` implements
    /// `trait_name` at `args`, as far as the declaration says what `ty`
    /// is: what would be owed for one of the impl's own variables is the
    /// impl's uses' to owe, not the declaration's.
    pub(super) fn verify_declared(
        &mut self,
        trait_name: TraitKey,
        args: &[Type],
        ty: &Type,
        span: Span,
    ) {
        let owed = self.wanted.len();
        self.verify_trait_obligation(
            trait_name,
            args,
            ty,
            Origin {
                span,
                callee: None,
                op: None,
            },
            None,
            None,
        );
        self.wanted.truncate(owed);
    }

    /// Consistency-tracking variant of the per-slot compatibility walk
    /// for `verify_trait_obligation`'s self-type-args check.
    /// A stateless per-pair `trait_arg_compatible` would lose the
    /// cross-slot linkage of NON-LINEAR impl self types — a repeated
    /// binder, reachable only via alias expansion (the parser rejects
    /// duplicate binders in direct impl targets): `Pair(a) = (a, a)` →
    /// `(Var a', Var a')`, ditto `Square(a) = Map(a, a)`. An obligated
    /// `(Int, Fn)` would defer each slot against `Var a'` alone, satisfy
    /// the bound, and die at runtime. Here a binding map is
    /// threaded across ALL slots: the first obligated type an impl-side
    /// `Var` meets binds it; every re-encounter must be compatible with
    /// that binding. Obligated-side `Var`s still defer (inference may
    /// resolve them later), and concrete/concrete pairs walk structurally
    /// exactly as before.
    ///
    /// Enforcing the linkage here also keeps the first-occurrence-only
    /// where-clause obligation index (the `.position(..)` over
    /// `expanded_self_args` in `register_trait_impl`) sound: once every
    /// slot sharing a binder is forced equal, checking the bound at the
    /// binder's first slot covers all of them.
    fn impl_self_args_consistent(&self, obligated: &[Type], impl_args: &[Type]) -> bool {
        let mut bindings: HashMap<TyVar, Type> = HashMap::new();
        obligated.iter().zip(impl_args.iter()).all(|(ob, im)| {
            let ob = crate::types::canonical::canonicalize(&self.tables.resolver, ob);
            let im = crate::types::canonical::canonicalize(&self.tables.resolver, im);
            Self::impl_arg_matches_canon(&ob, &im, &mut bindings)
        })
    }

    /// One-sided structural matcher threading `bindings` for
    /// `impl_self_args_consistent`. Mirrors `trait_arg_compatible_canon`'s
    /// recursive arms; both inputs are pre-canonicalised (deep), so the
    /// recursion never re-canonicalises. Leaf pairs with no impl-side
    /// binder to thread (scalars, nominal `Record`/`Generic` head-name
    /// comparisons without args, `Never`, mismatches) delegate to the
    /// existing stateless walk via the catch-all.
    fn impl_arg_matches_canon(ob: &Type, im: &Type, bindings: &mut HashMap<TyVar, Type>) -> bool {
        match (ob, im) {
            (Type::Error, _) | (_, Type::Error) => true,
            // Obligated side unresolved: defer, as before. (Deliberately
            // no binding — a caller-side tyvar may resolve after this
            // check; rejecting on it would be a false negative.)
            (Type::Var(_), _) => true,
            // Impl-side binder: bind on first encounter, require
            // compatibility with the binding on re-encounter.
            // `trait_arg_compatible_canon` is the right comparator —
            // a nested `Var` on either side keeps deferring
            // conservatively, while concrete mismatches reject.
            (_, Type::Var(tv)) => match bindings.get(tv) {
                Some(bound) => Self::trait_arg_compatible_canon(bound, ob),
                None => {
                    bindings.insert(*tv, ob.clone());
                    true
                }
            },
            (Type::List(x), Type::List(y))
            | (Type::Set(x), Type::Set(y))
            | (Type::Channel(x), Type::Channel(y)) => Self::impl_arg_matches_canon(x, y, bindings),
            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                Self::impl_arg_matches_canon(k1, k2, bindings)
                    && Self::impl_arg_matches_canon(v1, v2, bindings)
            }
            (Type::Tuple(xs), Type::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| Self::impl_arg_matches_canon(x, y, bindings))
            }
            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                p1.len() == p2.len()
                    && p1
                        .iter()
                        .zip(p2.iter())
                        .all(|(x, y)| Self::impl_arg_matches_canon(x, y, bindings))
                    && Self::impl_arg_matches_canon(r1, r2, bindings)
            }
            (Type::Generic(n1, a1), Type::Generic(n2, a2)) => {
                n1 == n2
                    && a1.len() == a2.len()
                    && a1
                        .iter()
                        .zip(a2.iter())
                        .all(|(x, y)| Self::impl_arg_matches_canon(x, y, bindings))
            }
            (
                Type::AnonRecord {
                    fields: f1,
                    tail: t1,
                },
                Type::AnonRecord {
                    fields: f2,
                    tail: t2,
                },
            ) => {
                t1 == t2
                    && f1.len() == f2.len()
                    && f1.iter().zip(f2.iter()).all(|((k1, v1), (k2, v2))| {
                        k1 == k2 && Self::impl_arg_matches_canon(v1, v2, bindings)
                    })
            }
            _ => Self::trait_arg_compatible_canon(ob, im),
        }
    }

    /// Side-effect-free compatibility check between a bound's trait-arg
    /// and an impl's trait-arg. Returns true when the pair could unify:
    /// either side is a type variable (defer), or both are concrete and
    /// structurally equal. Used by `verify_trait_obligation` to reject
    /// `where a: TryInto(Int)` when only `TryInto(Float) for ...` exists.
    ///
    /// Both sides are canonicalised at entry.
    pub(super) fn trait_arg_compatible(&self, a: &Type, b: &Type) -> bool {
        let a = crate::types::canonical::canonicalize(&self.tables.resolver, a);
        let b = crate::types::canonical::canonicalize(&self.tables.resolver, b);
        Self::trait_arg_compatible_canon(&a, &b)
    }

    fn trait_arg_compatible_canon(a: &Type, b: &Type) -> bool {
        match (a, b) {
            (Type::Error, _) | (_, Type::Error) => true,
            (Type::Var(_), _) | (_, Type::Var(_)) => true,
            (Type::Int, Type::Int)
            | (Type::Float, Type::Float)
            | (Type::Bool, Type::Bool)
            | (Type::String, Type::String)
            | (Type::Unit, Type::Unit) => true,
            (Type::List(x), Type::List(y))
            | (Type::Set(x), Type::Set(y))
            | (Type::Channel(x), Type::Channel(y)) => Self::trait_arg_compatible_canon(x, y),
            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                Self::trait_arg_compatible_canon(k1, k2) && Self::trait_arg_compatible_canon(v1, v2)
            }
            (Type::Tuple(xs), Type::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| Self::trait_arg_compatible_canon(x, y))
            }
            (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
                p1.len() == p2.len()
                    && p1
                        .iter()
                        .zip(p2.iter())
                        .all(|(x, y)| Self::trait_arg_compatible_canon(x, y))
                    && Self::trait_arg_compatible_canon(r1, r2)
            }
            (Type::Generic(n1, a1), Type::Generic(n2, a2)) => {
                n1 == n2
                    && a1.len() == a2.len()
                    && a1
                        .iter()
                        .zip(a2.iter())
                        .all(|(x, y)| Self::trait_arg_compatible_canon(x, y))
            }
            // Structurally compare anonymous records so
            // bounds like `where a: Convert({a: Int, b: String})` accept
            // an impl whose trait-arg is the byte-equal record. Without
            // this arm two equal `AnonRecord` values would fall through to
            // the `_ => false` catch-all and the obligation be rejected.
            (
                Type::AnonRecord {
                    fields: f1,
                    tail: t1,
                },
                Type::AnonRecord {
                    fields: f2,
                    tail: t2,
                },
            ) => {
                t1 == t2
                    && f1.len() == f2.len()
                    && f1.iter().zip(f2.iter()).all(|((k1, v1), (k2, v2))| {
                        k1 == k2 && Self::trait_arg_compatible_canon(v1, v2)
                    })
            }
            // `Never` is uninhabited; only equal to itself. Symmetry arm
            // for completeness — entry-point `canonicalize` doesn't
            // collapse `Never` to anything else.
            (Type::Never, Type::Never) => true,
            // No `(AssocProj, _)` arm is needed: the entry-point
            // `canonicalize` resolves projections before this walk runs,
            // so the recursive comparator never sees `AssocProj`.
            _ => false,
        }
    }

    /// The trait the definition `id` is, if it is one.
    pub(super) fn trait_key(&self, id: crate::defs::DefId) -> Option<TraitKey> {
        if let Some((name, _)) = crate::defs::builtin_trait_of_id(id) {
            return Some(TraitKey::builtin(name));
        }
        let def = self.defs.as_ref()?.get(id);
        match def.kind {
            crate::defs::DefKind::Trait(t) => Some(TraitKey {
                id: t,
                name: def.name,
            }),
            _ => None,
        }
    }

    /// The trait a method entry is of: its own, or for a builtin trait's
    /// method of a builtin type, which the table keeps without one, that
    /// builtin trait.
    pub(super) fn entry_trait(&self, entry: &MethodEntry, method: Symbol) -> Option<TraitKey> {
        entry.trait_name.or_else(|| {
            crate::defs::builtin_trait_of_method(&resolve(method)).and_then(|t| self.trait_key(t.0))
        })
    }

    /// Whether the module checked sees the trait `t`: a trait of the
    /// prelude, a trait it declares, names by an import or reaches
    /// through a module it imports; a trait of a builtin module
    /// (`json.Encode`) where it imports that module or names the trait.
    /// Another module's private trait it never sees.
    pub(super) fn sees_trait(&self, t: TraitKey) -> bool {
        let Some(defs) = &self.defs else {
            return true;
        };
        let def = defs.get(t.id.0);
        if def.module.builtin_name().is_some() {
            return self.imports_builtin_trait(t, def.module);
        }
        if def.module == self.module || def.module.is_builtin() {
            return true;
        }
        let private = self.tables.traits.get(&t).is_some_and(|info| {
            info.private_to
                .is_some_and(|(owner, _)| owner != self.module)
        });
        !private && (self.seen_traits.contains(&t.id.0) || self.seen_modules.contains(&def.module))
    }

    /// The module that declares the trait `t`, by name, when the module
    /// checked does not import it, nor a module that does, and so on: a
    /// method of the trait is not called here. (Its impls may or may not
    /// be known to the session by now; what a module means does not
    /// depend on that.)
    pub(super) fn unreached(&self, t: TraitKey) -> Option<Symbol> {
        let module = self.defs.as_ref()?.get(t.id.0).module;
        if let Some(name) = module.builtin_name() {
            return (!self.imports_builtin_trait(t, module)).then(|| intern(name));
        }
        let reach = self.reach.as_ref()?;
        if module == self.module || module.is_builtin() || reach.contains(&module) {
            return None;
        }
        Some(
            self.tables
                .module_names
                .get(&module)
                .copied()
                .unwrap_or_else(|| intern("?")),
        )
    }

    /// Whether the module checked imports `module`, the builtin module
    /// of the trait `t`, by any import (`import json.{ stringify }`
    /// too), or names the trait by an import.
    fn imports_builtin_trait(&self, t: TraitKey, module: crate::session::ModuleId) -> bool {
        self.builtin_imports.contains(&module) || self.seen_traits.contains(&t.id.0)
    }

    /// The traits that give the type `ty` a method `method` for a call
    /// through the type (`T.method(..)`), in the order of their names:
    /// those of the session's impls, and the structural trait whose
    /// method it is when no impl of that trait is written for the type.
    pub(super) fn method_providers(&self, ty: TypeRef, method: Symbol) -> Vec<TraitKey> {
        let mut traits = self.tables.impl_methods.providers(ty, method).to_vec();
        if let Some(tr) = self.structural_provider(ty, method)
            && !traits.contains(&tr)
        {
            traits.push(tr);
        }
        traits.sort_by_key(|t| self.show_trait(*t));
        // Through the type, a method that makes a value: one that takes
        // a value of the type is called through its trait, so it is no
        // candidate where another trait gives the type a method of the
        // name that makes one.
        if traits.iter().any(|t| self.self_param(*t, method).is_none()) {
            traits.retain(|t| self.self_param(*t, method).is_none());
        }
        traits
    }

    /// The structural trait whose method `method` the type `ty` has by
    /// its structure: one of their names, of a type that has the name
    /// (`Tables::has_structural_name`) and no written impl of the trait.
    fn structural_provider(&self, ty: TypeRef, method: Symbol) -> Option<TraitKey> {
        let name = resolve(method);
        let (tr_name, _) = STRUCTURAL_METHODS.iter().find(|(_, m)| *m == name)?;
        let tr = TraitKey::builtin(tr_name);
        (self.tables.has_structural_name(tr, ty)
            && self.by_structure(tr, ty)
            && self.unreached(tr).is_none())
        .then_some(tr)
    }

    /// Of the traits that give a type a method (`method_providers`), the
    /// one a call in the module checked means: the only one; of several,
    /// the one the module sees (a trait it declares, names by an import
    /// or reaches through a module it imports, or a builtin trait).
    /// `Err` with the candidates when it sees none of several, or more
    /// than one: the call is ambiguous.
    fn meant_provider(&self, providers: &[TraitKey]) -> Result<Option<TraitKey>, Vec<TraitKey>> {
        match providers {
            [] => Ok(None),
            [t] => Ok(Some(*t)),
            _ => {
                let seen: Vec<TraitKey> = providers
                    .iter()
                    .copied()
                    .filter(|t| self.sees_trait(*t))
                    .collect();
                match seen.as_slice() {
                    [t] => Ok(Some(*t)),
                    [] => Err(providers.to_vec()),
                    _ => Err(seen),
                }
            }
        }
    }

    /// The method `method` of the type `ty`, if the module checked may
    /// call it: a method of a trait declared in a module it does not
    /// reach by its imports it has not. It is decided from the impls'
    /// rows each time. (For a call that is ambiguous the first
    /// candidate's: `ambiguous_method_call` reports it.)
    /// The trait whose method `method` a use on the type `ty` means,
    /// when it is a trait with parameters that has impls for the type: the
    /// use's trait arguments choose the impl (`From` for `AppError`).
    pub(super) fn trait_of_impls_by_args(&self, ty: TypeRef, method: Symbol) -> Option<TraitKey> {
        let providers = self.method_providers(ty, method);
        let Ok(Some(tr)) = self.meant_provider(&providers) else {
            return None;
        };
        let params = (self.tables.traits.get(&tr)).is_some_and(|info| !info.params.is_empty());
        (params && !self.tables.impl_methods.args_of(ty, method, tr).is_empty()).then_some(tr)
    }

    pub(super) fn method_entry(&self, ty: TypeRef, method: Symbol) -> Option<MethodEntry> {
        let providers = self.method_providers(ty, method);
        let tr = match self.meant_provider(&providers) {
            Ok(tr) => tr?,
            Err(candidates) => candidates[0],
        };
        if self.unreached(tr).is_some() {
            return None;
        }
        // (Of a trait with impls for the type at several trait arguments,
        // one of them: what the rows say of the method, its trait and its
        // `self`, is the same in each; which impl a use reaches its trait
        // arguments say, `trait_of_impls_by_args`.)
        let rows = self.tables.impl_methods.args_of(ty, method, tr);
        let row = match rows.first() {
            Some(args) => self.tables.impl_methods.get_at(ty, method, tr, *args),
            None => self.tables.impl_methods.get(ty, method, tr),
        };
        match row {
            Some(entry) => Some(entry.clone()),
            // The structural trait's: no row, and no type of its own
            // (`structural_method` gives it one for the receiver).
            None => Some(MethodEntry {
                method_type: Type::Error,
                structural: true,
                trait_name: None,
                preds: Vec::new(),
            }),
        }
    }

    /// Whether a call of `method` of `ty` is ambiguous here; if so, it is
    /// reported at `span`.
    pub(super) fn ambiguous_method_call(
        &mut self,
        ty: TypeRef,
        method: Symbol,
        span: Span,
    ) -> bool {
        let providers = self.method_providers(ty, method);
        let Err(traits) = self.meant_provider(&providers) else {
            return false;
        };
        let on = format!("type '{}'", self.show_type(&Type::Generic(ty, vec![])));
        self.ambiguous_method(method, &on, &traits, span);
        // Called through the type (`T.decode(v)`), there is no receiver
        // to annotate: a function bounded by the trait says which (a
        // program's trait where one is among them).
        let named = (traits.iter())
            .find(|t| crate::defs::builtin_trait_of_id(t.id.0).is_none())
            .or(traits.first())
            .map(|t| self.show_trait(*t))
            .unwrap_or_default();
        if let Some(last) = self.errors.last_mut() {
            last.help = vec![format!(
                "say which is meant: call it through a function bounded by the trait you mean, \
                 `where a: {named}`"
            )];
        }
        true
    }
}

/// The canonical text of the type `ty` in an impl's key: type
/// constructors by their definitions, each type variable by the place it
/// first appears in `vars`.
fn key_text(ty: &Type, vars: &mut Vec<TyVar>) -> String {
    let all = |tys: &[Type], vars: &mut Vec<TyVar>| -> String {
        let parts: Vec<String> = tys.iter().map(|ty| key_text(ty, vars)).collect();
        parts.join(",")
    };
    let var = |v: TyVar, vars: &mut Vec<TyVar>| -> String {
        let at = vars.iter().position(|seen| *seen == v).unwrap_or_else(|| {
            vars.push(v);
            vars.len() - 1
        });
        format!("_{at}")
    };
    match ty {
        Type::Var(v) => var(*v, vars),
        Type::Rigid(r) => var(r.var, vars),
        Type::Generic(name, args) => format!("{:?}({})", name.id, all(args, vars)),
        Type::Fun(params, ret) => format!("Fn({})->{}", all(params, vars), key_text(ret, vars)),
        Type::Tuple(parts) => format!("({})", all(parts, vars)),
        Type::List(x) => format!("List({})", key_text(x, vars)),
        Type::Set(x) => format!("Set({})", key_text(x, vars)),
        Type::Channel(x) => format!("Channel({})", key_text(x, vars)),
        Type::Map(k, v) => format!("Map({},{})", key_text(k, vars), key_text(v, vars)),
        other => format!("{other:?}"),
    }
}

/// Unify `a` and `b` in the substitution `subst` alone (`could_unify`).
fn probe_unify(a: &Type, b: &Type, subst: &mut HashMap<TyVar, Type>) -> bool {
    // (A variable the substitution binds, followed to what it stands for:
    // a copy only then.)
    let resolve = |ty: &Type, subst: &HashMap<TyVar, Type>| -> Option<Type> {
        let Type::Var(v) = ty else {
            return None;
        };
        let mut bound = subst.get(v)?;
        while let Type::Var(w) = bound
            && let Some(next) = subst.get(w)
        {
            bound = next;
        }
        Some(bound.clone())
    };
    let (ra, rb) = (resolve(a, subst), resolve(b, subst));
    let (a, b) = (ra.as_ref().unwrap_or(a), rb.as_ref().unwrap_or(b));
    let all = |xs: &[Type], ys: &[Type], subst: &mut HashMap<TyVar, Type>| {
        xs.len() == ys.len() && (xs.iter().zip(ys)).all(|(x, y)| probe_unify(x, y, subst))
    };
    match (a, b) {
        (Type::Error, _) | (_, Type::Error) => true,
        (Type::Var(x), Type::Var(y)) if x == y => true,
        (Type::Var(x), other) | (other, Type::Var(x)) => {
            if occurs(*x, other, subst) {
                return false;
            }
            subst.insert(*x, other.clone());
            true
        }
        (Type::Rigid(x), Type::Rigid(y)) => x.var == y.var,
        (Type::List(x), Type::List(y))
        | (Type::Set(x), Type::Set(y))
        | (Type::Channel(x), Type::Channel(y)) => probe_unify(x, y, subst),
        (Type::Map(xk, xv), Type::Map(yk, yv)) => {
            probe_unify(xk, yk, subst) && probe_unify(xv, yv, subst)
        }
        (Type::Tuple(xs), Type::Tuple(ys)) => all(xs, ys, subst),
        (Type::Fun(xs, x), Type::Fun(ys, y)) => all(xs, ys, subst) && probe_unify(x, y, subst),
        (Type::Generic(x, xs), Type::Generic(y, ys)) => x == y && all(xs, ys, subst),
        // (A projection not reduced yet, or a record: what the checker
        // decides of them later.)
        (Type::AssocProj { .. }, _) | (_, Type::AssocProj { .. }) => true,
        // Records field by field: a field one of them lacks is in the
        // other's open row, if it has one.
        (
            Type::AnonRecord {
                fields: xs,
                tail: x_tail,
            },
            Type::AnonRecord {
                fields: ys,
                tail: y_tail,
            },
        ) => {
            let open = |tail: &RowTail| matches!(tail, RowTail::Var(_));
            (xs.iter()).all(|(name, x)| match ys.get(name) {
                Some(y) => probe_unify(x, y, subst),
                None => open(y_tail),
            }) && (ys.keys()).all(|name| xs.contains_key(name) || open(x_tail))
        }
        _ => a == b,
    }
}

/// Whether the variable `v` is in `ty`, read through `subst`: binding it
/// to `ty` would make a type of infinite size.
fn occurs(v: TyVar, ty: &Type, subst: &HashMap<TyVar, Type>) -> bool {
    free_vars_in(ty)
        .into_iter()
        .any(|w| w == v || subst.get(&w).is_some_and(|bound| occurs(v, bound, subst)))
}

/// Whether `pattern` (an impl's trait argument, its variables in
/// `mapping` as they are matched) stands for `ty` (a use's) as it is:
/// each variable of the pattern for one type, consistently; a variable of
/// the use only for a variable of the pattern.
fn match_one_way(pattern: &Type, ty: &Type, mapping: &mut HashMap<TyVar, Type>) -> bool {
    let all = |xs: &[Type], ys: &[Type], mapping: &mut HashMap<TyVar, Type>| {
        xs.len() == ys.len() && (xs.iter().zip(ys)).all(|(x, y)| match_one_way(x, y, mapping))
    };
    match (pattern, ty) {
        (Type::Var(v), _) => match mapping.get(v) {
            Some(bound) => bound == ty,
            None => {
                mapping.insert(*v, ty.clone());
                true
            }
        },
        (_, Type::Var(_)) => false,
        (Type::Error, _) | (_, Type::Error) => true,
        (Type::List(x), Type::List(y))
        | (Type::Set(x), Type::Set(y))
        | (Type::Channel(x), Type::Channel(y)) => match_one_way(x, y, mapping),
        (Type::Map(xk, xv), Type::Map(yk, yv)) => {
            match_one_way(xk, yk, mapping) && match_one_way(xv, yv, mapping)
        }
        (Type::Tuple(xs), Type::Tuple(ys)) => all(xs, ys, mapping),
        (Type::Fun(xs, x), Type::Fun(ys, y)) => {
            all(xs, ys, mapping) && match_one_way(x, y, mapping)
        }
        (Type::Generic(x, xs), Type::Generic(y, ys)) => x == y && all(xs, ys, mapping),
        (
            Type::AnonRecord {
                fields: xs,
                tail: x_tail,
            },
            Type::AnonRecord {
                fields: ys,
                tail: y_tail,
            },
        ) => {
            x_tail == y_tail
                && xs.len() == ys.len()
                && (xs.iter())
                    .all(|(name, x)| ys.get(name).is_some_and(|y| match_one_way(x, y, mapping)))
        }
        _ => pattern == ty,
    }
}

/// `items` without repeats, each where it first is.
fn dedup_in_order(items: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

/// Whether `ty` has an anonymous record in it.
fn holds_record(ty: &Type) -> bool {
    match ty {
        Type::AnonRecord { .. } => true,
        Type::Generic(_, ts) | Type::Tuple(ts) => ts.iter().any(holds_record),
        Type::Fun(params, ret) => params.iter().any(holds_record) || holds_record(ret),
        Type::List(t) | Type::Set(t) | Type::Channel(t) => holds_record(t),
        Type::Map(k, v) => holds_record(k) || holds_record(v),
        _ => false,
    }
}

/// The outer type of a trait argument, as `impls_fitting` tells impls
/// apart before unifying: none for a variable (or what meets anything).
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub(crate) enum OuterHead {
    Named(TypeRef),
    Prim(u8),
    List,
    Set,
    Channel,
    Map,
    Tuple(usize),
    Fun(usize),
    Record,
}

fn outer_head(ty: &Type) -> Option<OuterHead> {
    Some(match ty {
        Type::Generic(name, _) => OuterHead::Named(*name),
        Type::Int => OuterHead::Prim(0),
        Type::Float => OuterHead::Prim(1),
        Type::Bool => OuterHead::Prim(2),
        Type::String => OuterHead::Prim(3),
        Type::Unit => OuterHead::Prim(4),
        Type::List(_) => OuterHead::List,
        Type::Set(_) => OuterHead::Set,
        Type::Channel(_) => OuterHead::Channel,
        Type::Map(..) => OuterHead::Map,
        Type::Tuple(parts) => OuterHead::Tuple(parts.len()),
        Type::Fun(params, _) => OuterHead::Fun(params.len()),
        Type::AnonRecord { .. } => OuterHead::Record,
        _ => return None,
    })
}

/// The impls of a trait for a type as `impls_fitting` reads them: each
/// with its trait arguments, canonical, and by the outer type of its
/// first trait argument (`None`: a variable, which every use meets).
#[derive(Default)]
pub(crate) struct ImplIndex {
    impls: Vec<(ImplKey, std::rc::Rc<[Type]>)>,
    by_first: HashMap<Option<OuterHead>, Vec<usize>>,
}
