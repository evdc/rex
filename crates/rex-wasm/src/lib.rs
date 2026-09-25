//! The JS face of the Rex engine: one `RexApp` wraps a checked program and a
//! live incremental circuit. The protocol is deliberately narrow — values
//! cross the boundary as the canonical string encoding (`rex::eval::encode`),
//! deltas come back as one JSON batch per step — so the boundary is crossed
//! exactly once per transaction and the shaper can key its maps on the
//! encoded strings directly.
//!
//! Every write reaches the engine through a named event (S-20/S-21/S-22):
//! `dispatch` for a declared `on`, `rebalance` for the one system event
//! (`@rebalance`) the language has no surface for. There is no other public
//! write path — `apply_new`/`update_field(s)`/`retract` bypassed the log and
//! are gone (S-22 subtask 3); a direct write here would be a correctness bug
//! the moment replay exists (MVP-PLAN §2.2 — "a direct write is a
//! corruption").

use rex::dbsp::{BaseSnapshot, Engine, Event, InputKey};
use rex::eval::{
    base_snapshot_to_json, decode_value, encode_value, event_to_json, json_quote, rows_to_json,
    step_result_to_json, Value,
};
use rex::events::{check_field, dispatch_event, replay};
use rex::types::shape_ir::EventDef;
use rex::types::ty::SortId;
use rex::types::typed::TStmt;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct RexApp {
    engine: Engine,
    env: rex::types::Env,
    /// `new`-bound ids from the program source (empty on a restoring boot —
    /// `forRestore` skips `new` statements, see [`RexApp::for_restore`]).
    values: HashMap<String, Value>,
    /// Declared events with their handler bodies (the EventIR, S-20).
    events: Vec<EventDef>,
}

#[wasm_bindgen]
impl RexApp {
    /// Parse, check, and apply a whole program, including its `new`
    /// statements (each logged as a synthetic `@genesis` event, S-21) — the
    /// no-prior-state boot path. Errors (rendered with source context) reject
    /// construction; warnings are ignored here — surface them at build time,
    /// not app boot.
    #[wasm_bindgen(constructor)]
    pub fn new(source: &str) -> Result<RexApp, JsValue> {
        Self::boot(source, false)
    }

    /// Parse and check a whole program but skip its `new` statements — the
    /// restoring boot path (S-22/S-80): a host that has a prior
    /// [`base_snapshot`](Self::base_snapshot) calls this, then
    /// [`restore`](Self::restore), instead of letting the program's seed
    /// data run again and double up with the restored rows.
    #[wasm_bindgen(js_name = forRestore)]
    pub fn for_restore(source: &str) -> Result<RexApp, JsValue> {
        Self::boot(source, true)
    }

    fn boot(source: &str, skip_new: bool) -> Result<RexApp, JsValue> {
        let parsed = rex::parse(source);
        if !parsed.diagnostics.is_empty() {
            return Err(render_all(&parsed.diagnostics, source));
        }
        let checked = rex::check(&parsed.program);
        let Some(typed) = checked.elaborated else {
            return Err(render_all(&checked.diagnostics, source));
        };

        let mut app = RexApp {
            engine: Engine::new(),
            env: checked.env,
            values: HashMap::new(),
            events: checked.shapes.events,
        };
        for stmt in &typed.stmts {
            if skip_new && matches!(stmt, TStmt::New { .. }) {
                continue;
            }
            app.engine.apply_typed_stmt(stmt, &mut app.values);
        }
        Ok(app)
    }

    /// Dispatch a named event as ONE atomic transaction (S-20). `args_json`
    /// is a flat object keyed by the event's declared parameter names, each
    /// value either a canonically-encoded scalar (`"t:a5"`) or — for a
    /// relation-typed param (S-42) — an array of `[k, v, w]` rows, the same
    /// shape the shaper already speaks for a view delta.
    /// Returns `{"ids":[…],"deltas":{"views":{…}}}` — `ids` are the keys
    /// minted by any `new` mutations, in order.
    pub fn dispatch(&mut self, name: &str, args_json: &str) -> Result<String, JsValue> {
        let raw: HashMap<String, serde_json::Value> = serde_json::from_str(args_json)
            .map_err(|e| JsValue::from_str(&format!("bad event args: {e}")))?;
        let mut args = HashMap::new();
        for (k, v) in raw {
            match v {
                serde_json::Value::String(s) => {
                    args.insert(k, rex::dbsp::ArgValue::Value(decode(&s)?));
                }
                serde_json::Value::Array(rows) => {
                    let mut rel = Vec::new();
                    for row in rows {
                        let bad = || JsValue::from_str(&format!("bad relation row for arg `{k}`"));
                        let row = row.as_array().ok_or_else(bad)?;
                        let l = decode(row.first().and_then(|x| x.as_str()).ok_or_else(bad)?)?;
                        let r = decode(row.get(1).and_then(|x| x.as_str()).ok_or_else(bad)?)?;
                        let w = row.get(2).and_then(|x| x.as_i64()).unwrap_or(1);
                        rel.push((l, r, w));
                    }
                    args.insert(k, rex::dbsp::ArgValue::Rel(rel));
                }
                _ => return Err(JsValue::from_str(&format!("bad value for arg `{k}`"))),
            }
        }
        let (ids, res) = dispatch_event(&mut self.engine, &self.env, &self.events, name, &args)
            .map_err(|e| JsValue::from_str(&e))?;
        let ids_json: Vec<String> = ids.iter().map(|v| json_quote(&encode_value(v))).collect();
        Ok(format!(
            r#"{{"ids":[{}],"deltas":{}}}"#,
            ids_json.join(","),
            step_result_to_json(&res)
        ))
    }

