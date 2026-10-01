//! Corner cases, one by one. Each of these was wrong, crashed, or was silently
//! accepted until the fuzz and model suites (`histories`, `models`,
//! `gen_queries`, `fuzz_frontend`) turned it up; the suites would find them
//! again, but a named test says what the rule is.

mod common;

use common::App;
use rex::eval::Value;

/// The first error's message, or `None` if the program checks.
fn error(src: &str) -> Option<String> {
    let parsed = rex::parse(src);
    let checked = if parsed.diagnostics.is_empty() { rex::check(&parsed.program).diagnostics } else { parsed.diagnostics };
    checked.into_iter().find(|d| d.severity == rex::diagnostic::Severity::Error).map(|d| d.message)
}

#[track_caller]
fn rejects(src: &str, needle: &str) {
    match error(src) {
        Some(msg) => assert!(msg.contains(needle), "wanted an error containing {needle:?}, got {msg:?}\n{src}"),
        None => panic!("accepted, but should be rejected ({needle}):\n{src}"),
    }
}

#[track_caller]
fn accepts(src: &str) {
    if let Some(msg) = error(src) {
        panic!("rejected ({msg}), but should check:\n{src}");
    }
}

/// The one value of a single-row view.
#[track_caller]
fn the(app: &App, view: &str) -> Value {
    match app.view(view).as_slice() {
        [(_, v, 1)] => v.clone(),
        other => panic!("`{view}` is not one row: {other:?}"),
    }
}

// --- arithmetic ----------------------------------------------------------------------

const NUMS: &str = "entity A { x: Int, y: Int, m: Money }\n";

fn nums(x: i64, y: i64, cents: i64, lets: &str) -> App {
    let m = format!("{}{}.{:02}", if cents < 0 { "-" } else { "" }, cents.abs() / 100, cents.abs() % 100);
    App::build(&format!("{NUMS}let a = new A {{ x: {x}, y: {y}, m: {m} }}\n{lets}"))
}

#[test]
fn integer_arithmetic_wraps_and_never_panics() {
    let max = i64::MAX;
    let app = nums(max, 2, 0, "let add : A -> Int = .x + .x\nlet mul : A -> Int = .x * .y\nlet sub : A -> Int = (0 - .x) - .y\nlet total : Unit -> Int = sum((A . (.x + 0)) by unit)\n");
    assert_eq!(the(&app, "add"), Value::Int(max.wrapping_add(max)));
    assert_eq!(the(&app, "mul"), Value::Int(max.wrapping_mul(2)));
    assert_eq!(the(&app, "sub"), Value::Int(0i64.wrapping_sub(max).wrapping_sub(2)));
    assert_eq!(the(&app, "total"), Value::Int(max));
}

#[test]
fn division_and_remainder_are_total() {
    // By zero is 0; the one overflowing quotient, MIN / -1, wraps.
    let app = nums(-7, 0, 0, "let d : A -> Int = .x / .y\nlet r : A -> Int = .x % .y\nlet h : A -> Int = .x / 2\nlet n : A -> Int = .x % 3\n");
    assert_eq!(the(&app, "d"), Value::Int(0));
    assert_eq!(the(&app, "r"), Value::Int(0));
    // Truncating, like Rust and JS: toward zero, remainder takes the dividend's sign.
    assert_eq!(the(&app, "h"), Value::Int(-3));
    assert_eq!(the(&app, "n"), Value::Int(-1));
    let app = nums(i64::MIN + 1, -1, 0, "let d : A -> Int = (.x - 1) / .y\nlet r : A -> Int = (.x - 1) % .y\n");
    assert_eq!(the(&app, "d"), Value::Int(i64::MIN));
    assert_eq!(the(&app, "r"), Value::Int(0));
}

#[test]
fn a_sum_that_overflows_still_retracts_exactly() {
    // The reason arithmetic wraps rather than saturates: a maintained sum is
    // old + delta, and that is exact only in a ring.
    let src = "entity A { x: Int }\nevent Add(x: Int)\nevent Drop(a: A)\non Add(x) => new A { x: x }\non Drop(a) => delete a\nlet total : Unit -> Int = sum((A . (.x + 0)) by unit)\n";
    let mut app = App::build(src);
    let ids: Vec<Value> = [i64::MAX, i64::MAX, 5].iter().map(|x| app.dispatch("Add", &[("x", Value::Int(*x))]).unwrap().0[0].clone()).collect();
    assert_eq!(the(&app, "total"), Value::Int(i64::MAX.wrapping_add(i64::MAX).wrapping_add(5)));
    app.dispatch("Drop", &[("a", ids[0].clone())]).unwrap();
    app.dispatch("Drop", &[("a", ids[1].clone())]).unwrap();
    assert_eq!(the(&app, "total"), Value::Int(5));
    common::check_views(&app, "after retracting").unwrap();
}

