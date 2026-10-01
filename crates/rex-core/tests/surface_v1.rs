//! The S-02 acceptance programs (MVP-PLAN.md): the example apps, written in
//! the v1 surface before the grammar existed. All four parse; the three MVP
//! apps check. `chat` is the one left: it needs `let x = new …` inside a
//! handler, which is not implemented (SYNTAX.md).

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
#[ignore = "`let x = new …` inside a handler is not implemented"]
fn chat_checks() { checks_clean(CHAT); }

#[test]
fn todomvc_checks() { checks_clean(TODOMVC); }

#[test]
fn bench_checks() { checks_clean(BENCH); }

#[test]
fn kanban_v1_checks() { checks_clean(KANBAN_V1); }
