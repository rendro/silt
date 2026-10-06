use crate::defs::Res;
use crate::intern::Symbol;
use crate::source::Span;
use crate::types::Type;

// ── Expressions ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    pub ty: Option<Type>,
    /// What the name means, filled in by the resolver: on an `Ident`; on
    /// a `FieldAccess` whose head is a module or a type (`m.f`,
    /// `m.Circle`, `Shape.Circle`, `m.Shape.Circle`), the member it
    /// names; on a `RecordCreate`, the record type. `None` elsewhere, and
    /// on what the checker synthesizes.
    pub res: Option<Res>,
    /// What a field access on a value means, filled in by the
    /// typechecker when the types are final: the compiler emits what it
    /// says. `None` elsewhere (and on `m.f`, `Type.method`, a variant:
    /// those are names, and `res` says what they name).
    pub sel: Option<Selection>,
}

/// What `recv.name`, on a value, means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// The record's field `name`.
    Field,
    /// In a call, `recv.name(args)`: the function the record's field
    /// `name` holds, called with `args`.
    FieldCall,
    /// In a call: the method `name` of the impl of the trait `tr` for
    /// the type `ty`, the receiver's, called with the receiver and
    /// `args`.
    Impl {
        tr: crate::defs::TraitId,
        ty: crate::defs::TypeId,
    },
    /// In a call: the method `name` of the builtin trait `tr`
    /// (Display, Equal, Compare, Hash) for a receiver whose type has no
    /// written impl of it: the type has the trait by its structure.
    Native { tr: crate::defs::TraitId },
    /// In a call: the method `name` of the trait `tr`, for a receiver
    /// whose type is known only where the code runs (a type variable
    /// bounded by the trait; a type passed as a value).
    Dynamic { tr: crate::defs::TraitId },
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self {
            kind,
            span,
            ty: None,
            res: None,
            sel: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    // Literals
    Int(i64),
    Float(f64),
    Bool(bool),
    /// String literal. The bool is `true` when written with triple-quote (`"""`) syntax.
    StringLit(String, bool),
    StringInterp(Vec<StringPart>),

    // Collections
    List(Vec<ListElem>),
    Map(Vec<(Expr, Expr)>),
    SetLit(Vec<Expr>),
    Tuple(Vec<Expr>),

    // Variables & access
    Ident(Symbol),
    /// `expr.field`, with the span of the field name.
    FieldAccess(Box<Expr>, Symbol, Span),

    // Operations
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Pipe(Box<Expr>, Box<Expr>),
    Range(Box<Expr>, Box<Expr>),
    QuestionMark(Box<Expr>),
    Ascription(Box<Expr>, TypeExpr),

    // Function-related
    Call(Box<Expr>, Vec<Expr>),
    Lambda {
        params: Vec<Param>,
        body: Box<Expr>,
    },

    // Records
    RecordCreate {
        /// The module qualifier of `util.Pt { x: 1 }`. It selects WHICH
        /// module's record declaration the literal is checked against
        /// (two imported modules may export same-named types); the
        /// constructed value is identical to the bare `Pt { ... }` form —
        /// codegen keys on `name` only, mirroring how variants resolve by
        /// bare name.
        module: Option<Qualifier>,
        name: Symbol,
        /// Span of the type name itself (the `Pt` of `util.Pt { .. }`).
        name_span: Span,
        fields: Vec<(Symbol, Expr)>,
    },
    RecordUpdate {
        expr: Box<Expr>,
        fields: Vec<(Symbol, Expr)>,
    },
    /// Anonymous (row-polymorphic) record literal: `{name: "A", age: 30}`
    /// or with a spread head `{...other, age: 30}`. The optional
    /// `spread` is the base record whose fields are copied; `fields`
    /// are added/overridden on top.
    AnonRecord {
        spread: Option<Box<Expr>>,
        fields: Vec<(Symbol, Expr)>,
    },

    // Control flow
    Match {
        expr: Option<Box<Expr>>,
        arms: Vec<MatchArm>,
    },
    Return(Option<Box<Expr>>),

    // Block
    Block(Vec<Stmt>),

    // Loop
    /// Loop expression: `loop x = init, y = init { body }`; each binding
    /// is its name, the span of the name, and its initialiser.
    Loop {
        bindings: Vec<(Symbol, Span, Expr)>,
        body: Box<Expr>,
    },
    /// Recur: `loop(args)` inside a loop body
    Recur(Vec<Expr>),

    // Unit
    Unit,
}

