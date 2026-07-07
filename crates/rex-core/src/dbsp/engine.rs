//! The engine: a live circuit plus entity-id minting — the incremental
//! counterpart of the batch interpreter's mutable store. `new` statements
//! become atomic base-table transactions, `let` statements extend the circuit
//! and backfill over existing data, and retraction is the same transaction
//! shape with negated weights.

use super::circuit::{Circuit, StepResult, Transaction};
use super::lower::lower;
use super::node::InputKey;
use crate::eval::intern::intern;
use crate::eval::interp::lit_value;
use crate::eval::relation::BinaryRelation;
use crate::eval::value::Value;
use crate::types::ty::SortId;
use crate::types::typed::{TExpr, TStmt, TValue};
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

    /// Update one field of an existing entity (see [`update_fields`](Self::update_fields)).
    pub fn update_field(&mut self, id: &Value, field: &str, new: Value) -> StepResult {
        self.update_fields(id, &[(field.to_string(), new)])
    }

    /// Update several fields of an existing entity in ONE atomic transaction:
    /// for each field, negate the rows currently held at `id` and assert the
    /// new value. This is the `−old/+new` same-key delta shape downstream
    /// consumers (the shaper's fusion classifier) key on — an update is never
    /// a retract-plus-insert of the whole entity, and a multi-field edit (a
    /// Kanban drag changing both `list` and `pos`) is one `step()`, so the
    /// shaper sees one consistent batch rather than a torn intermediate.
    ///
    /// A no-op returning an empty [`StepResult`] if `id` has no live identity
    /// row (never created, or already retracted): a stale update to a deleted
    /// entity must not resurrect it as an orphaned field row with no identity.
    pub fn update_fields(&mut self, id: &Value, updates: &[(String, Value)]) -> StepResult {
        let Value::Id(sort, _) = id else {
            panic!("update_fields requires an entity id, got {id}")
        };
        // Guard: refuse to write fields for an entity that isn't live.
        let alive = self
            .circuit
            .input_integral(&InputKey::Identity(*sort))
            .is_some_and(|ids| ids.weight(id, id) != 0);
        if !alive {
            return StepResult::default();
        }

        let mut tx = Transaction::new();
        for (field, new) in updates {
            let key = InputKey::Field(*sort, intern(field));
            let rows: Vec<(Value, i64)> = self
                .circuit
                .input_integral(&key)
                .map(|rel| rel.row(id).collect())
                .unwrap_or_default();
            for (v, w) in rows {
                tx.push(key, id.clone(), v, -w);
            }
            tx.push(key, id.clone(), new.clone(), 1);
        }
        self.circuit.step(&tx)
    }

    /// Apply one elaborated statement to the live engine, threading `values`
    /// (the `new`-bound id environment): resolve a `new`'s fields against it,
    /// dispatch to [`apply_new`](Self::apply_new) / [`add_view`](Self::add_view)
    /// / [`add_view_group`](Self::add_view_group), and record any minted id.
    /// This is the single statement-application loop every host shares — the
    /// REPL, the wasm bridge, and the integration tests — so `TStmt`'s shape
    /// and field resolution live in exactly one place. Anonymous `let`s have
    /// no observable view and don't grow the circuit.
    pub fn apply_typed_stmt(
        &mut self,
        stmt: &TStmt,
        values: &mut HashMap<String, Value>,
    ) -> StepResult {
        match stmt {
            TStmt::New { name, sort, fields } => {
                let resolved: Vec<(String, Value)> = fields
                    .iter()
                    .map(|(f, tv)| {
                        let v = match tv {
                            TValue::Lit(lit) => lit_value(lit),
                            TValue::Ref(n) => values[n].clone(),
                        };
                        (f.clone(), v)
                    })
                    .collect();
                let (id, res) = self.apply_new(*sort, &resolved);
                if let Some(name) = name {
                    values.insert(name.clone(), id);
                }
                res
            }
            TStmt::Let { name: Some(name), body } => self.add_view(name, body, values),
            TStmt::Let { name: None, .. } => StepResult::default(),
            TStmt::LetRec { bindings } => self.add_view_group(bindings, values),
        }
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
