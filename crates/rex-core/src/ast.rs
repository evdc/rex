//! The surface (untyped) AST. Every node carries a `Span` for diagnostics.
//!
//! The tree is deliberately close to the surface syntax: desugaring (e.g.
//! `A OP B` -> `(A,B).OP`, `X by Y` -> `~Y . X`) happens in later phases, not
//! here, so the parser stays a faithful mirror of what the programmer wrote.

use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Entity(EntityDecl),
    Let(LetDecl),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntityDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LetDecl {
    /// `None` for the anonymous binding `let _ = ...`.
    pub name: Option<String>,
    pub ty: Option<Type>,
    pub body: Expr,
    /// `let recursive` — the body may reference the binding name (and the
    /// names of adjacent recursive lets: consecutive recursive lets form one
    /// fixpoint group, §8).
    pub recursive: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Lt,
    Gt,
    Le,
    Ge,
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Lt => "<",
            CmpOp::Gt => ">",
            CmpOp::Le => "<=",
            CmpOp::Ge => ">=",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    // Primaries
    Ident(String),
    Id,
    FieldPath(Vec<String>),
    Atom(String),
    Int(i64),
    Decimal(String),
    Str(String),
    Date { year: i32, month: u32, day: u32 },

    // Combinators
    Compose(Box<Expr>, Box<Expr>),
    Fork(Box<Expr>, Box<Expr>),
    Union(Box<Expr>, Box<Expr>),
    Intersect(Box<Expr>, Box<Expr>),
    Restrict(Box<Expr>, Box<Expr>), // R[S]
    Inverse(Box<Expr>),             // ~R
    Distinct(Box<Expr>),
    /// `fst R` / `snd R`: project one component of a pair-valued right column.
    Proj(ProjSide, Box<Expr>),
    Where(Box<Expr>, Box<Expr>),
    By(Box<Expr>, Box<Expr>),
    Except(Box<Expr>, Box<Expr>),
    Antijoin(Box<Expr>, Box<Expr>),

    // Functional operators
    Mul(Box<Expr>, Box<Expr>),
    Concat(Box<Expr>, Box<Expr>),

    // Relational comparisons (`lhs` is `None` in prefix/filter position, e.g. `> 30`)
    Compare {
        op: CmpOp,
        lhs: Option<Box<Expr>>,
        rhs: Box<Expr>,
    },
    In {
        lhs: Option<Box<Expr>>,
        rhs: Box<Expr>,
    },

    Call {
        func: String,
        args: Vec<Expr>,
    },
    New {
        entity: String,
        fields: Vec<FieldInit>,
    },
}

/// Which component of a pair `fst`/`snd` selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjSide {
    Fst,
    Snd,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    Named(String),          // Int, Money, Text, CustID, ...
    AtomSingleton(String),  // `@north` as a (singleton) type
    Arrow(Box<Type>, Box<Type>),
    Product(Box<Type>, Box<Type>),
    Coproduct(Vec<Type>), // {@a + @b + ...}
}
