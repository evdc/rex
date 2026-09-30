//! The incremental (DBSP) backend: standing views maintained delta-at-a-time.
//!
//! A checked program lowers to a *circuit* — an arena of operator nodes wired
//! by composition (§10: the point-free program *is* the circuit). Base-table
//! changes arrive as Z-set deltas in a [`Transaction`]; one [`Circuit::step`]
//! pushes them through every operator's delta rule and returns each view's
//! delta, while per-node integrals accumulate the full view contents.
//!
//! Semantics are defined by the batch evaluator: for every operator,
//! integrating its emitted deltas over any transaction sequence must equal the
//! corresponding [`crate::eval::algebra`] kernel applied to the integrated
//! inputs — including under retraction (negative weights). `tests/dbsp.rs`
//! checks exactly this, property-style.

pub mod circuit;
pub mod engine;
pub mod log;
pub mod lower;
pub mod node;

pub use circuit::{Circuit, StepResult, Transaction};
pub use engine::{BaseSnapshot, DispatchOp, Engine};
pub use log::{ArgValue, Event, GENESIS, GENESIS_SORT, REBALANCE, REBALANCE_FIELD, REBALANCE_ROWS};
pub use lower::lower;
pub use node::{CoKeyedFn, InputKey, KeyAgg, Node, NodeId, Scalar, Updated};
