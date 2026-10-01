//! Lowering's rewrites (`dbsp/lower.rs`, P-1 and P-2b) and node sharing
//! (`Circuit::add_node`, P-2b) against the batch oracle, and the engine base
//! invariant the rewrites rely on.
//!
//! The rewrites are sound only while every base table the engine holds keeps
//! that invariant (identity rows at weight 1; at most one weight-1 field row
//! per id, and only for live ids). So the property here drives the *engine*,
//! through declared events only, and after every step checks (a) the
//! invariant and (b) every view, rewritten or not, against batch evaluation of
//! its unrewritten body over the same base tables, plus the step's delta.
//!
//! The driver knows nothing about any one program: it picks a declared event
//! and makes up its arguments from the parameter types, including ids that were
//! never minted or are long retracted. It runs over each example app, each
//! with extra `let`s that reach every rewrite and some shapes that look like
//! one but must not be rewritten (a field of ids, which can dangle).

use proptest::prelude::*;
use rex::dbsp::{ArgValue, Engine, InputKey, Node, StepResult};
use rex::eval::interp::{Store, eval_expr_with};
use rex::eval::relation::{BTreeRelation, BinaryRelation};
use rex::eval::{Value, intern};
use rex::events::dispatch_event;
use rex::types::shape_ir::{Encoding, EventDef, ParamTy};
use rex::types::typed::{TExpr, TStmt};
use std::collections::HashMap;