#[test]
fn comparison_is_exact_at_the_extremes() {
    // On a wrapping cents scale, MIN + 2 whole units is 200 cents: it used to
    // equal 2.
    let app = nums(i64::MIN + 2, i64::MAX, 250, "let eq2 : A = A where .x = 2\nlet lt : A = A where .x < .y\nlet gt : A = A where .y > 2\nlet m_gt_x : A = A where .m > .x\nlet m_lt_y : A = A where .m < .y\nlet two_fifty : A = A where .m = 2.50\nlet above_two : A = A where .m > 2\n");
    assert!(app.view("eq2").is_empty());
    for view in ["lt", "gt", "m_gt_x", "m_lt_y", "two_fifty", "above_two"] {
        assert_eq!(app.view(view).len(), 1, "{view}");
    }
}

#[test]
fn money_times_money_is_a_type_error() {
    rejects(&format!("{NUMS}let sq : A -> Money = .m * .m\n"), "money squared");
    accepts(&format!("{NUMS}let total : A -> Money = .m * .x\nlet also : A -> Money = .x * .m\n"));
    let app = nums(3, 0, 250, "let total : A -> Money = .m * .x\n");
    assert_eq!(the(&app, "total"), Value::Money(750));
}

#[test]
fn avg_is_the_mean_in_money_for_either_numeric_image() {
    // avg of the Ints 1 and 2 was `$0.01`.
    let src = "entity A { x: Int, m: Money }\nlet a = new A { x: 1, m: 1.00 }\nlet b = new A { x: 2, m: 2.01 }\nlet xs : A -> Int = .x\nlet ms : A -> Money = .m\nlet ax : Unit -> Money = avg(xs by unit)\nlet am : Unit -> Money = avg(ms by unit)\n";
    let app = App::build(src);
    assert_eq!(the(&app, "ax"), Value::Money(150));
    assert_eq!(the(&app, "am"), Value::Money(150)); // 301 / 2, truncated
    rejects("entity A { s: Text }\nlet ss : A -> Text = .s\nlet bad = avg(ss by unit)\n", "numeric");
}

// --- literals --------------------------------------------------------------------------

#[test]
fn literals_out_of_range_are_errors_not_garbage() {
    rejects("entity A { x: Int }\nlet a = new A { x: 99999999999999999999 }\n", "out of range");
    // Was `$0.99`: the whole part overflowed to 0.
    rejects("entity A { m: Money }\nlet a = new A { m: 99999999999999999999.99 }\n", "out of range");
    rejects("entity A { m: Money }\nlet a = new A { m: 92233720368547758.08 }\n", "out of range");
    accepts("entity A { m: Money }\nlet a = new A { m: 92233720368547757.99 }\nlet b = new A { m: -0.50 }\n");
}

#[test]
fn a_date_is_a_real_calendar_date_in_yyyy_mm_dd() {
    accepts("entity A { d: Date }\nlet a = new A { d: 2024-02-29 }\nlet b = new A { d: 2000-02-29 }\n");
    rejects("entity A { d: Date }\nlet a = new A { d: 2024-02-30 }\n", "not a calendar date");
    rejects("entity A { d: Date }\nlet a = new A { d: 2023-02-29 }\n", "not a calendar date");
    rejects("entity A { d: Date }\nlet a = new A { d: 1900-02-29 }\n", "not a calendar date");
    rejects("entity A { d: Date }\nlet a = new A { d: 2024-13-01 }\n", "not a calendar date");
    rejects("entity A { d: Date }\nlet a = new A { d: 2024-00-10 }\n", "not a calendar date");
    // Anything not shaped yyyy-mm-dd is subtraction: 10-2-3 is 5.
    let app = App::build("entity A { x: Int }\nlet a = new A { x: 0 }\nlet v : A -> Int = .x + 10-2-3\n");
    assert_eq!(the(&app, "v"), Value::Int(5));
}

#[test]
fn strings_hold_any_text_and_bad_escapes_are_errors_that_do_not_break_the_lexer() {
    let src = "entity A { s: Text }\nlet a = new A { s: \"q\\\"b\\\\s\\nn\\tt,(p)é😀\u{2028}\" }\nlet v : A -> Text = .s\n";
    assert_eq!(the(&App::build(src), "v"), Value::text("q\"b\\s\nn\tt,(p)é😀\u{2028}"));
    // An unknown escape of a multibyte character used to leave the lexer
    // inside that character, and the next token panicked.
    rejects("entity A { s: Text }\nlet a = new A { s: \"\\é\" }\n", "unknown escape");
    rejects("let a = \"\\\u{feff}", "unknown escape");
    rejects("entity A { s: Text }\nlet a = new A { s: \"abc\n", "unterminated");
}

