//! P-1: lowering's algebraic rewrites (`dbsp/lower.rs`) against the batch
//! oracle, and the engine base invariant they rely on.
//!
//! The rewrites are sound only while every base table the engine holds keeps
//! that invariant (identity rows at weight 1; at most one weight-1 field row
//! per id, and only for live ids). So the property here drives the *engine*,
//! through declared events only, with a random history that includes stale
//! ids, a delete and a create in one transaction, and bulk replace, and after
//! every step checks (a) the invariant and (b) every view, rewritten or not,
//! against batch evaluation of its unrewritten body over the same base tables.

use proptest::prelude::*;
use rex::dbsp::{ArgValue, Engine, InputKey, Node, StepResult};
use rex::eval::interp::{Store, eval_expr_with};
use rex::eval::relation::{BTreeRelation, BinaryRelation};
use rex::eval::{Value, intern};
use rex::events::dispatch_event;
use rex::types::ty::SortId;
use rex::types::typed::{TExpr, TStmt};
use std::collections::HashMap;

/// The js-framework-benchmark program, plus `let`s that reach each rewrite
/// (and a few that must stay unrewritten) outside the view desugaring.
fn source() -> String {
    let mut src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/js-framework-benchmark/src/app.rex"
    ))
    .unwrap();
    src.push_str(
        r#"
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
"#,
    );
    src
}

struct App {
    engine: Engine,
    env: rex::types::env::Env,
    events: Vec<rex::types::shape_ir::EventDef>,
    views: Vec<(String, TExpr)>,
    values: HashMap<String, Value>,
    row: SortId,
}

fn build() -> App {
    let parsed = rex::parse(&source());
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
    let row = checked.env.entity_sort("Row").unwrap();
    App { engine, env: checked.env, events: checked.shapes.events, views, values, row }
}

/// Reads base tables and earlier views straight out of the live engine, so
/// each view's *unrewritten* body is batch-evaluated over exactly the state
/// the circuit holds.
struct EngineStore<'a>(&'a App);

impl Store for EngineStore<'_> {
    fn field_rel(&self, sort: SortId, field: &str) -> BTreeRelation {
        let key = InputKey::Field(sort, intern(field));
        self.0.engine.circuit.input_integral(&key).cloned().unwrap_or_default()
    }
    fn identity_rel(&self, sort: SortId) -> BTreeRelation {
        let key = InputKey::Identity(sort);
        self.0.engine.circuit.input_integral(&key).cloned().unwrap_or_default()
    }
    fn view_rel(&self, name: &str) -> BTreeRelation {
        self.0.engine.circuit.view(name).cloned().unwrap_or_default()
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
                for (l, row) in rel.rows() {
                    let live = ids.is_some_and(|ids| ids.weight(l, l) == 1);
                    let entries: Vec<_> = row.iter().collect();
                    if !live || entries.len() != 1 || *entries[0].1 != 1 {
                        return Err(format!("{key:?} at {l} (live: {live}) holds {entries:?}"));
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
enum Op {
    Run(Vec<&'static str>),
    Add(Vec<&'static str>),
    Update,
    Clear,
    SwapRows,
    Select(u64),
    Delete(u64),
}

fn op() -> impl Strategy<Value = Op> {
    let labels = prop::collection::vec(prop::sample::select(vec!["a", "b", "c"]), 0..5);
    prop_oneof![
        labels.clone().prop_map(Op::Run),
        labels.prop_map(Op::Add),
        Just(Op::Update),
        Just(Op::Clear),
        Just(Op::SwapRows),
        // Ids past the last minted, and ids long since retracted, are both
        // fair game: a stale `set` or `delete` must leave the invariant intact.
        (0u64..24).prop_map(Op::Select),
        (0u64..24).prop_map(Op::Delete),
    ]
}

fn labels(ls: &[&str]) -> ArgValue {
    ArgValue::Rel(ls.iter().enumerate().map(|(i, l)| (Value::Int(i as i64), Value::text(l), 1)).collect())
}

fn dispatch(app: &mut App, op: &Op) -> StepResult {
    let row = |n: u64| ArgValue::Value(Value::Id(app.row, n));
    let (name, args): (&str, Vec<(&str, ArgValue)>) = match op {
        Op::Run(ls) => ("Run", vec![("n", ArgValue::Value(Value::Int(ls.len() as i64))), ("labels", labels(ls))]),
        Op::Add(ls) => ("Add", vec![("labels", labels(ls))]),
        Op::Update => ("Update", vec![]),
        Op::Clear => ("Clear", vec![]),
        Op::SwapRows => ("SwapRows", vec![]),
        Op::Select(n) => ("Select", vec![("r", row(*n))]),
        Op::Delete(n) => ("Delete", vec![("r", row(*n))]),
    };
    let args = args.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    dispatch_event(&mut app.engine, &app.env, &app.events, name, &args).unwrap().1
}

proptest! {
    #[test]
    fn rewritten_views_match_batch_under_any_history(ops in prop::collection::vec(op(), 1..16)) {
        let mut app = build();
        let mut before = batch_views(&app);
        for (name, _) in &app.views {
            prop_assert_eq!(app.engine.circuit.view(name).unwrap(), &before[name], "boot: `{}`", name);
        }
        for op in &ops {
            let step = dispatch(&mut app, op);
            check_base_invariant(&app.engine).map_err(|e| TestCaseError::fail(format!("after {op:?}: {e}")))?;
            let after = batch_views(&app);
            for (name, _) in &app.views {
                prop_assert_eq!(app.engine.circuit.view(name).unwrap(), &after[name], "after {:?}: `{}`", op, name);
                // The step's delta too, which for an aliased view is the
                // input's delta: exactly the change in the batch value.
                let mut expected = after[name].clone();
                for (l, r, w) in before[name].triples() {
                    expected.add(l.clone(), r.clone(), -w);
                }
                let empty = BTreeRelation::new();
                let got = step.view_deltas.get(name).unwrap_or(&empty);
                prop_assert_eq!(got, &expected, "delta after {:?}: `{}`", op, name);
            }
            before = after;
        }
    }
}

/// The rewrites actually fire: the benchmark's row level reads its inputs
/// directly, and each hidden keyset view is one node over one field.
#[test]
fn benchmark_views_lower_to_inputs_and_single_filters() {
    let app = build();
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
    // Two fields co-keyed are a real join, and not a coreflexive `Row[…]`
    // can drop: those stay.
    assert!(matches!(node("r_sum"), Node::CoKeyed { .. }));
    assert!(matches!(node("r_sumbig"), Node::Semijoin { .. }));
}