fn example(path: &str) -> String {
    std::fs::read_to_string(format!("{}/../../examples/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

// Each program's extras start with a few `new` rows, so every extra `let` is
// backfilled over data, and some reuse nodes the app's own views built and
// stepped (P-2b: shared, and recomputed by the backfill if they kept nothing).

/// The js-framework-benchmark program, plus `let`s that reach each rewrite
/// (and a few that must stay unrewritten) outside the view desugaring.
fn benchmark() -> String {
    example("js-framework-benchmark/src/app.rex")
        + r#"
let r0 = new Row { num: 3, label: "a", pos: 1, selected: False }
let r1 = new Row { num: 8, label: "b", pos: 2, selected: True }
let r_num    : Row -> Int  = Row . .num
let r_big    : Row -> Row  = Row[.num > 5]
let r_constl : Row -> Row  = Row[3 < .num]
let r_mapped : Row -> Int  = Row . (.num * 2 + 1)
let r_text   : Row -> Text = .label ++ "!"
let r_in     : Row -> Row  = Row[.pos in (1 | 2)]
let r_where  : Row = id where .num >= 4
let r_sum    : Row -> Int  = .num + .pos
let r_sumbig : Row -> Row  = Row[.num + .pos > 3]
let r_viewed : Row -> Row  = Row[r_mapped > 7]
let r_right  : Row -> Row  = r_big . Row
let r_both   : Row -> Row  = (Row where .selected) . (Row where not .selected)
let r_by     : Unit -> Int = count(Row by unit)
let r_bynum  : Int -> Int  = count(Row by .num)
let r_again  : Row = Row where not .selected
"#
}

/// TodoMVC plus a `Tag` entity whose `todo` field is left dangling by
/// `DeleteTodo` (nothing cascades), so `.todo . Todo` must keep its join.
fn todomvc() -> String {
    example("todomvc/src/app.rex")
        + r#"
entity Tag { todo: Todo }
event AddTag(t: Todo)
on AddTag(t) => new Tag { todo: t }

let t0 = new Todo { text: "a", completed: False }
let t1 = new Todo { text: "b", completed: True }
let g0 = new Tag { todo: t1 }

let t_unit    : Todo -> Unit = unit
let t_filter  : Todo -> Filter = Todo . unit . filter
let t_right   : Todo -> Todo = visible . Todo
let t_both    : Todo -> Todo = (Todo where .completed) . (Todo where not .completed)
let t_again   : Todo = Todo where not .completed
let t_done    : Unit -> Int = count((Todo where .completed) by unit)
let t_dangle  : Tag -> Todo = (.todo) . Todo
let t_tagged  : Todo -> Int = count(Tag by .todo)
let t_live    : Tag -> Tag = Tag[(.todo) . Todo]
let t_open    : Tag -> Tag = Tag[(.todo) except (Todo where .completed)]
let t_closed  : Tag -> Tag = Tag[(.todo)[Todo where .completed]]
"#
}

/// Kanban: `AddCard` and `MoveCard` accept any list id, so `Card.list` can
/// name a list that never existed.
fn kanban() -> String {
    example("kanban/src/board.rex")
        + r#"
let k0 = new Card { title: "x", pos: "a2", list: l_done }
let k_dangle : Card -> List = (.list) . List
let k_right  : List -> List = List . List
let k_by     : List -> Int  = count(Card by .list)
"#
}

struct App {
    engine: Engine,
    env: rex::types::env::Env,
    events: Vec<EventDef>,
    views: Vec<(String, TExpr)>,
    values: HashMap<String, Value>,
}

fn build(src: &str) -> App {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let prog = checked.elaborated.unwrap();
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    let mut views = Vec::new();
    for stmt in &prog.stmts {
        engine.apply_typed_stmt(stmt, &mut values);
        if let TStmt::Let { name: Some(name), body } = stmt {
            views.push((name.clone(), body.clone()));
        }
    }
    App { engine, env: checked.env, events: checked.shapes.events, views, values }
}

/// Reads base tables and earlier views straight out of the live engine, so
/// each view's *unrewritten* body is batch-evaluated over exactly the state
/// the circuit holds.
struct EngineStore<'a>(&'a App);

impl Store for EngineStore<'_> {
    fn field_rel(&self, sort: rex::types::ty::SortId, field: &str) -> BTreeRelation {
        let key = InputKey::Field(sort, intern(field));
        self.0.engine.circuit.input_integral(&key).map(|v| v.to_relation()).unwrap_or_default()
    }
    fn identity_rel(&self, sort: rex::types::ty::SortId) -> BTreeRelation {
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

fn batch_views(app: &App) -> HashMap<String, BTreeRelation> {
    app.views.iter().map(|(name, body)| (name.clone(), eval_expr_with(&EngineStore(app), body))).collect()
}

/// The base invariant, read from outside through the public API.
fn check_base_invariant(engine: &Engine) -> Result<(), String> {
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

/// One random dispatch: which declared event, and raw choices its arguments
/// are made from.
#[derive(Clone, Debug)]
struct Op {
    event: usize,
    picks: Vec<usize>,
}

fn op() -> impl Strategy<Value = Op> {
    (any::<usize>(), prop::collection::vec(any::<usize>(), 8)).prop_map(|(event, picks)| Op { event, picks })
}

/// Atoms any of the example programs declares, so a coproduct argument is
/// sometimes valid and sometimes rejected by the handler's field check.
const ATOMS: &[&str] = &["True", "False", "All", "Active", "Completed"];

/// An argument of type `ty` from the raw choice `n`, or `None` for a type the
/// driver doesn't make up (the event is then skipped).
fn arg(app: &App, ty: &ParamTy, n: usize, rel_len: usize) -> Option<ArgValue> {
    let text = |n: usize| Value::text(["a", "b", "c", "a0", "a1"][n % 5]);
    Some(ArgValue::Value(match ty {
        // Ids up to 12, whether minted, live, or long retracted.
        ParamTy::Id(entity) => Value::Id(app.env.entity_sort(entity)?, (n % 12) as u64),
        ParamTy::Scalar(Encoding::Text) => text(n),
        ParamTy::Scalar(Encoding::Int) => Value::Int((n % 6) as i64),
        ParamTy::Scalar(Encoding::Money) => Value::Money((n % 6) as i64 * 100),
        ParamTy::Scalar(Encoding::Atom) => Value::atom(ATOMS[n % ATOMS.len()]),
        ParamTy::Scalar(Encoding::Id) => return None,
        ParamTy::Rel(k, v) if k == "Int" && v == "Text" => {
            return Some(ArgValue::Rel(
                (0..rel_len).map(|i| (Value::Int(i as i64), text(n / 7 + i), 1)).collect(),
            ));
        }
        ParamTy::Rel(..) => return None,
    }))
}

/// Dispatch `op`, or `None` if the driver can't make its arguments or the
/// handler rejects them (a rejected dispatch changes nothing).
fn dispatch(app: &mut App, op: &Op) -> Option<StepResult> {
    let def = &app.events[op.event % app.events.len()];
    let name = def.name.clone();
    let mut args = HashMap::new();
    for (i, p) in def.params.iter().enumerate() {
        let n = op.picks[i % op.picks.len()];
        args.insert(p.name.clone(), arg(app, &p.ty, n, op.picks[7] % 5)?);
    }
    dispatch_event(&mut app.engine, &app.env, &app.events, &name, &args).ok().map(|(_, step)| step)
}

fn check_history(src: &str, ops: &[Op]) -> Result<(), TestCaseError> {
    let mut app = build(src);
    let mut before = batch_views(&app);
    for (name, _) in &app.views {
        prop_assert_eq!(app.engine.circuit.view(name).unwrap(), &before[name], "boot: `{}`", name);
    }
    for op in ops {
        let Some(step) = dispatch(&mut app, op) else { continue };
        let event = &app.events[op.event % app.events.len()].name;
        check_base_invariant(&app.engine).map_err(|e| TestCaseError::fail(format!("after {event}: {e}")))?;
        let after = batch_views(&app);
        for (name, _) in &app.views {
            prop_assert_eq!(app.engine.circuit.view(name).unwrap(), &after[name], "after {}: `{}`", event, name);
            // The step's delta too, which for an aliased or shared view is
            // another node's delta: exactly the change in the batch value.
            let mut expected = after[name].clone();
            for (l, r, w) in before[name].triples() {
                expected.add(l.clone(), r.clone(), -w);
            }
            let empty = rex::dbsp::Batch::new();
            let got = step.view_deltas.get(name).unwrap_or(&empty);
            prop_assert_eq!(got, &expected, "delta after {}: `{}`", event, name);
        }
        before = after;
    }
    Ok(())
}

proptest! {
    #[test]
    fn benchmark_views_match_batch_under_any_history(ops in prop::collection::vec(op(), 1..20)) {
        check_history(&benchmark(), &ops)?;
    }

    #[test]
    fn todomvc_views_match_batch_under_any_history(ops in prop::collection::vec(op(), 1..20)) {
        check_history(&todomvc(), &ops)?;
    }

    #[test]
    fn kanban_views_match_batch_under_any_history(ops in prop::collection::vec(op(), 1..20)) {
        check_history(&kanban(), &ops)?;
    }
}

/// The rewrites actually fire: the benchmark's row level reads its inputs
/// directly, and each hidden keyset view is one node over one field.
#[test]
fn benchmark_views_lower_to_inputs_and_single_filters() {
    let app = build(&benchmark());
    let c = &app.engine.circuit;
    let node = |view: &str| c.node(c.output(view).unwrap());
    for view in ["main#unit#row#num", "main#unit#row#label", "main#unit#row#order", "r_num"] {
        assert!(matches!(node(view), Node::Input(InputKey::Field(..))), "`{view}` should alias its field input");
    }
    assert!(matches!(node("main#unit#row"), Node::MapConst(..)));
    for view in ["main#unit#row#gate1", "on#Update#1", "on#SwapRows#2", "on#SwapRows#3", "on#Select#4"] {
        let Node::FilterMap(input, _) = node(view) else { panic!("`{view}` should be one FilterMap") };
        assert!(matches!(c.node(*input), Node::Input(InputKey::Field(..))), "`{view}` should read a field input");
    }
    // Two fields co-keyed are a real join, which stays. It is functional
    // (P-4a: one pairing per key), so a comparison over it is a weight-1
    // coreflexive and `Row[…]` drops.
    assert!(matches!(node("r_sum"), Node::CoKeyed { .. }));
    assert!(matches!(node("r_sumbig"), Node::FilterMap(..)));
}

/// P-2b fires where TodoMVC's desugarings put the waste: each repeated
/// subterm is one node, a right identity is dropped, `E where not …` is one
/// antijoin, and a field of ids keeps its join.
#[test]
fn todomvc_lowers_shared_and_without_identities() {
    let app = build(&todomvc());
    let c = &app.engine.circuit;
    let out = |view: &str| c.output(view).unwrap();
    // `visible . Todo`, and the `order by id` level, are `visible` itself.
    assert_eq!(out("t_right"), out("visible"));
    assert_eq!(out("main#unit#if1#visible#order"), out("visible"));
    // The same `where` in `active` and in `t_again` is one node, and it is
    // a single antijoin.
    assert!(matches!(c.node(out("t_again")), Node::Antijoin { .. }));
    let Node::Aggregate { input, .. } = c.node(out("active")) else { panic!("`active` is an aggregate") };
    let Node::Compose { r, .. } = c.node(*input) else { panic!("`active` counts a compose") };
    assert_eq!(*r, out("t_again"));
    // `count(E by unit)` counts `~(E -> Unit)` directly.
    let Node::Aggregate { input, .. } = c.node(out("total")) else { panic!("`total` is an aggregate") };
    assert!(matches!(c.node(*input), Node::Inverse(_)));
    // A field's ids can dangle: the join with the identity stays.
    assert!(matches!(c.node(out("t_dangle")), Node::Compose { .. }));
    // No two nodes are equal.
    let mut seen = HashMap::new();
    for i in 0..c.node_count() {
        let key = format!("{:?}", c.node(rex::dbsp::NodeId(i)));
        if let Some(j) = seen.insert(key.clone(), i) {
            panic!("nodes {j} and {i} are both {key}");
        }
    }
}
