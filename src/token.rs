//! Tokens produced by the lexer. Keywords get dedicated variants for a clean
//! parser; aggregation names (`sum`, `count`, ...) stay as plain `Ident`s and
//! are parsed as generic calls.

use crate::span::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Token {
        Token { kind, span }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    // Names & literals
    Ident(String),
    Atom(String), // `@west` -> "west"
    Int(i64),
    Decimal(String), // exact text preserved, e.g. "9.99"
    Str(String),     // unescaped contents
    Date { year: i32, month: u32, day: u32 },

    // Keywords
    KwEntity,
    KwLet,
    KwRecursive,
    KwNew,
    KwWhere,
    KwBy,
    KwDistinct,
    KwId,
    KwIn,
    KwExcept,
    KwAntijoin,
    KwFrom,

    // Operators & punctuation
    Arrow,   // ->
    Dot,     // .
    Comma,   // ,
    Colon,   // :
    Tilde,   // ~
    Plus,    // +
    Amp,     // &
    Star,    // *
    BarBar,  // ||
    Eq,      // =
    Lt,      // <
    Gt,      // >
    Le,      // <=
    Ge,      // >=
    LParen,  // (
    RParen,  // )
    LBracket, // [
    RBracket, // ]
    LBrace,  // {
    RBrace,  // }

    Eof,
}

impl TokenKind {
    /// Map an identifier lexeme to its keyword kind, if any.
    pub fn keyword(word: &str) -> Option<TokenKind> {
        Some(match word {
            "entity" => TokenKind::KwEntity,
            "let" => TokenKind::KwLet,
            "recursive" => TokenKind::KwRecursive,
            "new" => TokenKind::KwNew,
            "where" => TokenKind::KwWhere,
            "by" => TokenKind::KwBy,
            "distinct" => TokenKind::KwDistinct,
            "id" => TokenKind::KwId,
            "in" => TokenKind::KwIn,
            "except" => TokenKind::KwExcept,
            "antijoin" => TokenKind::KwAntijoin,
            "from" => TokenKind::KwFrom,
            _ => return None,
        })
    }

    /// A short human-readable name for diagnostics.
    pub fn describe(&self) -> String {
        use TokenKind::*;
        match self {
            Ident(s) => format!("identifier `{s}`"),
            Atom(s) => format!("atom `@{s}`"),
            Int(n) => format!("integer `{n}`"),
            Decimal(s) => format!("decimal `{s}`"),
            Str(_) => "string literal".to_string(),
            Date { year, month, day } => format!("date `{year:04}-{month:02}-{day:02}`"),
            KwEntity => "`entity`".into(),
            KwLet => "`let`".into(),
            KwRecursive => "`recursive`".into(),
            KwNew => "`new`".into(),
            KwWhere => "`where`".into(),
            KwBy => "`by`".into(),
            KwDistinct => "`distinct`".into(),
            KwId => "`id`".into(),
            KwIn => "`in`".into(),
            KwExcept => "`except`".into(),
            KwAntijoin => "`antijoin`".into(),
            KwFrom => "`from`".into(),
            Arrow => "`->`".into(),
            Dot => "`.`".into(),
            Comma => "`,`".into(),
            Colon => "`:`".into(),
            Tilde => "`~`".into(),
            Plus => "`+`".into(),
            Amp => "`&`".into(),
            Star => "`*`".into(),
            BarBar => "`||`".into(),
            Eq => "`=`".into(),
            Lt => "`<`".into(),
            Gt => "`>`".into(),
            Le => "`<=`".into(),
            Ge => "`>=`".into(),
            LParen => "`(`".into(),
            RParen => "`)`".into(),
            LBracket => "`[`".into(),
            RBracket => "`]`".into(),
            LBrace => "`{`".into(),
            RBrace => "`}`".into(),
            Eof => "end of input".into(),
        }
    }
}
