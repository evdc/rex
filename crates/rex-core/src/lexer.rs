//! Hand-written lexer: `&str` -> `Vec<Token>`. ASCII-only surface syntax.
//! Errors are collected as diagnostics; lexing recovers and continues so a
//! single pass reports as much as possible. The stream always ends with `Eof`.

use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub struct LexResult {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lex(src: &str) -> LexResult {
    Lexer::new(src).run()
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Lexer<'a> {
        Lexer {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            tokens: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn run(mut self) -> LexResult {
        while self.pos < self.bytes.len() {
            self.skip_trivia();
            if self.pos >= self.bytes.len() {
                break;
            }
            self.lex_token();
        }
        let end = self.bytes.len();
        self.tokens.push(Token::new(TokenKind::Eof, Span::point(end)));
        LexResult {
            tokens: self.tokens,
            diagnostics: self.diagnostics,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token::new(kind, Span::new(start, self.pos)));
    }

    /// Skip whitespace and `// ...` line comments.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => {
                    self.pos += 1;
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn lex_token(&mut self) {
        let start = self.pos;
        let b = self.peek().expect("lex_token called at EOF");

        match b {
            b'0'..=b'9' => self.lex_number(start),
            b'"' => self.lex_string(start),
            b'@' => self.lex_atom(start),
            c if is_ident_start(c) => self.lex_ident(start),
            _ => self.lex_operator(start),
        }
    }

    fn lex_ident(&mut self, start: usize) {
        while let Some(b) = self.peek() {
            if is_ident_continue(b) {
                self.pos += 1;
            } else {
                break;
            }
        }
        let word = &self.src[start..self.pos];
        let kind = TokenKind::keyword(word).unwrap_or_else(|| TokenKind::Ident(word.to_string()));
        self.push(kind, start);
    }

    fn lex_atom(&mut self, start: usize) {
        self.pos += 1; // consume '@'
        let name_start = self.pos;
        while let Some(b) = self.peek() {
            if is_ident_continue(b) {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == name_start {
            self.diagnostics.push(Diagnostic::error(
                Span::new(start, self.pos),
                "expected an atom name after `@`",
            ));
            return;
        }
        let name = self.src[name_start..self.pos].to_string();
        self.push(TokenKind::Atom(name), start);
    }

    /// Numbers: `Int`, `Decimal` (digits `.` digits), or `Date` (digits `-` digits `-` digits).
    fn lex_number(&mut self, start: usize) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }

        // Date: `\d+-\d+-\d+`
        if self.peek() == Some(b'-')
            && matches!(self.peek_at(1), Some(b'0'..=b'9'))
        {
            let int_end = self.pos;
            self.pos += 1; // '-'
            let month_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.peek() == Some(b'-') && matches!(self.peek_at(1), Some(b'0'..=b'9')) {
                let month_end = self.pos;
                self.pos += 1; // '-'
                let day_start = self.pos;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
                let year = self.src[start..int_end].parse::<i32>().unwrap_or(0);
                let month = self.src[month_start..month_end].parse::<u32>().unwrap_or(0);
                let day = self.src[day_start..self.pos].parse::<u32>().unwrap_or(0);
                self.push(TokenKind::Date { year, month, day }, start);
                return;
            }
            // Not a date shape; roll back to just the leading integer.
            self.pos = int_end;
            let text = &self.src[start..self.pos];
            self.push_int(text, start);
            return;
        }

        // Decimal: digits `.` digits
        if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(b'0'..=b'9')) {
            self.pos += 1; // '.'
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            let text = self.src[start..self.pos].to_string();
            self.push(TokenKind::Decimal(text), start);
            return;
        }

        let text = &self.src[start..self.pos];
        self.push_int(text, start);
    }

    fn push_int(&mut self, text: &str, start: usize) {
        match text.parse::<i64>() {
            Ok(n) => self.push(TokenKind::Int(n), start),
            Err(_) => {
                self.diagnostics.push(Diagnostic::error(
                    Span::new(start, self.pos),
                    format!("integer literal `{text}` is out of range"),
                ));
            }
        }
    }

    fn lex_string(&mut self, start: usize) {
        self.pos += 1; // opening quote
        let mut value = String::new();
        loop {
            match self.bump() {
                None => {
                    self.diagnostics.push(Diagnostic::error(
                        Span::new(start, self.pos),
                        "unterminated string literal",
                    ));
                    break;
                }
                Some(b'"') => break,
                Some(b'\\') => match self.bump() {
                    Some(b'"') => value.push('"'),
                    Some(b'\\') => value.push('\\'),
                    Some(b'n') => value.push('\n'),
                    Some(b't') => value.push('\t'),
                    Some(other) => {
                        self.diagnostics.push(Diagnostic::error(
                            Span::new(self.pos - 2, self.pos),
                            format!("unknown escape `\\{}`", other as char),
                        ));
                        value.push(other as char);
                    }
                    None => {
                        self.diagnostics.push(Diagnostic::error(
                            Span::new(start, self.pos),
                            "unterminated string literal",
                        ));
                        break;
                    }
                },
                // Multibyte UTF-8: decode the whole codepoint from the source,
                // not byte-by-byte (`byte as char` is Latin-1 and would split a
                // char like `×` into two mojibake codepoints).
                Some(other) if other >= 0x80 => {
                    let ch = self.decode_char_at(self.pos - 1, other);
                    value.push(ch);
                }
                Some(other) => value.push(other as char),
            }
        }
        self.push(TokenKind::Str(value), start);
    }

    fn lex_operator(&mut self, start: usize) {
        let b = self.bump().unwrap();
        let kind = match b {
            b'-' if self.peek() == Some(b'>') => {
                self.pos += 1;
                TokenKind::Arrow
            }
            b'+' if self.peek() == Some(b'+') => {
                self.pos += 1;
                TokenKind::PlusPlus
            }
            b'!' if self.peek() == Some(b'=') => {
                self.pos += 1;
                TokenKind::Ne
            }
            b'<' if self.peek() == Some(b'=') => {
                self.pos += 1;
                TokenKind::Le
            }
            b'>' if self.peek() == Some(b'=') => {
                self.pos += 1;
                TokenKind::Ge
            }
            b':' if self.peek() == Some(b'=') => {
                self.pos += 1;
                TokenKind::ColonEq
            }
            b'.' => TokenKind::Dot,
            b',' => TokenKind::Comma,
            b';' => TokenKind::Semi,
            b':' => TokenKind::Colon,
            b'~' => TokenKind::Tilde,
            b'+' => TokenKind::Plus,
            b'-' => TokenKind::Minus,
            b'/' => TokenKind::Slash,
            b'%' => TokenKind::Percent,
            b'|' => TokenKind::Bar,
            b'&' => TokenKind::Amp,
            b'*' => TokenKind::Star,
            b'=' if self.peek() == Some(b'>') => {
                self.pos += 1;
                TokenKind::FatArrow
            }
            b'=' => TokenKind::Eq,
            b'<' => TokenKind::Lt,
            b'>' => TokenKind::Gt,
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            other => {
                let ch = self.decode_char_at(start, other);
                self.diagnostics.push(Diagnostic::error(
                    Span::new(start, self.pos),
                    format!("unexpected character `{ch}`"),
                ));
                return;
            }
        };
        self.push(kind, start);
    }

    /// Decode a possibly-multibyte UTF-8 character starting at `start` for the
    /// error message, advancing `pos` past it so lexing continues cleanly.
    fn decode_char_at(&mut self, start: usize, first_byte: u8) -> char {
        if first_byte < 0x80 {
            return first_byte as char;
        }
        // Re-decode from the source string to skip the whole char.
        if let Some(ch) = self.src[start..].chars().next() {
            self.pos = start + ch.len_utf8();
            ch
        } else {
            first_byte as char
        }
    }
}

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

fn is_ident_continue(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}
