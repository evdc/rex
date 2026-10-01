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

mod app;

pub use app::App;
use wasm_bindgen::prelude::*;

fn js(e: String) -> JsValue {
    JsValue::from_str(&e)
}

/// The wasm-bindgen face of [`App`]: the same calls, with errors as JS
/// exceptions. See `app.rs` for what each does.
#[wasm_bindgen]
pub struct RexApp(App);

#[wasm_bindgen]
impl RexApp {
    /// Parse, check, and apply a whole program, including its `new`
    /// statements (each logged as a `@genesis` event) — the first-load boot.
    /// Errors (rendered with source context) reject construction.
    #[wasm_bindgen(constructor)]
    pub fn new(source: &str) -> Result<RexApp, JsValue> {
        App::new(source).map(RexApp).map_err(js)
    }

    /// Parse and check a program but skip its `new` statements — the
    /// restoring boot: follow with [`restore`](Self::restore) and/or
    /// [`replay`](Self::replay).
    #[wasm_bindgen(js_name = forRestore)]
    pub fn for_restore(source: &str) -> Result<RexApp, JsValue> {
        App::for_restore(source).map(RexApp).map_err(js)
    }

    /// Dispatch a named event as one atomic transaction. `args_json` is an
    /// object keyed by the event's parameter names: a canonically-encoded
    /// scalar (`"t:a5"`), or `[k, v, w]` rows for a relation-typed param.
    /// Returns `{"ids":[…],"deltas":{"views":{…}}}`.
    pub fn dispatch(&mut self, name: &str, args_json: &str) -> Result<String, JsValue> {
        self.0.dispatch(name, args_json).map_err(js)
    }

    /// Re-space one manual-order level's keys (`[[id, newKey], …]`) as one
    /// logged `@rebalance` transaction. Returns `{"views":{…}}`.
    pub fn rebalance(&mut self, field: &str, rows_json: &str) -> Result<String, JsValue> {
        self.0.rebalance(field, rows_json).map_err(js)
    }

    /// The full contents of every view as one `{"views":{…}}` batch.
    pub fn snapshot(&self) -> String {
        self.0.snapshot()
    }

    /// Every logged event with `seq >= seq`, as a JSON array.
    pub fn log_since(&self, seq: u64) -> String {
        self.0.log_since(seq)
    }

    /// Re-apply logged events (the shape `log_since` returns). `silent` skips
    /// per-step deltas.
    pub fn replay(&mut self, events_json: &str, silent: bool) -> Result<(), JsValue> {
        self.0.replay(events_json, silent).map_err(js)
    }

    /// The engine's input tables plus id-minting and log-cursor state.
    pub fn base_snapshot(&self) -> String {
        self.0.base_snapshot()
    }

    /// Load a `base_snapshot` into a `forRestore`-booted app.
    pub fn restore(&mut self, base_json: &str) -> Result<(), JsValue> {
        self.0.restore(base_json).map_err(js)
    }

    /// The id bound to a `let name = new …` in the program source, if any.
    pub fn bound_id(&self, name: &str) -> Option<String> {
        self.0.bound_id(name)
    }

    /// One view's full contents as `[key, value, weight]` triples.
    pub fn read_view(&self, view: &str) -> Result<String, JsValue> {
        self.0.read_view(view).map_err(js)
    }
}
