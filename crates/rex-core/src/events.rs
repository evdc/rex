//! Named-event dispatch (MVP-PLAN S-20): run one declared `event` with
//! arguments as ONE engine transaction. This is the only write path a host
//! should use after program setup; the wasm bridge and the tests share it.
//!
//! A synchronous `do E2(…)` inside an `on` body expands inline: the callee's
//! mutations join the same transaction, reading the same pre-event snapshot,
//! and only the outer event is what the host (and, from S-21, the log) sees.
//! The desugarer has already rejected `do` cycles, so expansion terminates.

use crate::dbsp::{
    ArgValue, BaseSnapshot, Circuit, DispatchOp, Engine, Event, InputKey, StepResult, GENESIS, GENESIS_SORT,
    REBALANCE, REBALANCE_FIELD, REBALANCE_ROWS,
};
use crate::eval::interp::{arith_values, compare_values, lit_value};
use crate::eval::intern::intern;
use crate::eval::relation::BinaryRelation;
use crate::eval::{encode_value, Value};
use crate::types::shape_ir::{
    Cond, EventDef, FromField, MutationIR, ParamTy, Target, ValExpr, ValRef, ROW_SELF, STATE_ENTITY,
};
use crate::types::typed::{FALSE, TRUE};
use crate::types::env::EntityKey;
use crate::types::{Env, SortId, ValueTy};
use std::collections::HashMap;

/// Why a dispatch did not happen. Either way nothing was written and
/// nothing was logged.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// The event does not apply to the state it met: its guard (or the guard
    /// of an event it `do`es) does not hold, or a value reads something that
    /// is not there — a row that is gone, a state that was never set. An
    /// ordinary outcome, with the reason to show for it.
    Rejected(String),
    /// The call itself is wrong: no such event, a missing or mistyped
    /// argument. A bug in the host, not something a user did.
    Invalid(String),
}

