//! The engine behind the wasm boundary, as plain Rust: every entry point of
//! `RexApp` with `String` errors instead of `JsValue`s, so the boundary's
//! behaviour — including what it does with malformed input from a host — can
//! be tested natively (`tests/boundary.rs`). `lib.rs` wraps this one-to-one.

use rex::dbsp::{BaseSnapshot, Engine, Event, InputKey};
use rex::eval::{
    base_snapshot_to_json, decode_value, encode_value, event_to_json, json_quote, rows_to_json,
    step_result_to_json, Value,
};
use rex::events::{check_rebalance, dispatch_event, replay, restore, Refusal};
use rex::types::shape_ir::EventDef;
use rex::types::ty::SortId;
use rex::types::typed::TStmt;
use std::collections::HashMap;

/// One booted program: the engine, its environment, and its declared events.
/// Every method that can fail returns the message as a `String`; the wasm
/// binding in `lib.rs` is a thin wrapper that turns those into JS errors.
pub struct App {
    engine: Engine,
    env: rex::types::Env,
    /// `new`-bound ids from the program source. A restoring boot skips the
    /// `new` statements but still binds their names to the same ids.
    values: HashMap<String, Value>,
    /// Declared events with their handler bodies (the EventIR, S-20).
    events: Vec<EventDef>,
}

impl App {
    /// Parse, check, and apply a whole program, including its `new`
    /// statements (each logged as a synthetic `@genesis` event, S-21) — the
    /// no-prior-state boot path. Errors (rendered with source context) reject
    /// construction; warnings are ignored here — surface them at build time,
    /// not app boot.
    pub fn new(source: &str) -> Result<App, String> {
        Self::boot(source, false)
    }

    /// Parse and check a whole program but skip its `new` statements — the
    /// restoring boot path (S-22/S-80): a host that has a prior
    /// [`base_snapshot`](Self::base_snapshot) calls this, then
    /// [`restore`](Self::restore), instead of letting the program's seed
    /// data run again and double up with the restored rows.
    pub fn for_restore(source: &str) -> Result<App, String> {
        Self::boot(source, true)
    }

