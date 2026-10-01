//! The front end under hostile input: nothing a user can type may panic,
//! overflow the stack, hang, or produce a diagnostic that cannot be rendered.
//!
//! Four generators, from structured to wild:
//!
//!  - **mutants** of the real programs (truncate, drop, duplicate, swap and
//!    substitute tokens and lines) — near-valid input, which is where
//!    recovery paths and half-built ASTs live;
//!  - **token soup**: random sequences over the language's own vocabulary;
//!  - **arbitrary text**, including the bytes lexers trip on;
//!  - **depth and width**: every nesting construct far past the limit, and
//!    programs with thousands of statements, fields and arms.
//!
//! A mutant that still type-checks is a new valid program nobody wrote, so it
//! is also booted and driven through `check_history` — the engine must be
//! exactly right on it, not merely upright.

mod common;

use common::{check_history, Op};
use proptest::prelude::*;
use rex::diagnostic::Severity;
use rex::parser::MAX_NESTING;

fn sources() -> Vec<(&'static str, String)> {
    let root = env!("CARGO_MANIFEST_DIR");
    [
        ("todomvc", "../../examples/todomvc/src/app.rex"),
        ("benchmark", "../../examples/js-framework-benchmark/src/app.rex"),
        ("kanban", "../../examples/kanban/src/board.rex"),
        ("chat", "../../examples/chat/src/app.rex"),
        ("spec12", "tests/fixtures/spec12.rex"),
        ("recursion", "tests/fixtures/recursion.rex"),
        ("board", "tests/fixtures/board.rex"),
    ]
    .into_iter()
    .map(|(name, path)| (name, std::fs::read_to_string(format!("{root}/{path}")).unwrap()))
    .collect()
}

/// Everything the front end does with a source, with every diagnostic
/// rendered both ways. Returns whether it checked clean.
fn front_end(src: &str) -> bool {
    let parsed = rex::parse(src);
    let render = |ds: &[rex::Diagnostic]| {
        for d in ds {
            assert!(d.span.start <= d.span.end && d.span.end <= src.len(), "span {:?} outside a {}-byte source", d.span, src.len());
            let _ = d.render(src);
            let _ = d.render_file(src, "fuzz.rex");
        }
    };
    render(&parsed.diagnostics);
    let _ = rex::program_to_sexpr(&parsed.program);
    if parsed.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return false;
    }
    let checked = rex::check(&parsed.program);
    render(&checked.diagnostics);
    let has_error = checked.diagnostics.iter().any(|d| d.severity == Severity::Error);
    // The contract every host relies on: elaborated iff no errors.
    assert_eq!(checked.elaborated.is_some(), !has_error, "elaborated/diagnostics disagree");
    // Checking is a function of the source.
    let again = rex::check(&rex::parse(src).program);
    assert_eq!(again.diagnostics, checked.diagnostics, "check is not deterministic");
    checked.elaborated.is_some()
}

