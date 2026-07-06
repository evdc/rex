//! The engine: a live circuit plus entity-id minting — the incremental
//! counterpart of the batch interpreter's mutable store. `new` statements
//! become atomic base-table transactions, `let` statements extend the circuit
//! and backfill over existing data, and retraction is the same transaction
//! shape with negated weights.

use super::circuit::{Circuit, StepResult, Transaction};
use super::lower::lower;
use super::node::InputKey;
use crate::eval::intern::intern;
use crate::eval::relation::BinaryRelation;
use crate::eval::value::Value;
use crate::types::ty::SortId;
use crate::types::typed::TExpr;
use std::collections::HashMap;

#[derive(Default)]
pub struct Engine {
    pub circuit: Circuit,
    /// Per-sort id sequence (the incremental home of `Interp::next_id`).
    next_id: HashMap<SortId, u64>,
}

impl Engine {
    pub fn new() -> Engine {
        Engine::default()
    }

    /// Apply one `new E { .. }`: mint a fresh id and write the identity row
    /// plus every field row in ONE atomic transaction (§4 — never N
    /// independent inserts that re-find the key). Returns the id and the
    /// resulting view deltas.
    pub fn apply_new(&mut self, sort: SortId, fields: &[(String, Value)]) -> (Value, StepResult) {
        let n = self.next_id.entry(sort).or_insert(0);
        let id = Value::Id(sort, *n);
        *n += 1;

        let mut tx = Transaction::new();
        tx.push(InputKey::Identity(sort), id.clone(), id.clone(), 1);
        for (field, v) in fields {
            tx.push(InputKey::Field(sort, intern(field)), id.clone(), v.clone(), 1);
        }
        (id.clone(), self.circuit.step(&tx))
    }

    /// Retract an entity: negate exactly the base rows currently held for
    /// `id` (identity + every field), one atomic transaction. This is the M1
    /// retraction surface — the language has no delete syntax yet.
    ///
    /// Deliberately does NOT cascade to entities holding `id` as a foreign
    /// key: a dangling `Order.customer` simply stops joining, which is the
    /// honest 6NF semantics.
    pub fn retract_entity(&mut self, id: &Value) -> StepResult {
        let Value::Id(sort, _) = id else {
            panic!("retract_entity requires an entity id, got {id}")
        };

        let mut tx = Transaction::new();
        let identity = InputKey::Identity(*sort);
        if let Some(ids) = self.circuit.input_integral(&identity) {
            let w = ids.weight(id, id);
            if w != 0 {
                tx.push(identity, id.clone(), id.clone(), -w);
            }
        }
        let field_keys: Vec<InputKey> = self
            .circuit
            .input_keys()
            .filter(|k| matches!(k, InputKey::Field(s, _) if s == sort))
            .copied()
            .collect();
        for key in field_keys {
            let rows: Vec<(Value, i64)> = self
                .circuit
                .input_integral(&key)
                .map(|rel| rel.row(id).collect())
                .unwrap_or_default();
            for (v, w) in rows {
                tx.push(key, id.clone(), v, -w);
            }
        }
        self.circuit.step(&tx)
    }

    /// Apply one `let name = body`: lower the body onto the circuit, register
    /// the view, and backfill its whole subgraph over the data already
    /// integrated. Must be called in program order (a body's `View` references
    /// resolve against already-added views).
    pub fn add_view(
        &mut self,
        name: &str,
        body: &TExpr,
        values: &HashMap<String, Value>,
    ) -> StepResult {
        let mark = self.circuit.node_count();
        let node = lower(&mut self.circuit, body, values);
        self.circuit.set_output(name, node);
        self.circuit.backfill(mark)
    }

    /// Apply one recursion group (`let recursive …`, §8): lower the group's
    /// nested fix region onto the circuit, register each member as a view, and
    /// backfill over the data already integrated.
    pub fn add_view_group(
        &mut self,
        bindings: &[(String, TExpr)],
        values: &HashMap<String, Value>,
    ) -> StepResult {
        let mark = self.circuit.node_count();
        let outs = super::lower::lower_group(&mut self.circuit, bindings, values);
        for ((name, _), node) in bindings.iter().zip(outs) {
            self.circuit.set_output(name, node);
        }
        self.circuit.backfill(mark)
    }
}
