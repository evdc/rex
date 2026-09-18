//! Named-event dispatch (MVP-PLAN S-20): run one declared `event` with
//! arguments as ONE engine transaction. This is the only write path a host
//! should use after program setup; the wasm bridge and the tests share it.
//!
//! A synchronous `do E2(…)` inside an `on` body expands inline: the callee's
//! mutations join the same transaction, reading the same pre-event snapshot,
//! and only the outer event is what the host (and, from S-21, the log) sees.
//! The desugarer has already rejected `do` cycles, so expansion terminates.

use crate::dbsp::{DispatchOp, Engine, StepResult};
use crate::eval::interp::lit_value;
use crate::eval::{encode_value, Value};
use crate::types::shape_ir::{EventDef, MutationIR, ParamTy, ValRef};
use crate::types::{Env, SortId, ValueTy};
use std::collections::HashMap;

/// Dispatch `name` with `args` (keyed by the event's declared param names).
/// Returns the ids minted by any `new`, in order, and the step's deltas.
pub fn dispatch_event(
    engine: &mut Engine,
    env: &Env,
    events: &[EventDef],
    name: &str,
    args: &HashMap<String, Value>,
) -> Result<(Vec<Value>, StepResult), String> {
    let def = find(events, name)?;
    // Bind the outer call's args by declared name, checking each.
    let mut bound = HashMap::new();
    for p in &def.params {
        let v = args
            .get(&p.name)
            .ok_or_else(|| format!("event `{name}` missing arg `{}`", p.name))?;
        check_param(env, name, &p.name, &p.ty, v)?;
        bound.insert(p.name.clone(), v.clone());
    }
    let mut ops = Vec::new();
    expand(env, events, def, &bound, &mut ops)?;
    Ok(engine.dispatch(&ops))
}

fn find<'a>(events: &'a [EventDef], name: &str) -> Result<&'a EventDef, String> {
    events
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| format!("unknown event `{name}`"))
}

/// Append `def`'s mutations (with `do` calls expanded) to `ops`.
fn expand(
    env: &Env,
    events: &[EventDef],
    def: &EventDef,
    args: &HashMap<String, Value>,
    ops: &mut Vec<DispatchOp>,
) -> Result<(), String> {
    let resolve = |v: &ValRef| -> Result<Value, String> {
        match v {
            ValRef::Lit(lit) => Ok(lit_value(lit)),
            ValRef::Arg(n) => args
                .get(n)
                .cloned()
                .ok_or_else(|| format!("event `{}`: unbound parameter `{n}`", def.name)),
        }
    };
    for m in &def.body {
        match m {
            MutationIR::Set { target, entity, updates } => {
                let id = resolve(&ValRef::Arg(target.0.clone()))?;
                let sort = entity_sort(env, entity)?;
                let mut resolved = Vec::new();
                for (field, val) in updates {
                    let value = resolve(val)?;
                    check_field(env, sort, field, &value)?;
                    resolved.push((field.clone(), value));
                }
                ops.push(DispatchOp::Set { id, updates: resolved });
            }
            MutationIR::Delete { target } => {
                ops.push(DispatchOp::Retract { id: resolve(&ValRef::Arg(target.0.clone()))? });
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
            MutationIR::Do { event, args: call_args } => {
                let callee = find(events, event)?;
                let mut inner = HashMap::new();
                for (p, a) in callee.params.iter().zip(call_args) {
                    inner.insert(p.name.clone(), resolve(a)?);
                }
                expand(env, events, callee, &inner, ops)?;
            }
        }
    }
    Ok(())
}

fn entity_sort(env: &Env, entity: &str) -> Result<SortId, String> {
    env.entity_sort(entity)
        .ok_or_else(|| format!("unknown entity `{entity}`"))
}

/// Reject a value that doesn't inhabit the declared param type before it
/// reaches the engine.
fn check_param(env: &Env, event: &str, param: &str, ty: &ParamTy, v: &Value) -> Result<(), String> {
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
        ParamTy::Rel(..) => {
            return Err(format!(
                "event `{event}`: relation-typed parameter `{param}` is not supported yet (MVP-PLAN S-42)"
            ))
        }
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
