//! The lowered UI IR the compiler emits JS from.
//!
//! A `view` desugars (in [`super::view`]) into two products that live beside
//! the elaborated [`TProgram`](super::typed::TProgram):
//!
//!  - a **ShapeIR** — a tree of [`ShapeLevel`]s, one per nesting level, naming
//!    the auto-derived membership / order / attribute views (ordinary `let`s
//!    the checker elaborates) and carrying the static DOM skeleton plus the
//!    child-index paths where dynamic values and event listeners land; and
//!  - an **EventIR** — one [`EventDef`] per declared `event`, carrying its
//!    typed params and the checked mutation body of its `on` handler; the
//!    engine runs one dispatch as one transaction.
//!
//! Codegen is a dumb emitter over these: no naming logic, no re-derivation.

use crate::ast::Extractor;

/// Everything a `view` (or several) compiles to, beside the core program.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapeProgram {
    /// One root level per `view` declaration.
    pub views: Vec<ShapeLevel>,
    /// Every declared `event` with its handler, keyed by event name.
    pub events: Vec<EventDef>,
    /// `import js "path" as alias` modules DOM handlers call (S-91).
    pub imports: Vec<JsImport>,
}

/// A JS module a DOM handler's extractor calls (`utils.randomLabels(1000)`).
#[derive(Clone, Debug, PartialEq)]
pub struct JsImport {
    pub alias: String,
    /// The path as written, relative to the `.rex` file.
    pub path: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapeLevel {
    /// Unique level id, also the membership view's `let` name (`board#list`).
    pub name: String,
    /// The entity/binder this level ranges over (`List`).
    pub entity: String,
    /// The `child -> parent` membership view (== `name`).
    pub membership_view: String,
    /// The `child -> orderKey` order view, if `order by` was given.
    pub order_view: Option<String>,
    /// The base field name behind the order view (for the rebalance sweep).
    pub order_field: Option<String>,
    /// `order by … desc`: the order view's keys sort descending (S-70).
    pub order_desc: bool,
    /// Child-index path, in the *parent* level's template, to the element this
    /// level mounts into (`ul(class="list") { … select … }`); empty means the
    /// parent's root element.
    pub slot: Vec<usize>,
    /// Index, in the slot element's template children, of the first static
    /// child that follows this level in the source. Rows of this level mount
    /// before it, so a level that appears while a later static sibling is
    /// already there lands in source order rather than at the end.
    pub anchor: Option<usize>,
    /// The static DOM skeleton for one row's element.
    pub template: Tpl,
    /// Dynamic value bindings, each with a child-index path to its target.
    pub attrs: Vec<AttrBinding>,
    /// Event listeners, each with a child-index path to its element.
    pub events: Vec<EventBinding>,
    /// Nested levels; their elements mount into this level's root element.
    pub children: Vec<ShapeLevel>,
}

/// The static DOM skeleton (no dynamic values, no child-level elements).
#[derive(Clone, Debug, PartialEq)]
pub enum Tpl {
    Elem {
        tag: String,
        classes: Vec<String>,
        /// Bare presentation modifiers: `draggable`, `dropTarget`.
        modifiers: Vec<String>,
        /// Static attributes (`type="checkbox"`).
        attrs: Vec<(String, String)>,
        children: Vec<Tpl>,
    },
    /// A static text node.
    Static(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttrBinding {
    /// The auto-derived view supplying the value (`board#list#title`).
    pub view: String,
    /// Child-index path from the level's root element to the bound element.
    pub path: Vec<usize>,
    pub kind: BindKind,
    /// The wire encoding of the bound value, so codegen decodes with
    /// `decodeInt`/`decodeMoney`/`decodeAtom` rather than always `decodeText`.
    pub encoding: Encoding,
}

/// How a bound value lands on its element.
#[derive(Clone, Debug, PartialEq)]
pub enum BindKind {
    /// `element.textContent = decode(value)`.
    Text,
    /// A DOM property, cursor-guarded (`value` on an `<input>`).
    Prop(String),
    /// A boolean DOM property (`checked`, `disabled`, …) set by the presence
    /// of a gate row, like a class: `checked=(active = 0)`.
    Flag(String),
    /// Toggle a class by the truthiness of the (atom/bool) value.
    Class(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventBinding {
    /// Child-index path from the level root to the listening element.
    pub path: Vec<usize>,
    pub dom_event: String,
    /// Event sub-selectors (`keydown.enter` -> `["enter"]`).
    pub modifiers: Vec<String>,
    /// The handler's extracted params, materialized from the DOM event in
    /// declaration order (later extractors may read earlier params).
    pub params: Vec<ArgSpec>,
    /// The `do E(args)` dispatches, run in order — each is its own logged
    /// event / engine transaction.
    pub dispatches: Vec<Dispatch>,
    /// Presentation actions run after the dispatches, in order.
    pub actions: Vec<UiAction>,
}

/// One `do E(args)` from a DOM handler: the event's params, each bound to a
/// value the listener has at hand.
#[derive(Clone, Debug, PartialEq)]
pub struct Dispatch {
    pub event: String,
    /// `(event param name, value)` in the event's declared order.
    pub args: Vec<(String, ArgRef)>,
}

/// A value a DOM listener can pass to a dispatch.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgRef {
    /// This level's row key.
    SelfKey,
    /// An enclosing level's row key: `1` is the parent row, `2` its parent…
    Ancestor(usize),
    /// One of the handler's extracted params, by name.
    Param(String),
    Lit(super::typed::Lit),
}

/// A presentation action in a DOM handler (`focus`, `clear`).
#[derive(Clone, Debug, PartialEq)]
pub enum UiAction {
    /// Focus the first input of the row a preceding dispatch created at the
    /// named child level.
    FocusNew { level: String },
    /// Focus a child element of this row by class.
    FocusClass(String),
    /// Reset the listening input's value.
    Clear,
    /// Restore the target input to the value its bind last set.
    Revert,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArgSpec {
    pub name: String,
    /// Canonical encoding tag for the value (`text`, `atom`, `int`, `id`).
    pub encoding: Encoding,
    pub extractor: Extractor,
    /// For a relation-typed param (`Int -> T`): the encoding of `T`. A JS
    /// extractor then returns an array, keyed by index (S-91).
    pub rel_value: Option<Encoding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Text,
    Int,
    Money,
    Atom,
    /// An entity id — already an encoded key string, passed through.
    Id,
}

// --- events ---------------------------------------------------------------

/// A declared `event E(p: T, …)` together with its `on E(p…)` handler body.
#[derive(Clone, Debug, PartialEq)]
pub struct EventDef {
    pub name: String,
    /// Declared parameters, in order.
    pub params: Vec<EventParam>,
    /// The checked mutation body, run as one transaction. Empty when the
    /// event has no `on` handler.
    pub body: Vec<MutationIR>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventParam {
    pub name: String,
    pub ty: ParamTy,
}

/// What an event parameter carries across the boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamTy {
    /// A scalar with its wire encoding (`Text`, `Int`, `Bool`…).
    Scalar(Encoding),
    /// An entity id of the named entity.
    Id(String),
    /// A relation `A -> B` (bulk args; dispatch support arrives in S-42).
    Rel(String, String),
}

impl ParamTy {
    pub fn encoding(&self) -> Encoding {
        match self {
            ParamTy::Scalar(e) => *e,
            ParamTy::Id(_) | ParamTy::Rel(..) => Encoding::Id,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MutationIR {
    /// Set fields of every row `target` names.
    Set {
        target: Target,
        /// Entity name of the target (for field validation at dispatch).
        entity: String,
        updates: Vec<(String, ValRef)>,
    },
    /// Retract every row `target` names.
    Delete { target: Target },
    /// Create an entity. `bind` is the name `let x = new …` gives the new
    /// row's id, in scope as a value for the handler's later statements —
    /// as an argument, a field value or a target, but not to read from: the
    /// row does not exist in the pre-event snapshot every read sees.
    Insert {
        entity: String,
        fields: Vec<(String, ValRef)>,
        bind: Option<String>,
    },
    /// `new Entity from rows as (k, v) { … }` (S-42): one entity per row of
    /// a relation-typed event param, minted in one transaction, in the
    /// param's key order.
    InsertFrom {
        entity: String,
        /// The relation-typed event param supplying rows.
        param: String,
        /// The `as (key, value)` binder names, bound per row when a field's
        /// value is a compound expression (`num: nextId + i`).
        key: String,
        value: String,
        fields: Vec<(String, FromField)>,
    },
    /// Synchronous `do E(args)`: the callee's mutations join this
    /// transaction (same pre-event snapshot); only the outer event is logged.
    /// `args` are in the callee's declared param order.
    Do { event: String, args: Vec<ValRef> },
}

/// One field initializer of an [`MutationIR::InsertFrom`] row: either the
/// per-row key/value binder itself, or an ordinary checked mutation value
/// (a literal or another event param) shared across every minted row.
#[derive(Clone, Debug, PartialEq)]
pub enum FromField {
    Key,
    Value,
    Val(ValRef),
}

/// A reference to a row key: an event param by name.
#[derive(Clone, Debug, PartialEq)]
pub struct Ref(pub String);

/// The synthetic scope/arg name a `where`-targeted bulk mutation's predicate
/// uses for "the row currently under test" (S-41): `types/view.rs`'s
/// `Desugar::mutation_target` rewrites a `.field` path to reference it before
/// checking the predicate as an ordinary `ValExpr::Field` hop, and
/// `events.rs`'s per-dispatch scan binds it to each candidate row's id in
/// turn. Not a legal Rex identifier, so it never collides with a real param.
pub const ROW_SELF: &str = "#row";

/// The hidden entity every `state` declaration becomes a field of (S-51). It
/// has exactly one row, minted as a genesis `new` so replay reproduces it,
/// and each `state s : T [= d]` is a field `s` on it — seeded iff a default
/// was written, so a defaultless state is genuinely absent rather than null.
/// `#` keeps the name unreachable from source.
pub const STATE_ENTITY: &str = "State#";
/// The `let` that binds the singleton's id.
pub const STATE_ROW: &str = "state#row";

/// Which rows a `Set`/`Delete` acts on (MVP-PLAN S-41).
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A single row named by a bound event param.
    One(Ref),
    /// Every row of `entity`'s whole identity relation — a predicate-free
    /// `delete Entity` (subtask 3): retract the lot in one transaction.
    All { entity: String },
    /// Every row of a hidden materialized keyset view (an arg-free `where`):
    /// the predicate doesn't read an event arg, so it's desugared to an
    /// ordinary `let` (`types/view.rs`'s `Desugar::mutation`) and read from
    /// `Circuit::view` at dispatch time — no per-dispatch scan.
    View { view: String, entity: String },
    /// Every row of `entity` whose predicate holds, evaluated at dispatch
    /// time: the arg-dependent case, O(N) over the entity since the
    /// predicate can't be precomputed (visible cost, MVP-PLAN §2.10/SPEC §10).
    Scan { entity: String, pred: ValExpr },
}

/// A value in a mutation: a literal, a reference to a param, or a checked
/// expression over the handler's params and their fields (S-40).
#[derive(Clone, Debug, PartialEq)]
pub enum ValRef {
    Lit(super::typed::Lit),
    Arg(String),
    Expr(ValExpr),
}

/// A mutation-value expression (MVP-PLAN S-40, §2.10: "Rex expressions
/// evaluated at a key" — a point-evaluation sublanguage over the handler's
/// bound params, not the general relational language `check_rel` types:
/// every leaf is a literal or a param, so evaluating one always yields
/// exactly one value, never a whole relation).
#[derive(Clone, Debug, PartialEq)]
pub enum ValExpr {
    Lit(super::typed::Lit),
    /// A handler param.
    Param(String),
    /// One field hop off another value (`t.completed`, `card.list.title`).
    Field(Box<ValExpr>, String),
    /// `not e`: flip a two-atom value to its other alternative (baked in at
    /// check time, since evaluation has no type environment to consult).
    Not(Box<ValExpr>, [String; 2]),
    /// `+ - / %` on numeric operands; `true` when the result is `Money`.
    Arith(super::typed::ArithKind, Box<ValExpr>, Box<ValExpr>, bool),
    /// `a ++ b` on `Text`.
    Concat(Box<ValExpr>, Box<ValExpr>),
    /// `a OP b`, decoded to the atom `@True`/`@False`.
    Compare(crate::ast::CmpOp, Box<ValExpr>, Box<ValExpr>),
    /// A bare `state` name (S-51): a point read of the one [`STATE_ENTITY`]
    /// row's field, against the pre-event snapshot like every other read
    /// here. A defaultless state that has never been `set` has no row, which
    /// is an error at dispatch rather than a silent null.
    State(String),
}