    /// Re-space one manual-order level's keys as ONE atomic transaction
    /// (S-22 subtask 4): `rows_json` is `[[id, newKey], ...]` (canonically
    /// encoded), the plan `maybeRebalance` computes client-side. Logged as
    /// the system `@rebalance` event, so a drag storm that triggers a sweep
    /// is one entry in the log, not N. Returns `{"views":{…}}`.
    pub fn rebalance(&mut self, field: &str, rows_json: &str) -> Result<String, JsValue> {
        let raw: Vec<(String, String)> = serde_json::from_str(rows_json)
            .map_err(|e| JsValue::from_str(&format!("bad rebalance rows: {e}")))?;
        let mut rows = Vec::new();
        for (id, value) in raw {
            let id = decode(&id)?;
            let Value::Id(sort, _) = id else {
                return Err(JsValue::from_str("rebalance target is not an entity id"));
            };
            let value = decode(&value)?;
            self.check_field(sort, field, &value)?;
            rows.push((id, value));
        }
        Ok(step_result_to_json(&self.engine.apply_rebalance(field, &rows)))
    }

    /// The full contents of every registered view as one `{"views":{…}}` batch
    /// — the initial render the shaper applies at boot (replaces per-view
    /// `read_view` calls).
    pub fn snapshot(&self) -> String {
        let mut names: Vec<&String> = self.engine.circuit.output_names().collect();
        names.sort();
        let views: Vec<String> = names
            .iter()
            .map(|n| {
                let rel = self.engine.circuit.view(n).expect("registered view");
                format!("{}:{}", json_quote(n), rows_to_json(rel))
            })
            .collect();
        format!(r#"{{"views":{{{}}}}}"#, views.join(","))
    }

    /// Every logged event with `seq >= seq` (S-22), as a JSON array in
    /// [`event_to_json`] order — what a persistence adapter's `appendEvents`
    /// step reads after a batch of dispatches.
    pub fn log_since(&self, seq: u64) -> String {
        let events: Vec<String> = self.engine.log_since(seq).map(event_to_json).collect();
        format!("[{}]", events.join(","))
    }

    /// Re-apply a whole event log (S-22): `events_json` is a JSON array in
    /// the same shape [`log_since`](Self::log_since) returns. `silent` skips
    /// per-step delta collection — the engine-only-cost path a page reload
    /// wants, since nobody is listening for per-step deltas mid-replay.
    pub fn replay(&mut self, events_json: &str, silent: bool) -> Result<(), JsValue> {
        let events = decode_events(events_json)?;
        replay(&mut self.engine, &self.env, &self.events, &events, silent).map_err(|e| JsValue::from_str(&e))
    }

    /// The engine's input-table contents plus id-minting and log-cursor state
    /// (S-22) — what a persistence adapter snapshots so a reload can skip
    /// replaying the whole log from empty. Pairs with [`restore`](Self::restore).
    pub fn base_snapshot(&self) -> String {
        base_snapshot_to_json(&self.engine.base_snapshot())
    }

    /// Load a [`base_snapshot`](Self::base_snapshot) taken from an earlier
    /// session. Call this on a [`for_restore`](Self::for_restore)-booted app,
    /// before any `dispatch`/`replay` — it derives every view's contents
    /// from the restored base data in one pass (no separate "backfill" call
    /// needed).
    pub fn restore(&mut self, base_json: &str) -> Result<(), JsValue> {
        let snap = decode_base_snapshot(base_json)?;
        self.engine.restore(&snap);
        Ok(())
    }

    /// The id bound to a `let name = new …` in the program source, if any —
    /// how the host addresses seed data. Empty on a [`for_restore`](Self::for_restore)
    /// boot (its `new` statements never ran).
    pub fn bound_id(&self, name: &str) -> Option<String> {
        self.values.get(name).map(encode_value)
    }

    /// Escape hatch: read a view's full integrated contents as one JSON array
    /// of `[key, value, weight]` triples (mount-time reads the shaper chose
    /// not to mirror).
    pub fn read_view(&self, view: &str) -> Result<String, JsValue> {
        let rel = self
            .engine
            .circuit
            .view(view)
            .ok_or_else(|| JsValue::from_str(&format!("unknown view `{view}`")))?;
        Ok(rows_to_json(rel))
    }
}

impl RexApp {
    fn check_field(&self, sort: SortId, field: &str, value: &Value) -> Result<(), JsValue> {
        check_field(&self.env, sort, field, value).map_err(|e| JsValue::from_str(&e))
    }
}

fn decode(s: &str) -> Result<Value, JsValue> {
    decode_value(s)
        .ok_or_else(|| JsValue::from_str(&format!("malformed value encoding: {s:?}")))
}

/// Decode a JSON array of events in [`event_to_json`]'s shape (what
/// [`RexApp::log_since`] emits) back into [`Event`]s for [`RexApp::replay`].
fn decode_events(json: &str) -> Result<Vec<Event>, JsValue> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| JsValue::from_str(&format!("bad events json: {e}")))?;
    arr.iter().map(decode_event).collect()
}