#[derive(Debug, Clone)]
pub enum StringPart {
    Literal(String),
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Gt,
    Leq,
    Geq,
    And,
    Or,
}

impl std::fmt::Display for BinOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BinOp::Add => write!(f, "+"),
            BinOp::Sub => write!(f, "-"),
            BinOp::Mul => write!(f, "*"),
            BinOp::Div => write!(f, "/"),
            BinOp::Mod => write!(f, "%"),
            BinOp::Eq => write!(f, "=="),
            BinOp::Neq => write!(f, "!="),
            BinOp::Lt => write!(f, "<"),
            BinOp::Gt => write!(f, ">"),
            BinOp::Leq => write!(f, "<="),
            BinOp::Geq => write!(f, ">="),
            BinOp::And => write!(f, "&&"),
            BinOp::Or => write!(f, "||"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
}

// ── Match arms ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Box<Expr>>,
    pub body: Expr,
}

/// An element in a list literal: either a single expression or a spread `..expr`.
#[derive(Debug, Clone)]
pub enum ListElem {
    Single(Expr),
    Spread(Expr),
}

// ── Patterns ─────────────────────────────────────────────────────────

/// A pattern node with its source span. The span points at the pattern's
/// own location in source so that diagnostics about pattern-internal
/// errors (constructor arity, field typos, shadow warnings, ...) can
/// attribute to the exact sub-pattern rather than the enclosing match
/// scrutinee or let binding. Mirrors the `Expr`/`ExprKind` split.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
    /// What a `Constructor` or `Record` pattern names, filled in by the
    /// resolver: the variant, or the record type.
    pub res: Option<Res>,
    /// Whether the pattern matches every value of the type it is matched
    /// against, so that no test is needed. A wildcard and a name do by
    /// their form; for every other pattern the typechecker decides, and
    /// until it has the pattern counts as one that can fail.
    pub irrefutable: bool,
}