#[test]
fn a_byte_order_mark_is_not_part_of_the_program() {
    accepts("\u{feff}entity A { x: Int }\nlet v = A . .x\n");
    // …and spans are still offsets into the source as given.
    let src = "\u{feff}entity A { x: Int }\nlet v = A . .zz\n";
    let d = rex::check(&rex::parse(src).program).diagnostics;
    assert_eq!(d[0].span.slice(src), ".zz");
}

// --- names -----------------------------------------------------------------------------

#[test]
fn a_name_is_declared_once() {
    rejects("entity A { x: Int, x: Text }\n", "already has a field `x`");
    rejects("entity A { x: Int }\nlet a = new A { x: 1, x: 2 }\n", "sets `x` twice");
    rejects("entity A { x: Int }\nevent E()\non E() => new A { x: 1, x: 2 }\n", "sets `x` twice");
    rejects("entity A { x: Int }\nevent E(a: A)\non E(a) => update a { x: 1, x: 2 }\n", "sets `x` twice");
    rejects("state s : Int = 1\nstate s : Int = 2\n", "state `s` is already defined");
    rejects("entity A { x: Int }\nlet v = A\nlet v = A . .x\n", "binding `v` is already defined");
    rejects("entity A { x: Int }\nview main = div { p \"a\" }\nview main = div { p \"b\" }\n", "view `main` is already defined");
    rejects("entity A { x: Int }\nstate A : Int = 1\n", "already defined as an entity");
    rejects("entity All { x: Int }\ntype F = All | Some\n", "already defined as an entity");
    rejects("entity A { x: Int }\nlet A = A . .x\n", "already defined as an entity");
    rejects("state s : Int = 1\nentity A { x: Int }\nlet s = A\n", "already defined as a state");
    // Anonymous bindings repeat freely.
    accepts("entity A { x: Int }\nlet _ = new A { x: 1 }\nlet _ = new A { x: 2 }\n");
}

#[test]
fn built_in_types_cannot_be_redefined_but_documented_shadowing_still_works() {
    for name in ["Int", "Text", "Money", "Date", "Bool", "Unit"] {
        rejects(&format!("entity {name} {{ x: Int }}\n"), "built-in type");
        rejects(&format!("type {name} = P | Q\n"), "built-in type");
    }
    // A binding may shadow `unit` and a constructor (S-50).
    accepts("entity A { x: Int }\nlet unit = A\n");
    accepts("type Filter = All | Active\nentity Todo { text: Text }\nlet All : Todo -> Text = .text\nlet echo : Todo -> Text = All\n");
}

// --- queries ---------------------------------------------------------------------------

const TAGGED: &str = "type Tag = Red | Green | Blue\nentity P { n: Int, t: Tag }\nentity C { n: Int, p: P }\nlet p0 = new P { n: 1, t: Red }\nlet p1 = new P { n: 2, t: Green }\nlet c0 = new C { n: 7, p: p1 }\nlet pn : P -> Int = .n\n";

#[test]
fn a_constructor_is_a_literal_in_an_in_set() {
    let app = App::build(&format!("{TAGGED}let warm : P = P where .t in (Red | Blue)\nlet cold : P = P where not .t in (Red | Blue)\n"));
    assert_eq!(app.view("warm").len(), 1);
    assert_eq!(app.view("cold").len(), 1);
    rejects(&format!("{TAGGED}let bad : P = P where .t in (Red | Nope)\n"), "set of literals");
}

#[test]
fn a_spaced_dot_after_a_path_joins_a_relation_when_there_is_no_such_field() {
    // `.p . pn` parses as the path `.p.pn`; `pn` is no field of P, but it is
    // a view, so it is the join it looks like.
    let app = App::build(&format!("{TAGGED}let a : C -> Int = .p . pn\nlet b : C -> Int = (.p) . pn\nlet c : C -> P = .p . P\n"));
    assert_eq!(app.view("a"), app.view("b"));
    assert_eq!(the(&app, "a"), Value::Int(2));
    assert_eq!(app.view("c").len(), 1);
    // A field still wins over a relation of the same name, and a name that is
    // neither is still an unknown field.
    assert_eq!(the(&App::build(&format!("{TAGGED}let n : P -> Int = P . (.n + 100)\nlet a : C -> Int = .p . n\n")), "a"), Value::Int(2));
    rejects(&format!("{TAGGED}let a : C -> Int = .p . nope\n"), "unknown field `nope`");
}

