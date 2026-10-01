//! A shared harness for the model-based and generative tests: boot a program
//! into a live [`Engine`], drive it with made-up events, and compare what it
//! holds against independent accounts of what it should hold —
//!
//!  - **the batch oracle**: every view's elaborated body, batch-evaluated over
//!    the engine's own base tables (`eval::interp`, the semantics' definition);
//!  - **the deltas**: each step's reported change is exactly the change in the
//!    batch value, so a consumer integrating deltas (the shaper) never drifts;
//!  - **replay**: the event log, replayed onto an empty engine, rebuilds the
//!    same state;
//!  - **snapshot + tail**: a base snapshot taken at any point, restored and
//!    followed by the rest of the log, does too.
//!
//! Each test file uses a different part of this, hence the `dead_code` allow.
#![allow(dead_code)]

use rex::dbsp::{ArgValue, BaseSnapshot, Batch, Engine, Event, InputKey, StepResult};
use rex::eval::interp::{eval_expr_with, eval_fixpoint, Store};
use rex::eval::relation::{BTreeRelation, BinaryRelation};
use rex::eval::{intern, Value};
use rex::events::{dispatch_event, replay};
use rex::types::env::Env;
use rex::types::shape_ir::{Encoding, EventDef, ParamTy};
use rex::types::ty::SortId;
use rex::types::typed::{TExpr, TProgram, TStmt};
use std::collections::HashMap;

/// One program statement the oracle re-evaluates: a view, or a recursion group.
pub enum ViewDef {
    Let(String, TExpr),
    Rec(Vec<(String, TExpr)>),
}

pub struct App {
    pub engine: Engine,
    pub env: Env,
    pub events: Vec<EventDef>,
    pub program: TProgram,
    pub views: Vec<ViewDef>,
    pub values: HashMap<String, Value>,
}

/// Parse and check `src`; the rendered diagnostics if it has errors.
pub fn check(src: &str) -> Result<(TProgram, Env, Vec<EventDef>), String> {
    let parsed = rex::parse(src);
    let errors = |ds: &[rex::Diagnostic]| ds.iter().map(|d| d.render(src)).collect::<Vec<_>>().join("\n");
    if !parsed.diagnostics.is_empty() {
        return Err(errors(&parsed.diagnostics));
    }
    let checked = rex::check(&parsed.program);
    match checked.elaborated {
        Some(program) => Ok((program, checked.env, checked.shapes.events)),
        None => Err(errors(&checked.diagnostics)),
    }
}

impl App {
    /// Boot `src` the way a first page load does: every statement applied, the
    /// program's `new`s logged as genesis events.
    pub fn build(src: &str) -> App {
        Self::try_build(src).unwrap_or_else(|e| panic!("program does not check:\n{e}"))
    }

    pub fn try_build(src: &str) -> Result<App, String> {
        Ok(Self::boot(check(src)?, false))
    }

    /// Boot `src` the way a restoring load does: views registered, no seed
    /// data (it comes back from a snapshot or the log's genesis events).
    pub fn build_empty(src: &str) -> App {
        Self::boot(check(src).unwrap_or_else(|e| panic!("program does not check:\n{e}")), true)
    }

    fn boot((program, env, events): (TProgram, Env, Vec<EventDef>), skip_new: bool) -> App {
        let mut engine = Engine::new();
        let mut values = HashMap::new();
        let mut views = Vec::new();
        let mut seeded = HashMap::new();
        for stmt in &program.stmts {
            match stmt {
                TStmt::New { .. } if skip_new => {
                    Engine::bind_seed_id(stmt, &mut seeded, &mut values);
                    continue;
                }
                TStmt::Let { name: Some(name), body } => views.push(ViewDef::Let(name.clone(), body.clone())),
                TStmt::LetRec { bindings } => views.push(ViewDef::Rec(bindings.clone())),
                _ => {}
            }
            engine.apply_typed_stmt(stmt, &mut values);
        }
        App { engine, env, events, program, views, values }
    }

    pub fn event(&self, name: &str) -> &EventDef {
        self.events.iter().find(|e| e.name == name).unwrap_or_else(|| panic!("no event `{name}`"))
    }

    /// Dispatch a declared event with scalar args.
    pub fn dispatch(&mut self, name: &str, args: &[(&str, Value)]) -> Result<(Vec<Value>, StepResult), String> {
        let args = args.iter().map(|(k, v)| (k.to_string(), ArgValue::Value(v.clone()))).collect();
        self.dispatch_args(name, &args)
    }

    pub fn dispatch_args(
        &mut self,
        name: &str,
        args: &HashMap<String, ArgValue>,
    ) -> Result<(Vec<Value>, StepResult), String> {
        self.try_dispatch(name, args).map_err(|e| e.to_string())
    }

