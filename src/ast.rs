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

#[derive(Debug, Clone)]
pub enum Pattern {
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

#[derive(Debug, Clone)]
pub struct Param {
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
}

#[derive(Debug, Clone)]
pub enum TypeExpr {
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

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: Symbol,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub where_clauses: Vec<(Symbol, Symbol)>,
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
}

#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub name: Symbol,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TraitImpl {
    pub trait_name: Symbol,
    pub target_type: Symbol,
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
    },
}

// ── Program ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Program {
    pub decls: Vec<Decl>,
}
