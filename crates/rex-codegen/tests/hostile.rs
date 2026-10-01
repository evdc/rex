//! Codegen under hostile programs: whatever checks must generate a module
//! that is at least well-formed text — every string a valid JS literal that
//! says what the program said, every bracket closed — and nothing may panic.

use std::panic::catch_unwind;

/// The JS string literal starting at `at` in `out`, decoded.
fn literal_at(out: &str, at: usize) -> String {
    let mut stream = serde_json::Deserializer::from_str(&out[at..]).into_iter::<String>();
    stream.next().expect("a literal").unwrap_or_else(|e| panic!("not a JS/JSON string literal: {e}\n{}", &out[at..(at + 80).min(out.len())]))
}

/// Brackets balance once string literals and comments are skipped.
fn assert_balanced(out: &str) {
    let mut stack = Vec::new();
    let mut chars = out.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                let mut escaped = false;
                for (_, d) in chars.by_ref() {
                    match d {
                        '\\' if !escaped => escaped = true,
                        '"' if !escaped => break,
                        '\n' => panic!("a string literal runs past its line at byte {i}"),
                        _ => escaped = false,
                    }
                }
            }
            '/' if matches!(chars.peek(), Some((_, '/'))) => {
                for (_, d) in chars.by_ref() {
                    if d == '\n' {
                        break;
                    }
                }
            }
            '(' | '[' | '{' => stack.push(c),
            ')' | ']' | '}' => {
                let open = stack.pop().unwrap_or_else(|| panic!("unmatched `{c}` at byte {i}"));
                assert_eq!((open, c), match c { ')' => ('(', ')'), ']' => ('[', ']'), _ => ('{', '}') }, "at byte {i}");
            }
            _ => {}
        }
    }
    assert!(stack.is_empty(), "unclosed {stack:?}");
}

const TEXTS: &[&str] = &[
    "plain", "", "\"quoted\"", "back\\slash", "new\nline", "tab\there", "carriage\rreturn", "nul\u{0}byte",
    "sep\u{2028}arator\u{2029}", "</script><script>alert(1)</script>", "`tick` ${interp}", "'single'", "é😀𝒳",
    "\u{7f}\u{1}\u{1f}", "*/ comment-ish //", "\\\"", "\\n",
];

/// A Rex string literal for `s` (the lexer knows `\" \\ \n \t`; everything
/// else is written raw).
fn rex_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[test]
fn static_text_and_attributes_become_valid_js_literals_that_say_the_same() {
    for text in TEXTS {
        let src = format!(
            "entity Todo {{ text: Text }}\nevent Add(text: Text)\non Add(text) => new Todo {{ text: text }}\n\
             view main = div(title={lit}) {{ p {lit} button(on click => do Add({lit})) \"go\" }}\n",
            lit = rex_str(text)
        );
        let out = rex_codegen::generate(&src, "./app.rex?raw").unwrap_or_else(|d| panic!("{text:?} does not check: {d:?}"));
        assert_balanced(&out);
        // The static text node.
        let at = out.find("createTextNode(").expect("a text node") + "createTextNode(".len();
        assert_eq!(literal_at(&out, at), *text, "text node for {text:?}");
        // The static attribute.
        let at = out.find("setAttribute(\"title\", ").expect("the attribute") + "setAttribute(\"title\", ".len();
        assert_eq!(literal_at(&out, at), *text, "attribute for {text:?}");
        // The literal event argument, on its way to `encodeText`.
        let at = out.find("\"text\": encodeText(").expect("the dispatch arg") + "\"text\": encodeText(".len();
        assert_eq!(literal_at(&out, at), *text, "dispatch argument for {text:?}");
        // No raw line terminator inside the module's code lines other than `\n`.
        assert!(!out.contains('\r') && !out.contains('\u{2028}') && !out.contains('\u{2029}'), "{text:?}");
    }
}

#[test]
fn names_that_are_js_keywords_or_emitter_locals_generate_distinct_bindings() {
    // Handler params are prefixed, so none of these can collide with the
    // listener's own names or be a syntax error.
    for name in ["key", "ev", "e0", "shaper", "engine", "dispatch", "class", "function", "var", "await", "new_", "_ids", "d", "el", "v"] {
        let src = format!(
            "entity Todo {{ text: Text }}\nevent Edit(t: Todo, text: Text)\non Edit(t, text) => t.text := text\n\
             view main = ul {{ Todo as t select li {{ input(value=.text on change({name} = value) => do Edit(t, {name})) }} }}\n"
        );
        match rex_codegen::generate(&src, "./app.rex?raw") {
            Ok(out) => {
                assert_balanced(&out);
                assert!(out.contains(&format!("const p_{name} = ")), "{name}: {out}");
                assert!(out.contains(&format!("\"text\": p_{name}")), "{name}");
                assert!(out.contains("\"t\": key"), "{name}: the row key must still be the row key");
            }
            // A reserved word of Rex itself is refused by the parser instead.
            Err(d) => assert!(!d.is_empty(), "{name}"),
        }
    }
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

#[test]
fn mutants_of_the_examples_generate_well_formed_modules_or_diagnostics() {
    let root = env!("CARGO_MANIFEST_DIR");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut ok, mut total) = (0, 0);
    for path in ["todomvc/src/app.rex", "js-framework-benchmark/src/app.rex", "kanban/src/board.rex", "chat/src/app.rex"] {
        let src = std::fs::read_to_string(format!("{root}/../../examples/{path}")).unwrap();
        let toks: Vec<&str> = src.split_inclusive(char::is_whitespace).collect();
        for _ in 0..500 {
            let mut v = toks.clone();
            for _ in 0..1 + rng.below(2) {
                let (a, b) = (rng.below(v.len()), rng.below(v.len()));
                match rng.below(4) {
                    0 => {
                        v.remove(a);
                    }
                    1 => v.swap(a, b),
                    2 => v.insert(a, toks[b]),
                    _ => v.truncate(a.max(1)),
                }
            }
            let mutant = v.concat();
            total += 1;
            match catch_unwind(|| rex_codegen::generate(&mutant, "./app.rex?raw")) {
                Err(_) => panic!("codegen panicked on a mutant of {path}:\n{mutant}"),
                Ok(Ok(out)) => {
                    ok += 1;
                    assert_balanced(&out);
                    assert!(out.starts_with("// GENERATED"));
                    // Deterministic: the same program is the same module.
                    assert_eq!(rex_codegen::generate(&mutant, "./app.rex?raw").unwrap(), out);
                }
                Ok(Err(diags)) => assert!(!diags.is_empty(), "rejected without a diagnostic:\n{mutant}"),
            }
        }
    }
    assert!(ok > total / 20, "only {ok} of {total} mutants generated");
}