/// Run `f` on a thread with the stack a Rex host really has: 1 MB in an
/// optimised build (the browser's wasm stack), and the 8 MB of a native main
/// thread in a debug build, whose frames are several times larger.
fn on_host_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let stack = if cfg!(debug_assertions) { 8 << 20 } else { 1 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(f).unwrap().join().expect("the front end panicked")
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// The language's vocabulary, for substitutions and for soup.
const VOCAB: &[&str] = &[
    "entity", "let", "view", "state", "event", "on", "type", "rel", "import", "js", "as", "new", "update", "delete", "set",
    "do", "where", "by", "in", "not", "if", "then", "else", "match", "select", "order", "desc", "from", "recursive",
    "except", "antijoin", "distinct", "fst", "snd", "id", "unit", "local", "children", "focus", "clear", "revert",
    "count", "sum", "avg", "min", "max", "Int", "Text", "Money", "Bool", "Date", "Unit", "True", "False",
    "Todo", "t", "x", "main", "div", "li", ".text", ".completed", ".x", "@a", "_",
    "(", ")", "{", "}", "[", "]", ",", ".", ":", ";", "=", ":=", "=>", "->", "|", "&", "~", "+", "-", "*", "/", "%", "++",
    "<", "<=", ">", ">=", "!=", "0", "1", "42", "-1", "9.99", "2026-01-15", "\"s\"", "\"\"", "class.x=", "value=", "\n",
];

/// Split into tokens that keep their trailing whitespace, so a mutant rejoins
/// into text laid out like the original.
fn pieces(src: &str) -> Vec<&str> {
    src.split_inclusive(char::is_whitespace).collect()
}

fn mutant(src: &str, rng: &mut Rng) -> String {
    if src.is_empty() {
        return String::new();
    }
    let toks = pieces(src);
    let lines: Vec<&str> = src.split_inclusive('\n').collect();
    let (t, l) = (rng.below(toks.len()), rng.below(lines.len()));
    match rng.below(9) {
        // Truncate at a character boundary.
        0 => {
            let mut at = rng.below(src.len());
            while !src.is_char_boundary(at) {
                at -= 1;
            }
            src[..at].to_string()
        }
        1 => toks.iter().enumerate().filter(|(i, _)| *i != t).map(|(_, s)| *s).collect(),
        2 => lines.iter().enumerate().filter(|(i, _)| *i != l).map(|(_, s)| *s).collect(),
        3 => {
            let mut v = toks.clone();
            let u = rng.below(v.len());
            v.swap(t, u);
            v.concat()
        }
        4 => {
            let mut v = toks.clone();
            v.insert(t, toks[rng.below(toks.len())]);
            v.concat()
        }
        5 => {
            let mut v: Vec<String> = toks.iter().map(|s| s.to_string()).collect();
            v[t] = format!("{} ", VOCAB[rng.below(VOCAB.len())]);
            v.concat()
        }
        6 => {
            let mut v = lines.clone();
            let m = rng.below(v.len());
            v.swap(l, m);
            v.concat()
        }
        // Splice a line of one program into another place.
        7 => {
            let mut v = lines.clone();
            v.insert(l, lines[rng.below(lines.len())]);
            v.concat()
        }
        // Delete one character.
        _ => {
            let chars: Vec<char> = src.chars().collect();
            let at = rng.below(chars.len());
            chars.iter().enumerate().filter(|(i, _)| *i != at).map(|(_, c)| *c).collect()
        }
    }
}

#[test]
fn mutants_of_real_programs_never_panic_and_valid_ones_run_exactly() {
    let per_source = common::cases(700) as usize;
    let (total, valid, ran) = on_host_stack(move || {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let (mut total, mut valid, mut ran) = (0, 0, 0);
        for (name, src) in sources() {
            assert!(front_end(&src), "`{name}` itself should check");
            for _ in 0..per_source {
                // One or two mutations: near-valid first, then compounding.
                let mut m = mutant(&src, &mut rng);
                if rng.below(3) == 0 {
                    m = mutant(&m, &mut rng);
                }
                total += 1;
                if !front_end(&m) {
                    continue;
                }
                valid += 1;
                // A valid program nobody wrote: the engine must still be exact.
                // Batch evaluation must not panic on it either.
                let _ = rex::eval::run(&rex::parse(&m).program);
                let ops: Vec<Op> = (0..6)
                    .map(|_| Op { event: rng.below(usize::MAX), picks: (0..8).map(|_| rng.below(usize::MAX)).collect() })
                    .collect();
                // A program with no events has nothing to drive.
                if common::check(&m).is_ok_and(|(_, _, events)| !events.is_empty()) {
                    if let Err(e) = check_history(&m, &ops, 3) {
                        panic!("a mutant of `{name}` checks but the engine is wrong on it:\n{e}\n--- mutant ---\n{m}");
                    }
                    ran += 1;
                }
            }
        }
        (total, valid, ran)
    });
    println!("{total} mutants: {valid} still check, {ran} driven through the engine oracle");
    // The mutator must keep producing both kinds, or it tests nothing.
    assert!(valid * 20 > total && valid * 10 < total * 9, "{valid} of {total} mutants check");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: common::cases(600), ..ProptestConfig::default() })]

    #[test]
    fn token_soup_never_panics(picks in prop::collection::vec(0usize..VOCAB.len(), 0..80)) {
        let src: String = picks.iter().map(|i| format!("{} ", VOCAB[*i])).collect();
        on_host_stack(move || front_end(&src));
    }

    /// A statement's worth of soup after a real schema, so the checker (not
    /// just the parser's recovery) sees it.
    #[test]
    fn soup_after_a_schema_never_panics(
        head in 0usize..6,
        picks in prop::collection::vec(0usize..VOCAB.len(), 0..40),
    ) {
        let heads = ["let v = ", "let v : Todo -> Int = ", "view main = ", "event E(t: Todo)\non E(t) => ", "let v = Todo where ", "state s : Int = "];
        let soup: String = picks.iter().map(|i| format!("{} ", VOCAB[*i])).collect();
        let src = format!("entity Todo {{ text: Text, completed: Bool, x: Int }}\nlet a = new Todo {{ text: \"s\", completed: True, x: 1 }}\n{}{soup}\n", heads[head]);
        on_host_stack(move || front_end(&src));
    }

    #[test]
    fn arbitrary_text_never_panics(src in "\\PC{0,200}") {
        front_end(&src);
    }

    /// The characters lexers trip on: quotes and escapes that never close,
    /// separators, NUL, a BOM, digits running into dots and dashes.
    #[test]
    fn lexer_hazards_never_panic(src in "[\"\\\\'\n\r\t\u{0}\u{feff}\u{2028} a-c0-9.:@#/*+-]{0,120}") {
        front_end(&src);
    }
}

