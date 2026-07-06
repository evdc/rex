//! Lexer tests: assert token streams (kinds) for each lexical form.

use rex::lex;
use rex::token::TokenKind::{self, *};

/// Lex `src`, asserting there are no diagnostics, and return the token kinds
/// without the trailing `Eof`.
fn kinds(src: &str) -> Vec<TokenKind> {
    let result = lex(src);
    assert!(
        result.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        result.diagnostics
    );
    let mut ks: Vec<TokenKind> = result.tokens.into_iter().map(|t| t.kind).collect();
    assert_eq!(ks.pop(), Some(Eof), "stream must end with Eof");
    ks
}

#[test]
fn empty_input_is_just_eof() {
    let result = lex("");
    assert_eq!(result.tokens.len(), 1);
    assert_eq!(result.tokens[0].kind, Eof);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn whitespace_and_comments_are_trivia() {
    assert_eq!(kinds("   \n\t  "), vec![]);
    assert_eq!(kinds("// a comment\n"), vec![]);
    assert_eq!(kinds("42 // trailing\n"), vec![Int(42)]);
}

#[test]
fn integers() {
    assert_eq!(kinds("0 42 1000"), vec![Int(0), Int(42), Int(1000)]);
}

#[test]
fn decimals() {
    assert_eq!(
        kinds("9.99 24.50"),
        vec![Decimal("9.99".into()), Decimal("24.50".into())]
    );
}

#[test]
fn dates() {
    assert_eq!(
        kinds("2026-01-15"),
        vec![Date { year: 2026, month: 1, day: 15 }]
    );
}

#[test]
fn strings_with_escapes() {
    assert_eq!(kinds(r#""Widget""#), vec![Str("Widget".into())]);
    assert_eq!(kinds(r#""a\"b\n""#), vec![Str("a\"b\n".into())]);
}

#[test]
fn atoms() {
    assert_eq!(
        kinds("@west @north"),
        vec![Atom("west".into()), Atom("north".into())]
    );
}

#[test]
fn identifiers_and_keywords() {
    assert_eq!(
        kinds("entity let recursive new where by distinct id in except antijoin from"),
        vec![
            KwEntity, KwLet, KwRecursive, KwNew, KwWhere, KwBy, KwDistinct, KwId, KwIn, KwExcept,
            KwAntijoin, KwFrom,
        ]
    );
    assert_eq!(
        kinds("Customer custspend sum count"),
        vec![
            Ident("Customer".into()),
            Ident("custspend".into()),
            Ident("sum".into()),
            Ident("count".into()),
        ]
    );
    // `_` is an ordinary identifier; the parser treats a lone `_` as anonymous.
    assert_eq!(kinds("_"), vec![Ident("_".into())]);
}

#[test]
fn operators_maximal_munch() {
    assert_eq!(
        kinds("-> || <= >= . , : ~ + & * = < > ( ) [ ] { }"),
        vec![
            Arrow, BarBar, Le, Ge, Dot, Comma, Colon, Tilde, Plus, Amp, Star, Eq, Lt, Gt, LParen,
            RParen, LBracket, RBracket, LBrace, RBrace,
        ]
    );
}

#[test]
fn field_path_tokens() {
    // `:product.price` lexes as Colon Ident Dot Ident; the parser assembles the path.
    assert_eq!(
        kinds(":product.price"),
        vec![Colon, Ident("product".into()), Dot, Ident("price".into())]
    );
}

#[test]
fn multiply_vs_decimal_dot() {
    // `:qty * :product.price`
    assert_eq!(
        kinds(":qty * :product.price"),
        vec![
            Colon,
            Ident("qty".into()),
            Star,
            Colon,
            Ident("product".into()),
            Dot,
            Ident("price".into()),
        ]
    );
}

#[test]
fn dot_after_integer_is_compose_when_not_decimal() {
    // `3 .foo` — the dot is not followed by a digit, so it's a compose dot.
    assert_eq!(
        kinds("3 .foo"),
        vec![Int(3), Dot, Ident("foo".into())]
    );
}

#[test]
fn arrow_and_annotation() {
    assert_eq!(
        kinds("lineprice : Line -> Money"),
        vec![
            Ident("lineprice".into()),
            Colon,
            Ident("Line".into()),
            Arrow,
            Ident("Money".into()),
        ]
    );
}

#[test]
fn spans_cover_lexemes() {
    let result = lex("let x");
    assert!(result.diagnostics.is_empty());
    assert_eq!(result.tokens[0].kind, KwLet);
    assert_eq!(result.tokens[0].span.slice("let x"), "let");
    assert_eq!(result.tokens[1].kind, Ident("x".into()));
    assert_eq!(result.tokens[1].span.slice("let x"), "x");
}

#[test]
fn unterminated_string_reports_and_recovers() {
    let result = lex(r#""oops"#);
    assert!(!result.diagnostics.is_empty());
    assert!(result.diagnostics[0].message.contains("unterminated"));
}

#[test]
fn unexpected_character_reports() {
    let result = lex("a $ b");
    assert_eq!(result.diagnostics.len(), 1);
    assert!(result.diagnostics[0].message.contains("unexpected character"));
    // recovery: the tokens on either side still lex
    let kinds: Vec<_> = result.tokens.iter().map(|t| &t.kind).collect();
    assert!(kinds.contains(&&Ident("a".into())));
    assert!(kinds.contains(&&Ident("b".into())));
}

#[test]
fn lexes_spec12_fixture_without_diagnostics() {
    let src = include_str!("fixtures/spec12.rex");
    let result = lex(src);
    assert!(
        result.diagnostics.is_empty(),
        "diagnostics: {:?}",
        result.diagnostics
    );
    // Sanity: the fixture has plenty of tokens and ends with Eof.
    assert!(result.tokens.len() > 100);
    assert_eq!(result.tokens.last().unwrap().kind, Eof);
}
