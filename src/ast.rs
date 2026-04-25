use crate::intern::Symbol;
use crate::lexer::Span;
use crate::types::Type;

// ── Expressions ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    pub ty: Option<Type>,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self {
            kind,
            span,
            ty: None,
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
    FieldAccess(Box<Expr>, Symbol),

    // Operations
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Pipe(Box<Expr>, Box<Expr>),
    Range(Box<Expr>, Box<Expr>),
    QuestionMark(Box<Expr>),
    /// Float else: `expr else fallback` — narrows ExtFloat to Float.
    FloatElse(Box<Expr>, Box<Expr>),
    Ascription(Box<Expr>, TypeExpr),

    // Function-related
    Call(Box<Expr>, Vec<Expr>),
    Lambda {
        params: Vec<Param>,
        body: Box<Expr>,
    },

    // Records
    RecordCreate {
        name: Symbol,
        fields: Vec<(Symbol, Expr)>,
    },
    RecordUpdate {
        expr: Box<Expr>,
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
    /// Loop expression: `loop x = init, y = init { body }`
    Loop {
        bindings: Vec<(Symbol, Expr)>,
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
}

impl Pattern {
    pub fn new(kind: PatternKind, span: Span) -> Self {
        Self { kind, span }
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
    Constructor(Symbol, Vec<Pattern>),
    Record {
        name: Option<Symbol>,
        fields: Vec<(Symbol, Option<Pattern>)>,
        has_rest: bool,
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
}

impl TypeExpr {
    pub fn new(kind: TypeExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

#[derive(Debug, Clone)]
pub enum TypeExprKind {
    Named(Symbol),
    Generic(Symbol, Vec<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Function(Vec<TypeExpr>, Box<TypeExpr>),
    SelfType,
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

/// A where-clause constraint. `(type_var, trait_name, trait_args)` —
/// `trait_args` is empty for parameter-less traits (`where a: Display`)
/// and carries the supplied args for parameterized traits
/// (`where a: TryInto(Int)` yields `[TypeExpr::Named("Int")]`).
pub type WhereClause = (Symbol, Symbol, Vec<TypeExpr>);

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: Symbol,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub where_clauses: Vec<WhereClause>,
    pub body: Expr,
    pub is_pub: bool,
    pub span: Span,
    /// True when this declaration was synthesized by parser error recovery
    /// (Option B: salvage the header and emit a stub so downstream references
    /// to `name` do not cascade into "undefined variable" errors). The body
    /// of a recovery stub is an empty/synthetic `Block` and must NOT be
    /// type-checked; at call sites, the stub's signature is trusted only
    /// enough to return a fresh type variable (no arity/arg-type cascade).
    pub is_recovery_stub: bool,
    /// True when this is an abstract trait method (signature only, no body).
    /// Set by the parser when neither `= expr` nor `{ block }` follows the
    /// method header. The `body` field still holds an `ExprKind::Unit`
    /// placeholder so the AST shape stays uniform, but downstream consumers
    /// (typechecker default-method synthesis, formatter) use this flag to
    /// distinguish abstract methods from methods that legitimately return
    /// unit via an explicit `= ()` or `{ }` body.
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
}

#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: Symbol,
    pub fields: Vec<TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct RecordField {
    pub name: Symbol,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub name: Symbol,
    pub params: Vec<Symbol>,
    pub body: TypeBody,
    pub is_pub: bool,
    pub span: Span,
    /// Doc comment immediately preceding the decl token. See `FnDecl::doc`.
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub name: Symbol,
    /// Type parameters on the trait itself: `trait TryInto(b) { ... }`
    /// yields `[b]`. Each lowercase ident binds a fresh type variable
    /// that is in scope throughout the trait's method signatures.
    /// Empty for parameter-less traits (the common case).
    pub params: Vec<Symbol>,
    /// Supertrait references (e.g. `trait Ordered: Equal + Hash` yields
    /// `[(Equal, []), (Hash, [])]`). Implementing this trait on a type
    /// requires the type to also implement every supertrait. Inside a
    /// `where a: Ordered` context, methods from supertraits are also
    /// callable on `a`. Parameterized supertraits carry type
    /// expressions that may reference the enclosing trait's own params:
    /// `trait Sub(a): Super(a)` yields `[(Super, [TypeExpr::Named("a")])]`.
    pub supertraits: Vec<(Symbol, Vec<TypeExpr>)>,
    /// Where bounds on the trait's own type parameters, e.g.
    /// `trait HashTable(k) where k: Hash + Equal { ... }`. Every impl
    /// is required to supply type args that satisfy each bound;
    /// verified at `register_trait_impl` time.
    pub param_where_clauses: Vec<WhereClause>,
    pub methods: Vec<FnDecl>,
    pub span: Span,
    /// Doc comment immediately preceding the decl token. See `FnDecl::doc`.
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TraitImpl {
    pub trait_name: Symbol,
    /// Arguments supplied to the trait at impl time:
    /// `trait TryInto(Int) for String { ... }` yields `[Int]`.
    /// Empty for traits declared without parameters.
    pub trait_args: Vec<TypeExpr>,
    /// Head symbol of the impl target. For `trait X for Box(a)` this is
    /// `Box`; for the bare-target form `trait X for Int` it is `Int`.
    /// Kept as a Symbol so method_table keys, qualified-name emission in
    /// the compiler, and coherence checks can reference the impl by head
    /// name without having to inspect the type arguments.
    pub target_type: Symbol,
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
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ImportTarget {
    Module(Symbol),
    Items(Symbol, Vec<Symbol>),
    Alias(Symbol, Symbol),
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
        /// Doc comment immediately preceding the decl token. See `FnDecl::doc`.
        doc: Option<String>,
    },
}

// ── Program ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Program {
    pub decls: Vec<Decl>,
}
