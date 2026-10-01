//! Generated listeners and level anchors: names a program chooses must not
//! collide with the emitter's own, and a level needs to know what follows it.

const HEAD: &str = r#"
entity Todo { text: Text }
event Edit(t: Todo, text: Text)
on Edit(t, text) => t.text := text
let total : Unit -> Int = count(Todo by unit)
"#;

fn generate(view: &str) -> String {
    rex_codegen::generate(&format!("{HEAD}{view}"), "./app.rex?raw").expect("clean codegen")
}

#[test]
fn handler_params_cannot_shadow_the_listeners_own_names() {
    // `key` is the row key and `ev` the DOM event inside every listener.
    let out = generate(
        r#"view main = ul { Todo as t select li {
             input(value=.text on change(key = value) => do Edit(t, key))
             input(value=.text on change(ev = value) => do Edit(t, ev))
           } }"#,
    );
    assert!(out.contains("const p_key = encodeText((ev.currentTarget"), "{out}");
    assert!(out.contains("const p_ev = encodeText((ev.currentTarget"), "{out}");
    assert!(out.contains("\"text\": p_key"), "{out}");
    assert!(out.contains("\"text\": p_ev"), "{out}");
    assert!(!out.contains("const key ="), "{out}");
    assert!(!out.contains("const ev ="), "{out}");
    // The row's own key still reaches the dispatch.
    assert!(out.contains("\"t\": key"), "{out}");
}

#[test]
fn a_level_followed_by_a_static_sibling_gets_an_anchor() {
    let out = generate(r#"view main = div { if (total > 0) { p "gated" } span "after" }"#);
    assert!(
        out.contains("anchor: (root) => ((root.childNodes[0] as HTMLElement) as HTMLElement | undefined) ?? null"),
        "{out}"
    );
}

#[test]
fn a_level_that_is_last_in_its_element_has_none() {
    let out = generate(r#"view main = div { span "before" if (total > 0) { p "gated" } }"#);
    assert!(!out.contains("anchor:"), "{out}");
}

#[test]
fn levels_sharing_an_element_share_a_slot_key() {
    let out = generate(
        r#"view main = div { section { if (total > 0) { p "a" } if (total > 1) { p "b" } } }"#,
    );
    assert_eq!(out.matches("slotKey: \"0\"").count(), 2, "{out}");
}
