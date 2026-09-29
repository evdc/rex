//! S-53: codegen over a `Unit`-root view — nothing new in the emitter, but it
//! must accept the root level, its `if` gate levels, and their scalar binds.

const SRC: &str = r#"
entity Todo { text: Text }
event Add(text: Text)
on Add(text) => new Todo { text: text }
let total : Unit -> Int = count(Todo by unit)
view main =
  section {
    h1 "todos"
    if (total > 0) {
      footer { span { total } " items" }
    }
    ul { Todo as t select li { .text } }
  }
"#;

#[test]
fn a_unit_root_view_generates_a_level_per_root_and_gate() {
    let out = rex_codegen::generate(SRC, "./app.rex?raw").expect("clean codegen");
    assert!(out.contains("main#unit"), "{out}");
    assert!(out.contains("main#unit#if"), "{out}");
    // The count is an Int bind, decoded as one.
    assert!(out.contains("decodeInt"), "{out}");
}

#[test]
fn order_by_desc_reaches_the_shape_node() {
    let src = r#"
entity Todo { text: Text, n: Int }
view main = ul { Todo as t order by .n desc select li { .text } }
"#;
    let out = rex_codegen::generate(src, "./app.rex?raw").expect("clean codegen");
    assert!(out.contains("orderDesc: true"), "{out}");
    let asc = rex_codegen::generate(&src.replace(" desc", ""), "./app.rex?raw").unwrap();
    assert!(!asc.contains("orderDesc"), "{asc}");
}
