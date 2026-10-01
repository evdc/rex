//! The elaborated (typed) AST produced by the checker.
//!
//! Every node carries its [`RelTy`], field paths are resolved to concrete
//! `(SortId, field)` hops, identifiers are resolved to their kind (view /
//! entity-identity / value), and filter-position built-ins (`> 30`, `in (...)`)
//! are lowered to explicit [`Filter`](TExprKind::Filter) nodes. The evaluator
//! consumes this directly — no domain threading, env lookups, or type
//! recomputation needed.

use super::ty::{RelTy, SortId};
use crate::ast::{CmpOp, ProjSide};
use crate::span::Span;

/// A literal, kept in a types-level form so the typed AST doesn't depend on the
/// evaluator's `Value`.
#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    /// The one point of the `Unit` sort, written `unit` at the surface (S-50).
    Unit,
    Int(i64),
    Decimal(String),
    Str(String),
    Date { year: i32, month: u32, day: u32 },
    Atom(String),
}

/// The surface spelling of the constant relation onto `Unit` (S-50): a
/// built-in name, not a reserved word, so a user binding of the same name
/// shadows it.
pub const UNIT: &str = "unit";

/// The desugarer's spelling of the coreflexive point `{unit -> unit}` (S-53):
/// the membership relation and bind base of a `Unit`-root view level. Unlike
/// `unit` it needs no ambient domain (it *is* a `Unit -> Unit`), and the `#`
/// keeps a user program from ever writing it.
pub const UNIT_ROOT: &str = "unit#root";

/// The constructors of the predeclared `type Bool = True | False`
/// (MVP-PLAN §5 decision 3). A constructor names its atom verbatim (S-50),
/// so these are the atom names a comparison yields and a `Bool` field holds —
/// one spelling, shared by the checker, the desugarer and the dispatcher.
pub const TRUE: &str = "True";
pub const FALSE: &str = "False";

/// The keys an aggregate has whether or not its image has rows for them; see
/// [`TExprKind::Agg`]. An aggregate is a fold, and a fold over nothing is its
/// initial value — but only where there is a key to hold it, and which keys
/// exist is decided by the key's *type*: the one `Unit` point, the live rows
/// of an entity, the constructors of an enum. A scalar key (`Int`, `Text`)
/// has no such domain, and its groups exist only as the image produces them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Total {
    /// Keys appear only as the image produces them.
    No,
    /// The `Unit` point is always a key.
    Unit,
    /// Every live row of this entity is a key.
    Entity(SortId),
    /// Every one of these atoms is a key.
    Atoms(Vec<String>),
}

impl Total {
    /// The keys that are there by construction, in order.
    pub fn static_keys(&self) -> Vec<crate::eval::Value> {
        use crate::eval::Value;
        match self {
            Total::Unit => vec![Value::Unit],
            Total::Atoms(atoms) => {
                let mut keys: Vec<Value> = atoms.iter().map(|a| Value::atom(a)).collect();
                keys.sort();
                keys.dedup();
                keys
            }
            Total::No | Total::Entity(_) => Vec::new(),
        }
    }
}

