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
        Self { kind, span, ty: None }
    }
}

impl ExprKind {
    pub fn kind_name(&self) -> &str {
        match self {
            ExprKind::Ident(name) => name,
            _ => "<expr>",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    // Literals
    Int(i64),
    Float(f64),
    Bool(bool),
    StringLit(String),
    StringInterp(Vec<StringPart>),

    // Collections
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Tuple(Vec<Expr>),

    // Variables & access
    Ident(String),
    FieldAccess(Box<Expr>, String),

    // Operations
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Pipe(Box<Expr>, Box<Expr>),
    Range(Box<Expr>, Box<Expr>),
    QuestionMark(Box<Expr>),

    // Function-related
    Call(Box<Expr>, Vec<Expr>),
    Lambda {
        params: Vec<Param>,
        body: Box<Expr>,
    },

    // Records
    RecordCreate {
        name: String,
        fields: Vec<(String, Expr)>,
    },
    RecordUpdate {
        expr: Box<Expr>,
        fields: Vec<(String, Expr)>,
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
        bindings: Vec<(String, Expr)>,
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

// ── Patterns ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard,
    Ident(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    StringLit(String),
    Tuple(Vec<Pattern>),
    Constructor(String, Vec<Pattern>),
    Record {
        name: Option<String>,
        fields: Vec<(String, Option<Pattern>)>,
        has_rest: bool,
    },
    /// Match a list: [a, b, c] or [head, ...tail] or []
    List(Vec<Pattern>, Option<Box<Pattern>>),
    /// Or-pattern: 0 | 1 -> "small"
    Or(Vec<Pattern>),
    /// Range pattern: 1..10 (inclusive on both ends)
    Range(i64, i64),
    /// Map pattern: #{ "key": value }
    Map(Vec<(String, Pattern)>),
    /// Pin pattern: ^name -- matches against the existing variable's value
    Pin(String),
}

// ── Parameters & type expressions ────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Param {
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
}

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named(String),
    Generic(String, Vec<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Function(Vec<TypeExpr>, Box<TypeExpr>),
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
    Expr(Expr),
}

// ── Declarations ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub where_clauses: Vec<(String, String)>,
    pub body: Expr,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum TypeBody {
    Enum(Vec<EnumVariant>),
    Record(Vec<RecordField>),
}

#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Vec<TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct RecordField {
    pub name: String,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub name: String,
    pub params: Vec<String>,
    pub body: TypeBody,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub name: String,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TraitImpl {
    pub trait_name: String,
    pub target_type: String,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ImportTarget {
    Module(String),
    Items(String, Vec<String>),
    Alias(String, String),
}

#[derive(Debug, Clone)]
pub enum Decl {
    Fn(FnDecl),
    Type(TypeDecl),
    Trait(TraitDecl),
    TraitImpl(TraitImpl),
    Import(ImportTarget),
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
