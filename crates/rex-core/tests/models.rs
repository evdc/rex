//! Reference models: each app's event semantics written again, by hand, as
//! ordinary Rust over ordinary structs — no relations, no engine — and held
//! against the real thing after every event.
//!
//! `tests/histories.rs` checks the engine against batch evaluation of the same
//! program, which catches incremental-maintenance bugs but would agree with a
//! handler that does the wrong thing consistently. These models are the
//! independent account of what a handler *means*: which rows a `where` picks,
//! that reads see the pre-event state, that a `new … from` mints in key order,
//! that an update to a deleted row is nothing, that ids are never reused.

mod common;

use common::{check_base_invariant, check_replay, check_views, App, TEXTS};
use proptest::prelude::*;
use rex::dbsp::ArgValue;
use rex::eval::Value;
use std::collections::HashMap;

fn example(path: &str) -> String {
    std::fs::read_to_string(format!("{}/../../examples/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn text() -> impl Strategy<Value = String> {
    (0..TEXTS.len()).prop_map(|i| TEXTS[i].to_string())
}

fn id(app: &App, entity: &str, n: u64) -> Value {
    Value::Id(app.env.entity_sort(entity).unwrap(), n)
}

fn seq(v: &Value) -> u64 {
    match v {
        Value::Id(_, n) => *n,
        other => panic!("not an id: {other}"),
    }
}

fn bool_atom(b: bool) -> Value {
    Value::atom(if b { "True" } else { "False" })
}

/// A field's rows as `seq -> value`.
fn column(app: &App, entity: &str, field: &str) -> Vec<(u64, Value)> {
    app.field(entity, field).into_iter().map(|(k, v)| (seq(&k), v)).collect()
}

/// A coreflexive view's keys, as id sequence numbers.
fn keyset(app: &App, view: &str) -> Vec<u64> {
    app.view(view).into_iter().map(|(k, _, w)| { assert_eq!(w, 1, "`{view}` weight"); seq(&k) }).collect()
}

/// A `Unit -> Int` view's one value.
fn scalar(app: &App, view: &str) -> i64 {
    match app.view(view).as_slice() {
        [(Value::Unit, Value::Int(n), 1)] => *n,
        other => panic!("`{view}` is not one Int: {other:?}"),
    }
}

// --- TodoMVC ---------------------------------------------------------------------

#[derive(Clone, Debug)]
enum TodoOp {
    Add(String),
    Toggle(u64),
    Edit(u64, String),
    Delete(u64),
    ToggleAll(bool),
    ClearCompleted,
    SetFilter(u8),
}

fn todo_op() -> impl Strategy<Value = TodoOp> {
    // Ids run a little past what a short history mints: some never existed.
    let t = 0u64..10;
    prop_oneof![
        4 => text().prop_map(TodoOp::Add),
        3 => t.clone().prop_map(TodoOp::Toggle),
        2 => (t.clone(), text()).prop_map(|(t, s)| TodoOp::Edit(t, s)),
        2 => t.prop_map(TodoOp::Delete),
        1 => any::<bool>().prop_map(TodoOp::ToggleAll),
        1 => Just(TodoOp::ClearCompleted),
        2 => (0u8..3).prop_map(TodoOp::SetFilter),
    ]
}

const FILTERS: [&str; 3] = ["All", "Active", "Completed"];

#[derive(Default)]
struct TodoModel {
    /// `(id, text, completed)`, in id order.
    todos: Vec<(u64, String, bool)>,
    next: u64,
    filter: usize,
}

impl TodoModel {
    /// Apply `op`; `false` if the app must refuse it.
    fn apply(&mut self, op: &TodoOp) -> bool {
        match op {
            TodoOp::Add(text) => {
                self.todos.push((self.next, text.clone(), false));
                self.next += 1;
            }
            // `t.completed := not t.completed` reads the row: none, no event.
            TodoOp::Toggle(t) => match self.todos.iter_mut().find(|r| r.0 == *t) {
                Some(row) => row.2 = !row.2,
                None => return false,
            },
            // A plain write to a row that is gone is accepted and does nothing.
            TodoOp::Edit(t, text) => {
                if let Some(row) = self.todos.iter_mut().find(|r| r.0 == *t) {
                    row.1 = text.clone();
                }
            }
            TodoOp::Delete(t) => self.todos.retain(|r| r.0 != *t),
            TodoOp::ToggleAll(done) => self.todos.iter_mut().for_each(|r| r.2 = *done),
            TodoOp::ClearCompleted => self.todos.retain(|r| !r.2),
            TodoOp::SetFilter(f) => self.filter = *f as usize,
        }
        true
    }

    fn visible(&self) -> Vec<u64> {
        self.todos
            .iter()
            .filter(|r| match self.filter {
                0 => true,
                1 => !r.2,
                _ => r.2,
            })
            .map(|r| r.0)
            .collect()
    }
}

fn todo_dispatch(app: &mut App, op: &TodoOp) -> Result<(), String> {
    let t = |app: &App, n: u64| id(app, "Todo", n);
    let r = match op {
        TodoOp::Add(s) => app.dispatch("AddTodo", &[("text", Value::text(s))]),
        TodoOp::Toggle(n) => app.dispatch("ToggleTodo", &[("t", t(app, *n))]),
        TodoOp::Edit(n, s) => app.dispatch("EditTodo", &[("t", t(app, *n)), ("text", Value::text(s))]),
        TodoOp::Delete(n) => app.dispatch("DeleteTodo", &[("t", t(app, *n))]),
        TodoOp::ToggleAll(b) => app.dispatch("ToggleAll", &[("done", bool_atom(*b))]),
        TodoOp::ClearCompleted => app.dispatch("ClearCompleted", &[]),
        TodoOp::SetFilter(f) => app.dispatch("SetFilter", &[("f", Value::atom(FILTERS[*f as usize]))]),
    };
    r.map(|_| ())
}

fn todo_agrees(app: &App, m: &TodoModel) -> Result<(), String> {
    let want_text: Vec<_> = m.todos.iter().map(|r| (r.0, Value::text(&r.1))).collect();
    let want_done: Vec<_> = m.todos.iter().map(|r| (r.0, bool_atom(r.2))).collect();
    let ids: Vec<u64> = app.ids("Todo").iter().map(seq).collect();
    let checks: [(&str, bool); 8] = [
        ("ids", ids == m.todos.iter().map(|r| r.0).collect::<Vec<_>>()),
        ("text", column(app, "Todo", "text") == want_text),
        ("completed", column(app, "Todo", "completed") == want_done),
        ("filter", app.field("State#", "filter").iter().map(|(_, v)| v.clone()).collect::<Vec<_>>() == [Value::atom(FILTERS[m.filter])]),
        ("visible", keyset(app, "visible") == m.visible()),
        ("total", scalar(app, "total") == m.todos.len() as i64),
        ("active", scalar(app, "active") == m.todos.iter().filter(|r| !r.2).count() as i64),
        ("completed count", scalar(app, "completed") == m.todos.iter().filter(|r| r.2).count() as i64),
    ];
    match checks.iter().find(|(_, ok)| !ok) {
        Some((what, _)) => Err(format!("`{what}` disagrees with the model: todos {:?}, filter {}", m.todos, FILTERS[m.filter])),
        None => Ok(()),
    }
}

// --- js-framework-benchmark --------------------------------------------------------

#[derive(Clone, Debug)]
enum RowOp {
    Run(i64, Vec<(i64, String, i64)>),
    Add(Vec<(i64, String, i64)>),
    Update,
    Clear,
    SwapRows,
    Select(u64),
    Delete(u64),
}

/// A relation-typed `labels` argument. Usually `index -> label` as the DOM
/// layer sends it; sometimes with a repeated key, or a zero or negative
/// weight (a host can send anything).
fn labels() -> impl Strategy<Value = Vec<(i64, String, i64)>> {
    let honest = prop::collection::vec(text(), 0..8)
        .prop_map(|ls| ls.into_iter().enumerate().map(|(i, l)| (i as i64, l, 1)).collect::<Vec<_>>());
    let odd = prop::collection::vec((0i64..4, text(), -1i64..3), 0..6);
    prop_oneof![3 => honest, 1 => odd]
}

fn row_op() -> impl Strategy<Value = RowOp> {
    let r = 0u64..14;
    // `Run(997, …)` leaves `nextPos` at 998, so a following `Add` lands rows
    // on position 999 — `SwapRows`' other end — without a thousand rows.
    let n = prop_oneof![Just(0i64), Just(3), Just(997), Just(1000), Just(-5), Just(i64::MAX)];
    prop_oneof![
        3 => (n, labels()).prop_map(|(n, l)| RowOp::Run(n, l)),
        3 => labels().prop_map(RowOp::Add),
        2 => Just(RowOp::Update),
        1 => Just(RowOp::Clear),
        2 => Just(RowOp::SwapRows),
        2 => r.clone().prop_map(RowOp::Select),
        2 => r.prop_map(RowOp::Delete),
    ]
}

#[derive(Clone, Debug, PartialEq)]
struct Row {
    id: u64,
    num: i64,
    label: String,
    pos: i64,
    selected: bool,
}

struct RowModel {
    rows: Vec<Row>,
    next: u64,
    next_id: i64,
    next_pos: i64,
}

impl RowModel {
    /// `new Row from labels as (i, label) { … }`: one row per tuple at
    /// positive weight, minted in key order.
    fn insert(&mut self, labels: &[(i64, String, i64)], num: impl Fn(i64) -> i64, pos: impl Fn(i64) -> i64) {
        let mut live: Vec<_> = labels.iter().filter(|(_, _, w)| *w > 0).collect();
        live.sort_by_key(|(k, _, _)| *k);
        for (i, label, _) in live {
            self.rows.push(Row { id: self.next, num: num(*i), label: label.clone(), pos: pos(*i), selected: false });
            self.next += 1;
        }
    }

    fn apply(&mut self, op: &RowOp) {
        // Every read below is of the state before this event.
        let (next_id, next_pos) = (self.next_id, self.next_pos);
        match op {
            RowOp::Run(n, labels) => {
                self.rows.clear();
                self.insert(labels, |i| next_id.wrapping_add(i), |i| i.wrapping_add(1));
                self.next_id = next_id.wrapping_add(*n);
                self.next_pos = n.wrapping_add(1);
            }
            RowOp::Add(labels) => {
                self.insert(labels, |i| next_id.wrapping_add(i), |i| next_pos.wrapping_add(i));
                self.next_id = next_id.wrapping_add(1000);
                self.next_pos = next_pos.wrapping_add(1000);
            }
            RowOp::Update => {
                for r in self.rows.iter_mut().filter(|r| r.num.wrapping_rem(10) == 1) {
                    r.label.push_str(" !!!");
                }
            }
            RowOp::Clear => self.rows.clear(),
            RowOp::SwapRows => {
                for r in &mut self.rows {
                    r.pos = match r.pos {
                        2 => 999,
                        999 => 2,
                        p => p,
                    };
                }
            }
            // Deselect whatever was selected, then select `r` — if it is
            // still there; a deleted row stays deleted.
            RowOp::Select(id) => self.rows.iter_mut().for_each(|r| r.selected = r.id == *id),
            RowOp::Delete(id) => self.rows.retain(|r| r.id != *id),
        }
    }
}

fn rel_arg(labels: &[(i64, String, i64)]) -> ArgValue {
    ArgValue::Rel(labels.iter().map(|(k, l, w)| (Value::Int(*k), Value::text(l), *w)).collect())
}

fn row_dispatch(app: &mut App, op: &RowOp) -> Result<(), String> {
    let one = |k: &str, v: ArgValue| HashMap::from([(k.to_string(), v)]);
    let r = match op {
        RowOp::Run(n, labels) => {
            let mut args = one("labels", rel_arg(labels));
            args.insert("n".into(), ArgValue::Value(Value::Int(*n)));
            app.dispatch_args("Run", &args)
        }
        RowOp::Add(labels) => app.dispatch_args("Add", &one("labels", rel_arg(labels))),
        RowOp::Update => app.dispatch("Update", &[]),
        RowOp::Clear => app.dispatch("Clear", &[]),
        RowOp::SwapRows => app.dispatch("SwapRows", &[]),
        RowOp::Select(n) => app.dispatch("Select", &[("r", id(app, "Row", *n))]),
        RowOp::Delete(n) => app.dispatch("Delete", &[("r", id(app, "Row", *n))]),
    };
    r.map(|_| ())
}

fn row_agrees(app: &App, m: &RowModel) -> Result<(), String> {
    let col = |f: fn(&Row) -> Value| m.rows.iter().map(|r| (r.id, f(r))).collect::<Vec<_>>();
    let state = |name: &str| app.field("State#", name).iter().map(|(_, v)| v.clone()).collect::<Vec<_>>();
    let checks: [(&str, bool); 7] = [
        ("ids", app.ids("Row").iter().map(seq).collect::<Vec<_>>() == m.rows.iter().map(|r| r.id).collect::<Vec<_>>()),
        ("num", column(app, "Row", "num") == col(|r| Value::Int(r.num))),
        ("label", column(app, "Row", "label") == col(|r| Value::text(&r.label))),
        ("pos", column(app, "Row", "pos") == col(|r| Value::Int(r.pos))),
        ("selected", column(app, "Row", "selected") == col(|r| bool_atom(r.selected))),
        ("nextId", state("nextId") == [Value::Int(m.next_id)]),
        ("nextPos", state("nextPos") == [Value::Int(m.next_pos)]),
    ];
    match checks.iter().find(|(_, ok)| !ok) {
        Some((what, _)) => {
            let shown: Vec<_> = m.rows.iter().take(6).collect();
            Err(format!("`{what}` disagrees with the model ({} rows, first {shown:?}; nextId {}, nextPos {})", m.rows.len(), m.next_id, m.next_pos))
        }
        None => Ok(()),
    }
}

// --- Kanban ------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum CardOp {
    Add(u64, String),
    Move(u64, u64, String),
    Rename(u64, String),
    Delete(u64),
}

fn card_op() -> impl Strategy<Value = CardOp> {
    // Lists 0..3 exist; 3..5 never do. Nothing stops a card naming one.
    let (card, list) = (0u64..10, 0u64..5);
    prop_oneof![
        3 => (list.clone(), text()).prop_map(|(l, p)| CardOp::Add(l, p)),
        3 => (card.clone(), list, text()).prop_map(|(c, l, p)| CardOp::Move(c, l, p)),
        2 => (card.clone(), text()).prop_map(|(c, t)| CardOp::Rename(c, t)),
        2 => card.prop_map(CardOp::Delete),
    ]
}

/// `(id, title, pos, list)`.
type Card = (u64, String, String, u64);

fn card_apply(cards: &mut Vec<Card>, next: &mut u64, op: &CardOp) {
    match op {
        CardOp::Add(list, pos) => {
            cards.push((*next, "New card".into(), pos.clone(), *list));
            *next += 1;
        }
        CardOp::Move(c, list, pos) => {
            if let Some(card) = cards.iter_mut().find(|k| k.0 == *c) {
                card.2 = pos.clone();
                card.3 = *list;
            }
        }
        CardOp::Rename(c, title) => {
            if let Some(card) = cards.iter_mut().find(|k| k.0 == *c) {
                card.1 = title.clone();
            }
        }
        CardOp::Delete(c) => cards.retain(|k| k.0 != *c),
    }
}

fn card_dispatch(app: &mut App, op: &CardOp) -> Result<(), String> {
    let (card, list) = (|a: &App, n| id(a, "Card", n), |a: &App, n| id(a, "List", n));
    let r = match op {
        CardOp::Add(l, p) => app.dispatch("AddCard", &[("list", list(app, *l)), ("pos", Value::text(p))]),
        CardOp::Move(c, l, p) => {
            app.dispatch("MoveCard", &[("card", card(app, *c)), ("list", list(app, *l)), ("pos", Value::text(p))])
        }
        CardOp::Rename(c, t) => app.dispatch("RenameCard", &[("card", card(app, *c)), ("title", Value::text(t))]),
        CardOp::Delete(c) => app.dispatch("DeleteCard", &[("card", card(app, *c))]),
    };
    r.map(|_| ())
}

// --- a stockroom: handlers the examples don't have -----------------------------------
//
// Arg-dependent `where` targets (a scan at dispatch, not a maintained view),
// per-row values that read the row being written, comparisons across `Int`
// and `Money`, and several statements in one handler — where every read is of
// the state before the event and writes to one cell compose, last one wins.

const STOCK: &str = r#"
type Kind = Food | Tool | Toy
entity Item { name: Text, qty: Int, price: Money, kind: Kind, flagged: Bool }
state budget : Int = 10

event Stock(name: Text, qty: Int, price: Money, kind: Kind)
event Restock(k: Kind, n: Int)
event Purge(limit: Int)
event Flag(name: Text)
event Pricey(limit: Int)
event Suffix(k: Kind, suffix: Text)
event Sell(i: Item, n: Int)
event Cull(k: Kind, limit: Int)
event Twice(k: Kind)
event Double(limit: Int)

on Stock(name, qty, price, kind) => new Item { name: name, qty: qty, price: price, kind: kind, flagged: False }
on Restock(k, n)     => update Item where .kind = k { qty: .qty + n }
on Purge(limit)      => delete Item where .qty <= limit
on Flag(name)        => update Item where .name = name { flagged: not .flagged }
on Pricey(limit)     => update Item where .price > limit { flagged: True }
on Suffix(k, suffix) => update Item where .kind = k { name: .name ++ suffix }
on Sell(i, n) {
  i.qty := i.qty - n
  set budget = budget + n
}
on Cull(k, limit) {
  delete Item where .kind = k
  update Item where .qty > limit { flagged: True }
}
on Twice(k) {
  update Item where .kind = k { qty: .qty + 1 }
  update Item where .kind = k { qty: .qty + 10 }
}
on Double(limit) => update Item where .qty < limit { price: .price + .price, qty: .qty - 1 }

let flagged_n : Unit -> Int = count((Item where .flagged) by unit)
"#;

const KINDS: [&str; 3] = ["Food", "Tool", "Toy"];

#[derive(Clone, Debug)]
enum StockOp {
    Stock(String, i64, i64, usize),
    Restock(usize, i64),
    Purge(i64),
    Flag(String),
    Pricey(i64),
    Suffix(usize, String),
    Sell(u64, i64),
    Cull(usize, i64),
    Twice(usize),
    Double(i64),
}

fn stock_op() -> impl Strategy<Value = StockOp> {
    let n = || prop_oneof![4 => -3i64..8, 1 => Just(i64::MAX), 1 => Just(i64::MIN)];
    let k = || 0usize..3;
    // Few distinct names, so `where .name = name` hits several rows.
    let name = || prop_oneof![Just("a".to_string()), Just("b".to_string()), Just(String::new()), text()];
    prop_oneof![
        5 => (name(), n(), -200i64..900, k()).prop_map(|(s, q, p, k)| StockOp::Stock(s, q, p, k)),
        2 => (k(), n()).prop_map(|(k, n)| StockOp::Restock(k, n)),
        2 => n().prop_map(StockOp::Purge),
        2 => name().prop_map(StockOp::Flag),
        2 => n().prop_map(StockOp::Pricey),
        1 => (k(), text()).prop_map(|(k, s)| StockOp::Suffix(k, s)),
        2 => (0u64..10, n()).prop_map(|(i, n)| StockOp::Sell(i, n)),
        2 => (k(), n()).prop_map(|(k, n)| StockOp::Cull(k, n)),
        1 => k().prop_map(StockOp::Twice),
        2 => n().prop_map(StockOp::Double),
    ]
}

#[derive(Clone, Debug)]
struct Item {
    id: u64,
    name: String,
    qty: i64,
    /// Cents.
    price: i64,
    kind: usize,
    flagged: bool,
}

#[derive(Default)]
struct StockModel {
    items: Vec<Item>,
    next: u64,
    budget: i64,
}

impl StockModel {
    /// Apply `op`; `false` if the app must refuse it.
    fn apply(&mut self, op: &StockOp) -> bool {
        match op {
            StockOp::Stock(name, qty, price, kind) => {
                self.items.push(Item { id: self.next, name: name.clone(), qty: *qty, price: *price, kind: *kind, flagged: false });
                self.next += 1;
            }
            StockOp::Restock(k, n) => self.each(|i| i.kind == *k, |i| i.qty = i.qty.wrapping_add(*n)),
            StockOp::Purge(limit) => self.items.retain(|i| i.qty > *limit),
            StockOp::Flag(name) => self.each(|i| i.name == *name, |i| i.flagged = !i.flagged),
            // `Money > Int`: the Int is whole units, compared exactly.
            StockOp::Pricey(limit) => self.each(|i| (i.price as i128) > (*limit as i128) * 100, |i| i.flagged = true),
            StockOp::Suffix(k, suffix) => self.each(|i| i.kind == *k, |i| i.name.push_str(suffix)),
            // Reads `i.qty`: no such row, no event — and no budget change.
            StockOp::Sell(id, n) => match self.items.iter_mut().find(|i| i.id == *id) {
                Some(item) => {
                    item.qty = item.qty.wrapping_sub(*n);
                    self.budget = self.budget.wrapping_add(*n);
                }
                None => return false,
            },
            // The `update` picks its rows from the pre-event state, but a row
            // the same event deleted stays deleted.
            StockOp::Cull(k, limit) => {
                self.items.retain(|i| i.kind != *k);
                self.each(|i| i.qty > *limit, |i| i.flagged = true);
            }
            // Both statements read the pre-event `qty`; the second write wins.
            StockOp::Twice(k) => self.each(|i| i.kind == *k, |i| i.qty = i.qty.wrapping_add(10)),
            StockOp::Double(limit) => self.each(
                |i| i.qty < *limit,
                |i| {
                    i.price = i.price.wrapping_add(i.price);
                    i.qty = i.qty.wrapping_sub(1);
                },
            ),
        }
        true
    }

    fn each(&mut self, pick: impl Fn(&Item) -> bool, change: impl Fn(&mut Item)) {
        self.items.iter_mut().filter(|i| pick(i)).for_each(change);
    }
}

fn stock_dispatch(app: &mut App, op: &StockOp) -> Result<(), String> {
    let kind = |k: &usize| Value::atom(KINDS[*k]);
    let r = match op {
        StockOp::Stock(name, qty, price, k) => app.dispatch(
            "Stock",
            &[("name", Value::text(name)), ("qty", Value::Int(*qty)), ("price", Value::Money(*price)), ("kind", kind(k))],
        ),
        StockOp::Restock(k, n) => app.dispatch("Restock", &[("k", kind(k)), ("n", Value::Int(*n))]),
        StockOp::Purge(limit) => app.dispatch("Purge", &[("limit", Value::Int(*limit))]),
        StockOp::Flag(name) => app.dispatch("Flag", &[("name", Value::text(name))]),
        StockOp::Pricey(limit) => app.dispatch("Pricey", &[("limit", Value::Int(*limit))]),
        StockOp::Suffix(k, s) => app.dispatch("Suffix", &[("k", kind(k)), ("suffix", Value::text(s))]),
        StockOp::Sell(i, n) => app.dispatch("Sell", &[("i", id(app, "Item", *i)), ("n", Value::Int(*n))]),
        StockOp::Cull(k, limit) => app.dispatch("Cull", &[("k", kind(k)), ("limit", Value::Int(*limit))]),
        StockOp::Twice(k) => app.dispatch("Twice", &[("k", kind(k))]),
        StockOp::Double(limit) => app.dispatch("Double", &[("limit", Value::Int(*limit))]),
    };
    r.map(|_| ())
}

fn stock_agrees(app: &App, m: &StockModel) -> Result<(), String> {
    let col = |f: &dyn Fn(&Item) -> Value| m.items.iter().map(|i| (i.id, f(i))).collect::<Vec<_>>();
    let checks: [(&str, bool); 8] = [
        ("ids", app.ids("Item").iter().map(seq).collect::<Vec<_>>() == m.items.iter().map(|i| i.id).collect::<Vec<_>>()),
        ("name", column(app, "Item", "name") == col(&|i| Value::text(&i.name))),
        ("qty", column(app, "Item", "qty") == col(&|i| Value::Int(i.qty))),
        ("price", column(app, "Item", "price") == col(&|i| Value::Money(i.price))),
        ("kind", column(app, "Item", "kind") == col(&|i| Value::atom(KINDS[i.kind]))),
        ("flagged", column(app, "Item", "flagged") == col(&|i| bool_atom(i.flagged))),
        ("budget", app.field("State#", "budget").iter().map(|(_, v)| v.clone()).collect::<Vec<_>>() == [Value::Int(m.budget)]),
        ("flagged_n", scalar(app, "flagged_n") == m.items.iter().filter(|i| i.flagged).count() as i64),
    ];
    match checks.iter().find(|(_, ok)| !ok) {
        Some((what, _)) => Err(format!("`{what}` disagrees with the model: {:?}, budget {}", m.items, m.budget)),
        None => Ok(()),
    }
}

fn fail(e: String) -> TestCaseError {
    TestCaseError::fail(e)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: common::cases(128), ..ProptestConfig::default() })]

    #[test]
    fn todomvc_matches_its_model(ops in prop::collection::vec(todo_op(), 0..40)) {
        let src = example("todomvc/src/app.rex");
        let mut app = App::build(&src);
        let mut model = TodoModel::default();
        todo_agrees(&app, &model).map_err(fail)?;
        for (i, op) in ops.iter().enumerate() {
            let cursor = app.engine.cursor();
            let accepted = model.apply(op);
            let got = todo_dispatch(&mut app, op);
            prop_assert_eq!(got.is_ok(), accepted, "step {} {:?}: {:?}", i, op, got);
            prop_assert_eq!(app.engine.cursor(), cursor + accepted as u64, "step {}: log", i);
            todo_agrees(&app, &model).map_err(|e| fail(format!("after step {i} {op:?}: {e}")))?;
            check_base_invariant(&app.engine).map_err(fail)?;
        }
        check_views(&app, "at the end").map_err(fail)?;
        check_replay(&src, &app).map_err(fail)?;
    }

    #[test]
    fn benchmark_matches_its_model(ops in prop::collection::vec(row_op(), 0..16)) {
        let src = example("js-framework-benchmark/src/app.rex");
        let mut app = App::build(&src);
        let mut model = RowModel { rows: Vec::new(), next: 0, next_id: 1, next_pos: 1 };
        row_agrees(&app, &model).map_err(fail)?;
        for (i, op) in ops.iter().enumerate() {
            model.apply(op);
            row_dispatch(&mut app, op).map_err(|e| fail(format!("step {i} {op:?} was refused: {e}")))?;
            row_agrees(&app, &model).map_err(|e| fail(format!("after step {i}: {e}")))?;
            check_base_invariant(&app.engine).map_err(fail)?;
        }
        check_views(&app, "at the end").map_err(fail)?;
        check_replay(&src, &app).map_err(fail)?;
    }

    #[test]
    fn stockroom_matches_its_model(ops in prop::collection::vec(stock_op(), 0..40)) {
        let mut app = App::build(STOCK);
        let mut model = StockModel { budget: 10, ..StockModel::default() };
        stock_agrees(&app, &model).map_err(fail)?;
        for (i, op) in ops.iter().enumerate() {
            let cursor = app.engine.cursor();
            let accepted = model.apply(op);
            let got = stock_dispatch(&mut app, op);
            prop_assert_eq!(got.is_ok(), accepted, "step {} {:?}: {:?}", i, op, got);
            prop_assert_eq!(app.engine.cursor(), cursor + accepted as u64, "step {}: log", i);
            stock_agrees(&app, &model).map_err(|e| fail(format!("after step {i} {op:?}: {e}")))?;
            check_base_invariant(&app.engine).map_err(fail)?;
        }
        check_views(&app, "at the end").map_err(fail)?;
        check_replay(STOCK, &app).map_err(fail)?;
    }

    #[test]
    fn kanban_matches_its_model(ops in prop::collection::vec(card_op(), 0..40)) {
        let src = example("kanban/src/board.rex");
        let mut app = App::build(&src);
        let mut cards: Vec<Card> = vec![
            (0, "Design the schema".into(), "a0".into(), 0),
            (1, "Lower to circuits".into(), "a1".into(), 0),
            (2, "Ship the shaper".into(), "a0".into(), 1),
        ];
        let mut next = 3;
        for (i, op) in ops.iter().enumerate() {
            card_apply(&mut cards, &mut next, op);
            card_dispatch(&mut app, op).map_err(|e| fail(format!("step {i} {op:?} was refused: {e}")))?;
            let list = |n: u64| id(&app, "List", n);
            let want = |f: &dyn Fn(&Card) -> Value| cards.iter().map(|c| (c.0, f(c))).collect::<Vec<_>>();
            prop_assert_eq!(column(&app, "Card", "title"), want(&|c| Value::text(&c.1)), "titles after step {}", i);
            prop_assert_eq!(column(&app, "Card", "pos"), want(&|c| Value::text(&c.2)), "pos after step {}", i);
            prop_assert_eq!(column(&app, "Card", "list"), want(&|c| list(c.3)), "list after step {}", i);
            // The card level's membership: every card, under the list it names.
            let members: Vec<_> = app.view("main#list#card").into_iter().map(|(c, l, _)| (seq(&c), l)).collect();
            prop_assert_eq!(members, want(&|c| list(c.3)), "membership after step {}", i);
            check_base_invariant(&app.engine).map_err(fail)?;
        }
        check_views(&app, "at the end").map_err(fail)?;
        check_replay(&src, &app).map_err(fail)?;
    }
}