/// `depth` copies of each nesting construct. Each is a complete program; the
/// shallow ones type-check.
fn nested(depth: usize) -> Vec<(&'static str, String)> {
    let e = "entity A { x: Int, b: Bool }\nlet a = new A { x: 3, b: True }\nevent E(a: A)\n";
    let r = |s: &str| s.repeat(depth);
    vec![
        ("parentheses", format!("{e}let v = A . {}.x{}\n", r("("), r(")"))),
        ("not", format!("{e}let v = A where {}.b\n", r("not "))),
        ("compose chain", format!("{e}let v = A{}\n", r(" . id"))),
        ("union chain", format!("{e}let v = {}A\n", r("A | "))),
        ("arithmetic chain", format!("{e}let v : A -> Int = {}.x\n", r(".x + "))),
        ("concat chain", format!("{e}let v : A -> Text = {}\"z\"\n", r("\"a\" ++ "))),
        ("comparison chain", format!("{e}let v = A where {}.x\n", r(".x = "))),
        ("fork chain", format!("{e}let v = A . ({}.x)\n", r(".x , "))),
        ("if", format!("{e}let v : A -> Int = {}2\n", r("if .b then 1 else "))),
        ("match", format!("{e}let v : A -> Int = {}2{}\n", r("match .b { True => 1, _ => "), r(" }"))),
        ("inverse", format!("{e}let v = {}A\n", r("~"))),
        ("unary minus", format!("{e}let v : A -> Int = .x + {}1\n", r("-"))),
        ("semijoin", format!("{e}let v = A{}{}\n", r("[A"), r("]"))),
        ("call", format!("{e}let v = {}A by unit{}\n", r("count("), r(")"))),
        ("fst", format!("{e}let v = {}(A , A)\n", r("fst "))),
        ("distinct", format!("{e}let v = {}A\n", r("distinct "))),
        ("product type", format!("{e}let v : {}A = A\n", r("A * "))),
        ("arrow type", format!("{e}let v : {}A = A\n", r("A -> "))),
        ("elements", format!("{e}view main = {}p \"x\"{}\n", r("div { "), r(" }"))),
        ("view if", format!("{e}view main = div {{ {}p \"x\"{} }}\n", r("if (A) { div { "), r(" } }"))),
        ("select", format!("{e}view main = ul {{ {}li \"x\"{} }}\n", r("A as a select div { "), r(" }"))),
        ("handler value", format!("{e}on E(a) => a.x := {}a.x{}\n", r("("), r(")"))),
        ("handler chain", format!("{e}on E(a) => a.x := a.x{}\n", r(" + 1"))),
        ("handler not", format!("{e}on E(a) => a.b := {}a.b\n", r("not "))),
        ("where predicate", format!("{e}on E(a) => delete A where {}.b{}\n", r("("), r(")"))),
        ("braces", format!("{e}on E(a) {}delete a{}\n", r("{ "), r(" }"))),
        ("handler if", format!("{e}on E(a) {{ {}delete a{} }}\n", r("if (a.b) { "), r(" }"))),
        ("handler else if", format!("{e}on E(a) {{ {}if (a.b) {{ delete a }} }}\n", r("if (a.b) { delete a } else "))),
        ("guard", format!("{e}on E(a) where ({}a.b{}) => delete a\n", r("("), r(")"))),
        ("reject", format!("{e}on E(a) {{ {}reject \"no\"{} }}\n", r("if (a.b) { "), r(" }"))),
        ("row test", format!("{e}on E(a) where ({}A where .b{}) => delete a\n", r("not ("), r(")"))),
        ("row test conjuncts", format!("{e}on E(a) where (A where .b{}) => delete a\n", r(" & .b"))),
        ("guard conjuncts", format!("{e}on E(a) where ({}a.b) => delete a\n", r("a.b & "))),
        ("brackets alone", r("[")),
        ("parens alone", r("(")),
        ("braces alone", format!("view main = {}", r("{"))),
    ]
}

#[test]
fn nesting_past_the_limit_is_an_error_not_a_stack_overflow() {
    for depth in [MAX_NESTING + 1, 4 * MAX_NESTING, 20_000, 300_000] {
        for (what, src) in nested(depth) {
            on_host_stack(move || {
                assert!(!front_end(&src), "{what} nested {depth} deep was accepted");
            });
        }
    }
}

