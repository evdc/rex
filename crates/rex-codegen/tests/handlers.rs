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

#[test]
fn a_bind_beside_other_children_gets_its_own_text_node() {
    // `td { .text ":" }`: setting the cell's textContent would wipe the ":".
    let out = generate(r#"view main = ul { Todo as t select li { .text ": " b { .text } " end" } }"#);
    // The skeleton holds a placeholder where the bind goes, in order.
    let build = &out[out.find("function build_shape_main_unit_todo").unwrap()..];
    let order: Vec<usize> = [r#"createTextNode("")"#, r#"createTextNode(": ")"#, r#"createElement("b")"#, r#"createTextNode(" end")"#]
        .iter()
        .map(|needle| build.find(needle).unwrap_or_else(|| panic!("no {needle} in:\n{build}")))
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "children out of order: {order:?}\n{build}");
    // The bind writes that node (child 0), not the element.
    assert!(out.contains("((el.childNodes[0] as HTMLElement)).textContent = String(decodeText(v));"), "{out}");
    // An only child still binds the element itself: no placeholder, no hop.
    assert!(out.contains("((el.childNodes[2] as HTMLElement)).textContent = String(decodeText(v));"), "{out}");
    assert_eq!(out.matches(r#"createTextNode("")"#).count(), 1, "{out}");
}

#[test]
fn a_refused_event_ends_the_listener_without_throwing() {
    let out = generate(
        r#"view main = ul { Todo as t select li {
             input(value=.text on change(v = value) { do Edit(t, v); do Edit(t, v); clear })
           } }"#,
    );
    // `dispatch` reports a refusal as null instead of letting it escape…
    assert!(out.contains("): readonly string[] | null {"), "{out}");
    assert!(out.contains("if (res.rejected !== undefined) {") && out.contains("return null;"), "{out}");
    // …and each `do` stops the handler on one, before a later `do` or `clear`.
    assert_eq!(out.matches("if (_r === null) return;").count(), 2, "{out}");
    let listener = &out[out.find("addEventListener(\"change\"").unwrap()..];
    assert!(listener.find("if (_r === null) return;").unwrap() < listener.find(".value = \"\"").unwrap());
}

#[test]
fn an_enclosing_binder_argument_is_typed_as_present() {
    let out = rex_codegen::generate(
        "entity Todo { text: Text }\nentity Sub { todo: Todo, s: Text }\nevent Tap(t: Todo, s: Sub)\n\
         view main = ul { Todo as t select li { Sub as s where .todo = t select b(on click => do Tap(t, s)) { .s } } }\n",
        "./app.rex?raw",
    )
    .expect("clean codegen");
    assert!(out.contains("\"t\": ancestors[0]!, \"s\": key"), "{out}");
}
