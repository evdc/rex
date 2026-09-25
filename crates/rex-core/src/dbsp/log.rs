//! The append-only event log (MVP-PLAN S-21): every write to a live engine
//! is the record of one named event, so a page reload can restore state by
//! replaying the log from empty rather than trusting a persisted snapshot of
//! derived views. See `crate::events` for how a declared `event`/`on` turns
//! into a logged [`Event`] (the write path), and [`Engine::replay`] for the
//! read-back path — split there rather than here because reconstructing a
//! non-genesis event's effect needs the checked `EventDef` registry
//! (`crate::events::dispatch_event` already owns that machinery).

use crate::eval::value::Value;

/// One event argument: a plain value, or (S-42) a small relation crossing
/// the boundary as the same `[k, v, w]` triples the shaper speaks.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    Value(Value),
    Rel(Vec<(Value, Value, i64)>),
}

/// One entry in the append-only log: `(seq, name, args)` plus two fields
/// reserved for M4's async effect membrane (MVP-PLAN §5 decision 2) — always
/// `None` in MVP, kept now so the log's shape never has to migrate once
/// they're used. `args` are keyed by the event's declared parameter names.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: u64,
    pub name: String,
    pub args: Vec<(String, ArgValue)>,
    pub cause: Option<u64>,
    pub intent: Option<String>,
}

/// The synthetic event name a program-setup `new` statement logs under
/// (`Engine::apply_typed_stmt`'s `TStmt::New` arm) — never a declared `on`
/// handler, so replay special-cases it instead of looking it up in the
/// `EventDef` registry.
pub const GENESIS: &str = "@genesis";

/// Reserved genesis arg name carrying the target entity's sort id (as a
/// `Value::Int`); every other arg is a field value verbatim.
pub const GENESIS_SORT: &str = "__sort";

/// The system event `maybeRebalance` dispatches (S-22 subtask 4): re-spacing
/// a manual-order level's keys is a write like any other, so it goes through
/// [`Engine::apply_rebalance`](super::engine::Engine::apply_rebalance) rather
/// than the N independent field writes the pre-log implementation used,
/// and it is logged/replayed like a declared event even though it has no
/// checked `EventDef` (there is no surface syntax for it — the desugarer
/// never emits one).
pub const REBALANCE: &str = "@rebalance";

/// Rebalance arg name for the order field being re-spaced (a `Value::Text`).
pub const REBALANCE_FIELD: &str = "field";

/// Rebalance arg name for the `(id, new key)` rows, as an [`ArgValue::Rel`].
pub const REBALANCE_ROWS: &str = "rows";