impl Pattern {
    pub fn new(kind: PatternKind, span: Span) -> Self {
        let irrefutable = matches!(kind, PatternKind::Wildcard | PatternKind::Ident(_));
        Self {
            kind,
            span,
            res: None,
            irrefutable,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PatternKind {
    Wildcard,
    Ident(Symbol),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// The bool is `true` when written with triple-quote (`"""`) syntax.
    StringLit(String, bool),
    Tuple(Vec<Pattern>),
    /// Enum-constructor pattern: `Some(x)`, `Rect(w, h)`, or a bare
    /// unit variant `Red`.
    Constructor {
        /// The segments before the variant name, at most two: the owning
        /// enum (`Shape.Circle(r)`), an imported module or alias
        /// (`shapes.Circle(r)`), or both (`shapes.Shape.Circle(r)`). The
        /// qualifier is validated and (for module qualifiers) used to
        /// pick the right enum when two modules export same-named types —
        /// but match IDENTITY stays the bare `name`: variants resolve
        /// globally by bare name, so exhaustiveness, duplicate-arm
        /// analysis, and codegen all treat `shapes.Circle(r)` and
        /// `Circle(r)` as the SAME constructor and ignore this field.
        qualifier: Vec<Qualifier>,
        name: Symbol,
        /// Span of the variant name itself.
        name_span: Span,
        args: Vec<Pattern>,
    },
    Record {
        /// The module qualifier of `util.Pt { x }`; same contract as
        /// [`PatternKind::Constructor::qualifier`] — checked by the
        /// typechecker, invisible to exhaustiveness and codegen.
        module: Option<Qualifier>,
        name: Option<Symbol>,
        /// Span of the type name itself.
        name_span: Span,
        /// Each field's name, the span of the name, and its sub-pattern;
        /// `None` for the shorthand `{ x }`, which binds `x`.
        fields: Vec<(Symbol, Span, Option<Pattern>)>,
        has_rest: bool,
    },
    /// Anonymous record pattern with optional named rest binding:
    /// `{name: n, ...rest}` binds `rest` to a record carrying the
    /// unmatched fields. `rest = None` means closed (no extra fields
    /// allowed). v1 forbids unnamed rest (`{x, ...}`); use `{x, ...rest}`
    /// or omit the pattern entirely.
    AnonRecord {
        /// As in [`PatternKind::Record::fields`].
        fields: Vec<(Symbol, Span, Option<Pattern>)>,
        /// The rest binder and the span of its name.
        rest: Option<(Symbol, Span)>,
    },
    /// Match a list: [a, b, c] or [head, ...tail] or []
    List(Vec<Pattern>, Option<Box<Pattern>>),
    /// Or-pattern: 0 | 1 -> "small"
    Or(Vec<Pattern>),
    /// Range pattern: 1..10 (inclusive on both ends)
    Range(i64, i64),
    /// Float range pattern: 1.0..10.0 (inclusive on both ends)
    FloatRange(f64, f64),
    /// Map pattern: #{ "key": value } — keys are string literals, not identifiers
    Map(Vec<(String, Pattern)>),
    /// Pin pattern: ^name -- matches against the existing variable's value
    Pin(Symbol),
}

// ── Parameters & type expressions ────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamKind {
    /// Regular data parameter: `name: Type` or `name` (type inferred)
    Data,
    /// Type-as-value parameter: `type a` — the argument at this position is
    /// a type, and the identifier is in scope as a type variable in the rest
    /// of the signature and body.
    Type,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub kind: ParamKind,
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
}

/// Wrapper over `TypeExprKind` that carries a `Span` for diagnostics.
/// The span points at the start of the type-expr token (e.g. the `Int`
/// in `trait Foo(Int)` or the opening `(` of a tuple type). Mirrors the
/// `Expr`/`ExprKind` and `Pattern`/`PatternKind` splits so diagnostics
/// can attach the caret to the offending argument rather than the
/// enclosing decl's opener.
#[derive(Debug, Clone)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
    /// What the type name of a `Named` or `Generic` type names (a type
    /// variable is [`Res::Local`]), or the trait of an `AssocProj`;
    /// filled in by the resolver.
    pub res: Option<Res>,
}

impl TypeExpr {
    pub fn new(kind: TypeExprKind, span: Span) -> Self {
        Self {
            kind,
            span,
            res: None,
        }
    }
}

/// A segment written before a name, with its span: the `m` of `m.Shape`,
/// or each of `m` and `Shape` in the pattern `m.Shape.Circle(r)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qualifier {
    pub name: Symbol,
    pub span: Span,
}