    /// Dispatch, keeping *why* a refused dispatch was refused: rejected (the
    /// event does not apply to this state) or invalid (the call is wrong).
    pub fn try_dispatch(
        &mut self,
        name: &str,
        args: &HashMap<String, ArgValue>,
    ) -> Result<(Vec<Value>, StepResult), rex::events::Refusal> {
        dispatch_event(&mut self.engine, &self.env, &self.events, name, args)
    }

    /// The reason `name(args)` is rejected; panics if it is accepted or invalid.
    pub fn rejected(&mut self, name: &str, args: &[(&str, Value)]) -> String {
        let args = args.iter().map(|(k, v)| (k.to_string(), ArgValue::Value(v.clone()))).collect();
        match self.try_dispatch(name, &args) {
            Err(rex::events::Refusal::Rejected(reason)) => reason,
            other => panic!("`{name}` should be rejected, got {:?}", other.map(|(ids, _)| ids)),
        }
    }

    /// A base table's rows, sorted.
    pub fn field(&self, entity: &str, field: &str) -> Vec<(Value, Value)> {
        let sort = self.env.entity_sort(entity).unwrap_or_else(|| panic!("no entity `{entity}`"));
        let key = InputKey::Field(sort, intern(field));
        let mut rows: Vec<_> = self
            .engine
            .circuit
            .input_integral(&key)
            .map(|rel| rel.triples().map(|(l, r, _)| (l.clone(), r.clone())).collect())
            .unwrap_or_default();
        rows.sort();
        rows
    }

    /// The live ids of an entity, in id order.
    pub fn ids(&self, entity: &str) -> Vec<Value> {
        let sort = self.env.entity_sort(entity).unwrap_or_else(|| panic!("no entity `{entity}`"));
        let mut ids: Vec<_> = self
            .engine
            .circuit
            .input_integral(&InputKey::Identity(sort))
            .map(|rel| rel.keys().cloned().collect())
            .unwrap_or_default();
        ids.sort();
        ids
    }

    /// A view's rows as `(left, right, weight)`, sorted.
    pub fn view(&self, name: &str) -> Vec<(Value, Value, i64)> {
        self.engine.circuit.view(name).unwrap_or_else(|| panic!("no view `{name}`")).to_sorted_vec()
    }
}

/// Reads base tables and earlier views straight out of the live engine, so a
/// view's body is batch-evaluated over exactly the state the circuit holds.
struct EngineStore<'a>(&'a App);

impl Store for EngineStore<'_> {
    fn field_rel(&self, sort: SortId, field: &str) -> BTreeRelation {
        let key = InputKey::Field(sort, intern(field));
        self.0.engine.circuit.input_integral(&key).map(|v| v.to_relation()).unwrap_or_default()
    }
    fn identity_rel(&self, sort: SortId) -> BTreeRelation {
        let key = InputKey::Identity(sort);
        self.0.engine.circuit.input_integral(&key).map(|v| v.to_relation()).unwrap_or_default()
    }
    fn view_rel(&self, name: &str) -> BTreeRelation {
        self.0.engine.circuit.view(name).map(|v| v.to_relation()).unwrap_or_default()
    }
    fn value(&self, name: &str) -> Value {
        self.0.values[name].clone()
    }
}

/// Every view, batch-evaluated over the engine's current base tables.
pub fn batch_views(app: &App) -> HashMap<String, BTreeRelation> {
    let store = EngineStore(app);
    let mut out = HashMap::new();
    for def in &app.views {
        match def {
            ViewDef::Let(name, body) => {
                out.insert(name.clone(), eval_expr_with(&store, body));
            }
            ViewDef::Rec(bindings) => out.extend(eval_fixpoint(&store, bindings)),
        }
    }
    out
}

