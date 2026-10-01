//! `Diagnostic::render_file`: the `file:line:col` rendering the CLI prints.

use rex::diagnostic::Diagnostic;
use rex::span::Span;

#[test]
fn points_at_the_span_with_a_path_and_gutter() {
    let src = "entity A { x: Int }\nlet y = A . .zz\n";
    let at = src.find(".zz").unwrap();
    let d = Diagnostic::error(Span::new(at, at + 3), "unknown field `zz`");
    assert_eq!(
        d.render_file(src, "a.rex"),
        "error: unknown field `zz`\n --> a.rex:2:13\n  |\n2 | let y = A . .zz\n  |             ^^^"
    );
}

#[test]
fn the_gutter_widens_with_the_line_number() {
    let src = "\n".repeat(11) + "oops\n";
    let d = Diagnostic::warning(Span::new(11, 15), "careful");
    let out = d.render_file(&src, "w.rex");
    assert!(out.starts_with("warning: careful\n  --> w.rex:12:1\n   |\n12 | oops\n   | ^^^^"), "{out}");
}

#[test]
fn a_multi_line_span_is_underlined_to_the_end_of_its_first_line() {
    let src = "view main = ul {\n  x\n}\n";
    let d = Diagnostic::error(Span::new(5, src.len()), "bad");
    let out = d.render_file(src, "m.rex");
    assert!(out.ends_with("1 | view main = ul {\n  |      ^^^^^^^^^^^"), "{out}");
}

#[test]
fn tabs_are_kept_in_the_caret_padding() {
    let src = "\tlet x";
    let d = Diagnostic::error(Span::new(1, 4), "t");
    assert!(d.render_file(src, "t.rex").ends_with("| \t^^^"), "{}", d.render_file(src, "t.rex"));
}

#[test]
fn a_span_past_the_end_of_the_source_does_not_panic() {
    let d = Diagnostic::error(Span::new(3, 99), "eof");
    let out = d.render_file("abc", "e.rex");
    assert!(out.contains("e.rex:1:4"), "{out}");
}