impl Refusal {
    pub fn message(&self) -> &str {
        match self {
            Refusal::Rejected(m) | Refusal::Invalid(m) => m,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// Errors raised as plain strings are about the call, not the state.
impl From<String> for Refusal {
    fn from(message: String) -> Refusal {
        Refusal::Invalid(message)
    }
}

/// One relation-typed event arg's rows, keyed by param name (S-42): `new
/// Entity from param as (k, v) { … }` reads its rows here rather than from
/// the scalar `args` map every other mutation value reads.
type RelArgs = HashMap<String, Vec<(Value, Value, i64)>>;

/// Dispatch `name` with `args` (keyed by the event's declared param names).
/// Returns the ids minted by any `new`, in order, and the step's deltas. This
/// is the only write path a host should use once a program has booted — the
/// wasm bridge and the tests share it — and it logs the event (S-21) so
/// [`replay`] can reproduce the same effect later from an empty engine.
pub fn dispatch_event(
    engine: &mut Engine,
    env: &Env,
    events: &[EventDef],
    name: &str,
    args: &HashMap<String, ArgValue>,
) -> Result<(Vec<Value>, StepResult), Refusal> {
    let def = find(events, name)?;
    // Bind the outer call's args by declared name, checking each.
    let mut bound = HashMap::new();
    let mut rel_bound: RelArgs = HashMap::new();
    let mut logged = Vec::new();
    for p in &def.params {
        let v = args
            .get(&p.name)
            .ok_or_else(|| format!("event `{name}` missing arg `{}`", p.name))?;
        check_param(env, name, &p.name, &p.ty, v)?;
        match v {
            ArgValue::Value(val) => {
                bound.insert(p.name.clone(), val.clone());
            }
            ArgValue::Rel(rel) => {
                rel_bound.insert(p.name.clone(), rel.clone());
            }
        }
        logged.push((p.name.clone(), v.clone()));
    }
    let ops = build_ops(env, engine, events, def, &bound, &rel_bound)?;
    Ok(engine.apply_event(name, &ops, logged))
}

/// Re-apply a whole event log against `engine` (expected to start with its
/// views registered but no data — the boot sequence runs every non-`new`
/// program statement, then this) in log order: a [`GENESIS`] event (a
/// program-setup `new`, S-21 subtask 3) mints directly from its tagged sort;
/// every other event re-derives its dispatch ops from the same declared
/// handler `dispatch_event` used to produce it (arguments checked the same
/// way: the log comes from storage), then applies them. Doesn't
/// re-append to `engine`'s log — the caller already has one (this *is* it).
///
/// `silent` skips per-step delta collection (`Circuit::step_silent`): the
/// engine-only-cost path a page reload wants, since nobody is listening for
/// per-step deltas mid-replay.
pub fn replay(
    engine: &mut Engine,
    env: &Env,
    events: &[EventDef],
    log: &[Event],
    silent: bool,
) -> Result<(), String> {
    // Check the whole log's numbering before touching the engine: a refused
    // log must leave it as it was, and a seq the host cannot represent would
    // poison every later event's.
    let mut next = engine.cursor();
    for event in log {
        if event.seq < next || event.seq >= MAX_SEQ {
            return Err(format!("event log is out of order or out of range at seq {} (expected {next} or later)", event.seq));
        }
        next = event.seq + 1;
    }
    for event in log {
        if event.name == GENESIS {
            replay_genesis(engine, env, event, silent)?;
        } else if event.name == REBALANCE {
            replay_rebalance(engine, env, event, silent)?;
        } else {
            // Nothing rejected is ever logged, so a logged event that is
            // rejected on replay means the log does not belong to this state.
            let ops = ops_for_event(env, engine, events, event)
                .map_err(|e| format!("logged event `{}` (seq {}) cannot be replayed: {e}", event.name, event.seq))?;
            if silent {
                engine.dispatch_silent(&ops);
            } else {
                engine.dispatch(&ops);
            }
        }
        engine.advance_cursor(event.seq);
    }
    Ok(())
}

fn replay_genesis(engine: &mut Engine, env: &Env, event: &Event, silent: bool) -> Result<(), String> {
    let mut sort = None;
    let mut fields = Vec::new();
    for (name, arg) in &event.args {
        let ArgValue::Value(v) = arg else {
            return Err(format!("genesis event: relation-valued arg `{name}` is not supported"));
        };
        if name == GENESIS_SORT {
            let Value::Int(n) = v else {
                return Err("genesis event: malformed sort tag".to_string());
            };
            sort = Some(SortId(usize::try_from(*n).map_err(|_| "genesis event: bad sort tag")?));
        } else {
            fields.push((name.clone(), v.clone()));
        }
    }
    let sort = sort.ok_or_else(|| "genesis event missing sort tag".to_string())?;
    if !env.has_sort(sort) {
        return Err(format!("genesis event names sort {}, which this program does not have", sort.0));
    }
    for (i, (field, value)) in fields.iter().enumerate() {
        check_field(env, sort, field, value)?;
        if fields[..i].iter().any(|(f, _)| f == field) {
            return Err(format!("genesis event sets `{field}` twice"));
        }
    }
    // A seed row holds its key like any other. (The program's own seeds are
    // checked when it is compiled; this is a log read back from storage.)
    if let Some(key) = env.key(sort) {
        let value = key_value(key, &fields)
            .ok_or_else(|| format!("genesis event: a `{}` is missing part of its key", key.entity))?;
        if key_taken(engine, key, &value, &Default::default()) {
            return Err(format!("genesis event: {}", key_conflict(key)));
        }
    }
    if silent {
        engine.apply_new_silent(sort, &fields);
    } else {
        engine.apply_new(sort, &fields);
    }
    Ok(())
}

/// Replay a logged [`REBALANCE`] system event: rebuild the same `Set` ops
/// [`Engine::apply_rebalance`] built (never re-logged — same "the caller
/// already has the log" rule as [`replay_genesis`]).
fn replay_rebalance(engine: &mut Engine, env: &Env, event: &Event, silent: bool) -> Result<(), String> {
    let mut field = None;
    let mut rows = None;
    for (name, arg) in &event.args {
        match name.as_str() {
            REBALANCE_FIELD => {
                let ArgValue::Value(Value::Text(s)) = arg else {
                    return Err("rebalance event: malformed field arg".to_string());
                };
                field = Some(s.as_str().to_string());
            }
            REBALANCE_ROWS => {
                let ArgValue::Rel(rel) = arg else {
                    return Err("rebalance event: malformed rows arg".to_string());
                };
                rows = Some(rel.iter().map(|(l, r, _)| (l.clone(), r.clone())).collect::<Vec<_>>());
            }
            other => return Err(format!("rebalance event: unknown arg `{other}`")),
        }
    }
    let field = field.ok_or_else(|| "rebalance event missing field".to_string())?;
    let rows = rows.ok_or_else(|| "rebalance event missing rows".to_string())?;
    check_rebalance(env, &field, &rows)?;
    let ops = Engine::rebalance_ops(&field, &rows);
    if silent {
        engine.dispatch_silent(&ops);
    } else {
        engine.dispatch(&ops);
    }
    Ok(())
}

/// Rebuild an event's dispatch ops from its logged args: the same
/// bind-check-[`expand`] `dispatch_event` runs. The check is not skipped: a
/// log is read back from storage, and an argument of the wrong type would
/// otherwise reach the engine's write path, which trusts its input.
fn ops_for_event(env: &Env, engine: &Engine, events: &[EventDef], event: &Event) -> Result<Vec<DispatchOp>, Refusal> {
    let def = find(events, &event.name)?;
    let mut bound = HashMap::new();
    let mut rel_bound: RelArgs = HashMap::new();
    for p in &def.params {
        let (_, arg) = event
            .args
            .iter()
            .find(|(name, _)| *name == p.name)
            .ok_or_else(|| format!("missing arg `{}`", p.name))?;
        check_param(env, &event.name, &p.name, &p.ty, arg)?;
        match arg {
            ArgValue::Value(v) => {
                bound.insert(p.name.clone(), v.clone());
            }
            ArgValue::Rel(rel) => {
                rel_bound.insert(p.name.clone(), rel.clone());
            }
        }
    }
    build_ops(env, engine, events, def, &bound, &rel_bound)
}

/// One event's whole transaction: its handler expanded, then checked against
/// the keys its entities declare.
fn build_ops(
    env: &Env,
    engine: &Engine,
    events: &[EventDef],
    def: &EventDef,
    args: &HashMap<String, Value>,
    rel_args: &RelArgs,
) -> Result<Vec<DispatchOp>, Refusal> {
    let mut ops = Vec::new();
    expand(env, engine, events, def, args, rel_args, &mut ops)?;
    check_keys(env, engine, &ops)?;
    Ok(ops)
}

/// `fields`' value of `key`, as the index view holds it: the one value, or
/// pairs nested to the right. `None` if a key field is not given.
fn key_value(key: &EntityKey, fields: &[(String, Value)]) -> Option<Value> {
    let mut parts = key
        .fields
        .iter()
        .rev()
        .map(|k| fields.iter().rev().find(|(f, _)| f == k).map(|(_, v)| v.clone()));
    let mut acc = parts.next()??;
    for part in parts {
        acc = Value::Pair(Box::new(part?), Box::new(acc));
    }
    Some(acc)
}

/// Whether a live row other than the ones in `gone` already holds `key`.
fn key_taken(engine: &Engine, key: &EntityKey, value: &Value, gone: &std::collections::BTreeSet<&Value>) -> bool {
    engine
        .circuit
        .view(&key.view)
        .is_some_and(|index| index.row_ref(value).any(|(id, w)| w > 0 && !gone.contains(id)))
}

fn key_conflict(key: &EntityKey) -> Refusal {
    Refusal::Rejected(format!("a `{}` with this `{}` already exists", key.entity, key.fields.join("` and `")))
}

/// A key (`entity E { …, key (f, g) }`) is a constraint on the state: no two
/// live rows agree on it. A transaction that would break it is rejected,
/// whole, like any other event that does not apply. Rows are checked in
/// statement order against the pre-event state and against the rows this
/// transaction has already created; a row it has already deleted has given
/// its key up.
fn check_keys(env: &Env, engine: &Engine, ops: &[DispatchOp]) -> Result<(), Refusal> {
    use std::collections::BTreeSet;
    let mut gone: BTreeSet<&Value> = BTreeSet::new();
    let mut made: BTreeSet<(SortId, Value)> = BTreeSet::new();
    for op in ops {
        match op {
            DispatchOp::Retract { id } => {
                gone.insert(id);
            }
            DispatchOp::New { sort, fields } => {
                let Some(key) = env.key(*sort) else { continue };
                let value = key_value(key, fields)
                    .ok_or_else(|| format!("a new `{}` is missing part of its key", key.entity))?;
                if key_taken(engine, key, &value, &gone) || !made.insert((*sort, value)) {
                    return Err(key_conflict(key));
                }
            }
            // Key fields are never assigned (the checker refuses it).
            DispatchOp::Set { .. } => {}
        }
    }
    Ok(())
}

fn find<'a>(events: &'a [EventDef], name: &str) -> Result<&'a EventDef, String> {
    events
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| format!("unknown event `{name}`"))
}

