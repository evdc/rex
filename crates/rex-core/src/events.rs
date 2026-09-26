//! Named-event dispatch (MVP-PLAN S-20): run one declared `event` with
//! arguments as ONE engine transaction. This is the only write path a host
//! should use after program setup; the wasm bridge and the tests share it.
//!
//! A synchronous `do E2(…)` inside an `on` body expands inline: the callee's
//! mutations join the same transaction, reading the same pre-event snapshot,
//! and only the outer event is what the host (and, from S-21, the log) sees.
//! The desugarer has already rejected `do` cycles, so expansion terminates.

use crate::dbsp::{
    ArgValue, Circuit, DispatchOp, Engine, Event, InputKey, StepResult, GENESIS, GENESIS_SORT, REBALANCE,
    REBALANCE_FIELD, REBALANCE_ROWS,
};
use crate::eval::interp::{arith_values, compare_values, lit_value};
use crate::eval::intern::intern;
use crate::eval::relation::BinaryRelation;
use crate::eval::{encode_value, Value};
use crate::types::shape_ir::{
    EventDef, FromField, MutationIR, ParamTy, Target, ValExpr, ValRef, ROW_SELF, STATE_ENTITY,
};
use crate::types::typed::{FALSE, TRUE};
use crate::types::{Env, SortId, ValueTy};
use std::collections::HashMap;

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
) -> Result<(Vec<Value>, StepResult), String> {
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
    let mut ops = Vec::new();
    expand(env, &engine.circuit, events, def, &bound, &rel_bound, &mut ops)?;
    Ok(engine.apply_event(name, &ops, logged))
}

/// Re-apply a whole event log against `engine` (expected to start with its
/// views registered but no data — the boot sequence runs every non-`new`
/// program statement, then this) in log order: a [`GENESIS`] event (a
/// program-setup `new`, S-21 subtask 3) mints directly from its tagged sort;
/// every other event re-derives its dispatch ops from the same declared
/// handler `dispatch_event` used to produce it, then applies them. Doesn't
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
    for event in log {
        if event.name == GENESIS {
            replay_genesis(engine, event, silent)?;
        } else if event.name == REBALANCE {
            replay_rebalance(engine, event, silent)?;
        } else {
            let ops = ops_for_event(env, &engine.circuit, events, event)?;
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

fn replay_genesis(engine: &mut Engine, event: &Event, silent: bool) -> Result<(), String> {
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
fn replay_rebalance(engine: &mut Engine, event: &Event, silent: bool) -> Result<(), String> {
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
    let ops = Engine::rebalance_ops(&field, &rows);
    if silent {
        engine.dispatch_silent(&ops);
    } else {
        engine.dispatch(&ops);
    }
    Ok(())
}

/// Rebuild an event's dispatch ops from its logged args: the same
/// bind-then-[`expand`] `dispatch_event` runs, minus `check_param` — the log
/// was produced by an already-checked dispatch, so replay is pure engine
/// cost (MVP-PLAN §2.2/§2.10).
fn ops_for_event(env: &Env, circuit: &Circuit, events: &[EventDef], event: &Event) -> Result<Vec<DispatchOp>, String> {
    let def = find(events, &event.name)?;
    let mut bound = HashMap::new();
    let mut rel_bound: RelArgs = HashMap::new();
    for (name, arg) in &event.args {
        match arg {
            ArgValue::Value(v) => {
                bound.insert(name.clone(), v.clone());
            }
            ArgValue::Rel(rel) => {
                rel_bound.insert(name.clone(), rel.clone());
            }
        }
    }
    let mut ops = Vec::new();
    expand(env, circuit, events, def, &bound, &rel_bound, &mut ops)?;
    Ok(ops)
}

fn find<'a>(events: &'a [EventDef], name: &str) -> Result<&'a EventDef, String> {
    events
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| format!("unknown event `{name}`"))
}