#[test]
fn a_match_with_hundreds_of_arms_checks_and_evaluates() {
    let n = 300;
    let ctors: Vec<String> = (0..n).map(|i| format!("K{i}")).collect();
    let arms = ctors.iter().enumerate().map(|(i, c)| format!("{c} => .x + {i}")).collect::<Vec<_>>().join(", ");
    let src = format!("type K = {}\nentity A {{ k: K, x: Int }}\nlet a = new A {{ k: K250, x: 1000 }}\nlet v : A -> Int = match .k {{ {arms} }}\nlet w : A -> Int = match .k {{ K0 => 0, _ => 1 }}\n", ctors.join(" | "));
    let app = App::build(&src);
    assert_eq!(the(&app, "v"), Value::Int(1250));
    assert_eq!(the(&app, "w"), Value::Int(1));
}

#[test]
fn nesting_has_a_limit_and_says_so() {
    let deep = |n: usize| format!("entity A {{ x: Int }}\nlet v = A . {}.x{}\n", "(".repeat(n), ")".repeat(n));
    accepts(&deep(100));
    rejects(&deep(rex::parser::MAX_NESTING + 1), "levels deep");
    rejects(&deep(50_000), "levels deep");
    // One error for the statement, and the next statement still parses.
    let src = format!("{}let w = A . .zz\n", deep(500));
    let errs: Vec<_> = rex::parse(&src).diagnostics;
    assert_eq!(errs.len(), 1, "{errs:?}");
}

// --- handlers and persistence ------------------------------------------------------------

#[test]
fn a_view_over_a_seed_row_survives_a_restoring_boot() {
    // The restoring boot skips `new` statements; a view that names one of
    // their rows used to panic in lowering ("unbound value").
    let src = "entity L { title: Text }\nentity C { list: L }\nlet todo = new L { title: \"Todo\" }\nlet done = new L { title: \"Done\" }\nlet c = new C { list: done }\nevent Add(l: L)\non Add(l) => new C { list: l }\nlet in_done : C = C[(.list) . done]\nlet done_title : L -> Text = done . .title\n";
    let mut app = App::build(src);
    let done = app.values["done"].clone();
    app.dispatch("Add", &[("l", done.clone())]).unwrap();
    assert_eq!(app.view("in_done").len(), 2);
    common::check_replay(src, &app).unwrap();
    let snap = app.engine.base_snapshot();
    common::check_restore(src, &app, &snap).unwrap();
    // And the name means the same id on both kinds of boot.
    assert_eq!(App::build_empty(src).values["done"], done);
}

#[test]
fn an_update_to_a_deleted_row_does_nothing_and_never_resurrects_it() {
    let src = "entity A { x: Int }\nevent Set(a: A, x: Int)\nevent Bump(a: A)\nevent Drop(a: A)\nevent Both(a: A)\non Set(a, x) => a.x := x\non Bump(a) => a.x := a.x + 1\non Drop(a) => delete a\non Both(a) {\n  delete a\n  a.x := 9\n}\nlet a0 = new A { x: 1 }\nlet a1 = new A { x: 2 }\nlet xs : A -> Int = .x\n";
    let mut app = App::build(src);
    let (a0, a1) = (app.values["a0"].clone(), app.values["a1"].clone());
    app.dispatch("Drop", &[("a", a0.clone())]).unwrap();
    // A plain write to a row that is gone: accepted, logged, no effect.
    app.dispatch("Set", &[("a", a0.clone()), ("x", Value::Int(5))]).unwrap();
    assert_eq!(app.view("xs").len(), 1);
    // A write that reads the row has nothing to read: refused, not logged.
    let cursor = app.engine.cursor();
    assert!(app.dispatch("Bump", &[("a", a0.clone())]).is_err());
    assert_eq!(app.engine.cursor(), cursor);
    // Delete then write, in one transaction: still deleted.
    app.dispatch("Both", &[("a", a1)]).unwrap();
    assert!(app.view("xs").is_empty());
    assert!(app.ids("A").is_empty());
    common::check_base_invariant(&app.engine).unwrap();
    common::check_replay(src, &app).unwrap();
}