impl Qualifier {
    /// `name` as written after the optional qualifier `module`: `Shape`
    /// or `m.Shape`.
    pub fn written(module: Option<Qualifier>, name: Symbol) -> String {
        match module {
            Some(m) => format!("{}.{}", m.name, name),
            None => name.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TypeExprKind {
    /// A type name, `Shape` or `m.Shape`. `name_span` is the span of the
    /// name itself; the type expression's span covers the qualifier too.
    Named {
        module: Option<Qualifier>,
        name: Symbol,
        name_span: Span,
    },
    /// An applied type, `Box(Int)` or `m.Box(Int)`; as [`Self::Named`].
    Generic {
        module: Option<Qualifier>,
        name: Symbol,
        name_span: Span,
        args: Vec<TypeExpr>,
    },
    Tuple(Vec<TypeExpr>),
    Function(Vec<TypeExpr>, Box<TypeExpr>),
    SelfType,
    /// Associated-type projection.
    ///
    /// Two surface forms feed this single AST shape:
    ///
    /// - `Self::Item` inside a trait body — `receiver` is
    ///   `TypeExprKind::SelfType` and `trait_name` is the enclosing
    ///   trait (filled in by the parser from its current trait context).
    /// - `<TypeExpr as Trait>::Item` outside a trait body — `receiver`
    ///   is the user-supplied type expression and `trait_name` is the
    ///   trait the projection is qualified against.
    ///
    /// The typechecker resolves this to `Type::AssocProj { ... }`,
    /// which canonicalises to the impl's binding when the receiver is
    /// concrete and stays abstract on a type variable.
    AssocProj {
        receiver: Box<TypeExpr>,
        /// The module qualifier of the trait: `<T as m.Iter>::Item`.
        trait_module: Option<Qualifier>,
        trait_name: Symbol,
        assoc_name: Symbol,
    },
    /// Anonymous structural record type, optionally row-polymorphic:
    /// `{name: String, age: Int}` (closed) or `{name: String, ...r}`
    /// (open, where `r` is a row variable name). Multiple annotations
    /// in the same scope sharing a row name `r` refer to the same
    /// row variable, so `fn id(p: {name: String, ...r}) -> {name: String, ...r}` threads it.
    AnonRecord {
        fields: Vec<(Symbol, TypeExpr)>,
        tail: Option<Symbol>,
    },
}

// ── Statements ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        pattern: Pattern,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    When {
        pattern: Pattern,
        expr: Expr,
        else_body: Expr,
    },
    WhenBool {
        condition: Expr,
        else_body: Expr,
    },
    Expr(Expr),
}

// ── Declarations ─────────────────────────────────────────────────────

/// A where-clause constraint. `where a: Display` records the bound
/// type variable, the trait name, and the trait's supplied arguments.
///
/// `trait_args` is empty for parameter-less traits (`where a: Display`)
/// and carries the supplied args for parameterized traits
/// (`where a: TryInto(Int)` yields `[TypeExpr::Named("Int")]`).
///
/// `trait_name_span` points at the trait-name identifier in source so
/// LSP rename / references / goto-def can land precisely on the trait
/// reference (round-75 DX-4 fix). A synthesized clause (auto-derive) has
/// the span of the type declaration that caused it.
#[derive(Debug, Clone)]
pub struct WhereClause {
    pub type_param: Symbol,
    /// The trait's module qualifier: `where a: m.Describe`.
    pub trait_module: Option<Qualifier>,
    pub trait_name: Symbol,
    pub trait_args: Vec<TypeExpr>,
    pub trait_name_span: Span,
    /// What the trait name names; filled in by the resolver.
    pub trait_res: Option<Res>,
}

/// An associated-type declaration inside a trait body.
///
/// `trait Iterator { type Item; ... }` — declares an abstract type that
/// each impl must bind. v1 supports declared bounds:
/// `trait Container { type Item: Compare + Hash; ... }` requires every
/// impl's `type Item = X` to satisfy each listed bound. Defaults are
/// not supported in v1; the parser rejects `type Item = Default`.
#[derive(Debug, Clone)]
pub struct AssocTypeDecl {
    pub name: Symbol,
    /// Declared trait bounds, e.g. `[Compare, Hash]` for
    /// `type Item: Compare + Hash`, with any trait args
    /// (parameterized traits like `TryInto(Int)`).
    /// Empty for unbounded `type Item`.
    pub bounds: Vec<TraitRef>,
    pub span: Span,
}

/// An associated-type binding inside a trait impl body.
///
/// `impl Iterator for IntList { type Item = Int; ... }` binds the
/// trait's `Item` to `Int`. The typechecker verifies that the bound
/// type satisfies each declared bound and that every assoc-type the
/// trait declares has a binding.
#[derive(Debug, Clone)]
pub struct AssocTypeBinding {
    pub name: Symbol,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: Symbol,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub where_clauses: Vec<WhereClause>,
    pub body: Expr,
    pub is_pub: bool,
    pub span: Span,
    /// Span of the declared name identifier itself (the `foo` in
    /// `fn foo(...)`). Distinct from `span`, which currently points at
    /// the leading `fn` keyword and is used for diagnostics / goto-def
    /// landing zones. The LSP rename / references / definition /
    /// document-highlight handlers need the *identifier* range so that
    /// rename edits replace the name and not the keyword. For
    /// synthesized FnDecls (auto-derive, recovery stubs, builtin trait
    /// signatures) this falls back to `span`.
    pub name_span: Span,
    /// True when this declaration was synthesized by parser error recovery
    /// (Option B: salvage the header and emit a stub so downstream references
    /// to `name` do not cascade into "undefined variable" errors). The body
    /// of a recovery stub is an empty/synthetic `Block` and must NOT be
    /// type-checked; at call sites, the stub's signature is trusted only
    /// enough to return a fresh type variable (no arity/arg-type cascade).
    pub is_recovery_stub: bool,
    /// True when this is an abstract trait method (signature only, no body).
    /// Set by the parser when no `{ block }` follows the method header.
    /// The `body` field still holds an `ExprKind::Unit` placeholder so the
    /// AST shape stays uniform, but downstream consumers (typechecker
    /// default-method synthesis, formatter) use this flag to distinguish
    /// abstract methods from methods that legitimately return unit via an
    /// explicit `{ }` body.
    pub is_signature_only: bool,
    /// Doc comment immediately preceding the decl token (or the `pub`
    /// keyword on a `pub fn`). Collected by the parser from `--` line
    /// comments and/or `{- ... -}` block comments that end on the line
    /// immediately above the declaration with no blank line in between.
    /// Multiple adjacent comment segments are concatenated with `\n`.
    /// LSP hover / completion / signature-help render this as Markdown.
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub enum TypeBody {
    Enum(Vec<EnumVariant>),
    Record(Vec<RecordField>),
    /// Type alias: `type Bytes = List(Int)` or `type Pair(a) = (a, a)`.
    /// The right-hand side is any TypeExpr the parser produces for fn
    /// parameter annotations. Aliases are transparent: every mention of
    /// the alias name reduces to the target's canonical form for
    /// typechecking, dispatch, and runtime. Parametric aliases bind their
    /// `params` in the target so `type Pair(a) = (a, a)` can be
    /// instantiated as `Pair(Int)` and substitute `a -> Int` in the
    /// target before canonicalisation.
    ///
    /// Phase D of the canonical type-equality refactor (see
    /// `src/types/canonical.rs` module doc) — the canonicaliser owns
    /// alias expansion via a global registry the typechecker populates
    /// on decl processing.
    Alias(TypeExpr),
}

#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: Symbol,
    /// Span of the variant-name identifier itself (the `Circle` in
    /// `Circle(Int)`). Mirrors `FnDecl::name_span` / `TypeDecl::name_span`
    /// (round-63 B1): LSP rename / references / goto-def need the name
    /// token's range, not the enclosing decl's `type`-keyword span —
    /// without it, renaming a variant from a usage site rewrote the
    /// `type` keyword. A variant synthesized outside the parser
    /// (auto-derive's VariantInfo round-trip) has the span of its type's
    /// declaration.
    pub name_span: Span,
    pub fields: Vec<TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct RecordField {
    pub name: Symbol,
    /// Span of the field-name identifier.
    pub name_span: Span,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub name: Symbol,
    pub params: Vec<Symbol>,
    pub body: TypeBody,
    pub is_pub: bool,
    pub span: Span,
    /// Span of the declared name identifier itself (the `Foo` in
    /// `type Foo { ... }`). Mirrors `FnDecl::name_span`; see that doc
    /// for the rationale (LSP rename / references need the name range,
    /// not the `type` keyword range).
    pub name_span: Span,
    /// Doc comment immediately preceding the decl token. See `FnDecl::doc`.
    pub doc: Option<String>,
}

/// A reference to a trait in a supertrait list or an associated-type
/// bound: `Equal`, `m.Describe`, `TryInto(Int)`. `span` is the span of
/// the trait name itself.
#[derive(Debug, Clone)]
pub struct TraitRef {
    pub module: Option<Qualifier>,
    pub name: Symbol,
    pub args: Vec<TypeExpr>,
    pub span: Span,
    /// What the name names; filled in by the resolver.
    pub res: Option<Res>,
}

impl TraitRef {
    /// The where-clause `type_param: <self>`.
    pub fn bound_on(self, type_param: Symbol) -> WhereClause {
        WhereClause {
            type_param,
            trait_module: self.module,
            trait_name: self.name,
            trait_args: self.args,
            trait_name_span: self.span,
            trait_res: self.res,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub name: Symbol,
    /// Span of the trait-name identifier (the `Foo` in
    /// `trait Foo { ... }`). Distinct from `span`, which points at the
    /// `trait` keyword. Round-75 DX-2: LSP rename / references / goto-def
    /// need the *identifier* range so rename edits replace the name and
    /// not the keyword. For synthesized TraitDecls (built-in trait
    /// signatures) this falls back to `span`.
    pub name_span: Span,
    /// Type parameters on the trait itself: `trait TryInto(b) { ... }`
    /// yields `[b]`. Each lowercase ident binds a fresh type variable
    /// that is in scope throughout the trait's method signatures.
    /// Empty for parameter-less traits (the common case).
    pub params: Vec<Symbol>,
    /// Supertrait references (e.g. `trait Ordered: Equal + Hash` yields
    /// `Equal` and `Hash`). Implementing this trait on a type requires
    /// the type to also implement every supertrait. Inside a
    /// `where a: Ordered` context, methods from supertraits are also
    /// callable on `a`. Parameterized supertraits carry type expressions
    /// that may reference the enclosing trait's own params:
    /// `trait Sub(a): Super(a)`. Each reference's span points at the
    /// supertrait-name identifier in source so LSP rename / references
    /// can edit the supertrait reference (round-75 DX-4).
    pub supertraits: Vec<TraitRef>,
    /// Where bounds on the trait's own type parameters, e.g.
    /// `trait HashTable(k) where k: Hash + Equal { ... }`. Every impl
    /// is required to supply type args that satisfy each bound;
    /// verified at `register_trait_impl` time.
    pub param_where_clauses: Vec<WhereClause>,
    pub methods: Vec<FnDecl>,
    /// Associated-type declarations: `trait Iterator { type Item; ... }`.
    /// Empty for traits with no associated types (the common case).
    /// Each entry's bounds must be satisfied by the impl-supplied type;
    /// every entry must have a corresponding binding in every impl.
    pub assoc_types: Vec<AssocTypeDecl>,
    /// True for `pub trait`.
    pub is_pub: bool,
    pub span: Span,
    /// Doc comment immediately preceding the decl token. See `FnDecl::doc`.
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TraitImpl {
    /// The trait's module qualifier: `trait m.Describe for T`.
    pub trait_module: Option<Qualifier>,
    pub trait_name: Symbol,
    /// What the trait name names; filled in by the resolver.
    pub trait_res: Option<Res>,
    /// Span of the trait-name identifier in `trait <Name> for ...`.
    /// Used by LSP rename / references so cursor on the impl's trait
    /// reference resolves to the trait declaration (round-75 DX-4).
    /// For synthesized impls (auto-derive) this falls back to `span`.
    pub trait_name_span: Span,
    /// Arguments supplied to the trait at impl time:
    /// `trait TryInto(Int) for String { ... }` yields `[Int]`.
    /// Empty for traits declared without parameters.
    pub trait_args: Vec<TypeExpr>,
    /// The target's module qualifier: `trait Display for m.Pt`.
    pub target_module: Option<Qualifier>,
    /// Head symbol of the impl target. For `trait X for Box(a)` this is
    /// `Box`; for the bare-target form `trait X for Int` it is `Int`.
    /// Kept as a Symbol so method_table keys, qualified-name emission in
    /// the compiler, and coherence checks can reference the impl by head
    /// name without having to inspect the type arguments.
    pub target_type: Symbol,
    /// What the target's head names; filled in by the resolver.
    pub target_res: Option<Res>,
    /// Span of the target-type head-name identifier in `... for <Target>`
    /// (e.g. `Int` in `for Int`, `Box` in `for Box(a)`). Used by LSP
    /// rename / references on the impl-target reference. For synthesized
    /// impls (auto-derive) this falls back to `span`.
    pub target_type_span: Span,
    /// Type arguments on the target, if any. `trait X for Box(a)` yields
    /// `[TypeExpr::Named("a")]`; the bare `trait X for Int` yields `[]`.
    /// Each lowercase `Named` entry binds a fresh type variable in the
    /// impl's methods' signatures and bodies via param_map; the lowercase
    /// convention matches fn-signature polymorphism elsewhere in silt.
    pub target_type_args: Vec<TypeExpr>,
    /// Lowercase type-variable names extracted from `target_type_args`
    /// (deduplicated, in source order). Populated by the parser during
    /// impl-header parsing. The typechecker pre-seeds each method's
    /// param_map with fresh TyVars keyed on these names so method bodies
    /// see `a` as a concrete (but polymorphic) tyvar instead of a lexical
    /// ident. Empty for the bare-target form.
    pub target_param_names: Vec<Symbol>,
    /// Impl-level `where` clauses on the target's type parameters, e.g.
    /// `trait Greet for Box(a) where a: Greet { ... }`. Each clause is
    /// `(type_var_name, trait_name)` — multi-trait bounds via `+` are
    /// flattened into separate entries sharing a type_var. Constraints
    /// here apply to every method in the impl, and the typechecker
    /// appends them to each method's scheme during register_trait_impl.
    /// The `where` clause syntax matches fn-decl syntax exactly,
    /// including multi-trait bounds via `+`.
    pub where_clauses: Vec<WhereClause>,
    pub methods: Vec<FnDecl>,
    /// Associated-type bindings supplied by this impl. Each entry binds
    /// one of the trait's declared associated types to a concrete type
    /// expression. The typechecker enforces:
    ///   - every assoc-type the trait declares has a binding here;
    ///   - no name appears twice;
    ///   - the bound type satisfies each declared trait bound.
    pub assoc_type_bindings: Vec<AssocTypeBinding>,
    pub span: Span,
    /// True when this impl block was synthesized by the auto-derive pass
    /// (Display / Compare / Equal / Hash for user-declared enums and
    /// records). Synthesized impls register their methods into the
    /// method_table with `is_auto_derived: true` so that a subsequent
    /// user-written `trait Display for Color { ... }` is allowed to
    /// override the generated body without colliding with the
    /// duplicate-impl coherence check in `register_trait_impl`. (A
    /// user-written impl of the sealed Equal / Compare / Hash is an
    /// error.)
    /// Default false for parser-produced impls.
    pub is_auto_derived: bool,
}

#[derive(Debug, Clone)]
pub enum ImportTarget {
    Module(Symbol),
    /// `import m.{ a, b }`: the module and each item with its own span.
    Items(Symbol, Vec<(Symbol, Span)>),
    /// `import m as a`: the module, the alias and the alias's span.
    Alias(Symbol, Symbol, Span),
}

#[derive(Debug, Clone)]
pub enum Decl {
    Fn(FnDecl),
    Type(TypeDecl),
    Trait(TraitDecl),
    TraitImpl(TraitImpl),
    Import(ImportTarget, Span),
    Let {
        pattern: Pattern,
        ty: Option<TypeExpr>,
        value: Expr,
        is_pub: bool,
        span: Span,
        /// Span of the binding's name identifier when `pattern` is a bare
        /// `Ident` (e.g. the `counter` in `let counter = 42` or `pub let
        /// counter = 42`). `None` when the binding is a destructuring
        /// pattern (`let (a, b) = ...`, `let User { name } = ...`, etc.)
        /// — those have no single name to point at, so LSP rename through
        /// the let bails. Mirrors `FnDecl::name_span` / `TypeDecl::name_span`
        /// (round-63 B1); without it, `span` (the `let` keyword) gets used
        /// for rename / references and corrupts the keyword.
        name_span: Option<Span>,
        /// Doc comment immediately preceding the decl token. See `FnDecl::doc`.
        doc: Option<String>,
    },
}

// ── Program ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Program {
    pub decls: Vec<Decl>,
}
