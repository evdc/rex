//! The surface (untyped) AST. Every node carries a `Span` for diagnostics.
//!
//! The tree is deliberately close to the surface syntax: desugaring (e.g.
//! `A OP B` -> `(A,B).OP`, `X by Y` -> `~Y . X`) happens in later phases, not
//! here, so the parser stays a faithful mirror of what the programmer wrote.
//!
//! Surface v1 (SYNTAX.md): `.` is compose-join everywhere and `.f` is a field
//! path; elements are `tag(props) { children }`; handler bodies are brace
//! blocks of [`HStmt`]s; `event`/`on`/`type`/`import` are top-level statements.

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
    /// Boxed: `ViewDecl` is by far the largest variant (nested element
    /// trees), so inlining it would widen every `Stmt` (clippy
    /// `large_enum_variant`).
    View(Box<ViewDecl>),
    /// A singleton piece of application state (e.g. a filter selection).
    /// Desugars to a hidden `AppState` entity with one seeded row.
    State(StateDecl),
    /// `rel CardList(Card, List)` — a named binary relation `Card -> List`.
    /// Sugar: desugars to a functional field named `CardList` on `Card` plus a
    /// `let CardList = Card . CardList`, so the relation is usable by name.
    Rel(RelDecl),
    /// `event E(p: T, …)` — a named, typed event (SYNTAX §3).
    Event(EventDecl),
    /// `on E(p, …) { … }` — the handler for a named event.
    On(OnDecl),
    /// `type Filter = All | Active | Completed` — a named union type.
    Type(TypeDecl),
    /// `import js "./utils.js" as utils` — a DOM-layer JS module for extractors.
    Import(ImportDecl),
}

/// `rel Name(From, To)` — a named functional relation `From -> To`.
#[derive(Clone, Debug, PartialEq)]
pub struct RelDecl {
    pub name: String,
    pub from: String,
    pub to: String,
    pub span: Span,
}

// --- events -----------------------------------------------------------------