/// Append `def`'s mutations (with `do` calls expanded) to `ops`. `circuit` is
/// the pre-event snapshot a `ValRef::Expr` (S-40) reads through — every read
/// sees state as of before this whole transaction, never a sibling
/// mutation's write (MVP-PLAN §2.2/§2.3).
fn expand(
    env: &Env,
    circuit: &Circuit,
    events: &[EventDef],
    def: &EventDef,
    args: &HashMap<String, Value>,
    rel_args: &RelArgs,
    ops: &mut Vec<DispatchOp>,
) -> Result<(), String> {
    let resolve = |v: &ValRef| -> Result<Value, String> {
        match v {
            ValRef::Lit(lit) => Ok(lit_value(lit)),
            ValRef::Arg(n) => args
                .get(n)
                .cloned()
                .ok_or_else(|| format!("event `{}`: unbound parameter `{n}`", def.name)),
            ValRef::Expr(ve) => eval_val_expr(env, circuit, args, ve),
        }
    };
    for m in &def.body {
        match m {
            MutationIR::Set { target, entity, updates } => {
                let sort = entity_sort(env, entity)?;
                for id in resolve_target(env, circuit, args, target)? {
                    let mut resolved = Vec::new();
                    for (field, val) in updates {
                        let value = resolve(val)?;
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
            MutationIR::Insert { entity, fields } => {
                let sort = entity_sort(env, entity)?;
                let mut resolved = Vec::new();
                for (field, val) in fields {
                    let value = resolve(val)?;
                    check_field(env, sort, field, &value)?;
                    resolved.push((field.clone(), value));
                }
                ops.push(DispatchOp::New { sort, fields: resolved });
            }
            MutationIR::InsertFrom { entity, param, fields } => {
                let sort = entity_sort(env, entity)?;
                let rows = rel_args
                    .get(param)
                    .ok_or_else(|| format!("event `{}`: unbound relation parameter `{param}`", def.name))?;
                // Every id in one transaction, minted in key order (S-42
                // subtask 2) — never N independent `new`s that re-find a key.
                let mut live: Vec<&(Value, Value, i64)> = rows.iter().filter(|(_, _, w)| *w > 0).collect();
                live.sort_by(|a, b| a.0.cmp(&b.0));
                for (k, v, _) in live {
                    let mut resolved = Vec::new();
                    for (field, fr) in fields {
                        let value = match fr {
                            FromField::Key => k.clone(),
                            FromField::Value => v.clone(),
                            FromField::Val(vr) => resolve(vr)?,
                        };
                        check_field(env, sort, field, &value)?;
                        resolved.push((field.clone(), value));
                    }
                    ops.push(DispatchOp::New { sort, fields: resolved });
                }
            }
            MutationIR::Do { event, args: call_args } => {
                let callee = find(events, event)?;
                let mut inner = HashMap::new();
                for (p, a) in callee.params.iter().zip(call_args) {
                    inner.insert(p.name.clone(), resolve(a)?);
                }
                expand(env, circuit, events, callee, &inner, rel_args, ops)?;
            }
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
) -> Result<Value, String> {
    match e {
        ValExpr::Lit(lit) => Ok(lit_value(lit)),
        ValExpr::Param(name) => args
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unbound parameter `{name}`")),
        ValExpr::Field(base, field) => {
            let id = eval_val_expr(env, circuit, args, base)?;
            let Value::Id(sort, _) = &id else {
                return Err(format!("`.{field}`: not an entity id"));
            };
            let key = InputKey::Field(*sort, intern(field));
            circuit
                .input_integral(&key)
                .and_then(|rel| rel.row(&id).next())
                .map(|(v, _)| v)
                .ok_or_else(|| format!("no live `{field}` value for this row"))
        }
        ValExpr::Not(inner, [a, b]) => {
            let v = eval_val_expr(env, circuit, args, inner)?;
            let Value::Atom(cur) = &v else {
                return Err("`not` operand is not an atom".to_string());
            };
            let cur = cur.as_str();
            if cur == a {
                Ok(Value::atom(b))
            } else if cur == b {
                Ok(Value::atom(a))
            } else {
                Err(format!("`not`: value `{cur}` is not one of its declared alternatives"))
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
                .ok_or_else(|| format!("state `{name}` has no value yet"))
        }
        ValExpr::Arith(kind, a, b, money) => {
            let va = eval_val_expr(env, circuit, args, a)?;
            let vb = eval_val_expr(env, circuit, args, b)?;
            Ok(arith_values(*kind, &va, &vb, *money))
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
fn resolve_target(env: &Env, circuit: &Circuit, args: &HashMap<String, Value>, target: &Target) -> Result<Vec<Value>, String> {
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
                if eval_val_expr(env, circuit, &row_args, pred)? == Value::atom(TRUE) {
                    hit.push(id);
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
