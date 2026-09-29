//! S-02 acceptance programs (MVP-PLAN.md): the three MVP apps in the v1
//! surface. They do not parse yet; each test is un-ignored by the story that
//! makes it pass (S-10 for parsing, S-20+ for checking).

const TODOMVC: &str = include_str!("../../../examples/todomvc/src/app.rex");
const BENCH: &str = include_str!("../../../examples/js-framework-benchmark/src/app.rex");
const CHAT: &str = include_str!("../../../examples/chat/src/app.rex");
const KANBAN_V1: &str = include_str!("../../../examples/kanban/src/board.rex");

fn parses_clean(src: &str) {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
}

fn checks_clean(src: &str) {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.elaborated.is_some(), "check: {:?}", checked.diagnostics);
}

#[test]

fn todomvc_parses() { parses_clean(TODOMVC); }

#[test]

fn bench_parses() { parses_clean(BENCH); }

#[test]

fn kanban_v1_parses() { parses_clean(KANBAN_V1); }

#[test]

fn chat_parses() { parses_clean(CHAT); }

#[test]
#[ignore = "`let x = new …` inside a handler (unassigned)"]
fn chat_checks() { checks_clean(CHAT); }

#[test]
fn todomvc_checks() { checks_clean(TODOMVC); }

#[test]
#[ignore = "S-42/S-61: `import js`, untyped extractor params, components"]
fn bench_checks() { checks_clean(BENCH); }

#[test]
fn kanban_v1_checks() { checks_clean(KANBAN_V1); }
