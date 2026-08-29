//! The lowered UI IR the compiler emits JS from.
//!
//! A `view` desugars (in [`super::view`]) into two products that live beside
//! the elaborated [`TProgram`](super::typed::TProgram):
//!
//!  - a **ShapeIR** — a tree of [`ShapeLevel`]s, one per nesting level, naming
//!    the auto-derived membership / order / attribute views (ordinary `let`s
//!    the checker elaborates) and carrying the static DOM skeleton plus the
//!    child-index paths where dynamic values and event listeners land; and
//!  - a **HandlerIR** — one [`HandlerDef`] per inline `on … =>` handler, a
//!    checked list of relational mutations the engine dispatches as one
//!    transaction.
//!
//! Codegen is a dumb emitter over these: no naming logic, no re-derivation.

use crate::ast::Extractor;

/// Everything a `view` (or several) compiles to, beside the core program.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapeProgram {
    /// One root level per `view` declaration.
    pub views: Vec<ShapeLevel>,
    /// Every inline handler, keyed by its generated `name`.
    pub handlers: Vec<HandlerDef>,
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
}

/// How a bound value lands on its element.
#[derive(Clone, Debug, PartialEq)]
pub enum BindKind {
    /// `element.textContent = decode(value)`.
    Text,
    /// A DOM property, cursor-guarded (`value` on an `<input>`).
    Prop(String),
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
    /// The [`HandlerDef::name`] to dispatch.
    pub handler: String,
    /// How to materialize each dispatch argument from the DOM event.
    pub args: Vec<ArgSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArgSpec {
    pub name: String,
    /// Canonical encoding tag for the value (`text`, `atom`, `int`, `id`).
    pub encoding: Encoding,
    pub extractor: Extractor,
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

// --- handlers -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct HandlerDef {
    /// Unique name (`board#list#card@drop`).
    pub name: String,
    /// The binders in scope, name -> encoding of the value passed as an arg.
    /// Includes `self` (the current row key) and every enclosing binder.
    pub binders: Vec<(String, Encoding)>,
    /// Declared parameters, name -> encoding.
    pub params: Vec<(String, Encoding)>,
    /// The checked mutation body, run as one transaction.
    pub body: Vec<MutationIR>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MutationIR {
    /// Set fields of the entity a reference names (a binder or param).
    Set {
        target: Ref,
        /// Entity name of the target (for field validation at dispatch).
        entity: String,
        updates: Vec<(String, ValRef)>,
    },
    /// Retract the entity a reference names.
    Delete { target: Ref },
    /// Create an entity, binding the new id to `bind` for later ops.
    Insert {
        entity: String,
        fields: Vec<(String, ValRef)>,
    },
}

/// A reference to a row key: a handler arg (binder or param) by name.
#[derive(Clone, Debug, PartialEq)]
pub struct Ref(pub String);

/// A value in a mutation: a literal, or a reference to an arg.
#[derive(Clone, Debug, PartialEq)]
pub enum ValRef {
    Lit(super::typed::Lit),
    Arg(String),
}
