//! S-61: components by inline expansion. A `view Name(p: E) = <element>` is a
//! template; a call renames its params to the caller's binders, so the checked
//! program is the one you would have written out by hand.

const ENTITIES: &str = r#"
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: List }
event Rename(c: Card, title: Text)
event Add(l: List)
on Rename(c, title) => c.title := title
on Add(l) => new Card { title: "new", pos: "z", list: l }
"#;

fn errors(src: &str) -> Vec<String> {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    rex::check(&parsed.program).diagnostics.iter().map(|d| d.message.clone()).collect()
}

fn checked(src: &str) -> rex::types::check::CheckResult {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let c = rex::check(&parsed.program);
    assert!(c.diagnostics.is_empty(), "check: {:?}", c.diagnostics);
    c
}

fn with(view: &str) -> String {
    format!("{ENTITIES}\n{view}")
}

const INLINED: &str = r#"
view board =
  List as l order by .pos select
    section {
      h2 { .title }
      Card as c where .list = l order by .pos select
        li(on click => do Rename(c, "x")) { .title }
      button(on click => do Add(l)) "+"
    }
"#;

const COMPONENTS: &str = r#"
view CardItem(k: Card) = li(on click => do Rename(k, "x")) { .title }
view ListBox(list: List) =
  section {
    h2 { .title }
    children
    button(on click => do Add(list)) "+"
  }
view board =
  List as l order by .pos select ListBox(l) {
    Card as c where .list = l order by .pos select CardItem(c)
  }
"#;

#[test]
fn a_component_expands_to_the_same_shapes_as_the_inlined_view() {
    let a = checked(&with(INLINED));
    let b = checked(&with(COMPONENTS));
    assert_eq!(a.shapes.views, b.shapes.views);
}

#[test]
fn a_param_is_renamed_to_the_callers_binder() {
    // `k` in `CardItem` is `c` at the call site, so the dispatch passes `c`'s
    // row key — the level's own key.
    let b = checked(&with(COMPONENTS));
    let card = &b.shapes.views[0].children[0];
    assert_eq!(card.entity, "Card");
    assert_eq!(card.events[0].dispatches[0].event, "Rename");
}

#[test]
fn a_select_over_a_keyset_let_knows_its_entity() {
    let src = with(
        r#"
let open : Card = Card where .title = "open"
view CardItem(k: Card) = li(on click => do Rename(k, "x")) { .title }
view board = ul { open as c select CardItem(c) }
"#,
    );
    let b = checked(&src);
    assert_eq!(b.shapes.views[0].children[0].entity, "Card");
}

#[test]
fn a_component_may_call_another() {
    let src = with(
        r#"
view Title(k: Card) = span { .title }
view CardItem(k: Card) = li { Title(k) }
view board = ul { Card as c select CardItem(c) }
"#,
    );
    checked(&src);
}

#[test]
fn recursion_between_components_is_an_error() {
    let errs = errors(&with(
        r#"
view A(k: Card) = li { B(k) }
view B(k: Card) = li { A(k) }
view board = ul { Card as c select A(c) }
"#,
    ));
    assert!(errs.iter().any(|e| e.contains("recursive")), "{errs:?}");
}

#[test]
fn a_block_needs_exactly_one_children_slot() {
    let none = errors(&with(
        r#"
view Box(l: List) = section { h2 { .title } }
view board = List as l select Box(l) { h1 "x" }
"#,
    ));
    assert!(none.iter().any(|e| e.contains("no `children` slot")), "{none:?}");
    let two = errors(&with(
        r#"
view Box(l: List) = section { children children }
view board = List as l select Box(l) { h1 "x" }
"#,
    ));
    assert!(two.iter().any(|e| e.contains("more than one `children` slot")), "{two:?}");
}

#[test]
fn arguments_are_checked() {
    let wrong_type = errors(&with(
        r#"
view Box(l: List) = section { h2 { .title } }
view board = Card as c select Box(c)
"#,
    ));
    assert!(wrong_type.iter().any(|e| e.contains("expects a `List`")), "{wrong_type:?}");
    let arity = errors(&with(
        r#"
view Box(l: List) = section { h2 { .title } }
view board = List as l select Box(l, l)
"#,
    ));
    assert!(arity.iter().any(|e| e.contains("takes 1 argument")), "{arity:?}");
    let unbound = errors(&with(
        r#"
view Box(l: List) = section { h2 { .title } }
view board = List as l select Box(nope)
"#,
    ));
    assert!(unbound.iter().any(|e| e.contains("unknown binder `nope`")), "{unbound:?}");
    let missing = errors(&with("view board = ul { Nope(x) }\n"));
    assert!(missing.iter().any(|e| e.contains("unknown component `Nope`")), "{missing:?}");
}

#[test]
fn a_children_slot_may_sit_inside_a_select() {
    // The block lands inside the template's nested level, and still sees the
    // caller's binders.
    checked(&with(
        r#"
view Box(b: List) = div { Card as i where .list = b select p { children } }
view board = List as l select Box(l) { span { l.title } }
"#,
    ));
}

#[test]
fn a_templates_own_binders_cannot_collide_with_the_callers() {
    checked(&with(
        r#"
view Box(l: List) = div { Card as x where .list = l select span { x.title } }
view board = List as x select Box(x)
"#,
    ));
    // A block in the template's level still means the caller's `x`, not the
    // template's.
    checked(&with(
        r#"
view Box(l: List) = div { Card as x where .list = l select p { children } }
view board = List as x select Box(x) { button(on click => do Add(x)) "+" }
"#,
    ));
}

#[test]
fn a_failed_call_is_reported_once() {
    let errs = errors(&with("view board = Card as t select Nope(t)\n"));
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("unknown component `Nope`"), "{errs:?}");
}