/// The base invariant every write path must keep: an identity row is
/// `(id, id)` at weight 1, and a field holds at most one row per id, at
/// weight 1, only for a live id.
pub fn check_base_invariant(engine: &Engine) -> Result<(), String> {
    let keys: Vec<InputKey> = engine.circuit.input_keys().copied().collect();
    for key in &keys {
        let rel = engine.circuit.input_integral(key).unwrap();
        match key {
            InputKey::Identity(_) => {
                for (l, r, w) in rel.triples() {
                    if l != r || w != 1 {
                        return Err(format!("{key:?} holds ({l}, {r}, {w})"));
                    }
                }
            }
            InputKey::Field(sort, _) => {
                let ids = engine.circuit.input_integral(&InputKey::Identity(*sort));
                for l in rel.keys() {
                    let live = ids.is_some_and(|ids| ids.weight(l, l) == 1);
                    let entries: Vec<_> = rel.row_ref(l).collect();
                    if !live || entries.len() != 1 || entries[0].1 != 1 {
                        return Err(format!("{key:?} at {l} (live: {live}) holds {entries:?}"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// The engine agrees with the batch oracle on every view.
pub fn check_views(app: &App, when: &str) -> Result<HashMap<String, BTreeRelation>, String> {
    let batch = batch_views(app);
    for (name, want) in &batch {
        let got = app.engine.circuit.view(name).ok_or_else(|| format!("{when}: no view `{name}`"))?;
        if got != want {
            return Err(format!(
                "{when}: view `{name}` diverged from batch\n  engine: {:?}\n  batch:  {:?}",
                got.to_sorted_vec(),
                want.to_sorted_vec()
            ));
        }
    }
    Ok(batch)
}

/// `step`'s delta for every view is exactly `after − before`.
pub fn check_deltas(
    step: &StepResult,
    before: &HashMap<String, BTreeRelation>,
    after: &HashMap<String, BTreeRelation>,
    when: &str,
) -> Result<(), String> {
    let empty = Batch::new();
    for (name, now) in after {
        let mut expected = now.clone();
        for (l, r, w) in before[name].triples() {
            expected.add(l.clone(), r.clone(), -w);
        }
        let got = step.view_deltas.get(name).unwrap_or(&empty);
        if got != &expected {
            return Err(format!(
                "{when}: delta of `{name}` is not the change in its value\n  delta:    {:?}\n  expected: {:?}",
                got.to_sorted_vec(),
                expected.to_sorted_vec()
            ));
        }
    }
    Ok(())
}

/// Two engines hold the same base tables and the same views.
pub fn check_same(a: &Engine, b: &Engine, what: &str) -> Result<(), String> {
    let mut names: Vec<&String> = a.circuit.output_names().collect();
    names.sort();
    for name in names {
        let av = a.circuit.view(name).unwrap().to_sorted_vec();
        let bv = b.circuit.view(name).map(|v| v.to_sorted_vec()).unwrap_or_default();
        if av != bv {
            return Err(format!("{what}: view `{name}` differs\n  live:  {av:?}\n  other: {bv:?}"));
        }
    }
    let keys: std::collections::HashSet<InputKey> =
        a.circuit.input_keys().chain(b.circuit.input_keys()).copied().collect();
    for key in keys {
        let rows = |e: &Engine| e.circuit.input_integral(&key).map(|r| r.to_sorted_vec()).unwrap_or_default();
        if rows(a) != rows(b) {
            return Err(format!("{what}: base table {key:?} differs\n  live:  {:?}\n  other: {:?}", rows(a), rows(b)));
        }
    }
    if a.cursor() != b.cursor() {
        return Err(format!("{what}: log cursor {} vs {}", a.cursor(), b.cursor()));
    }
    Ok(())
}

/// Replaying `app`'s whole log onto an empty engine reproduces it.
pub fn check_replay(src: &str, app: &App) -> Result<(), String> {
    for silent in [true, false] {
        let mut fresh = App::build_empty(src);
        replay(&mut fresh.engine, &fresh.env, &fresh.events, app.engine.log(), silent)
            .map_err(|e| format!("replay (silent: {silent}) failed: {e}"))?;
        check_same(&app.engine, &fresh.engine, &format!("replay (silent: {silent})"))?;
    }
    Ok(())
}

/// Restoring `snap` and replaying the log from its cursor reproduces `app`.
pub fn check_restore(src: &str, app: &App, snap: &BaseSnapshot) -> Result<(), String> {
    let mut fresh = App::build_empty(src);
    fresh.engine.restore(snap);
    let tail: Vec<Event> = app.engine.log().iter().filter(|e| e.seq >= snap.cursor).cloned().collect();
    replay(&mut fresh.engine, &fresh.env, &fresh.events, &tail, true)
        .map_err(|e| format!("replay of the tail after cursor {} failed: {e}", snap.cursor))?;
    check_same(&app.engine, &fresh.engine, &format!("snapshot at {} + tail of {}", snap.cursor, tail.len()))?;
    // And the restored engine is itself consistent: its views are what batch
    // evaluation says, not merely equal to the live engine's.
    check_views(&fresh, "restored").map(|_| ())
}

/// How many cases a property runs: `default` normally, `REX_FUZZ_CASES` when
/// set — `REX_FUZZ_CASES=20000 cargo test --release` is the long soak.
pub fn cases(default: u32) -> u32 {
    std::env::var("REX_FUZZ_CASES").ok().and_then(|n| n.parse().ok()).unwrap_or(default)
}

// --- a driver that knows nothing about any one program -----------------------

/// One made-up dispatch: which declared event, and raw choices to build its
/// arguments from.
#[derive(Clone, Debug)]
pub struct Op {
    pub event: usize,
    pub picks: Vec<usize>,
}

/// Text arguments, most of them chosen to break something: empty, the wire
/// encoding's structural characters, JSON's, things that look like other
/// encodings, non-BMP and separator code points.
pub const TEXTS: &[&str] = &[
    "a", "b", "", "a0", " ", "a,b", "(x)", "\\", "\\,", "\"q\"", "t:a", "#0:1", "@True", "i:3", "é", "😀",
    "\u{2028}", "\n", "\t", "\u{0}", "p(a,b)", "a\\",
];

/// Atoms the example programs declare (and one none does), so a coproduct
/// argument is sometimes valid and sometimes rejected.
pub const ATOMS: &[&str] = &["True", "False", "All", "Active", "Completed", "Red", "Green", "Blue", "Nope"];

pub const INTS: &[i64] = &[0, 1, 2, 3, 4, 5, 7, 10, 11, -1, -7, 999, 1000, i64::MAX, i64::MIN, i64::MAX - 1];

/// An argument of type `ty` from the raw choice `n`, or `None` for a type the
/// driver doesn't make up (the event is then skipped). Ids range a little past
/// what is ever minted, so some are live, some retracted, some never existed.
pub fn arg(app: &App, ty: &ParamTy, n: usize, rel_len: usize) -> Option<ArgValue> {
    let text = |n: usize| Value::text(TEXTS[n % TEXTS.len()]);
    Some(ArgValue::Value(match ty {
        ParamTy::Id(entity) => Value::Id(app.env.entity_sort(entity)?, (n % 12) as u64),
        ParamTy::Scalar(Encoding::Text) => text(n),
        ParamTy::Scalar(Encoding::Int) => Value::Int(INTS[n % INTS.len()]),
        ParamTy::Scalar(Encoding::Money) => Value::Money(INTS[n % INTS.len()]),
        ParamTy::Scalar(Encoding::Atom) => Value::atom(ATOMS[n % ATOMS.len()]),
        ParamTy::Scalar(Encoding::Id) => return None,
        ParamTy::Rel(k, v) if k == "Int" && v == "Text" => {
            // Mostly a well-formed `index -> label`; sometimes a zero or
            // negative weight, or a repeated key.
            return Some(ArgValue::Rel(
                (0..rel_len)
                    .map(|i| {
                        let key = if n % 11 == 3 { 0 } else { i as i64 };
                        let w = if n % 13 == 5 { [1, 0, -1, 2][i % 4] } else { 1 };
                        (Value::Int(key), text(n / 7 + i), w)
                    })
                    .collect(),
            ));
        }
        ParamTy::Rel(..) => return None,
    }))
}

/// Dispatch `op`; `None` if the driver can't make its arguments or the handler
/// rejects them. A rejected dispatch must change nothing — the caller checks.
pub fn dispatch_op(app: &mut App, op: &Op) -> Option<(String, StepResult)> {
    let def = &app.events[op.event % app.events.len()];
    let name = def.name.clone();
    let mut args = HashMap::new();
    for (i, p) in def.params.iter().enumerate() {
        let n = op.picks[i % op.picks.len()];
        args.insert(p.name.clone(), arg(app, &p.ty, n, op.picks[op.picks.len() - 1] % 5)?);
    }
    app.dispatch_args(&name, &args).ok().map(|(_, step)| (name, step))
}

/// Run `ops` against `src`, checking after every step: the base invariant,
/// every view against the batch oracle, and the step's deltas. Then: replay,
/// and a snapshot taken at step `cut` plus the tail. Returns the final app.
pub fn check_history(src: &str, ops: &[Op], cut: usize) -> Result<App, String> {
    let mut app = App::build(src);
    let mut before = check_views(&app, "at boot")?;
    let mut snap = (cut == 0).then(|| app.engine.base_snapshot());
    for (i, op) in ops.iter().enumerate() {
        let cursor = app.engine.cursor();
        match dispatch_op(&mut app, op) {
            Some((event, step)) => {
                let when = format!("after step {i} ({event})");
                check_base_invariant(&app.engine).map_err(|e| format!("{when}: {e}"))?;
                let after = check_views(&app, &when)?;
                check_deltas(&step, &before, &after, &when)?;
                if app.engine.cursor() != cursor + 1 {
                    return Err(format!("{when}: one dispatch logged {} events", app.engine.cursor() - cursor));
                }
                before = after;
            }
            None => {
                // Refused, or not attempted: nothing may have moved.
                if app.engine.cursor() != cursor {
                    return Err(format!("step {i}: a refused dispatch was logged"));
                }
                let now = batch_views(&app);
                if now != before {
                    return Err(format!("step {i}: a refused dispatch changed state"));
                }
            }
        }
        if i + 1 == cut {
            snap = Some(app.engine.base_snapshot());
        }
    }
    check_replay(src, &app)?;
    if let Some(snap) = &snap {
        check_restore(src, &app, snap)?;
    }
    Ok(app)
}