    fn boot(source: &str, skip_new: bool) -> Result<App, String> {
        let parsed = rex::parse(source);
        if !parsed.diagnostics.is_empty() {
            return Err(render_all(&parsed.diagnostics, source));
        }
        let checked = rex::check(&parsed.program);
        let Some(typed) = checked.elaborated else {
            return Err(render_all(&checked.diagnostics, source));
        };

        let mut app = App {
            engine: Engine::new(),
            env: checked.env,
            values: HashMap::new(),
            events: checked.shapes.events,
        };
        let mut seeded = HashMap::new();
        for stmt in &typed.stmts {
            if skip_new && matches!(stmt, TStmt::New { .. }) {
                // The row comes back from the snapshot or the log; its name
                // still has to mean the id it was first given, for any view
                // that refers to it.
                Engine::bind_seed_id(stmt, &mut seeded, &mut app.values);
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
    ///
    /// An event that is **rejected** — its guard does not hold, or it reads
    /// something that is not there — returns `{"rejected":"<reason>"}`:
    /// nothing was written or logged. A call that is itself wrong (no such
    /// event, a mistyped argument) is an `Err`.
    pub fn dispatch(&mut self, name: &str, args_json: &str) -> Result<String, String> {
        let raw: HashMap<String, serde_json::Value> = serde_json::from_str(args_json)
            .map_err(|e| format!("bad event args: {e}"))?;
        let mut args = HashMap::new();
        for (k, v) in raw {
            match v {
                serde_json::Value::String(s) => {
                    args.insert(k, rex::dbsp::ArgValue::Value(decode(&s)?));
                }
                serde_json::Value::Array(rows) => {
                    let mut rel = Vec::new();
                    for row in rows {
                        let bad = || format!("bad relation row for arg `{k}`");
                        let row = row.as_array().ok_or_else(bad)?;
                        let l = decode(row.first().and_then(|x| x.as_str()).ok_or_else(bad)?)?;
                        let r = decode(row.get(1).and_then(|x| x.as_str()).ok_or_else(bad)?)?;
                        let w = row.get(2).and_then(|x| x.as_i64()).unwrap_or(1);
                        rel.push((l, r, w));
                    }
                    args.insert(k, rex::dbsp::ArgValue::Rel(rel));
                }
                _ => return Err(format!("bad value for arg `{k}`")),
            }
        }
        let (ids, res) = match dispatch_event(&mut self.engine, &self.env, &self.events, name, &args) {
            Ok(done) => done,
            // An event that does not apply to this state is an outcome, not
            // an error: the host is told why, and nothing has changed.
            Err(Refusal::Rejected(reason)) => return Ok(format!(r#"{{"rejected":{}}}"#, json_quote(&reason))),
            Err(Refusal::Invalid(message)) => return Err(message),
        };
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
    pub fn rebalance(&mut self, field: &str, rows_json: &str) -> Result<String, String> {
        let raw: Vec<(String, String)> = serde_json::from_str(rows_json)
            .map_err(|e| format!("bad rebalance rows: {e}"))?;
        let mut rows = Vec::new();
        for (id, value) in raw {
            rows.push((decode(&id)?, decode(&value)?));
        }
        check_rebalance(&self.env, field, &rows)?;
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
    pub fn replay(&mut self, events_json: &str, silent: bool) -> Result<(), String> {
        let events = decode_events(events_json)?;
        replay(&mut self.engine, &self.env, &self.events, &events, silent)
    }

    /// The engine's input-table contents plus id-minting and log-cursor state
    /// (S-22) — what a persistence adapter snapshots so a reload can skip
    /// replaying the whole log from empty. Pairs with [`restore`](Self::restore).
    pub fn base_snapshot(&self) -> String {
        base_snapshot_to_json(&self.engine.base_snapshot())
    }

    /// Load a [`base_snapshot`](Self::base_snapshot) taken from an earlier
    /// session. A snapshot that is not a state this program could have reached
    /// (damaged, or another program's) is refused and nothing is loaded. Call this on a [`for_restore`](Self::for_restore)-booted app,
    /// before any `dispatch`/`replay` — it derives every view's contents
    /// from the restored base data in one pass (no separate "backfill" call
    /// needed).
    pub fn restore(&mut self, base_json: &str) -> Result<(), String> {
        let snap = decode_base_snapshot(base_json)?;
        restore(&mut self.engine, &self.env, &snap)
    }

    /// The id bound to a `let name = new …` in the program source, if any —
    /// how the host addresses seed data. The same on a
    /// [`for_restore`](Self::for_restore) boot, whose `new` statements do not run.
    pub fn bound_id(&self, name: &str) -> Option<String> {
        self.values.get(name).map(encode_value)
    }

    /// Escape hatch: read a view's full integrated contents as one JSON array
    /// of `[key, value, weight]` triples (mount-time reads the shaper chose
    /// not to mirror).
    pub fn read_view(&self, view: &str) -> Result<String, String> {
        let rel = self
            .engine
            .circuit
            .view(view)
            .ok_or_else(|| format!("unknown view `{view}`"))?;
        Ok(rows_to_json(rel))
    }
}

fn decode(s: &str) -> Result<Value, String> {
    decode_value(s)
        .ok_or_else(|| format!("malformed value encoding: {s:?}"))
}

/// Decode a JSON array of events in [`event_to_json`]'s shape (what
/// [`RexApp::log_since`] emits) back into [`Event`]s for [`RexApp::replay`].
fn decode_events(json: &str) -> Result<Vec<Event>, String> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| format!("bad events json: {e}"))?;
    arr.iter().map(decode_event).collect()
}

fn decode_event(v: &serde_json::Value) -> Result<Event, String> {
    let bad = || String::from("malformed event json");
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
fn decode_base_snapshot(json: &str) -> Result<BaseSnapshot, String> {
    let bad = || String::from("malformed snapshot json");
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("bad snapshot json: {e}"))?;
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
        .collect::<Result<Vec<_>, String>>()?;
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
                .collect::<Result<Vec<_>, String>>()?;
            Ok((key, rows))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(BaseSnapshot { cursor, next_id, inputs })
}

fn decode_input_key(s: &str) -> Result<InputKey, String> {
    let bad = || String::from("malformed input key");
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

fn render_all(diags: &[rex::Diagnostic], src: &str) -> String {
    let msgs: Vec<String> = diags.iter().map(|d| d.render(src)).collect();
    msgs.join("\n")
}