#[test]
fn ids_are_never_reused() {
    let src = "entity A { x: Int }\nevent Add()\nevent Clear()\non Add() => new A { x: 0 }\non Clear() => delete A\n";
    let mut app = App::build(src);
    let first = app.dispatch("Add", &[]).unwrap().0[0].clone();
    app.dispatch("Clear", &[]).unwrap();
    let second = app.dispatch("Add", &[]).unwrap().0[0].clone();
    assert_ne!(first, second);
    // …including across a snapshot and restore.
    let snap = app.engine.base_snapshot();
    let mut restored = App::build_empty(src);
    restored.engine.restore(&snap);
    let third = restored.dispatch("Add", &[]).unwrap().0[0].clone();
    assert!(third != first && third != second);
}

#[test]
fn a_damaged_snapshot_or_log_is_refused_and_leaves_the_engine_untouched() {
    use rex::dbsp::InputKey;
    use rex::events::{replay, restore};
    let src = "entity A { x: Int }\nevent Set(a: A, x: Int)\non Set(a, x) => a.x := x\nlet a0 = new A { x: 1 }\nlet xs : A -> Int = .x\n";
    let mut app = App::build(src);
    app.dispatch("Set", &[("a", app.values["a0"].clone()), ("x", Value::Int(4))]).unwrap();
    let good = || app.engine.base_snapshot();
    let sort = app.env.entity_sort("A").unwrap();
    let field = InputKey::Field(sort, rex::eval::intern("x"));
    let id = |n| Value::Id(sort, n);
    let table = |snap: &mut rex::dbsp::BaseSnapshot, key: InputKey| snap.inputs.iter().position(|(k, _)| *k == key).unwrap();

    type Damage<'a> = Box<dyn Fn(&mut rex::dbsp::BaseSnapshot) + 'a>;
    let cases: Vec<(&str, Damage)> = vec![
        ("a field value of the wrong type", Box::new(|s| { let t = table(s, field); s.inputs[t].1[0].1 = Value::text("x"); })),
        ("two values in one field", Box::new(|s| { let t = table(s, field); s.inputs[t].1.push((id(0), Value::Int(9), 1)); })),
        ("a weight of 2", Box::new(|s| { let t = table(s, field); s.inputs[t].1[0].2 = 2; })),
        ("a field row of a row that does not exist", Box::new(|s| { let t = table(s, field); s.inputs[t].1[0].0 = id(7); })),
        ("an identity row that is not (id, id)", Box::new(|s| { let t = table(s, InputKey::Identity(sort)); s.inputs[t].1[0].1 = id(3); })),
        ("an id that was never minted", Box::new(|s| { let t = table(s, InputKey::Identity(sort)); s.inputs[t].1.push((id(50), id(50), 1)); })),
        ("a table of a sort the program lacks", Box::new(|s| s.inputs.push((InputKey::Identity(rex::types::SortId(9)), vec![])))),
        ("a field the entity lacks", Box::new(|s| s.inputs.push((InputKey::Field(sort, rex::eval::intern("nope")), vec![(id(0), Value::Int(1), 1)])))),
        ("the same table twice", Box::new(|s| { let t = s.inputs[0].clone(); s.inputs.push(t); })),
        ("a cursor no host can hold", Box::new(|s| s.cursor = u64::MAX)),
        ("an id counter no host can hold", Box::new(|s| s.next_id[0].1 = u64::MAX)),
    ];
    for (what, break_it) in cases {
        let mut snap = good();
        break_it(&mut snap);
        let mut fresh = App::build_empty(src);
        assert!(restore(&mut fresh.engine, &fresh.env, &snap).is_err(), "{what} was loaded");
        assert!(fresh.view("xs").is_empty() && fresh.engine.cursor() == 0, "{what}: refused, but something was loaded");
    }
    let mut fresh = App::build_empty(src);
    restore(&mut fresh.engine, &fresh.env, &good()).unwrap();
    assert_eq!(fresh.view("xs"), app.view("xs"));

    // A log: out of order, an argument of the wrong type, an unknown event.
    let log = app.engine.log().to_vec();
    let mut swapped = log.clone();
    swapped.swap(0, 1);
    let mut wrong = log.clone();
    wrong[1].args[0].1 = rex::dbsp::ArgValue::Value(Value::Int(3));
    let mut unknown = log.clone();
    unknown[1].name = "Nope".into();
    let mut far = log.clone();
    far[1].seq = u64::MAX;
    for (what, bad) in [("out of order", swapped), ("a mistyped argument", wrong), ("an unknown event", unknown), ("a seq out of range", far)] {
        let mut fresh = App::build_empty(src);
        assert!(replay(&mut fresh.engine, &fresh.env, &fresh.events, &bad, true).is_err(), "a log with {what} replayed");
    }
}