/// Append `def`'s mutations (with `do` calls expanded) to `ops`. The engine's
/// circuit is the pre-event snapshot a `ValRef::Expr` (S-40) reads through —
/// every read sees state as of before this whole transaction, never a sibling
/// mutation's write (MVP-PLAN §2.2/§2.3).
///
/// `let x = new E { … }` binds `x` for the statements after it. The row's id
/// is not minted until the transaction is built, but it is known: ids are
/// per-sort and sequential, so it is the engine's next id for the sort plus
/// the `new`s of that sort already in `ops`. [`Engine::apply_event`] mints in
/// the same order, which is also why replay reproduces the same ids.
fn expand(
    env: &Env,
    engine: &Engine,
    events: &[EventDef],
    def: &EventDef,
    args: &HashMap<String, Value>,
    rel_args: &RelArgs,
    ops: &mut Vec<DispatchOp>,
) -> Result<(), Refusal> {
    // The handler's params, plus each row it has created and named so far.
    let mut args = args.clone();
    expand_body(env, engine, events, def, &def.body, &mut args, rel_args, ops)
}

/// Whether every condition holds in the pre-event state.
fn holds(env: &Env, circuit: &Circuit, args: &HashMap<String, Value>, conds: &[Cond]) -> Result<bool, Refusal> {
    for cond in conds {
        let ok = match cond {
            Cond::Holds { view } => circuit.view(view).is_some_and(|rows| !rows.is_empty()),
            Cond::HoldsAt { view, param } => {
                let id = args.get(param).ok_or_else(|| format!("unbound parameter `{param}`"))?;
                circuit.view(view).is_some_and(|rows| rows.has_left(id))
            }
            Cond::Exists { target, negate } => resolve_target(env, circuit, args, target)?.is_empty() == *negate,
            // A condition over something absent is simply not met.
            Cond::Eval(e) => match eval_val_expr(env, circuit, args, e) {
                Ok(v) => v == Value::atom(TRUE),
                Err(Refusal::Rejected(_)) => false,
                Err(invalid) => return Err(invalid),
            },
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Append one block of statements. `args` grows as the block names rows it
/// creates; a nested block (an `if` branch) works on a copy.
#[allow(clippy::too_many_arguments)]
fn expand_body(
    env: &Env,
    engine: &Engine,
    events: &[EventDef],
    def: &EventDef,
    body: &[MutationIR],
    args: &mut HashMap<String, Value>,
    rel_args: &RelArgs,
    ops: &mut Vec<DispatchOp>,
) -> Result<(), Refusal> {
    let circuit = &engine.circuit;
    // `bound` is the event's args, plus per-row names (`ROW_SELF`, a
    // `new … from` binder) when resolving a value once per row.
    let resolve_in = |v: &ValRef, bound: &HashMap<String, Value>| -> Result<Value, Refusal> {
        match v {
            ValRef::Lit(lit) => Ok(lit_value(lit)),
            ValRef::Arg(n) => Ok(bound
                .get(n)
                .cloned()
                .ok_or_else(|| format!("event `{}`: unbound parameter `{n}`", def.name))?),
            ValRef::Expr(ve) => eval_val_expr(env, circuit, bound, ve),
        }
    };
    for m in body {
        // A row this statement creates and names, in scope from the next one.
        let mut bound: Option<(String, Value)> = None;
        {
            let args = &*args;
            let resolve = |v: &ValRef| resolve_in(v, args);
            match m {
                MutationIR::Set { target, entity, updates } => {
                    let sort = entity_sort(env, entity)?;
                    let mut row_args = args.clone();
                    for id in resolve_target(env, circuit, args, target)? {
                        row_args.insert(ROW_SELF.to_string(), id.clone());
                        let mut resolved = Vec::new();
                        for (field, val) in updates {
                            let value = resolve_in(val, &row_args)?;
                            check_field(env, sort, field, &value)?;
                            resolved.push((field.clone(), value));
                        }
                        ops.push(DispatchOp::Set { id, updates: resolved });
                    }
                }
                MutationIR::Delete { target } => {
                    for id in resolve_target(env, circuit, args, target)? {
                        ops.push(DispatchOp::Retract { id });
                    }
                }
                MutationIR::Insert { entity, fields, bind } => {
                    let sort = entity_sort(env, entity)?;
                    let mut resolved = Vec::new();
                    for (field, val) in fields {
                        let value = resolve(val)?;
                        check_field(env, sort, field, &value)?;
                        resolved.push((field.clone(), value));
                    }
                    if let Some(name) = bind {
                        let earlier = ops.iter().filter(|op| matches!(op, DispatchOp::New { sort: s, .. } if *s == sort)).count();
                        bound = Some((name.clone(), Value::Id(sort, engine.next_id(sort) + earlier as u64)));
                    }
                    ops.push(DispatchOp::New { sort, fields: resolved });
                }
                MutationIR::InsertFrom { entity, param, key, value: value_name, fields } => {
                    let sort = entity_sort(env, entity)?;
                    let rows = rel_args
                        .get(param)
                        .ok_or_else(|| format!("event `{}`: unbound relation parameter `{param}`", def.name))?;
                    // Every id in one transaction, minted in key order (S-42
                    // subtask 2) — never N independent `new`s that re-find a key.
                    let mut live: Vec<&(Value, Value, i64)> = rows.iter().filter(|(_, _, w)| *w > 0).collect();
                    live.sort_by(|a, b| a.0.cmp(&b.0));
                    let mut row_args = args.clone();
                    for (k, v, _) in live {
                        row_args.insert(key.clone(), k.clone());
                        row_args.insert(value_name.clone(), v.clone());
                        let mut resolved = Vec::new();
                        for (field, fr) in fields {
                            let value = match fr {
                                FromField::Key => k.clone(),
                                FromField::Value => v.clone(),
                                FromField::Val(vr) => resolve_in(vr, &row_args)?,
                            };
                            check_field(env, sort, field, &value)?;
                            resolved.push((field.clone(), value));
                        }
                        ops.push(DispatchOp::New { sort, fields: resolved });
                    }
                }
                // Both branches are checked; the pre-event state picks one.
                // What it binds is its own, so it runs on a copy of `args`.
                MutationIR::If { conds, then, els } => {
                    let branch = if holds(env, circuit, args, conds)? { then } else { els };
                    expand_body(env, engine, events, def, branch, &mut args.clone(), rel_args, ops)?;
                }
                // Ends the whole event, whatever was expanded before it and
                // whoever `do`es this handler: the ops are simply dropped.
                MutationIR::Reject { reason } => return Err(Refusal::Rejected(reason.clone())),
                MutationIR::Do { event, args: call_args } => {
                    let callee = find(events, event)?;
                    let mut inner = HashMap::new();
                    for (p, a) in callee.params.iter().zip(call_args) {
                        inner.insert(p.name.clone(), resolve(a)?);
                    }
                    expand(env, engine, events, callee, &inner, rel_args, ops)?;
                }
            }
        }
        if let Some((name, id)) = bound {
            args.insert(name, id);
        }
    }
    Ok(())
}

/// Point-evaluate a mutation-value expression (S-40, §2.3: "arg-dependent
/// values are point evaluations"): every leaf is a literal or a bound param,
/// so this always yields exactly one value, read against `circuit`'s current
/// input integrals — the pre-event snapshot, never the transaction being
/// built. Deliberately not the general relational evaluator
/// (`eval::interp::eval_expr_with`): that materializes whole relations,
/// while a mutation value only ever needs one row.
fn eval_val_expr(
    env: &Env,
    circuit: &Circuit,
    args: &HashMap<String, Value>,
    e: &ValExpr,
) -> Result<Value, Refusal> {
    match e {
        ValExpr::Lit(lit) => Ok(lit_value(lit)),
        ValExpr::Param(name) => args
            .get(name)
            .cloned()
            .ok_or_else(|| Refusal::Invalid(format!("unbound parameter `{name}`"))),
        ValExpr::Field(base, field) => {
            let id = eval_val_expr(env, circuit, args, base)?;
            let Value::Id(sort, _) = &id else {
                return Err(Refusal::Invalid(format!("`.{field}`: not an entity id")));
            };
            let key = InputKey::Field(*sort, intern(field));
            circuit
                .input_integral(&key)
                .and_then(|rel| rel.row(&id).next())
                .map(|(v, _)| v)
                .ok_or_else(|| Refusal::Rejected(format!("the row {} has no `{field}` (it may be gone)", encode_value(&id))))
        }
        ValExpr::Not(inner, [a, b]) => {
            let v = eval_val_expr(env, circuit, args, inner)?;
            let Value::Atom(cur) = &v else {
                return Err(Refusal::Invalid("`not` operand is not an atom".to_string()));
            };
            let cur = cur.as_str();
            if cur == a {
                Ok(Value::atom(b))
            } else if cur == b {
                Ok(Value::atom(a))
            } else {
                Err(Refusal::Invalid(format!("`not`: value `{cur}` is not one of its declared alternatives")))
            }
        }
        // The one `State#` row's field (S-51). Read like any other field,
        // against the pre-event snapshot; a defaultless state that has never
        // been `set` has no row, and saying so beats inventing a null.
        ValExpr::State(name) => {
            let sort = entity_sort(env, STATE_ENTITY)?;
            let key = InputKey::Field(sort, intern(name));
            circuit
                .input_integral(&key)
                .and_then(|rel| rel.triples().find(|(_, _, w)| *w > 0).map(|(_, v, _)| v.clone()))
                .ok_or_else(|| Refusal::Rejected(format!("state `{name}` has no value yet")))
        }
        ValExpr::Concat(a, b) => {
            let (Value::Text(x), Value::Text(y)) = (
                eval_val_expr(env, circuit, args, a)?,
                eval_val_expr(env, circuit, args, b)?,
            ) else {
                return Err(Refusal::Invalid("`++` operand is not Text".to_string()));
            };
            Ok(Value::text(&format!("{}{}", x.as_str(), y.as_str())))
        }
        ValExpr::Arith(kind, a, b, money) => {
            let va = eval_val_expr(env, circuit, args, a)?;
            let vb = eval_val_expr(env, circuit, args, b)?;
            Ok(arith_values(*kind, &va, &vb, *money))
        }
        ValExpr::And(a, b) | ValExpr::Or(a, b) => {
            let yes = Value::atom(TRUE);
            let (x, y) = (eval_val_expr(env, circuit, args, a)? == yes, eval_val_expr(env, circuit, args, b)? == yes);
            let both = matches!(e, ValExpr::And(..));
            Ok(Value::atom(if (both && x && y) || (!both && (x || y)) { TRUE } else { FALSE }))
        }
        ValExpr::Compare(op, a, b) => {
            let va = eval_val_expr(env, circuit, args, a)?;
            let vb = eval_val_expr(env, circuit, args, b)?;
            Ok(Value::atom(if compare_values(*op, &va, &vb) { TRUE } else { FALSE }))
        }
    }
}

/// The row ids a `Set`/`Delete` target names (S-41), read against `circuit`'s
/// pre-event snapshot like every other mutation-value read: a single bound
/// param, every row of a hidden keyset view (the arg-free `where` case,
/// resolved with one `circuit.view` lookup — no scan), the whole identity
/// relation (a predicate-free `delete Entity`), or — the arg-dependent case —
/// every row of the entity whose predicate evaluates true, checked one at a
/// time by binding [`ROW_SELF`] to each candidate in turn (O(N), the visible
/// cost a static keyset view avoids).
fn resolve_target(env: &Env, circuit: &Circuit, args: &HashMap<String, Value>, target: &Target) -> Result<Vec<Value>, Refusal> {
    match target {
        Target::One(r) => {
            let id = args.get(&r.0).cloned().ok_or_else(|| format!("unbound parameter `{}`", r.0))?;
            Ok(vec![id])
        }
        Target::All { entity } => {
            let sort = entity_sort(env, entity)?;
            let key = InputKey::Identity(sort);
            Ok(circuit
                .input_integral(&key)
                .map(|rel| rel.triples().filter(|(_, _, w)| *w > 0).map(|(l, _, _)| l.clone()).collect())
                .unwrap_or_default())
        }
        Target::View { view, .. } => Ok(circuit
            .view(view)
            .map(|rel| rel.triples().filter(|(_, _, w)| *w > 0).map(|(l, _, _)| l.clone()).collect())
            .unwrap_or_default()),
        Target::Scan { entity, pred } => {
            let sort = entity_sort(env, entity)?;
            let key = InputKey::Identity(sort);
            let Some(rel) = circuit.input_integral(&key) else {
                return Ok(Vec::new());
            };
            let mut hit = Vec::new();
            for id in rel.triples().filter(|(_, _, w)| *w > 0).map(|(l, _, _)| l.clone()) {
                let mut row_args = args.clone();
                row_args.insert(ROW_SELF.to_string(), id.clone());
                // A row with nothing where the predicate looks does not match;
                // it does not make the whole event fail.
                match eval_val_expr(env, circuit, &row_args, pred) {
                    Ok(v) if v == Value::atom(TRUE) => hit.push(id),
                    Ok(_) | Err(Refusal::Rejected(_)) => {}
                    Err(invalid) => return Err(invalid),
                }
            }
            Ok(hit)
        }
    }
}

fn entity_sort(env: &Env, entity: &str) -> Result<SortId, String> {
    env.entity_sort(entity)
        .ok_or_else(|| format!("unknown entity `{entity}`"))
}

/// Reject an arg that doesn't inhabit the declared param type before it
/// reaches the engine: a relation-typed param needs an [`ArgValue::Rel`]
/// (S-42) — every other shape needs a scalar [`ArgValue::Value`], checked as
/// before. A relation's own rows are validated per-field when
/// `MutationIR::InsertFrom` builds its `New` ops (`check_field`), the same
/// boundary every other mutation value crosses.
fn check_param(env: &Env, event: &str, param: &str, ty: &ParamTy, v: &ArgValue) -> Result<(), String> {
    match (ty, v) {
        (ParamTy::Rel(..), ArgValue::Rel(_)) => Ok(()),
        (ParamTy::Rel(..), ArgValue::Value(_)) => {
            Err(format!("event `{event}`: argument `{param}` must be a relation"))
        }
        (_, ArgValue::Rel(_)) => Err(format!("event `{event}`: argument `{param}` must be a scalar value")),
        (_, ArgValue::Value(v)) => check_scalar_param(env, event, param, ty, v),
    }
}

fn check_scalar_param(env: &Env, event: &str, param: &str, ty: &ParamTy, v: &Value) -> Result<(), String> {
    let ok = match ty {
        ParamTy::Id(entity) => matches!(v, Value::Id(s, _) if env.entity_sort(entity) == Some(*s)),
        ParamTy::Scalar(enc) => {
            use crate::types::shape_ir::Encoding;
            match enc {
                Encoding::Text => matches!(v, Value::Text(_)),
                Encoding::Int => matches!(v, Value::Int(_)),
                Encoding::Money => matches!(v, Value::Money(_)),
                Encoding::Atom => matches!(v, Value::Atom(_)),
                Encoding::Id => matches!(v, Value::Id(..)),
            }
        }
        ParamTy::Rel(..) => unreachable!("check_param routes relation params separately"),
    };
    if ok {
        Ok(())
    } else {
        Err(format!("event `{event}`: argument `{param}` has the wrong type: {}", encode_value(v)))
    }
}

/// Reject host input that doesn't match the schema: an unknown field, or a
/// value whose type doesn't inhabit the declared field type. Without this a
/// bad arg silently stores (say) an Int in a Text column, or a non-pair
/// value in a `T×T` field that a downstream `fst`/`snd` view then traps on.
pub fn check_field(env: &Env, sort: SortId, field: &str, value: &Value) -> Result<(), String> {
    let Some(ty) = env.field_ty(sort, field) else {
        return Err(format!("no field `{field}` on `{}`", env.sort_name(sort)));
    };
    if !admits(ty, value) {
        return Err(format!(
            "field `{field}` expects `{}`, got value {}",
            env.show(ty),
            encode_value(value)
        ));
    }
    Ok(())
}

/// Whether a runtime value inhabits a declared field type — the value-level
/// dual of the checker's typing, used to validate untrusted host input.
pub fn admits(ty: &ValueTy, v: &Value) -> bool {
    match (ty, v) {
        (ValueTy::Unit, Value::Unit) => true,
        (ValueTy::Int, Value::Int(_)) => true,
        (ValueTy::Money, Value::Money(_)) => true,
        (ValueTy::Text, Value::Text(_)) => true,
        (ValueTy::Date, Value::Date { .. }) => true,
        (ValueTy::Id(s), Value::Id(vs, _)) => s == vs,
        (ValueTy::Atom(name), Value::Atom(sym)) => sym.as_str() == name,
        // A coproduct value is one of its (atom) alternatives.
        (ValueTy::Coproduct(elems), _) => elems.iter().any(|e| admits(e, v)),
        (ValueTy::Product(x, y), Value::Pair(a, b)) => admits(x, a) && admits(y, b),
        _ => false,
    }
}

/// The largest event seq, id sequence number or log cursor a host can hold:
/// they cross the boundary as JSON numbers and key the persisted log, and a
/// JavaScript number is exact only below 2^53.
pub const MAX_SEQ: u64 = 1 << 53;

/// Reject a rebalance — `[(id, new key)]` for `field` — that is not a set of
/// writes to one field of rows of one entity: an id that is not an entity id,
/// a sort the program does not have, a field that sort lacks, a key of the
/// wrong type. Shared by the live path and by replay, which must refuse a
/// logged `@rebalance` exactly when the live call would have.
pub fn check_rebalance(env: &Env, field: &str, rows: &[(Value, Value)]) -> Result<(), String> {
    for (id, value) in rows {
        let Value::Id(sort, _) = id else {
            return Err("rebalance target is not an entity id".to_string());
        };
        if !env.has_sort(*sort) {
            return Err(format!("rebalance target {} is of a sort this program does not have", encode_value(id)));
        }
        check_field(env, *sort, field, value)?;
    }
    Ok(())
}

/// Load a [`BaseSnapshot`] into a freshly booted engine — [`Engine::restore`]
/// behind a check that the snapshot is a state this program could have
/// reached. A snapshot comes from storage: it may be truncated, written by
/// another version, or simply damaged, and loading one that breaks the
/// engine's base invariant gives wrong views or a panic several events later.
/// Refused snapshots leave the engine untouched.
pub fn restore(engine: &mut Engine, env: &Env, snap: &BaseSnapshot) -> Result<(), String> {
    check_snapshot(env, snap)?;
    engine.restore(snap);
    Ok(())
}

/// The base invariant, checked on a snapshot before it is loaded: every table
/// belongs to a sort and field the program declares; an identity row is
/// `(id, id)` at weight 1 for an id already minted; a field holds at most one
/// row per id, at weight 1, of the field's type, and only for a live id; and
/// the live rows of a keyed entity each hold a whole key that no other does.
pub fn check_snapshot(env: &Env, snap: &BaseSnapshot) -> Result<(), String> {
    use std::collections::{HashMap as Map, HashSet};
    if snap.cursor >= MAX_SEQ {
        return Err(format!("snapshot cursor {} is out of range", snap.cursor));
    }
    let mut minted: Map<SortId, u64> = Map::new();
    for (sort, n) in &snap.next_id {
        if !env.has_sort(*sort) {
            return Err(format!("snapshot counts ids for sort {}, which this program does not have", sort.0));
        }
        if *n >= MAX_SEQ || minted.insert(*sort, *n).is_some() {
            return Err(format!("snapshot id counter for `{}` is out of range or repeated", env.sort_name(*sort)));
        }
    }
    let mut tables: HashSet<InputKey> = HashSet::new();
    let mut live: Map<SortId, HashSet<&Value>> = Map::new();
    for (key, rows) in &snap.inputs {
        if !tables.insert(*key) {
            return Err(format!("snapshot holds table {key:?} twice"));
        }
        let InputKey::Identity(sort) = key else { continue };
        if !env.has_sort(*sort) {
            return Err(format!("snapshot holds rows of sort {}, which this program does not have", sort.0));
        }
        let ids = live.entry(*sort).or_default();
        for (l, r, w) in rows {
            let ok = matches!(l, Value::Id(s, n) if s == sort && *n < minted.get(sort).copied().unwrap_or(0));
            if !ok || l != r || *w != 1 || !ids.insert(l) {
                return Err(format!(
                    "snapshot identity row ({}, {}, {w}) of `{}` is not a live, minted id held once",
                    encode_value(l),
                    encode_value(r),
                    env.sort_name(*sort)
                ));
            }
        }
    }
    for (key, rows) in &snap.inputs {
        let InputKey::Field(sort, field) = key else { continue };
        if !env.has_sort(*sort) {
            return Err(format!("snapshot holds a field of sort {}, which this program does not have", sort.0));
        }
        let mut seen: HashSet<&Value> = HashSet::new();
        for (l, r, w) in rows {
            check_field(env, *sort, field.as_str(), r)?;
            let alive = live.get(sort).is_some_and(|ids| ids.contains(l));
            if !alive || *w != 1 || !seen.insert(l) {
                return Err(format!(
                    "snapshot field row ({}, {}, {w}) of `{}`.`{}` is not one value of a live row",
                    encode_value(l),
                    encode_value(r),
                    env.sort_name(*sort),
                    field.as_str()
                ));
            }
        }
    }
    for (sort, ids) in &live {
        let Some(key) = env.key(*sort) else { continue };
        let mut rows: Map<&Value, Vec<(String, Value)>> = Map::new();
        for (table, table_rows) in &snap.inputs {
            let InputKey::Field(s, field) = table else { continue };
            if s == sort && key.fields.iter().any(|k| k == field.as_str()) {
                for (id, v, _) in table_rows {
                    rows.entry(id).or_default().push((field.as_str().to_string(), v.clone()));
                }
            }
        }
        let mut held: std::collections::BTreeSet<Value> = Default::default();
        for id in ids {
            let whole = rows.get(*id).and_then(|fields| key_value(key, fields));
            if !whole.is_some_and(|value| held.insert(value)) {
                return Err(format!(
                    "snapshot row {} of `{}` has no key of its own (`{}` must be whole and unique)",
                    encode_value(id),
                    key.entity,
                    key.fields.join("`, `")
                ));
            }
        }
    }
    Ok(())
}