/// The benchmark's own shape: a thousand rows, then swap positions 2 and 999.
#[test]
fn swap_rows_exchanges_exactly_two_of_a_thousand() {
    let src = example("js-framework-benchmark/src/app.rex");
    let mut app = App::build(&src);
    let mut model = RowModel { rows: Vec::new(), next: 0, next_id: 1, next_pos: 1 };
    let labels: Vec<_> = (0..1000).map(|i| (i as i64, format!("row {i}"), 1)).collect();
    for op in [RowOp::Run(1000, labels), RowOp::SwapRows, RowOp::Update, RowOp::SwapRows, RowOp::Select(998), RowOp::Delete(1)] {
        model.apply(&op);
        let (_, step) = match &op {
            RowOp::SwapRows => app.dispatch("SwapRows", &[]).unwrap(),
            _ => {
                row_dispatch(&mut app, &op).unwrap();
                continue;
            }
        };
        // A swap is two rows changing one field: a −/+ pair each, nothing else.
        let order = &step.view_deltas["main#unit#row#order"];
        assert_eq!(order.len(), 4, "{:?}", order.to_sorted_vec());
        row_agrees(&app, &model).unwrap();
    }
    row_agrees(&app, &model).unwrap();
    check_views(&app, "at the end").unwrap();
}
