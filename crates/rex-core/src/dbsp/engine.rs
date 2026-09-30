//! The engine: a live circuit plus entity-id minting — the incremental
//! counterpart of the batch interpreter's mutable store. `new` statements
//! become atomic base-table transactions, `let` statements extend the circuit
//! and backfill over existing data, and retraction is the same transaction
//! shape with negated weights.

use super::circuit::{Circuit, StepResult, Transaction};
use super::log::{ArgValue, Event, GENESIS, GENESIS_SORT, REBALANCE, REBALANCE_FIELD, REBALANCE_ROWS};
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
    /// The append-only event log (S-21): every write to this engine, in
    /// order, seq'd by its index. Complete from empty — a program-setup
    /// `new` (in [`apply_typed_stmt`](Self::apply_typed_stmt)) logs a
    /// synthetic [`GENESIS`] event, so [`replay`](crate::events::replay)
    /// never needs an out-of-band snapshot to make sense of the log.
    log: Vec<Event>,
    /// The seq the *next* logged event gets. Equal to `log.len()` for an
    /// engine that has only ever logged through this instance; diverges from
    /// it after [`restore`](Self::restore) or a replay (S-22), because
    /// neither re-appends to `log` (the host already owns the full log —
    /// `crate::events::replay`'s doc comment) but both must still leave a
    /// later `apply_event` numbering its seq as if they had.
    next_seq: u64,
}