/// A grounded coreflexive built-in used in filter position.
#[derive(Clone, Debug, PartialEq)]
pub enum Pred {
    Cmp(CmpOp, Lit),
    InSet(Vec<Lit>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggKind {
    Sum,
    Count,
    Avg,
    Min,
    Max,
}

/// One hop of a resolved field path: field `field` of entity sort `sort`.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldHop {
    pub sort: SortId,
    pub field: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TExpr {
    pub kind: TExprKind,
    pub ty: RelTy,
    pub span: Span,
}

impl TExpr {
    pub fn new(kind: TExprKind, ty: RelTy, span: Span) -> TExpr {
        TExpr { kind, ty, span }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TExprKind {
    /// Identity/diagonal over all ids of a sort (`id`, or an entity name).
    Identity(SortId),
    /// Reference to a view (`let`) binding.
    View(String),
    /// Reference to a member of the enclosing `LetRec` group — the recursive
    /// knot (§8). Distinct from `View` so the fixpoint drivers and the
    /// stratification check can tell it from an ordinary back-reference.
    RecVar(String),
    /// Reference to a `new`-bound entity id.
    ValueRef(String),
    /// A resolved field path, first hop first.
    Field(Vec<FieldHop>),
    /// A constant relation `dom -> lit` over every id of `dom`.
    Const { lit: Lit, dom: SortId },
    /// A singleton coreflexive `{@a -> @a}`.
    Atom(String),
    /// The singleton coreflexive `{unit -> unit}` on `Unit` (S-53).
    UnitPoint,
    /// A constant relation `unit -> lit` on `Unit` itself: what a literal
    /// grounds to when the ambient domain is `Unit` rather than an entity
    /// (S-53), so `total > 0` and `filter = All` type-check at a root level.
    UnitConst(Lit),

    Compose(Box<TExpr>, Box<TExpr>),
    Semijoin(Box<TExpr>, Box<TExpr>),
    /// Filter the left relation's right column by a grounded predicate.
    Filter(Box<TExpr>, Pred),
    Fork(Box<TExpr>, Box<TExpr>),
    Union(Box<TExpr>, Box<TExpr>),
    Intersect(Box<TExpr>, Box<TExpr>),
    Inverse(Box<TExpr>),
    Distinct(Box<TExpr>),
    /// `fst R` / `snd R`: project one component of a pair-valued right column.
    Proj(ProjSide, Box<TExpr>),
    /// `X by Y` (kept as the idiom `~Y . X`).
    By(Box<TExpr>, Box<TExpr>),
    /// `except`/`antijoin`: `A - A[B]`.
    Antijoin(Box<TExpr>, Box<TExpr>),
    Mul(Box<TExpr>, Box<TExpr>),
    Concat(Box<TExpr>, Box<TExpr>),
    /// `+ - / %` on co-keyed numeric columns (SYNTAX v1 §4).
    Arith(ArithKind, Box<TExpr>, Box<TExpr>),

    /// A standalone coreflexive built-in (rare; usually folded into `Filter`).
    Coreflexive(Pred),
    /// Binary comparison `a OP b`, a coreflexive on the shared key.
    BinCompare(CmpOp, Box<TExpr>, Box<TExpr>),
    /// `lhs in {set}`, a coreflexive on `lhs`'s key.
    InRel(Box<TExpr>, Vec<Lit>),
    /// `count(X by g)`. `total` marks a group-key domain that is non-empty by
    /// construction — in v1 that is exactly `by unit`, whose key set is the
    /// single `Unit` point. A total group emits its monoid *identity* for an
    /// empty image (`count(Todo by unit)` is `0`, not "no row"), which is what
    /// makes a global counter a real relation rather than a missing one.
    /// `Min`/`Max` have no identity and `Avg` is not a monoid, so they still
    /// emit nothing when empty (the batch kernel's behaviour, unchanged).
    Agg(AggKind, Box<TExpr>, Total),
}

/// A field value in a `new`: either a literal or a reference to a bound id.
#[derive(Clone, Debug, PartialEq)]
pub enum TValue {
    Lit(Lit),
    Ref(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TStmt {
    Let {
        name: Option<String>,
        body: TExpr,
    },
    /// One recursion group: consecutive `let recursive` statements whose
    /// bodies may reference any member via [`TExprKind::RecVar`]. Semantics is
    /// the joint least fixpoint (Kleene iteration from ∅) with a forced
    /// `distinct` at each knot (§8).
    LetRec {
        bindings: Vec<(String, TExpr)>,
    },
    New {
        name: Option<String>,
        sort: SortId,
        fields: Vec<(String, TValue)>,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TProgram {
    pub stmts: Vec<TStmt>,
}

/// The four arithmetic operators beyond `*` (which predates them and keeps
/// its own node). `Div`/`Mod` are Int-only; `Add`/`Sub` follow `*`'s
/// Money-if-either rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithKind {
    Add,
    Sub,
    Div,
    Mod,
}
