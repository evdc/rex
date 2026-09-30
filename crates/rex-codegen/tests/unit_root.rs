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

// S-90: the TodoMVC gaps found by running it in a browser.

fn todomvc() -> String {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/todomvc/src/app.rex")).unwrap();
    rex_codegen::generate(&src, "./app.rex?raw").expect("todomvc generates cleanly")
}

#[test]
fn a_select_mounts_into_the_element_it_was_written_in() {
    let out = todomvc();
    // `visible as t …` sits in `ul.todo-list`, child 2 of `section.main`.
    assert!(out.contains("slot: (root) => (root.childNodes[2] as HTMLElement)"), "{out}");
    let flat = rex_codegen::generate(SRC, "./app.rex?raw").unwrap();
    assert!(flat.contains("slot: (root) => (root.childNodes[1] as HTMLElement)"), "the `ul` in SRC: {flat}");
}

#[test]
fn key_selectors_filter_the_listener() {
    let out = todomvc();
    assert!(out.contains(r#"if (!["Enter"].includes((ev as KeyboardEvent).key)) return;"#), "{out}");
    assert!(out.contains(r#"["Escape"]"#), "{out}");
}

#[test]
fn autofocus_is_an_attribute_not_a_class() {
    let out = todomvc();
    assert!(out.contains(r#"setAttribute("autofocus", "")"#), "{out}");
    assert!(!out.contains(r#"classList.add("autofocus")"#), "{out}");
}

#[test]
fn a_comparison_bound_to_checked_is_a_presence_flag() {
    let out = todomvc();
    assert!(out.contains(".checked = v !== undefined;"), "{out}");
}

#[test]
fn revert_restores_the_bound_value() {
    let out = todomvc();
    assert!(out.contains("_i.value = _i.defaultValue;"), "{out}");
    assert!(out.contains("_i.defaultValue = _s;"), "{out}");
}

// S-91: `import js` extractors.

#[test]
fn a_js_extractor_imports_its_module_and_encodes_the_array_as_a_relation() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/js-framework-benchmark/src/app.rex")).unwrap();
    let out = rex_codegen::generate(&src, "./app.rex?raw").expect("the benchmark generates cleanly");
    assert!(out.contains(r#"import * as utils from "./utils.js";"#), "{out}");
    assert!(out.contains("encodeRel(utils.randomLabels(10000), encodeText)"), "{out}");
    // Both dispatch args are sent as-is: the relation is a JSON array.
    assert!(out.contains("args: Record<string, unknown>"), "{out}");
}
