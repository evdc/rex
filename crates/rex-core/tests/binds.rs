//! S-60: a bind may be any relation co-keyed with the level's row — an
//! aggregate, a join through a foreign key, or a binder used as a value.

use rex::types::shape_ir::{BindKind, Encoding, ShapeLevel};

const ENTITIES: &str = r#"
entity User { name: Text }
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: List, owner: User }
state current : User
"#;

fn diagnostics(view: &str) -> Vec<rex::Diagnostic> {
    let src = format!("{ENTITIES}\n{view}");
    let parsed = rex::parse(&src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    rex::check(&parsed.program).diagnostics
}

fn checked(view: &str) -> rex::types::check::CheckResult {
    let src = format!("{ENTITIES}\n{view}");
    let parsed = rex::parse(&src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let c = rex::check(&parsed.program);
    assert!(c.diagnostics.is_empty(), "check: {:?}", c.diagnostics);
    c
}

fn text_binds(l: &ShapeLevel) -> Vec<Encoding> {
    l.attrs.iter().filter(|a| a.kind == BindKind::Text).map(|a| a.encoding).collect()
}

#[test]
fn an_aggregate_can_be_written_inline_as_a_bind() {
    let c = checked("view board = List as l select section { h2 { .title } span { (count(Card by .list)) } }");
    // Typed from the elaborated view: an `Int`, not the desugarer's `Text` guess.
    assert_eq!(text_binds(&c.shapes.views[0]), vec![Encoding::Text, Encoding::Int]);
}

#[test]
fn a_join_through_a_foreign_key_is_a_bind() {
    let c = checked("view board = Card as c select li { .owner.name }");
    assert_eq!(text_binds(&c.shapes.views[0]), vec![Encoding::Text]);
}

#[test]
fn a_binder_is_a_value_in_a_bind() {
    // `c.owner.name` is the level's own row, `l.title` an enclosing level's.
    let c = checked(
        r#"
view board =
  List as l select section {
    Card as k where .list = l select li { k.owner.name " in " l.title }
  }
"#,
    );
    let card = &c.shapes.views[0].children[0];
    assert_eq!(text_binds(card), vec![Encoding::Text, Encoding::Text]);
}

#[test]
fn a_binder_compares_in_a_gate() {
    checked("view board = User as u select button(class.selected=(u = current)) { .name }");
}

#[test]
fn a_binder_reaches_two_levels_up() {
    checked(
        r#"
view board =
  List as l select section {
    Card as k where .list = l select li {
      div { .title l.title }
    }
  }
"#,
    );
}

#[test]
fn a_bind_on_the_wrong_domain_says_which_row_it_needed() {
    // `count(Card by .list)` is keyed by List; a Card level cannot bind it.
    let ds = diagnostics("view board = Card as c select li { (count(Card by .list)) }");
    let msg = &ds.iter().find(|d| d.message.contains("co-keyed")).expect("a co-keying error").message;
    assert!(msg.contains("`c : Card`"), "{msg}");
}

#[test]
fn a_bind_error_points_at_the_bind() {
    let src = format!("{ENTITIES}\nview board = Card as c select li {{ (count(Card by .list)) }}");
    let parsed = rex::parse(&src);
    let ds = rex::check(&parsed.program).diagnostics;
    let d = ds.iter().find(|d| d.message.contains("co-keyed")).unwrap();
    let at = &src[d.span.start..d.span.end];
    assert!(at.contains("count"), "error points at `{at}`");
}
