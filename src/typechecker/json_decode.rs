//! `json.Decode`: whether a type has it by its structure is the json
//! decoder's own judgement (`decode_refusal` with json's [`Decoding`]),
//! which also gives the text of a refusal: "`json.parse` cannot decode
//! `Holder`: field `s` has type `Set(Int)`, which has no decoder". A type
//! with a written impl has it, and is not looked into, at the top and as
//! the type of a field.
//!
//! A refusal names the decoder the need comes from (`Asker`) and, in a
//! note, the function the use calls that passes the type on to it. What
//! a declared bound asks for is what its body gives the variable to,
//! known once the module's bodies are checked: the refusals are
//! reported then (`settle_decode_gaps`).
//!
//! [`Decoding`]: crate::builtins::registry::decoding::Decoding

use super::builtin_checks::refused_argument;
use super::solve::Origin;
use super::*;
use crate::builtins::registry::decoding::Decoding;

/// A type that lacks `json.Decode` at a use, reported once the module's
/// bodies are checked.
#[derive(Clone, Debug)]
pub(crate) struct DecodeGap {
    subject: Type,
    origin: Origin,
    asker: Option<Asker>,
    /// The module whose definition, at this use, asks for the type: it
    /// may decode an opaque type of its own.
    owner: Option<crate::session::ModuleId>,
}

impl TypeChecker {
    /// The trait `json.Decode`.
    pub(super) fn is_decode(tr: TraitKey) -> bool {
        tr.is_builtin("Decode")
    }

    /// Whether `tr` has `json.Decode` among its supertraits, at any
    /// depth: a bound of it asks what a bound of `json.Decode` does.
    pub(super) fn implies_decode(&self, tr: TraitKey) -> bool {
        !Self::is_decode(tr) && self.supertraits_of(tr).into_iter().any(Self::is_decode)
    }

    /// Whether `tr` has `json.Encode` among its supertraits, at any
    /// depth: a bound of it encodes what a bound of `json.Encode` does.
    pub(super) fn implies_encode(&self, tr: TraitKey) -> bool {
        !tr.is_builtin("Encode")
            && (self.supertraits_of(tr).into_iter()).any(|tr| tr.is_builtin("Encode"))
    }

    /// The asker of a declared bound `var: tr`: the bound itself, for
    /// `json.Decode` or a trait that has it among its supertraits.
    pub(super) fn asker_of_bound(&self, tr: TraitKey, var: TyVar) -> Option<Asker> {
        match self.implies_decode(tr) {
            true => Asker::of_bound(TraitKey::builtin("Decode"), var),
            false => Asker::of_bound(tr, var),
        }
    }

    /// Whether the program wrote a `json.Decode` impl for `head`: every
    /// decoder (json's, toml's, postgres's) decodes it by the impl, and
    /// does not look into its fields.
    pub(super) fn has_decode_impl(&self, head: TypeRef) -> bool {
        !self.by_structure(TraitKey::builtin("Decode"), head)
    }

    /// Whether an impl of `tr` for `target` is refused: `json.Decode`
    /// for a generic type (a decoder builds a value of one type).
    pub(super) fn decode_impl_refused(&self, tr: TraitKey, target: TypeRef) -> bool {
        Self::is_decode(tr) && self.tables.is_generic(target)
    }

    /// The use `origin` owes `json.Decode` of `ty`, a type with no
    /// written impl of it, for `asker`: refused, if the decoder has no
    /// value of the type, once the module's bodies are checked.
    pub(super) fn want_decode(
        &mut self,
        ty: &Type,
        origin: Origin,
        asker: Option<Asker>,
        owner: Option<crate::session::ModuleId>,
    ) {
        // (As it is now: what a variable in it is bound to later, a
        // type a scope's end picks, is no part of what it was given.)
        self.decode_gaps.push(DecodeGap {
            subject: self.apply(ty),
            origin,
            asker,
            owner,
        });
    }

    /// A body owes `json.Decode` of its annotation variable `r`, for
    /// `asker`: what its declared bound asks for.
    pub(super) fn note_bound_asker(&mut self, r: RigidId, asker: Option<Asker>) {
        let var = self.rigid_rep_var(r.var);
        if let Some(asker) = asker
            && asker.by != Asked::Bound(var)
            && asker.by != Asked::Unknown
        {
            self.bound_askers.entry(var).or_insert(asker.by);
        }
    }

    /// The decoder `asker` comes from: a declared bound asks for what
    /// its body gives the variable to, if that is known.
    pub(super) fn asking_decoder(&self, asker: Option<Asker>) -> Option<Symbol> {
        self.asked_decoder(asker?.by, &mut Vec::new())
    }

