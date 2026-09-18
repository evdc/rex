//! The JS face of the Rex engine: one `RexApp` wraps a checked program and a
//! live incremental circuit. The protocol is deliberately narrow — values
//! cross the boundary as the canonical string encoding (`rex::eval::encode`),
//! deltas come back as one JSON batch per step — so the boundary is crossed
//! exactly once per transaction and the shaper can key its maps on the
//! encoded strings directly.

use rex::dbsp::Engine;
use rex::eval::{decode_value, encode_value, json_quote, rows_to_json, step_result_to_json, Value};
use rex::events::{check_field, dispatch_event};
use rex::types::shape_ir::EventDef;
use rex::types::ty::SortId;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct RexApp {
    engine: Engine,
    env: rex::types::Env,
    /// `new`-bound ids from the program source.
    values: HashMap<String, Value>,
    /// Declared events with their handler bodies (the EventIR, S-20).
    events: Vec<EventDef>,
}

#[wasm_bindgen]
impl RexApp {
    /// Parse, check, and apply a whole program. Errors (rendered with source
    /// context) reject construction; warnings are ignored here — surface them
    /// at build time, not app boot.
    #[wasm_bindgen(constructor)]
    pub fn new(source: &str) -> Result<RexApp, JsValue> {
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
            app.engine.apply_typed_stmt(stmt, &mut app.values);
        }
        Ok(app)
    }

    /// Dispatch a named event as ONE atomic transaction (S-20). `args_json`
    /// is a flat object of canonically-encoded values keyed by the event's
    /// declared parameter names (`{"card":"…","list":"…","pos":"t:a5"}`).
    /// Returns `{"ids":[…],"deltas":{"views":{…}}}` — `ids` are the keys
    /// minted by any `new` mutations, in order.
    pub fn dispatch(&mut self, name: &str, args_json: &str) -> Result<String, JsValue> {
        let raw: HashMap<String, String> = serde_json::from_str(args_json)
            .map_err(|e| JsValue::from_str(&format!("bad event args: {e}")))?;
        let mut args = HashMap::new();
        for (k, v) in raw {
            args.insert(k, decode(&v)?);
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

    /// Create an entity: `fields` as parallel name/value arrays, values in
    /// canonical encoding. Returns `{"id":"<key>","deltas":{...}}`.
    pub fn apply_new(
        &mut self,
        entity: &str,
        field_names: Vec<String>,
        field_values: Vec<String>,
    ) -> Result<String, JsValue> {
        let sort = self.entity_sort(entity)?;
        if field_names.len() != field_values.len() {
            return Err(JsValue::from_str("field name/value arrays differ in length"));
        }
        let fields: Vec<(String, Value)> = field_names
            .into_iter()
            .zip(&field_values)
            .map(|(n, v)| {
                let value = decode(v)?;
                self.check_field(sort, &n, &value)?;
                Ok((n, value))
            })
            .collect::<Result<_, JsValue>>()?;
        let (id, res) = self.engine.apply_new(sort, &fields);
        Ok(format!(
            r#"{{"id":{},"deltas":{}}}"#,
            json_quote(&encode_value(&id)),
            step_result_to_json(&res)
        ))
    }

    /// Update one field of an entity (id and value in canonical encoding).
    /// Emits the `−old/+new` same-key delta shape the shaper fuses.
    pub fn update_field(&mut self, id: &str, field: &str, value: &str) -> Result<String, JsValue> {
        self.update_fields(id, vec![field.to_string()], vec![value.to_string()])
    }

    /// Update several fields of an entity in ONE atomic transaction (parallel
    /// name/value arrays, values canonically encoded). A multi-field edit — a
    /// Kanban drag changing both `list` and `pos` — must be one step so the
    /// shaper classifies it as a single reparent, not a torn move-then-reparent.
    pub fn update_fields(
        &mut self,
        id: &str,
        field_names: Vec<String>,
        field_values: Vec<String>,
    ) -> Result<String, JsValue> {
        let id = decode(id)?;
        let Value::Id(sort, _) = id else {
            return Err(JsValue::from_str("update target is not an entity id"));
        };
        if field_names.len() != field_values.len() {
            return Err(JsValue::from_str("field name/value arrays differ in length"));
        }
        let updates: Vec<(String, Value)> = field_names
            .into_iter()
            .zip(&field_values)
            .map(|(n, v)| {
                let value = decode(v)?;
                self.check_field(sort, &n, &value)?;
                Ok((n, value))
            })
            .collect::<Result<_, JsValue>>()?;
        Ok(step_result_to_json(&self.engine.update_fields(&id, &updates)))
    }

    /// Retract an entity (identity plus every field), one atomic transaction.
    pub fn retract(&mut self, id: &str) -> Result<String, JsValue> {
        let id = decode(id)?;
        Ok(step_result_to_json(&self.engine.retract_entity(&id)))
    }

    /// The id bound to a `let name = new …` in the program source, if any —
    /// how the host addresses seed data.
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
    fn entity_sort(&self, entity: &str) -> Result<SortId, JsValue> {
        self.env
            .entity_sort(entity)
            .ok_or_else(|| JsValue::from_str(&format!("unknown entity `{entity}`")))
    }

    fn check_field(&self, sort: SortId, field: &str, value: &Value) -> Result<(), JsValue> {
        check_field(&self.env, sort, field, value).map_err(|e| JsValue::from_str(&e))
    }
}

fn decode(s: &str) -> Result<Value, JsValue> {
    decode_value(s)
        .ok_or_else(|| JsValue::from_str(&format!("malformed value encoding: {s:?}")))
}

fn render_all(diags: &[rex::Diagnostic], src: &str) -> JsValue {
    let msgs: Vec<String> = diags.iter().map(|d| d.render(src)).collect();
    JsValue::from_str(&msgs.join("\n"))
}
