//! The builtin traits: their declarations, and which builtin types have
//! which of them.

use super::*;

/// The declarations of the builtin traits of the prelude, as a program
/// would write them, with the documentation an editor shows for each.
///
/// It returns the same `TraitDecl` shape a parser produces for a trait
/// a program writes, so `register_trait_decl_inner` registers the
/// builtins and the user traits one way (the duplicate-method check,
/// supertraits, where clauses).
///
/// The five built-ins:
/// - `Display`: `fn display(self) -> String` (signature only).
/// - `Compare`: `fn compare(self, other: Self) -> Int` (signature only).
/// - `Equal`:   `fn equal(self, other: Self) -> Bool` (signature only).
/// - `Hash`:    `fn hash(self) -> Int` (signature only).
/// - `Error: Display { fn message(self) -> String { "{self}" } }`: an
///   impl may leave `message` out, and its message is then the value
///   shown as `display` shows it.
pub fn builtin_trait_decls() -> Vec<TraitDecl> {
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

    // `Error.message` has a default: an impl may leave it out. It shows
    // the value, as `display` does: `"{self}"`. (No program declares the
    // trait, so the body is not checked; each program compiles it once,
    // into the default's slot.)
    let error_default_body = Expr::new(
        ExprKind::StringInterp(vec![StringPart::Expr(Expr::new(
            ExprKind::Ident(self_sym),
            dummy_span,
        ))]),
        dummy_span,
    );
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

    let mut decls = vec![
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
            doc: Some(
                "How a value is shown: `println`, `print`, interpolation and\n\
                 `string.from` show it so. A type has it by its structure, or by an\n\
                 impl written in the module that declares the type."
                    .to_string(),
            ),
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
            doc: Some(
                "The order of values: `<`, `>`, `<=` and `>=` need it. A type made of\n\
                 types that have it has it by its structure; an impl is an error."
                    .to_string(),
            ),
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
            doc: Some(
                "Whether two values are equal: `==` and `!=` need it. A type made of\n\
                 types that have it has it by its structure; an impl is an error."
                    .to_string(),
            ),
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
            doc: Some(
                "The types arithmetic is on, `Int` and `Float`: `+`, `-`, `*`, `/`,\n\
                 `%` and unary `-` need it. No other type has it."
                    .to_string(),
            ),
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
            doc: Some(
                "A value as a map's key or a set's element. A type made of types that\n\
                 have it has it by its structure; an impl is an error."
                    .to_string(),
            ),
        },
        // trait Error: Display { fn message(self) -> String { "{self}" } }
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
            doc: Some(
                "An error a program reports: `message` says what went wrong. An impl\n\
                 may leave it out: it is then the value as `display` shows it."
                    .to_string(),
            ),
        },
    ];
    // The traits of type constructors, as HK14 writes them: their
    // impls are passed with dictionaries, like a program's traits.
    let constructors = crate::builtins::registry::parse(CONSTRUCTOR_TRAITS);
    decls.extend(constructors.decls.into_iter().map(|decl| match decl {
        Decl::Trait(mut td) => {
            td.doc = CONSTRUCTOR_TRAIT_DOCS
                .iter()
                .find(|(name, _)| resolve(td.name) == *name)
                .map(|(_, doc)| doc.to_string());
            td
        }
        _ => unreachable!("the text declares traits"),
    }));
    decls
}

/// What hover says of each builtin trait of type constructors, as the
/// built-in traits' table of docs/language/traits.md does.
const CONSTRUCTOR_TRAIT_DOCS: &[(&str, &str)] = &[
    (
        "Functor",
        "`map` over a type constructor. `List`, `Option`, `Result`, `Channel`;\n\
         types with an impl.",
    ),
    (
        "Applicative",
        "`pure` and `map2`; includes `Functor`. `List`, `Option`, `Result`; types\n\
         with an impl.",
    ),
    (
        "Monad",
        "`flat_map`; includes `Applicative`. `List`, `Option`, `Result`; types with\n\
         an impl.",
    ),
];

/// The declarations of the builtin traits of type constructors, in the
/// order of their methods that a dictionary of an impl of one holds
/// (`builtins::applicative::PURE`, `MAP2`).
const CONSTRUCTOR_TRAITS: &str = "\
pub trait Functor {
  fn map(xs: Self(a), f: Fn(a) -> b) -> Self(b)
}
pub trait Applicative: Functor {
  fn pure(x: a) -> Self(a)
  fn map2(xs: Self(a), ys: Self(b), f: Fn(a, b) -> c) -> Self(c)
}
pub trait Monad: Applicative {
  fn flat_map(xs: Self(a), f: Fn(a) -> Self(b)) -> Self(b)
}
";

/// Register the builtin traits of the prelude ([`builtin_trait_decls`])
/// through the unified `register_trait_decl_inner` pipeline: the code
/// path a program's `trait X { fn ... }` takes, minus the
/// `BUILTIN_TRAIT_NAMES` redefinition check (which is keyed off user
/// input and lives on `register_trait_decl_user`). Which of them are
/// traits of type constructors is settled first, as for a program's.
/// Before the builtin functions: a row's bound names one
/// (`list.traverse`'s `where m: Applicative`).
pub(super) fn register_builtin_traits(checker: &mut TypeChecker) {
    let decls: Vec<Decl> = builtin_trait_decls().into_iter().map(Decl::Trait).collect();
    checker.settle_trait_kinds(&decls);
    for decl in &decls {
        if let Decl::Trait(td) = decl {
            checker.register_trait_decl_inner(td);
        }
    }
}

