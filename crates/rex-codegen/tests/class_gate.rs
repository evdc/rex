//! S-52 subtask 2: a `class.x=` bind is a *gate*, not a value.
//!
//! Its hidden view is coreflexive on the level's key — present exactly when
//! the class should be on — so the generated code toggles on presence and
//! never decodes a boolean. That is also what lets the gate be a comparison
//! rather than only a `Bool` field, and it is why the binding is marked
//! `presence: true`: the shaper has to apply it when the row *goes away*,
//! which is the "turn the class off" case.

const SRC: &str = r#"
entity Todo { title: Text, completed: Bool, pri: Int }
view board =
  Todo select
    li(class.done=.completed class.urgent=(.pri > 5)) {
      span { .title }
    }
"#;

fn generate() -> String {
    rex_codegen::generate(SRC, "./board.rex?raw").expect("clean codegen")
}

#[test]
fn a_class_bind_toggles_on_presence_not_on_a_decoded_value() {
    let out = generate();
    assert!(out.contains(r#"classList.toggle("done", v !== undefined)"#), "{out}");
    assert!(out.contains(r#"classList.toggle("urgent", v !== undefined)"#), "{out}");
    // No boolean decoding on either gate — the old shape compared the value
    // against an encoded atom, which a coreflexive gate never carries.
    assert!(!out.contains("classList.toggle(\"done\", v === encodeAtom"), "{out}");
}

#[test]
fn a_class_bind_is_marked_as_a_presence_attribute() {
    // Without this the shaper skips the binding when the view has no row for
    // the key, and the class could be turned on but never off.
    let out = generate();
    assert_eq!(out.matches("presence: true").count(), 2, "{out}");
}

#[test]
fn a_bool_prop_still_compares_against_the_true_constructor() {
    // `checked` is a value attribute, not a gate — but the atom it compares
    // against is `@True`, the constructor of the predeclared `Bool` (S-50),
    // not the lowercase `@true` that predated constructor syntax.
    let src = r#"
entity Todo { title: Text, completed: Bool }
view board =
  Todo select
    li {
      input(type="checkbox" checked=.completed)
      span { .title }
    }
"#;
    let out = rex_codegen::generate(src, "./board.rex?raw").expect("clean codegen");
    assert!(out.contains(r#"checked = (v === encodeAtom("True"))"#), "{out}");
}