/// One operation in a handler dispatch (see [`Engine::dispatch`]).
pub enum DispatchOp {
    New { sort: SortId, fields: Vec<(String, Value)> },
    Set { id: Value, updates: Vec<(String, Value)> },
    Retract { id: Value },
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
        let mut tx = Transaction::new();
        let id = self.push_new(&mut tx, sort, fields);
        self.debug_assert_base_invariant(&tx);
        (id, self.circuit.step(&tx))
    }

    /// [`apply_new`](Self::apply_new)'s silent counterpart — no `StepResult`,
    /// no state difference otherwise. Used by [`replay`](crate::events::replay)
    /// to re-mint a [`GENESIS`]-logged entity without paying for a delta batch
    /// nobody reads.
    pub(crate) fn apply_new_silent(&mut self, sort: SortId, fields: &[(String, Value)]) -> Value {
        let mut tx = Transaction::new();
        let id = self.push_new(&mut tx, sort, fields);
        self.debug_assert_base_invariant(&tx);
        self.circuit.step_silent(&tx);
        id
    }

    /// Mint an id and append its identity + field rows to `tx` (no step). The
    /// transaction-builder shared by [`apply_new`](Self::apply_new) and
    /// [`dispatch`](Self::dispatch).
    fn push_new(&mut self, tx: &mut Transaction, sort: SortId, fields: &[(String, Value)]) -> Value {
        let n = self.next_id.entry(sort).or_insert(0);
        let id = Value::Id(sort, *n);
        *n += 1;
        tx.push(InputKey::Identity(sort), id.clone(), id.clone(), 1);
        for (field, v) in fields {
            tx.push(InputKey::Field(sort, intern(field)), id.clone(), v.clone(), 1);
        }
        id
    }

    /// Run a batch of create/set/retract operations as ONE atomic transaction
    /// — the engine face of an `on … =>` handler (§M5). Every read (the −old
    /// rows a set/retract negates) sees the pre-step snapshot, since the whole
    /// transaction is built before the single `step()`. Returns the ids minted
    /// by any `New` ops, in order.
    ///
    /// Crate-private (S-21): a write that runs but isn't logged is a
    /// correctness bug the moment replay exists (MVP-PLAN §2.2 — "a direct
    /// write is a corruption"), so every external write goes through
    /// [`apply_event`](Self::apply_event) instead, which logs it.
    pub(crate) fn dispatch(&mut self, ops: &[DispatchOp]) -> (Vec<Value>, StepResult) {
        let (tx, ids) = self.build_tx(ops);
        (ids, self.circuit.step(&tx))
    }

    /// [`dispatch`](Self::dispatch)'s silent counterpart, for
    /// [`replay`](crate::events::replay).
    pub(crate) fn dispatch_silent(&mut self, ops: &[DispatchOp]) -> Vec<Value> {
        let (tx, ids) = self.build_tx(ops);
        self.circuit.step_silent(&tx);
        ids
    }

    fn build_tx(&mut self, ops: &[DispatchOp]) -> (Transaction, Vec<Value>) {
        let mut tx = Transaction::new();
        let mut ids = Vec::new();
        for op in ops {
            match op {
                DispatchOp::New { sort, fields } => {
                    ids.push(self.push_new(&mut tx, *sort, fields));
                }
                DispatchOp::Set { id, updates } => self.push_set(&mut tx, id, updates),
                DispatchOp::Retract { id } => self.push_retract(&mut tx, id),
            }
        }
        self.debug_assert_base_invariant(&tx);
        (tx, ids)
    }

    /// The base invariant lowering's rewrites rely on (P-1, `dbsp/lower.rs`),
    /// checked for every id `tx` touches as it will stand after the step:
    /// the identity row is `(id, id)` at weight 1 or absent, and each field
    /// holds at most one row, at weight 1, and none once the identity is gone.
    /// `push_new` writes identity and fields together, `push_set` refuses
    /// dead ids and replaces rather than adds, and `push_retract` negates
    /// everything, so no write path can break it. O(|tx|); debug builds only.
    fn debug_assert_base_invariant(&self, tx: &Transaction) {
        if !cfg!(debug_assertions) {
            return;
        }
        // Net pending weight per (table, id, value).
        let mut pending: HashMap<(InputKey, &Value), HashMap<&Value, i64>> = HashMap::new();
        for (key, l, r, w) in &tx.deltas {
            *pending.entry((*key, l)).or_default().entry(r).or_default() += w;
        }
        let after = |key: InputKey, id: &Value| -> Vec<(Value, i64)> {
            let mut rows: HashMap<Value, i64> = self
                .circuit
                .input_integral(&key)
                .map(|rel| rel.row(id).collect())
                .unwrap_or_default();
            for (v, w) in pending.get(&(key, id)).into_iter().flatten() {
                *rows.entry((*v).clone()).or_default() += w;
            }
            rows.into_iter().filter(|(_, w)| *w != 0).collect()
        };
        let ids: std::collections::HashSet<&Value> = pending.keys().map(|(_, id)| *id).collect();
        for id in ids {
            let Value::Id(sort, _) = id else { panic!("base row keyed by non-id {id}") };
            let identity = after(InputKey::Identity(*sort), id);
            let alive = match identity.as_slice() {
                [] => false,
                [(v, 1)] if v == id => true,
                rows => panic!("identity of {id} would hold {rows:?}"),
            };
            let fields = self
                .circuit
                .input_keys()
                .copied()
                .chain(pending.keys().map(|(k, _)| *k))
                .filter(|k| matches!(k, InputKey::Field(s, _) if s == sort))
                .collect::<std::collections::HashSet<_>>();
            for key in fields {
                let rows = after(key, id);
                assert!(
                    rows.is_empty() || (alive && matches!(rows.as_slice(), [(_, 1)])),
                    "{key:?} of {} {id} would hold {rows:?}",
                    if alive { "live" } else { "dead" },
                );
            }
        }
    }

    /// Run `ops` as one transaction and append `name`/`args` to the log
    /// (S-21) — the write path every declared-event dispatch goes through
    /// (`crate::events::dispatch_event`). The only other public write path is
    /// [`apply_typed_stmt`](Self::apply_typed_stmt), whose `new` statements
    /// log a synthetic [`GENESIS`] event of their own.
    pub fn apply_event(
        &mut self,
        name: &str,
        ops: &[DispatchOp],
        args: Vec<(String, ArgValue)>,
    ) -> (Vec<Value>, StepResult) {
        let (ids, res) = self.dispatch(ops);
        self.log_event(name, args);
        (ids, res)
    }

    /// Append one entry to the log with the next sequence number. Does not
    /// itself run anything; callers push the effect first.
    fn log_event(&mut self, name: impl Into<String>, args: Vec<(String, ArgValue)>) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.log.push(Event { seq, name: name.into(), args, cause: None, intent: None });
        seq
    }

    /// The full event log, in order — what a persistence adapter appends and
    /// [`replay`](crate::events::replay) consumes.
    pub fn log(&self) -> &[Event] {
        &self.log
    }

    /// Every logged event with `seq >= seq` (S-22): what a persistence
    /// adapter fetches to append to its own store, or a host re-queries after
    /// a dropped connection. `log` only holds events logged through *this*
    /// engine instance (never the pre-restore history — see [`restore`]),
    /// so this is a session-local tail, not the full log.
    pub fn log_since(&self, seq: u64) -> impl Iterator<Item = &Event> {
        self.log.iter().filter(move |e| e.seq >= seq)
    }

    /// The seq the next logged event will get — what [`base_snapshot`]
    /// records as its cursor, so a later [`restore`] can resume numbering
    /// without colliding with history the snapshot already covers.
    ///
    /// [`base_snapshot`]: Self::base_snapshot
    /// [`restore`]: Self::restore
    pub fn cursor(&self) -> u64 {
        self.next_seq
    }

    /// Fast-forward the seq counter past `seq` without appending to `log`
    /// (S-22): [`crate::events::replay`] calls this per replayed event so a
    /// later real dispatch's seq continues where the (externally-held) log
    /// left off, even though replay deliberately doesn't re-log — see
    /// `replay`'s doc comment.
    pub(crate) fn advance_cursor(&mut self, seq: u64) {
        self.next_seq = self.next_seq.max(seq + 1);
    }

    /// Retract an entity: negate exactly the base rows currently held for
    /// `id` (identity + every field), one atomic transaction. This is the M1
    /// retraction surface — the language has no delete syntax yet.
    ///
    /// Deliberately does NOT cascade to entities holding `id` as a foreign
    /// key: a dangling `Order.customer` simply stops joining, which is the
    /// honest 6NF semantics.
    pub fn retract_entity(&mut self, id: &Value) -> StepResult {
        let mut tx = Transaction::new();
        self.push_retract(&mut tx, id);
        self.debug_assert_base_invariant(&tx);
        self.circuit.step(&tx)
    }

    /// The rows a base cell `(key, id)` will hold after `tx` is applied: the
    /// pre-step integral plus whatever `tx` has already pushed for that cell.
    /// A transaction's *reads* see the pre-event snapshot, but its *writes*
    /// to one cell compose (two `set`s of the same field in one event leave
    /// exactly the last value live, never a double-negated original).
    ///
    /// `pending` says an *earlier* op already wrote to `id` in `tx`; when it
    /// did not, there is nothing to compose with and the scan of `tx.deltas`
    /// (O(N) per call, so O(N²) for a bulk delete) is skipped. The caller
    /// reads it once, before its own pushes.
    fn net_rows(&self, tx: &Transaction, pending: bool, key: &InputKey, id: &Value) -> Vec<(Value, i64)> {
        let mut rows: Vec<(Value, i64)> = self
            .circuit
            .input_integral(key)
            .map(|rel| rel.row(id).collect())
            .unwrap_or_default();
        if pending {
            for (k, l, r, w) in &tx.deltas {
                if k == key && l == id {
                    match rows.iter_mut().find(|(v, _)| v == r) {
                        Some(row) => row.1 += w,
                        None => rows.push((r.clone(), *w)),
                    }
                }
            }
        }
        rows.retain(|(_, w)| *w != 0);
        rows
    }

    /// Append the negation of every base row held for `id` (identity +
    /// fields) after `tx`'s pending writes, to `tx`. No step.
    fn push_retract(&mut self, tx: &mut Transaction, id: &Value) {
        let Value::Id(sort, _) = id else {
            panic!("retract requires an entity id, got {id}")
        };
        let pending = tx.touches(id);
        let identity = InputKey::Identity(*sort);
        for (v, w) in self.net_rows(tx, pending, &identity, id) {
            tx.push(identity, id.clone(), v, -w);
        }
        let mut field_keys: Vec<InputKey> = self
            .circuit
            .input_keys()
            .filter(|k| matches!(k, InputKey::Field(s, _) if s == sort))
            .copied()
            .collect();
        // Fields first written in this transaction (a `new` then `delete`).
        if pending {
            for (k, l, _, _) in &tx.deltas {
                if matches!(k, InputKey::Field(s, _) if s == sort) && l == id && !field_keys.contains(k) {
                    field_keys.push(*k);
                }
            }
        }
        for key in field_keys {
            for (v, w) in self.net_rows(tx, pending, &key, id) {
                tx.push(key, id.clone(), v, -w);
            }
        }
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
        let mut tx = Transaction::new();
        self.push_set(&mut tx, id, updates);
        self.debug_assert_base_invariant(&tx);
        self.circuit.step(&tx)
    }

    /// Append the `−old/+new` field rows for an update to `tx`. A no-op (pushes
    /// nothing) if `id` has no live identity row after `tx`'s pending writes —
    /// a stale update to a deleted entity must not resurrect it as an
    /// orphaned field row. No step.
    fn push_set(&mut self, tx: &mut Transaction, id: &Value, updates: &[(String, Value)]) {
        let Value::Id(sort, _) = id else {
            panic!("set requires an entity id, got {id}")
        };
        // Two updates of one field inside this one op also compose.
        let dup = updates.iter().enumerate().any(|(i, (f, _))| updates[..i].iter().any(|(g, _)| g == f));
        let pending = tx.touches(id) || dup;
        let alive = !self.net_rows(tx, pending, &InputKey::Identity(*sort), id).is_empty();
        if !alive {
            return;
        }
        for (field, new) in updates {
            let key = InputKey::Field(*sort, intern(field));
            for (v, w) in self.net_rows(tx, pending, &key, id) {
                tx.push(key, id.clone(), v, -w);
            }
            tx.push(key, id.clone(), new.clone(), 1);
        }
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
                // Genesis (S-21 subtask 3): seed data is event 0..k of the
                // log, tagged with the target sort so `replay` can mint it
                // back without a checked `EventDef` (there isn't one — this
                // never went through a declared `on` handler).
                let mut args: Vec<(String, ArgValue)> =
                    vec![(GENESIS_SORT.to_string(), ArgValue::Value(Value::Int(sort.0 as i64)))];
                args.extend(resolved.iter().map(|(f, v)| (f.clone(), ArgValue::Value(v.clone()))));
                self.log_event(GENESIS, args);
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
        self.circuit.backfill(mark, &[name])
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
        let names: Vec<&str> = bindings.iter().map(|(n, _)| n.as_str()).collect();
        self.circuit.backfill(mark, &names)
    }

    /// Re-space one manual-order level's keys as ONE atomic transaction
    /// (S-22 subtask 4, MVP-PLAN §2.9/§2.10): `rows` is `[(id, new key)]`,
    /// the amortized maintenance sweep `maybeRebalance` computes client-side.
    /// Logged as the system [`REBALANCE`] event, so it appears in the log and
    /// replays deterministically like any declared event, and the N field
    /// writes it used to be (`js/rex-dom/src/interact.ts`, pre-S-22) become
    /// one — never a torn intermediate a concurrent reader could observe.
    pub fn apply_rebalance(&mut self, field: &str, rows: &[(Value, Value)]) -> StepResult {
        let ops = Self::rebalance_ops(field, rows);
        let args = vec![
            (REBALANCE_FIELD.to_string(), ArgValue::Value(Value::text(field))),
            (
                REBALANCE_ROWS.to_string(),
                ArgValue::Rel(rows.iter().map(|(l, r)| (l.clone(), r.clone(), 1)).collect()),
            ),
        ];
        self.apply_event(REBALANCE, &ops, args).1
    }

    /// The `Set` ops a rebalance of `rows` under `field` expands to — shared
    /// by [`apply_rebalance`](Self::apply_rebalance) (which also logs) and
    /// [`crate::events::replay`] (which must not re-log a replayed event).
    pub(crate) fn rebalance_ops(field: &str, rows: &[(Value, Value)]) -> Vec<DispatchOp> {
        rows.iter()
            .map(|(id, v)| DispatchOp::Set { id: id.clone(), updates: vec![(field.to_string(), v.clone())] })
            .collect()
    }

    /// The engine's input-table contents plus id-minting and log-cursor state
    /// (S-22): what a persistence adapter snapshots so a reload can skip
    /// replaying the whole log from empty (MVP-PLAN §2.2 — "views are
    /// backfilled on load"). Pairs with [`restore`](Self::restore).
    pub fn base_snapshot(&self) -> BaseSnapshot {
        let inputs = self
            .circuit
            .input_keys()
            .map(|key| {
                let rows = self
                    .circuit
                    .input_integral(key)
                    .expect("a key from input_keys() has an integral")
                    .triples()
                    .map(|(l, r, w)| (l.clone(), r.clone(), w))
                    .collect();
                (*key, rows)
            })
            .collect();
        let next_id = self.next_id.iter().map(|(s, n)| (*s, *n)).collect();
        BaseSnapshot { cursor: self.next_seq, next_id, inputs }
    }

    /// Load a [`base_snapshot`](Self::base_snapshot) into a freshly booted
    /// engine (views registered via `add_view`/`add_view_group`, no `new`
    /// statements run — the host skips those on a restoring boot so seed
    /// data isn't minted twice, MVP-PLAN §2.7/S-80). One transaction carries
    /// every base row at once; since this is the circuit's first real step,
    /// running it at floor 0 derives every view's integral in one pass — the
    /// same backfill-on-load semantics `add_view` gives a `let` added after
    /// data already exists, just for the whole graph at once.
    pub fn restore(&mut self, snap: &BaseSnapshot) {
        let mut tx = Transaction::new();
        for (key, rows) in &snap.inputs {
            for (l, r, w) in rows {
                tx.push(*key, l.clone(), r.clone(), *w);
            }
        }
        self.circuit.step_silent(&tx);
        for (sort, n) in &snap.next_id {
            self.next_id.insert(*sort, *n);
        }
        self.next_seq = snap.cursor;
    }
}

/// One base table's rows, as `(left, right, weight)` triples.
type BaseRows = Vec<(Value, Value, i64)>;

/// See [`Engine::base_snapshot`] / [`Engine::restore`].
pub struct BaseSnapshot {
    /// The seq the log had reached when this was taken; [`Engine::restore`]
    /// resumes numbering from here.
    pub cursor: u64,
    pub next_id: Vec<(SortId, u64)>,
    pub inputs: Vec<(InputKey, BaseRows)>,
}