#[test]
fn nesting_up_to_the_limit_fits_the_stack() {
    // Whatever the parser lets through, every later pass must survive —
    // including evaluation and the circuit, for the ones that check.
    for depth in [8, MAX_NESTING / 4, MAX_NESTING / 2, MAX_NESTING - 8, MAX_NESTING] {
        for (what, src) in nested(depth) {
            on_host_stack(move || {
                if front_end(&src) {
                    let app = common::App::build(&src);
                    common::check_views(&app, what).unwrap();
                }
            });
        }
    }
}

#[test]
fn a_reasonably_nested_program_still_checks() {
    // The limit must not bite real programs: these all check at depth 20.
    for (what, src) in nested(20) {
        let must = ["parentheses", "not", "compose chain", "union chain", "arithmetic chain", "concat chain", "inverse", "elements", "handler value", "handler chain", "distinct", "semijoin"];
        if must.contains(&what) {
            assert!(on_host_stack(move || front_end(&src)), "{what} at depth 20 should check");
        }
    }
}

#[test]
fn wide_programs_are_handled() {
    let n = 2000;
    let wide: Vec<(&str, String)> = vec![
        ("statements", format!("entity A {{ x: Int }}\n{}", (0..n).map(|i| format!("let v{i} = A . .x\n")).collect::<String>())),
        ("rows", format!("entity A {{ x: Int }}\n{}let v = A . .x\n", (0..n).map(|i| format!("let _ = new A {{ x: {i} }}\n")).collect::<String>())),
        ("fields", format!("entity A {{ {} }}\nlet v = A . .f7\n", (0..n).map(|i| format!("f{i}: Int")).collect::<Vec<_>>().join(", "))),
        ("entities", (0..n).map(|i| format!("entity E{i} {{ x: Int }}\n")).collect()),
        ("events", format!("entity A {{ x: Int }}\n{}", (0..500).map(|i| format!("event E{i}(a: A)\non E{i}(a) => a.x := a.x + {i}\n")).collect::<String>())),
        ("handler statements", format!("entity A {{ x: Int }}\nevent E(a: A)\non E(a) {{\n{}}}\n", "  a.x := a.x + 1\n".repeat(n))),
        ("constructors", format!("type T = {}\nstate s : T = C0\n", (0..n).map(|i| format!("C{i}")).collect::<Vec<_>>().join(" | "))),
        ("children", format!("view main = div {{ {} }}\n", "p \"x\" ".repeat(n))),
        ("attributes", format!("view main = div({}) {{ p \"x\" }}\n", (0..500).map(|i| format!("a{i}=\"v\"")).collect::<Vec<_>>().join(" "))),
        ("levels", format!("entity A {{ x: Int }}\nview main = div {{ {} }}\n", "ul { A as a select li { .x } } ".repeat(200))),
        ("a long string", format!("entity A {{ s: Text }}\nlet a = new A {{ s: \"{}\" }}\nlet v = A . .s\n", "é".repeat(200_000))),
        ("a long identifier", format!("entity {} {{ x: Int }}\n", "A".repeat(100_000))),
        ("a long comment", format!("// {}\nentity A {{ x: Int }}\n", "c".repeat(500_000))),
        ("blank lines", format!("{}entity A {{ x: Int }}\nlet v = A . .zz\n", "\n".repeat(100_000))),
    ];
    for (what, src) in wide {
        let ok = on_host_stack(move || front_end(&src));
        // The last is an error on purpose: a diagnostic 100,000 lines down.
        assert_eq!(ok, what != "blank lines", "{what}");
    }
}

/// A `match` nests as deep as it has arms once desugared, without the source
/// nesting at all — the limit has to hold there too.
#[test]
fn a_match_with_many_arms_fits_the_stack() {
    for arms in [3, 40, 120, 400] {
        let ctors: Vec<String> = (0..arms).map(|i| format!("C{i}")).collect();
        let src = format!(
            "type T = {}\nentity A {{ k: T, x: Int }}\nlet a = new A {{ k: C1, x: 5 }}\nlet v : A -> Int = match .k {{ {} }}\n",
            ctors.join(" | "),
            ctors.iter().enumerate().map(|(i, c)| format!("{c} => .x + {i}")).collect::<Vec<_>>().join(", ")
        );
        on_host_stack(move || {
            if front_end(&src) {
                let app = common::App::build(&src);
                common::check_views(&app, "match").unwrap();
                assert_eq!(app.view("v").len(), 1, "{arms} arms");
            } else {
                panic!("a {arms}-arm match should check");
            }
        });
    }
}
