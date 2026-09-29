//! S-61: a component call expands inline, so the generated TS is identical to
//! the hand-inlined view's — components add nothing at runtime.

const ENTITIES: &str = r#"
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: List }
event Rename(c: Card, title: Text)
on Rename(c, title) => c.title := title
"#;

const INLINED: &str = r#"
view board =
  List as l order by .pos select
    section {
      h2 { .title }
      Card as c where .list = l order by .pos select
        li(on click => do Rename(c, "x")) { .title }
    }
"#;

const COMPONENTS: &str = r#"
view CardItem(k: Card) = li(on click => do Rename(k, "x")) { .title }
view ListBox(list: List) = section { h2 { .title } children }
view board =
  List as l order by .pos select ListBox(l) {
    Card as c where .list = l order by .pos select CardItem(c)
  }
"#;

#[test]
fn generated_code_is_identical_to_the_inlined_version() {
    let render = |v: &str| rex_codegen::generate(&format!("{ENTITIES}{v}"), "./app.rex?raw").expect("clean codegen");
    assert_eq!(render(INLINED), render(COMPONENTS));
}