    /// The decoder `asked` comes from, not looking at the bounds `seen`
    /// again. A trait's bound on a method's variable, whose body is the
    /// impls', asks for what each of the module's impls of the method
    /// asks for, if they ask for one.
    fn asked_decoder(&self, asked: Asked, seen: &mut Vec<TyVar>) -> Option<Symbol> {
        let var = match asked {
            Asked::Builtin(name) => return Some(name),
            Asked::Unknown => return None,
            Asked::Bound(var) => self.rigid_rep_var(var),
        };
        if seen.contains(&var) {
            return None;
        }
        seen.push(var);
        if let Some(asked) = self.bound_askers.get(&var) {
            return self.asked_decoder(*asked, seen);
        }
        let mut found = None;
        for var in self.bound_impls.get(&var).into_iter().flatten() {
            match (found, self.asked_decoder(Asked::Bound(*var), seen)) {
                (_, None) => {}
                (None, decoder) => found = decoder,
                (Some(one), Some(other)) if one != other => return None,
                _ => {}
            }
        }
        found
    }

    /// `asker` as another module or a later REPL entry reads it: the
    /// decoder, where it is known.
    pub(super) fn settled_asker(&self, asker: Option<Asker>) -> Option<Asker> {
        let by = match self.asking_decoder(asker) {
            Some(decoder) => Asked::Builtin(decoder),
            None => Asked::Unknown,
        };
        asker.map(|asker| Asker { by, ..asker })
    }

    /// Report each type found to lack `json.Decode`, naming the decoder
    /// its need comes from and the function that passes it on; and decide
    /// what the `json.Decode` bounds of the module's traits' methods ask
    /// for, which another module reads.
    pub(super) fn settle_decode_gaps(&mut self) {
        let own_traits: Vec<TraitKey> = (self.tables.traits.keys())
            .filter(|key| self.tables.traits.is_own(key))
            .copied()
            .collect();
        for key in own_traits {
            let Some(info) = self.tables.traits.get(&key) else {
                continue;
            };
            let mut bounds = info.method_bounds.clone();
            for pred in bounds.values_mut().flatten() {
                if let Pred::Trait { asker, .. } = pred
                    && asker.is_some()
                {
                    *asker = self.settled_asker(*asker);
                }
            }
            if let Some(info) = self.tables.traits.get_mut(&key) {
                info.method_bounds = bounds;
            }
        }
        for gap in std::mem::take(&mut self.decode_gaps) {
            let ty = crate::types::canonical::canonicalize(self.tables.resolver(), &gap.subject);
            // A definition of an opaque type's module decodes it, wherever
            // it is called from.
            if gap.owner.is_some() && gap.owner == self.opaque_decoded_module(&ty) {
                continue;
            }
            // A decoder the bound's body hands the type to (`toml.parse`,
            // `postgres.query`) refused it here: that refusal, with the
            // note naming the function that passes it on, is the one
            // error.
            if (self.lacking).contains(&refused_argument(gap.origin.span, &ty)) {
                continue;
            }
            let decoder = self.asking_decoder(gap.asker);
            // The function or method that passes the type on, as the use
            // writes it (`store.load`).
            let passer = (gap.asker.and_then(|asker| asker.passer))
                .map(|passer| self.passer_at(passer, &gap.origin));
            // (Where no decoder is known, what the use calls is the one
            // that decodes; a use that names nothing, a supertrait an
            // impl's declaration owes: `json.decode`.)
            let named =
                (decoder.or(passer).or(gap.origin.callee)).unwrap_or_else(|| intern("json.decode"));
            let written = self.decode_aliases.get(&gap.origin.span).copied();
            let Some(mut diagnostic) =
                self.json_decode_refusal(&resolve(named), written, &ty, gap.origin.span)
            else {
                continue;
            };
            if !self.lacking.insert((
                gap.origin.span,
                format!("json.Decode {}", self.show_type(&ty)),
            )) {
                continue;
            }
            if let (Some(decoder), Some(passer)) = (decoder, passer) {
                diagnostic = diagnostic.with_note(format!("`{passer}` passes it to `{decoder}`"));
            }
            self.errors.push(diagnostic);
        }
    }

    /// The refusal of the json decoder `builtin` given `ty` (a use names
    /// it by the alias `written`, if by one) at `span`, if it has no
    /// value of the type: json's decoders' judgement, a type with a
    /// written `json.Decode` impl not looked into.
    pub(super) fn json_decode_refusal(
        &self,
        builtin: &str,
        written: Option<Symbol>,
        ty: &Type,
        span: Span,
    ) -> Option<Diagnostic> {
        match ty {
            Type::Var(_) | Type::Rigid(_) | Type::Error | Type::Never => None,
            Type::Generic(..)
            | Type::Int
            | Type::Float
            | Type::String
            | Type::Bool
            | Type::List(_)
            | Type::Map(..)
            | Type::Set(_)
            | Type::Channel(_) => {
                self.decode_refusal(builtin, Decoding::JSON, written, ty, None, span)
            }
            // A type no type argument names (a tuple, a function, an
            // anonymous record): what a decoder's type argument may be.
            other => {
                let accepted = Decoding::JSON.type_arguments(str::to_string);
                Some(
                    Diagnostic::error(
                        Code::Undecodable,
                        span,
                        format!(
                            "`{builtin}` cannot decode `{}`: its type argument must be {accepted}",
                            self.show_type(other)
                        ),
                    )
                    .with_help(Decoding::JSON.family.type_argument_help()),
                )
            }
        }
    }
}