fn decode_event(v: &serde_json::Value) -> Result<Event, JsValue> {
    let bad = || JsValue::from_str("malformed event json");
    let seq = v.get("seq").and_then(|s| s.as_u64()).ok_or_else(bad)?;
    let name = v.get("name").and_then(|s| s.as_str()).ok_or_else(bad)?.to_string();
    let args_obj = v.get("args").and_then(|a| a.as_object()).ok_or_else(bad)?;
    let mut args = Vec::new();
    for (k, val) in args_obj {
        let arg = match val {
            serde_json::Value::String(s) => rex::dbsp::ArgValue::Value(decode(s)?),
            serde_json::Value::Array(rows) => {
                let mut rel = Vec::new();
                for row in rows {
                    let row = row.as_array().ok_or_else(bad)?;
                    let l = decode(row.first().and_then(|x| x.as_str()).ok_or_else(bad)?)?;
                    let r = decode(row.get(1).and_then(|x| x.as_str()).ok_or_else(bad)?)?;
                    let w = row.get(2).and_then(|x| x.as_i64()).ok_or_else(bad)?;
                    rel.push((l, r, w));
                }
                rex::dbsp::ArgValue::Rel(rel)
            }
            _ => return Err(bad()),
        };
        args.push((k.clone(), arg));
    }
    let cause = v.get("cause").and_then(|c| c.as_u64());
    let intent = v.get("intent").and_then(|i| i.as_str()).map(|s| s.to_string());
    Ok(Event { seq, name, args, cause, intent })
}

/// Decode [`base_snapshot_to_json`]'s shape back into a [`BaseSnapshot`] for
/// [`RexApp::restore`].
fn decode_base_snapshot(json: &str) -> Result<BaseSnapshot, JsValue> {
    let bad = || JsValue::from_str("malformed snapshot json");
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| JsValue::from_str(&format!("bad snapshot json: {e}")))?;
    let cursor = v.get("cursor").and_then(|c| c.as_u64()).ok_or_else(bad)?;
    let next_id = v
        .get("nextId")
        .and_then(|a| a.as_array())
        .ok_or_else(bad)?
        .iter()
        .map(|pair| {
            let pair = pair.as_array().ok_or_else(bad)?;
            let sort = pair.first().and_then(|x| x.as_u64()).ok_or_else(bad)?;
            let n = pair.get(1).and_then(|x| x.as_u64()).ok_or_else(bad)?;
            Ok((SortId(sort as usize), n))
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    let inputs = v
        .get("inputs")
        .and_then(|a| a.as_array())
        .ok_or_else(bad)?
        .iter()
        .map(|entry| {
            let key = entry.get("key").and_then(|k| k.as_str()).ok_or_else(bad)?;
            let key = decode_input_key(key)?;
            let rows = entry
                .get("rows")
                .and_then(|r| r.as_array())
                .ok_or_else(bad)?
                .iter()
                .map(|row| {
                    let row = row.as_array().ok_or_else(bad)?;
                    let l = decode(row.first().and_then(|x| x.as_str()).ok_or_else(bad)?)?;
                    let r = decode(row.get(1).and_then(|x| x.as_str()).ok_or_else(bad)?)?;
                    let w = row.get(2).and_then(|x| x.as_i64()).ok_or_else(bad)?;
                    Ok((l, r, w))
                })
                .collect::<Result<Vec<_>, JsValue>>()?;
            Ok((key, rows))
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    Ok(BaseSnapshot { cursor, next_id, inputs })
}

fn decode_input_key(s: &str) -> Result<InputKey, JsValue> {
    let bad = || JsValue::from_str("malformed input key");
    if let Some(rest) = s.strip_prefix("id:") {
        let sort: usize = rest.parse().map_err(|_| bad())?;
        return Ok(InputKey::Identity(SortId(sort)));
    }
    if let Some(rest) = s.strip_prefix("f:") {
        let (sort, field) = rest.split_once(':').ok_or_else(bad)?;
        let sort: usize = sort.parse().map_err(|_| bad())?;
        return Ok(InputKey::Field(SortId(sort), rex::eval::intern(field)));
    }
    Err(bad())
}

fn render_all(diags: &[rex::Diagnostic], src: &str) -> JsValue {
    let msgs: Vec<String> = diags.iter().map(|d| d.render(src)).collect();
    JsValue::from_str(&msgs.join("\n"))
}