/// Register the traits the builtin modules declare and say which
/// builtin types have which builtin traits.
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
    // The traits a builtin module declares (`json.Encode`), from the
    // module's text in the builtin registry.
    let registry = crate::builtins::registry::registry();
    for module in &registry.modules {
        for decl in crate::builtins::registry::parse(module.traits).decls {
            if let Decl::Trait(td) = decl {
                checker.register_trait_decl_inner(&td);
            }
        }
    }

    // ── The structural traits of the builtin types ──────────────
    // (`Error` is not one: an impl of it is written.)
    let all_auto_traits: &[&str] = STRUCTURAL_TRAIT_NAMES;
    let non_ordering_traits: &[&str] = &["Equal", "Hash", "Display"];

    // Primitives + List: all four.
    register_structural_traits_for(
        checker,
        // The canonical key for the unit type is
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
    // `Map`: no `Compare`; `channel.ChannelOp` holds a channel: none;
    // `channel.Selected` holds one too: `Equal` only, as a channel).
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

    // `json.Encode`: the types the writer has a json form for
    // (`builtins::data`). A primitive, `()`, a list, a tuple and a map
    // (by its values: a key is written as its text), and each type the
    // builtin registry declares, by its structure. A set, a channel,
    // `Bytes`, a handle and a function have none.
    register_structural_traits_for(checker, JSON_FORMS, &["Encode"]);
    for (_, ty) in registry.types() {
        register_structural_traits_for(checker, &[ty.name], &["Encode"]);
    }
    // `json.Decode`: the builtin types a json decoder takes as its type
    // argument (`decode::target`). A record or enum type of the program
    // has it by its structure; a container, a date and the other
    // builtin types have none (a type written as a value names no
    // container's arguments).
    register_structural_traits_for(checker, JSON_DECODE_FORMS, &["Decode"]);
    checker.tables.trait_impl_set.insert((
        TraitKey::builtin("Decode"),
        TypeRef {
            id: crate::typeinfo::ty::JSON_VALUE,
            name: intern("Value"),
        },
    ));

    // `==` on a value of an opaque type is the value's own equality (a
    // handle's identity, the bytes' content).
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

    register_constructor_impls(checker);

    // TcpListener / TcpStream are registered in BUILTIN_TYPES so the
    // trait-impl-target gate says where an impl for them may be
    // written (rather than "type not declared") if a user tries to add
    // their own impls, but no built-in trait impls are stamped — they remain
    // unprintable opaque resources.
}

/// The impls of the builtin traits of type constructors for the builtin
/// types (`builtins::applicative::IMPLS`): entered as a program's impl
/// is, for the type's constructor (`Result(_, e)`), so their evidence
/// is an impl's, which the compiler passes as a dictionary. Each method
/// has the type the trait declares for it at that constructor.
fn register_constructor_impls(checker: &mut TypeChecker) {
    for (trait_name, type_name, _) in crate::builtins::applicative::IMPLS {
        let (tr, ty) = (TraitKey::builtin(trait_name), TypeRef::builtin(type_name));
        let key = (tr, ty, ImplArgs::none());
        checker.tables.add_impl(key);
        // The constructor: the type with its first argument open.
        let first = checker.fresh_var();
        let full = match *type_name {
            "List" => Type::List(Box::new(first)),
            "Channel" => Type::Channel(Box::new(first)),
            "Result" => Type::Generic(ty, vec![first, checker.fresh_var()]),
            _ => Type::Generic(ty, vec![first]),
        };
        let Some((head, rest, _)) = full.split_constructor() else {
            unreachable!("{type_name} takes a type argument");
        };
        let self_type = Type::Con(head, rest);
        checker
            .tables
            .impl_self_types
            .insert(key, self_type.clone());
        let Some(info) = checker.tables.traits.get(&tr).cloned() else {
            unreachable!("{trait_name} is declared before its impls");
        };
        for (method, declared) in &info.methods {
            let mut mapping: HashMap<TyVar, Type> = HashMap::new();
            mapping.insert(info.self_var, self_type.clone());
            for var in free_vars_in(declared) {
                mapping.entry(var).or_insert_with(|| checker.fresh_var());
            }
            let entry = MethodEntry {
                method_type: substitute_vars(declared, &mapping),
                structural: false,
                trait_name: Some(tr),
                preds: Vec::new(),
            };
            checker.register_method_entry(ty, *method, ImplArgs::none(), entry);
        }
    }
}

/// The builtin types that are no record or enum and have a json form,
/// so `json.Encode` by their structure.
pub(super) const JSON_FORMS: &[&str] = &[
    "Int", "Float", "Bool", "String", "Unit", "List", "Tuple", "Map",
];

/// The builtin types besides `json.Value` that are `json.Decode` by
/// their structure: the primitives a json decoder takes.
pub(super) const JSON_DECODE_FORMS: &[&str] = &["Int", "Float", "Bool", "String"];

/// Say that each of `type_names` has each of the structural traits
/// `trait_names`: a value of the type then has the trait's method as a
/// name (`Tables::has_structural_name`).
pub(super) fn register_structural_traits_for(
    checker: &mut TypeChecker,
    type_names: &[&str],
    trait_names: &[&str],
) {
    for type_name in type_names {
        for trait_name in trait_names {
            checker
                .tables
                .trait_impl_set
                .insert((TraitKey::builtin(trait_name), TypeRef::builtin(type_name)));
        }
    }
}
