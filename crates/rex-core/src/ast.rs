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
    /// A UI view: a nested relational expression with element constructors.
    /// Desugars to auto-derived membership/order/attribute `let`s plus a
    /// ShapeIR the compiler emits JS from (§M5, nesting-draft §9).
    View(ViewDecl),
    /// A singleton piece of application state (e.g. a filter selection).
    /// Desugars to a hidden `AppState` entity with one seeded row.
    State(StateDecl),
    /// `rel CardList(Card, List)` — a named binary relation `Card -> List`.
    /// Sugar: desugars to a functional field named `CardList` on `Card` plus a
    /// `let CardList = Card . :CardList`, so the relation is usable by name.
    Rel(RelDecl),
}

/// `rel Name(From, To)` — a named functional relation `From -> To`.
#[derive(Clone, Debug, PartialEq)]
pub struct RelDecl {
    pub name: String,
    pub from: String,
    pub to: String,
    pub span: Span,
}

// --- views (surface UI) ---------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ViewDecl {
    pub name: String,
    pub body: SelectExpr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StateDecl {
    pub name: String,
    pub ty: Type,
    pub default: Expr,
    pub span: Span,
}

/// `Entity [where pred..] [order by :path] select <element>` — one nesting
/// level. `entity` is both the source relation and the binder name in scope
/// inside `body`.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectExpr {
    pub entity: String,
    /// `Entity as l` — the row binder name in scope inside `body`. `None`
    /// defaults the binder to the entity name. An explicit alias (`as l`)
    /// disambiguates the row variable from the entity relation, so a child's
    /// membership reads `where :list = l` rather than reusing `List`.
    pub binder: Option<String>,
    /// Predicate conjuncts. Exactly one may relate a functional field to the
    /// enclosing parent binder (`:list == List`) — that is the membership
    /// relation; the rest are ordinary restrictions.
    pub wheres: Vec<Expr>,
    /// The `order by :path` field path, if present (designates the order view).
    pub order_by: Option<Vec<String>>,
    pub body: ElementExpr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ElementExpr {
    pub tag: String,
    pub classes: Vec<String>,
    /// Bare-ident presentation modifiers (`draggable`, `dropTarget`).
    pub modifiers: Vec<String>,
    pub attrs: Vec<AttrBind>,
    pub handlers: Vec<HandlerDecl>,
    pub children: Vec<Content>,
    pub span: Span,
}

/// A child item inside an element's `{ ... }` block.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Element(ElementExpr),
    Select(Box<SelectExpr>),
    /// Static text.
    Text(String),
    /// A dynamic text binding `:field` — a functional relation to a scalar.
    Bind(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttrBind {
    /// The attribute name; `class.foo` for a class toggle keyed on a Bool/Atom.
    pub name: String,
    pub value: AttrValue,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttrValue {
    Static(String),
    Bind(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct HandlerDecl {
    pub event: String,
    /// Event sub-selectors (`keydown.enter` -> `["enter"]`).
    pub modifiers: Vec<String>,
    pub params: Vec<HandlerParam>,
    pub body: Vec<Mutation>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HandlerParam {
    pub name: String,
    pub ty: Type,
    pub extractor: Extractor,
    pub span: Span,
}

/// A closed vocabulary of client-side event projections. How an arg is read
/// off a DOM event is presentation, implemented once in `rex-dom`.
#[derive(Clone, Debug, PartialEq)]
pub enum Extractor {
    Value,
    Checked,
    Drag(String),
    DropPos(String),
    EndOf(String),
    Prompt(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mutation {
    /// `[binder]:field := value` — set one field (of `self` if no binder).
    Set {
        binder: Option<String>,
        field: Vec<String>,
        value: Expr,
        span: Span,
    },
    /// `delete <binder>` — retract the entity a binder/param names.
    Delete { target: String, span: Span },
    /// `new Entity { .. }` — create an entity.
    New {
        entity: String,
        fields: Vec<FieldInit>,
        span: Span,
    },
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
