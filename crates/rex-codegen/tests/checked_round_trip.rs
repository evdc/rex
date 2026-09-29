//! The two halves of a checkbox must agree on how a `Bool` is spelled.
//!
//! `checked=` reads a value back out of a view and `checked` in an argument
//! list puts one in, so both have to name the same atoms (`True`/`False`).
//! When they drifted apart, clicking a toggle stored an atom the bind then
//! failed to match, and the box visually un-checked itself.

const SRC: &str = r#"
entity Todo { text: Text, completed: Bool }
event ToggleAll(done: Bool)
on ToggleAll(done) => update Todo { completed: done }
view board =
  Todo select
    li {
      input(type="checkbox" checked=.completed
        on change(done = checked) => do ToggleAll(done))
    }
"#;

#[test]
fn the_checked_extractor_and_the_checked_bind_spell_the_same_atoms() {
    let out = rex_codegen::generate(SRC, "./board.rex?raw").expect("clean codegen");
    assert!(
        out.contains(r#".checked ? encodeAtom("True") : encodeAtom("False")"#),
        "{out}"
    );
    assert!(out.contains(r#".checked = (v === encodeAtom("True"))"#), "{out}");
    // The lowercase spelling matched neither the bind nor `where .completed`.
    assert!(!out.contains(r#"encodeAtom("true")"#), "{out}");
}
