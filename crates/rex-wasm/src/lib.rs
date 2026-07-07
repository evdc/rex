//! The JS face of the Rex engine: one `RexApp` wraps a checked program and a
//! live incremental circuit. The protocol is deliberately narrow — values
//! cross the boundary as the canonical string encoding (`rex::eval::encode`),
//! deltas come back as one JSON batch per step — so the boundary is crossed
//! exactly once per transaction and the shaper can key its maps on the
//! encoded strings directly.

use rex::dbsp::Engine;
use rex::eval::{decode_value, encode_value, json_quote, rows_to_json, step_result_to_json, Value};
use rex::types::ty::SortId;
use rex::types::ValueTy;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct RexApp {
    engine: Engine,
    env: rex::types::Env,
    /// `new`-bound ids from the program source.
    values: HashMap<String, Value>,
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
        };
        for stmt in &typed.stmts {
            app.engine.apply_typed_stmt(stmt, &mut app.values);
        }
        Ok(app)
    }

    /// Create an entity: `fields` as parallel name/value arrays, values in
    /// canonical encoding. Returns `{"id":"<key>","deltas":{...}}`.
    pub fn apply_new(
        &mut self,
        entity: &str,
        field_names: Vec<String>,
        field_values: Vec<String>,
    ) -> Result<String, JsValue> {
        let sort = self
            .env
            .entity_sort(entity)
            .ok_or_else(|| JsValue::from_str(&format!("unknown entity `{entity}`")))?;
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
    /// Reject host input that doesn't match the schema before it reaches the
    /// engine: an unknown field, or a value whose type doesn't inhabit the
    /// declared field type. Without this a bad `update_field` silently stores
    /// (say) an Int in a Text column, or a non-pair value in a `T×T` field
    /// that a downstream `fst`/`snd` view then traps on.
    fn check_field(&self, sort: SortId, field: &str, value: &Value) -> Result<(), JsValue> {
        let Some(ty) = self.env.field_ty(sort, field) else {
            return Err(JsValue::from_str(&format!(
                "no field `{field}` on `{}`",
                self.env.sort_name(sort)
            )));
        };
        if !admits(ty, value) {
            return Err(JsValue::from_str(&format!(
                "field `{field}` expects `{}`, got value {}",
                self.env.show(ty),
                encode_value(value)
            )));
        }
        Ok(())
    }
}

fn decode(s: &str) -> Result<Value, JsValue> {
    decode_value(s)
        .ok_or_else(|| JsValue::from_str(&format!("malformed value encoding: {s:?}")))
}

/// Whether a runtime value inhabits a declared field type — the value-level
/// dual of the checker's typing, used to validate untrusted host input.
fn admits(ty: &ValueTy, v: &Value) -> bool {
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

fn render_all(diags: &[rex::Diagnostic], src: &str) -> JsValue {
    let msgs: Vec<String> = diags.iter().map(|d| d.render(src)).collect();
    JsValue::from_str(&msgs.join("\n"))
}