/// A typed parameter (`event`/component params).
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OnDecl {
    pub event: String,
    /// Parameter names, positionally matched to the event's declared params.
    pub params: Vec<String>,
    pub body: Vec<HStmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDecl {
    pub name: String,
    /// Constructor names, e.g. `All | Active | Completed`.
    pub ctors: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImportDecl {
    pub path: String,
    pub alias: String,
    pub span: Span,
}

/// A statement in a handler body (`on E(...) { … }` or a DOM handler).
/// Verb first, target an expression (SYNTAX §3).
#[derive(Clone, Debug, PartialEq)]
pub enum HStmt {
    /// `[let x =] new E [from R as (k, v)] { f: e, … }`.
    New {
        bind: Option<String>,
        entity: String,
        from: Option<FromClause>,
        fields: Vec<FieldInit>,
        span: Span,
    },
    /// `x.f := e` (or `.f := e` for the level's own row) — one-field sugar.
    Assign {
        binder: Option<String>,
        field: String,
        value: Expr,
        span: Span,
    },
    /// `update T { f: e, … }` — set fields on every row of the keyset `T`.
    Update {
        target: Expr,
        sets: Vec<FieldInit>,
        span: Span,
    },
    /// `delete T` — retract every row of the keyset `T`.
    Delete { target: Expr, span: Span },
    /// `set s = e` — write a `state` or a component `local`.
    Set { name: String, value: Expr, span: Span },
    /// `do E(args)` — dispatch a named event.
    Do {
        event: String,
        args: Vec<Expr>,
        span: Span,
    },
    /// DOM action: reset the handler's target input.
    Clear { span: Span },
    /// DOM action: put the target input back to the last value the view gave it.
    Revert { span: Span },
    /// DOM action: `focus(c)` (a level binder) or `focus(.cls)` (a child element).
    Focus { target: FocusTarget, span: Span },
}

impl HStmt {
    pub fn span(&self) -> Span {
        match self {
            HStmt::New { span, .. }
            | HStmt::Assign { span, .. }
            | HStmt::Update { span, .. }
            | HStmt::Delete { span, .. }
            | HStmt::Set { span, .. }
            | HStmt::Do { span, .. }
            | HStmt::Clear { span }
            | HStmt::Revert { span }
            | HStmt::Focus { span, .. } => *span,
        }
    }
}

/// `from R as (k, v)` on a bulk `new`.
#[derive(Clone, Debug, PartialEq)]
pub struct FromClause {
    pub source: Expr,
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FocusTarget {
    /// `focus(c)` — the first input of the row this dispatch created at level `c`.
    Level(String),
    /// `focus(.edit)` — a child element of this row by class.
    Class(String),
}

// --- views (surface UI) ---------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ViewDecl {
    pub name: String,
    /// Component params (`view TodoItem(t: Todo)`); empty for a root view.
    pub params: Vec<Param>,
    /// `local editing = False` declarations preceding the body.
    pub locals: Vec<LocalDecl>,
    pub body: ViewBody,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ViewBody {
    /// `R as x … select <element>` — the root ranges over a relation.
    Select(SelectExpr),
    /// A bare element — the body sits at the implicit `Unit` level.
    Element(ElementExpr),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalDecl {
    pub name: String,
    pub ty: Option<Type>,
    pub default: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StateDecl {
    pub name: String,
    pub ty: Type,
    /// `None`: the state starts empty (a 0-row singleton) until `set`.
    pub default: Option<Expr>,
    pub span: Span,
}

/// `R [as x] [where pred..] [order by e [desc]] select <element>` — one nesting
/// level. `entity` names the source relation (an entity or a sub-identity
/// `let`); `binder` is the row name in scope inside `body`.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectExpr {
    pub entity: String,
    /// `Entity as l` — the row binder name in scope inside `body`. `None`
    /// defaults the binder to the entity name.
    pub binder: Option<String>,
    /// Predicate conjuncts. Exactly one may relate a functional field to the
    /// enclosing parent binder (`.list = l`) — that is the membership
    /// relation; the rest are ordinary restrictions.
    pub wheres: Vec<Expr>,
    pub order_by: Option<OrderBy>,
    /// The per-row element: an [`Content::Element`] or a
    /// [`Content::Component`] call (`select TodoItem(t)`).
    pub body: Content,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderBy {
    pub expr: Expr,
    pub desc: bool,
}

/// `tag(props) ["text"] { children }`.
#[derive(Clone, Debug, PartialEq)]
pub struct ElementExpr {
    pub tag: String,
    /// Bare-ident presentation modifiers (`draggable`, `dropTarget`, `autofocus`).
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
    /// A dynamic text binding: any expression co-keyed with the level
    /// (`.text`, `l.by.name`, `active`, `(count(Card by .list))`).
    Bind(Expr),
    /// `if (c) { … }` — children present iff the coreflexive `c` holds.
    If {
        cond: Expr,
        children: Vec<Content>,
        span: Span,
    },
    /// `Name(args) [{ … }]` — a component call, expanded inline.
    Component {
        name: String,
        args: Vec<Expr>,
        children: Option<Vec<Content>>,
        span: Span,
    },
    /// `children` — the slot where a component call's block lands.
    ChildrenSlot(Span),
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
    Bind(Expr),
}

/// `on event[.mod](params) { stmts }` / `=> stmt`.
#[derive(Clone, Debug, PartialEq)]
pub struct HandlerDecl {
    pub event: String,
    /// Event sub-selectors (`keydown.enter` -> `["enter"]`).
    pub modifiers: Vec<String>,
    pub params: Vec<HandlerParam>,
    pub body: Vec<HStmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HandlerParam {
    pub name: String,
    /// Optional: inferred from the extractor or the event signature otherwise.
    pub ty: Option<Type>,
    pub extractor: Extractor,
    pub span: Span,
}

/// A closed vocabulary of client-side event projections. How an arg is read
/// off a DOM event is presentation, implemented once in `rex-dom`.
#[derive(Clone, Debug, PartialEq)]
pub enum Extractor {
    Value,
    Checked,
    /// `drag(E)` — the dragged row's key, typed as entity `E`.
    Drag(String),
    /// `dropPos(c, x)` — a fractional key at the pointer among level `c`'s
    /// rows, excluding param `x`. The desugarer resolves the binder `c` to a
    /// level name before codegen sees it.
    DropPos { level: String, exclude: String },
    /// `endOf(c)` — a fresh key after the last row of level `c`.
    EndOf(String),
    /// `utils.fn(args)` — a JS function from an `import js` module.
    Js {
        module: String,
        func: String,
        args: Vec<Expr>,
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
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "!=",
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
    /// `.a.b.c` — a field path from the ambient row.
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
    /// `not P` — complement of a filter within the ambient entity.
    Not(Box<Expr>),

    // Functional operators
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Mod(Box<Expr>, Box<Expr>),
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

    /// `match e { pat => rel, … }`.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// `if c then a else b`.
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
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

#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    pub pat: Pattern,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    /// A constructor or bound name (`All`, `x`).
    Ident(String),
    Atom(String),
    Int(i64),
    Str(String),
    /// `_`
    Wildcard,
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
    Named(String),          // Int, Money, Text, Customer, Bool, Filter, ...
    AtomSingleton(String),  // `@north` as a (singleton) type
    Arrow(Box<Type>, Box<Type>),
    Product(Box<Type>, Box<Type>),
    Coproduct(Vec<Type>), // {@a | @b | ...}
}
