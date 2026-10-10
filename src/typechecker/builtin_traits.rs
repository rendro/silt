//! The builtin traits: their declarations, and which builtin types have
//! which of them.

use super::*;

/// The declarations of the five builtin traits, as a program would
/// write them.
///
/// Round 62 (item 3 of type-design improvements) — built-in trait
/// registration was previously a separate code path that hand-rolled
/// `TraitInfo` entries directly, bypassing the duplicate-method check,
/// supertrait/where-clause processing, and any future feature added to
/// `register_trait_decl`. This function returns the same `TraitDecl`
/// shape a parser would produce for hand-written user code, so the
/// unified `register_trait_decl_inner` pipeline handles built-ins and
/// user traits identically.
///
/// The five built-ins:
/// - `Display`: `fn display(self) -> String` (signature only).
/// - `Compare`: `fn compare(self, other: Self) -> Int` (signature only).
/// - `Equal`:   `fn equal(self, other: Self) -> Bool` (signature only).
/// - `Hash`:    `fn hash(self) -> Int` (signature only).
/// - `Error: Display { fn message(self) -> String }`, with a default:
///   an impl may leave `message` out, and the VM then shows the value
///   as `display` does.
pub(super) fn builtin_trait_decls() -> Vec<TraitDecl> {
    let dummy_span = Span::BUILTIN;
    let self_sym = intern("self");
    let other_sym = intern("other");

    fn self_param(self_sym: Symbol, span: Span) -> Param {
        Param {
            kind: ParamKind::Data,
            pattern: Pattern::new(PatternKind::Ident(self_sym), span),
            ty: None,
        }
    }
    fn other_param(other_sym: Symbol, span: Span) -> Param {
        // `other: Self`
        Param {
            kind: ParamKind::Data,
            pattern: Pattern::new(PatternKind::Ident(other_sym), span),
            ty: Some(TypeExpr::new(TypeExprKind::SelfType, span)),
        }
    }
    fn unit_body(span: Span) -> Expr {
        Expr::new(ExprKind::Unit, span)
    }
    fn named_ret(name: &str, span: Span) -> Option<TypeExpr> {
        Some(TypeExpr::new(
            TypeExprKind::Named {
                module: None,
                name: intern(name),
                name_span: span,
            },
            span,
        ))
    }
    fn sig_only_method(name: &str, params: Vec<Param>, ret: &str, span: Span) -> FnDecl {
        FnDecl {
            name: intern(name),
            params,
            return_type: named_ret(ret, span),
            where_clauses: Vec::new(),
            body: unit_body(span),
            is_pub: true,
            span,
            // Synthesized built-in: no source identifier — mirror `span`.
            name_span: span,
            is_recovery_stub: false,
            is_signature_only: true,
            doc: None,
        }
    }

    // `Error.message` has a default: an impl may leave it out. (What
    // it does is the VM's: it shows the value, as `display` does. No
    // program declares the trait, so the body here is never checked or
    // compiled.)
    let error_default_body = Expr::new(ExprKind::Unit, dummy_span);
    let error_message_fn = FnDecl {
        name: intern("message"),
        params: vec![self_param(self_sym, dummy_span)],
        return_type: named_ret("String", dummy_span),
        where_clauses: Vec::new(),
        body: error_default_body,
        is_pub: true,
        span: dummy_span,
        // Synthesized built-in `Error.message`: no source ident — mirror span.
        name_span: dummy_span,
        is_recovery_stub: false,
        is_signature_only: false,
        doc: None,
    };

    vec![
        // trait Display { fn display(self) -> String }
        TraitDecl {
            name: intern("Display"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "display",
                vec![self_param(self_sym, dummy_span)],
                "String",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Compare: Equal { fn compare(self, other: Self) -> Int }
        // What is ordered can be compared for equality.
        TraitDecl {
            name: intern("Compare"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: vec![TraitRef {
                module: None,
                name: intern("Equal"),
                args: Vec::new(),
                span: dummy_span,
                res: None,
            }],
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "compare",
                vec![
                    self_param(self_sym, dummy_span),
                    other_param(other_sym, dummy_span),
                ],
                "Int",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Equal { fn equal(self, other: Self) -> Bool }
        TraitDecl {
            name: intern("Equal"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "equal",
                vec![
                    self_param(self_sym, dummy_span),
                    other_param(other_sym, dummy_span),
                ],
                "Bool",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Number {}: the types arithmetic is on, Int and Float.
        TraitDecl {
            name: intern("Number"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: Vec::new(),
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Hash { fn hash(self) -> Int }
        TraitDecl {
            name: intern("Hash"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: Vec::new(),
            param_where_clauses: Vec::new(),
            methods: vec![sig_only_method(
                "hash",
                vec![self_param(self_sym, dummy_span)],
                "Int",
                dummy_span,
            )],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
        // trait Error: Display { fn message(self) -> String { self.display() } }
        TraitDecl {
            name: intern("Error"),
            name_span: dummy_span,
            params: Vec::new(),
            supertraits: vec![TraitRef {
                module: None,
                name: intern("Display"),
                args: Vec::new(),
                span: dummy_span,
                res: None,
            }],
            param_where_clauses: Vec::new(),
            methods: vec![error_message_fn],
            assoc_types: Vec::new(),
            is_pub: true,
            span: dummy_span,
            doc: None,
        },
    ]
}

/// Register built-in trait declarations (Display/Compare/Equal/Hash/Error)
/// and say which builtin types have which structural traits.
///
/// This is the single source of truth for that policy. The builtin
/// environment every check starts from is built with it, so no two
/// checks diverge on which types implement which traits.
///
/// The trait declarations go through the same code as a program's
/// `trait X { ... }` (`builtin_trait_decls`, `register_trait_decl_inner`).
/// Which builtin type has which structural trait is said here:
/// - `Int`, `Float`, `Bool`, `String`, `()`, `List`, tuples: all four
///   (Equal, Compare, Hash, Display).
/// - `Map`, `Set`: Equal, Hash, Display; they have no order.
/// - A type of the builtin registry: what the registry says (`Option`
///   and `Result`: no Compare).
pub(super) fn register_builtin_trait_impls(checker: &mut TypeChecker) {
    // ── Register built-in trait declarations through the unified
    //    register_trait_decl_inner pipeline. Same code path user
    //    `trait X { fn ... }` declarations take, minus the
    //    BUILTIN_TRAIT_NAMES redefinition check (which is keyed off
    //    user input and lives on register_trait_decl_user). ─────────
    for td in builtin_trait_decls() {
        checker.register_trait_decl_inner(&td);
    }

    // ── The structural traits of the builtin types ──────────────
    // (`Error` is not one: an impl of it is written.)
    let all_auto_traits: &[&str] = STRUCTURAL_TRAIT_NAMES;
    let non_ordering_traits: &[&str] = &["Equal", "Hash", "Display"];

    // Primitives + List: all four.
    register_structural_traits_for(
        checker,
        // Round 75 TYPE-3 LATENT: canonical key for the unit type is
        // "Unit" (matches canonical_name(Type::Unit) and
        // dispatch_type_for_value(Value::Unit)); an impl target `()`
        // names it too.
        &["Int", "Float", "Bool", "String", "Unit"],
        all_auto_traits,
    );
    register_structural_traits_for(checker, &["List"], all_auto_traits);
    // Arithmetic is on Int and Float.
    for ty in ["Int", "Float"] {
        checker
            .tables
            .trait_impl_set
            .insert((TraitKey::builtin("Number"), TypeRef::builtin(ty)));
    }
    // A tuple has what its parts have; a map and a set have no order.
    register_structural_traits_for(checker, &["Tuple"], all_auto_traits);
    register_structural_traits_for(checker, &["Map", "Set"], non_ordering_traits);
    // A channel is equal to itself only (`ch1 == ch2`).
    register_structural_traits_for(checker, &["Channel"], &["Equal"]);
    // Each type the builtin registry declares derives what the registry
    // says: all four traits, unless it names fewer (`Option` and
    // `Result`, generic wrappers: no `Compare`; `http.Response` holds a
    // `Map`: no `Compare`; `channel.ChannelOp` holds a channel: none).
    // The stamps of a module's error enum add `Equal`/`Compare`/`Hash`
    // to the `Error` and `Display` the builtin environment entered for
    // it (insert is idempotent).
    for (_, ty) in crate::builtins::registry::registry().types() {
        register_structural_traits_for(checker, &[ty.name], ty.derives);
    }

    // Bytes: Display here (`Value::Bytes` prints as a short hex preview
    // and its length), Equal and Hash below. No Compare: bytes are not
    // an ordered key type (`bytes.to_hex` first).
    register_structural_traits_for(checker, &["Bytes"], &["Display"]);

    // `==` on a value of an opaque type is the value's own equality (a
    // handle's identity, the bytes' content).
    let registry = crate::builtins::registry::registry();
    let opaque = registry
        .modules
        .iter()
        .flat_map(|module| module.opaque.iter())
        .map(|(name, _)| *name)
        .chain(["Bytes"]);
    for name in opaque {
        register_structural_traits_for(checker, &[name], &["Equal"]);
    }
    // Bytes are a map key and a set element by their content.
    register_structural_traits_for(checker, &["Bytes"], &["Hash"]);

    // TcpListener / TcpStream are registered in BUILTIN_TYPES so the
    // trait-impl-target gate says where an impl for them may be
    // written (rather than "type not declared") if a user tries to add
    // their own impls, but no built-in trait impls are stamped — they remain
    // unprintable opaque resources.
}

/// Say that each of `type_names` has each of the structural traits
/// `trait_names`, and enter the traits' methods for it.
///
/// The four built-in trait methods are always considered. A method is
/// registered only when its parent trait appears in `trait_names`:
/// - `display` ← Display
/// - `equal`   ← Equal
/// - `compare` ← Compare
/// - `hash`    ← Hash
pub(super) fn register_structural_traits_for(
    checker: &mut TypeChecker,
    type_names: &[&str],
    trait_names: &[&str],
) {
    let has_display = trait_names.contains(&"Display");
    let has_equal = trait_names.contains(&"Equal");
    let has_compare = trait_names.contains(&"Compare");
    let has_hash = trait_names.contains(&"Hash");
    for type_name in type_names {
        for trait_name in trait_names {
            checker
                .tables
                .trait_impl_set
                .insert((TraitKey::builtin(trait_name), TypeRef::builtin(type_name)));
        }
        // Build method entries only for traits in `trait_names`.
        let mut methods: Vec<(&str, Type)> = Vec::with_capacity(4);
        if has_display {
            methods.push((
                "display",
                Type::Fun(vec![checker.fresh_var()], Box::new(Type::String)),
            ));
        }
        if has_equal {
            methods.push((
                "equal",
                Type::Fun(
                    vec![checker.fresh_var(), checker.fresh_var()],
                    Box::new(Type::Bool),
                ),
            ));
        }
        if has_compare {
            methods.push((
                "compare",
                Type::Fun(
                    vec![checker.fresh_var(), checker.fresh_var()],
                    Box::new(Type::Int),
                ),
            ));
        }
        if has_hash {
            methods.push((
                "hash",
                Type::Fun(vec![checker.fresh_var()], Box::new(Type::Int)),
            ));
        }
        for (method_name, method_type) in &methods {
            checker.tables.method_table.insert(
                (TypeRef::builtin(type_name), intern(method_name)),
                MethodEntry {
                    method_type: method_type.clone(),
                    structural: true,
                    trait_name: None,
                    preds: Vec::new(),
                },
            );
        }
    }
}
