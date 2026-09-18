//! M5.b gate: a `view` desugars to auto-derived membership/order/attribute
//! `let`s whose integrated contents match the hand-written 6NF `let`s the
//! Kanban app used before (the manual `board.rex`), and its ShapeIR/HandlerIR
//! carry the roles a compiler needs.

use rex::dbsp::Engine;
use rex::eval::relation::BTreeRelation;
use rex::types::shape_ir::{BindKind, MutationIR, Tpl};
use rex::types::typed::TProgram;
use std::collections::HashMap;

const SEED: &str = r#"
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: ListID }

let l_todo  = new List { title: "Todo",  pos: "a0" }
let l_doing = new List { title: "Doing", pos: "a1" }

let c1 = new Card { title: "Design", pos: "a0", list: l_todo }
let c2 = new Card { title: "Lower",  pos: "a1", list: l_todo }
let c3 = new Card { title: "Ship",   pos: "a0", list: l_doing }
"#;

/// The hand-written 6NF views the surface must reproduce.
const MANUAL: &str = r#"
let lists      : List -> ListID = id
let list_pos   : List -> Text = .pos
let list_title : List -> Text = .title
let card_list  : Card -> ListID = .list
let card_pos   : Card -> Text = .pos
let card_title : Card -> Text = .title
"#;

const VIEW: &str = r#"
view board =
  List as l order by .pos select
    section(class="list" dropTarget
      on drop(card = drag(Card), pos = dropPos(c, card)) { update card { list: l, pos: pos } }) {
      header {
        span { .title }
        button(on click(pos = endOf(c)) => new Card { title: "New card", pos: pos, list: l }) "+ card"
      }
      Card as c where .list = l order by .pos select
        div(class="card" draggable) {
          input(value=.title on change(v = value) => .title := v)
          button(on click => delete c) "x"
        }
    }
"#;

fn elaborate(src: &str) -> TProgram {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "check: {:?}", checked.diagnostics);
    checked.elaborated.expect("clean check")
}

fn engine_for(src: &str) -> Engine {
    let prog = elaborate(src);
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &prog.stmts {
        engine.apply_typed_stmt(stmt, &mut values);
    }
    engine
}

#[test]
fn generated_views_match_manual_6nf() {
    let manual = engine_for(&format!("{SEED}{MANUAL}"));
    let view = engine_for(&format!("{SEED}{VIEW}"));

    let pairs = [
        ("lists", "board#list"),
        ("list_pos", "board#list#order"),
        ("list_title", "board#list#title"),
        ("card_list", "board#list#card"),
        ("card_pos", "board#list#card#order"),
        ("card_title", "board#list#card#title"),
    ];
    for (m, g) in pairs {
        let empty = BTreeRelation::new();
        let man = manual.circuit.view(m).unwrap_or(&empty);
        let generated = view.circuit.view(g).unwrap_or(&empty);
        assert_eq!(man, generated, "generated `{g}` != manual `{m}`");
    }
}

/// The schema surface supports both the field form (`list: List`, no `ID`
/// suffix) and a named `rel CardList(Card, List)`, and row-binder aliases
/// (`List as l ... Card where … = l`). All three must produce the same
/// membership/order/attr views as the canonical field form.
#[test]
fn rel_and_alias_forms_match_field_form() {
    // Field form, no `ID` suffix, alias binders.
    let field = engine_for(
        r#"
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: List }
let l1 = new List { title: "A", pos: "a0" }
let k1 = new Card { title: "x", pos: "a0", list: l1 }
let k2 = new Card { title: "y", pos: "a1", list: l1 }
view board =
  List as l order by .pos select
    section(class="list") {
      header { span { .title } }
      Card as c where .list = l order by .pos select
        div(class="card") { input(value=.title) }
    }
"#,
    );
    // `rel` form: the relation is declared separately and referenced by name.
    let rel = engine_for(
        r#"
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text }
rel CardList(Card, List)
let l1 = new List { title: "A", pos: "a0" }
let k1 = new Card { title: "x", pos: "a0", CardList: l1 }
let k2 = new Card { title: "y", pos: "a1", CardList: l1 }
view board =
  List as l order by .pos select
    section(class="list") {
      header { span { .title } }
      Card as c where CardList = l order by .pos select
        div(class="card") { input(value=.title) }
    }
"#,
    );
    for g in ["board#list", "board#list#card", "board#list#card#order"] {
        let empty = BTreeRelation::new();
        let f = field.circuit.view(g).unwrap_or(&empty);
        let r = rel.circuit.view(g).unwrap_or(&empty);
        assert_eq!(f, r, "rel form `{g}` != field form");
    }
}

#[test]
fn shape_ir_roles() {
    let parsed = rex::parse(&format!("{SEED}{VIEW}"));
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let shapes = &checked.shapes;

    assert_eq!(shapes.views.len(), 1);
    let list = &shapes.views[0];
    assert_eq!(list.name, "board#list");
    assert_eq!(list.entity, "List");
    assert_eq!(list.order_view.as_deref(), Some("board#list#order"));
    // list title is a text bind on the header > span.
    assert!(list
        .attrs
        .iter()
        .any(|a| a.view == "board#list#title" && a.kind == BindKind::Text));

    // Nested card level.
    assert_eq!(list.children.len(), 1);
    let card = &list.children[0];
    assert_eq!(card.name, "board#list#card");
    assert_eq!(card.membership_view, "board#list#card");
    // card title binds the input's `value` prop.
    assert!(card
        .attrs
        .iter()
        .any(|a| a.view == "board#list#card#title" && a.kind == BindKind::Prop("value".into())));
    // card has a change handler and a delete handler.
    assert_eq!(card.events.len(), 2);

    // The drop handler on the list is one atomic multi-set on the dragged card.
    let drop = shapes
        .handlers
        .iter()
        .find(|h| h.name.contains("@drop"))
        .expect("drop handler");
    // `update card { list: l, pos: pos }` is ONE set with two field updates.
    assert_eq!(drop.body.len(), 1, "drop is a single update");
    let MutationIR::Set { entity, updates, .. } = &drop.body[0] else {
        panic!("drop body is a Set");
    };
    assert_eq!(entity, "Card");
    assert_eq!(updates.len(), 2, "drop sets list and pos");
}

#[test]
fn template_skeleton() {
    let parsed = rex::parse(&format!("{SEED}{VIEW}"));
    let checked = rex::check(&parsed.program);
    let card = &checked.shapes.views[0].children[0];
    let Tpl::Elem { tag, classes, modifiers, .. } = &card.template else {
        panic!("card template is an element");
    };
    assert_eq!(tag, "div");
    assert_eq!(classes, &["card"]);
    assert_eq!(modifiers, &["draggable"]);
}
