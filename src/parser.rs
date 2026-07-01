//! Hand-written parser: `Vec<Token>` -> `Program`.
//!
//! Statements are parsed by recursive descent; expressions by a Pratt (binding
//! power) parser. Diagnostics accumulate; on error the statement loop recovers
//! by skipping to the next `let`/`entity`.
//!
//! Precedence (loosest -> tightest), as binding powers:
//!   fork `,`(10) < union/intersect/except/antijoin(20) < where/by(30)
//!   < comparison/`in`(40) < `*` `||`(50) < compose `.`(60)
//!   < prefix `~`(70)/`distinct`(35) < postfix `[]` and call `()`(90)
//! Field paths (`:a.b.c`) are lexically greedy primaries, so hops inside a path
//! always bind tighter than any operator.

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::lexer;
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub struct ParseResult {
    pub program: Program,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lex and parse a source string in one step.
pub fn parse(src: &str) -> ParseResult {
    let lexed = lexer::lex(src);
    let mut parser = Parser::new(lexed.tokens, lexed.diagnostics);
    let program = parser.parse_program();
    ParseResult {
        program,
        diagnostics: parser.diagnostics,
    }
}

/// A parse failure whose diagnostic has already been recorded.
struct Bail;
type PResult<T> = Result<T, Bail>;

// Binding powers for infix operators.
const BP_FORK: u8 = 10;
const BP_SET: u8 = 20; // union, intersect, except, antijoin
const BP_WHERE_BY: u8 = 30;
const BP_CMP: u8 = 40; // comparisons and `in`
const BP_MUL: u8 = 50; // `*`, `||`
const BP_COMPOSE: u8 = 60;
const BP_POSTFIX: u8 = 90; // `[]` restrict, call `()`

// Right binding powers for prefix operators.
const RBP_DISTINCT: u8 = 35;
const RBP_CMP_PREFIX: u8 = 45;
const RBP_INVERSE: u8 = 70;

/// Minimum binding power for a call/`new` argument or field value: above fork so
/// that a top-level `,` separates arguments rather than building a fork.
const BP_ARG: u8 = BP_FORK;

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
}

impl Parser {
    fn new(tokens: Vec<Token>, diagnostics: Vec<Diagnostic>) -> Parser {
        Parser {
            tokens,
            pos: 0,
            diagnostics,
        }
    }

    // --- token cursor -----------------------------------------------------

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_at(&self, offset: usize) -> &TokenKind {
        let i = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[i].kind
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if !self.at_eof() {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek() == kind {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: &TokenKind, context: &str) -> PResult<Token> {
        if self.peek() == kind {
            Ok(self.bump())
        } else {
            self.error(format!(
                "expected {} {}, found {}",
                kind.describe(),
                context,
                self.peek().describe()
            ))
        }
    }

    fn expect_ident(&mut self, context: &str) -> PResult<(String, Span)> {
        let span = self.span();
        match self.peek().clone() {
            TokenKind::Ident(name) => {
                self.bump();
                Ok((name, span))
            }
            other => self.error(format!("expected {}, found {}", context, other.describe())),
        }
    }

    fn error<T>(&mut self, message: impl Into<String>) -> PResult<T> {
        self.diagnostics
            .push(Diagnostic::error(self.span(), message.into()));
        Err(Bail)
    }

    // --- program & statements --------------------------------------------

    fn parse_program(&mut self) -> Program {
        let mut stmts = Vec::new();
        while !self.at_eof() {
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(Bail) => self.recover(),
            }
        }
        Program { stmts }
    }

    /// Skip tokens until the start of the next statement (or EOF).
    fn recover(&mut self) {
        while !self.at_eof() && !matches!(self.peek(), TokenKind::KwLet | TokenKind::KwEntity) {
            self.bump();
        }
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        match self.peek() {
            TokenKind::KwEntity => self.parse_entity().map(Stmt::Entity),
            TokenKind::KwLet => self.parse_let().map(Stmt::Let),
            other => self.error(format!(
                "expected a statement (`entity` or `let`), found {}",
                other.describe()
            )),
        }
    }

    fn parse_entity(&mut self) -> PResult<EntityDecl> {
        let start = self.span();
        self.bump(); // `entity`
        let (name, _) = self.expect_ident("an entity name")?;
        self.expect(&TokenKind::LBrace, "to open the entity body")?;

        let mut fields = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let field_start = self.span();
            let (fname, _) = self.expect_ident("a field name")?;
            self.expect(&TokenKind::Colon, "after the field name")?;
            let ty = self.parse_type()?;
            let span = field_start.to(self.prev_span());
            fields.push(FieldDecl {
                name: fname,
                ty,
                span,
            });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let end = self.expect(&TokenKind::RBrace, "to close the entity body")?;
        Ok(EntityDecl {
            name,
            fields,
            span: start.to(end.span),
        })
    }

    fn parse_let(&mut self) -> PResult<LetDecl> {
        let start = self.span();
        self.bump(); // `let`
        let (raw_name, _) = self.expect_ident("a binding name after `let`")?;
        let name = if raw_name == "_" { None } else { Some(raw_name) };

        let ty = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };

        self.expect(&TokenKind::Eq, "before the definition body")?;
        let body = self.parse_expr(0)?;
        let span = start.to(self.prev_span());
        Ok(LetDecl {
            name,
            ty,
            body,
            span,
        })
    }

    // --- types ------------------------------------------------------------

    fn parse_type(&mut self) -> PResult<Type> {
        let left = self.parse_product_type()?;
        if self.eat(&TokenKind::Arrow) {
            let right = self.parse_type()?; // right-associative
            let span = left.span.to(right.span);
            Ok(Type {
                kind: TypeKind::Arrow(Box::new(left), Box::new(right)),
                span,
            })
        } else {
            Ok(left)
        }
    }

    fn parse_product_type(&mut self) -> PResult<Type> {
        let mut left = self.parse_atom_type()?;
        while self.eat(&TokenKind::Star) {
            let right = self.parse_atom_type()?;
            let span = left.span.to(right.span);
            left = Type {
                kind: TypeKind::Product(Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    fn parse_atom_type(&mut self) -> PResult<Type> {
        let span = self.span();
        match self.peek().clone() {
            TokenKind::Ident(name) => {
                self.bump();
                Ok(Type {
                    kind: TypeKind::Named(name),
                    span,
                })
            }
            TokenKind::Atom(name) => {
                self.bump();
                Ok(Type {
                    kind: TypeKind::AtomSingleton(name),
                    span,
                })
            }
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_type()?;
                let end = self.expect(&TokenKind::RParen, "to close the type")?;
                Ok(Type {
                    kind: inner.kind,
                    span: span.to(end.span),
                })
            }
            TokenKind::LBrace => self.parse_coproduct_type(span),
            other => self.error(format!("expected a type, found {}", other.describe())),
        }
    }

    fn parse_coproduct_type(&mut self, start: Span) -> PResult<Type> {
        self.bump(); // `{`
        let mut elems = Vec::new();
        elems.push(self.parse_atom_type()?);
        while self.eat(&TokenKind::Plus) {
            elems.push(self.parse_atom_type()?);
        }
        let end = self.expect(&TokenKind::RBrace, "to close the coproduct type")?;
        Ok(Type {
            kind: TypeKind::Coproduct(elems),
            span: start.to(end.span),
        })
    }

    // --- expressions (Pratt) ---------------------------------------------

    fn parse_expr(&mut self, min_bp: u8) -> PResult<Expr> {
        let mut lhs = self.parse_prefix()?;
        loop {
            let Some(lbp) = self.infix_bp() else { break };
            if lbp <= min_bp {
                break;
            }
            lhs = self.parse_infix(lhs, lbp)?;
        }
        Ok(lhs)
    }

    /// Binding power of the current token as an infix/postfix operator.
    fn infix_bp(&self) -> Option<u8> {
        Some(match self.peek() {
            TokenKind::Comma => BP_FORK,
            TokenKind::Plus | TokenKind::Amp | TokenKind::KwExcept | TokenKind::KwAntijoin => BP_SET,
            TokenKind::KwWhere | TokenKind::KwBy => BP_WHERE_BY,
            TokenKind::Eq
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::Le
            | TokenKind::Ge
            | TokenKind::KwIn => BP_CMP,
            TokenKind::Star | TokenKind::BarBar => BP_MUL,
            TokenKind::Dot | TokenKind::Colon => BP_COMPOSE,
            TokenKind::LBracket | TokenKind::LParen => BP_POSTFIX,
            _ => return None,
        })
    }

    fn parse_prefix(&mut self) -> PResult<Expr> {
        let start = self.span();
        match self.peek().clone() {
            // primaries
            TokenKind::Ident(name) => {
                self.bump();
                Ok(self.mk(ExprKind::Ident(name), start))
            }
            TokenKind::KwId => {
                self.bump();
                Ok(self.mk(ExprKind::Id, start))
            }
            TokenKind::Atom(name) => {
                self.bump();
                Ok(self.mk(ExprKind::Atom(name), start))
            }
            TokenKind::Int(n) => {
                self.bump();
                Ok(self.mk(ExprKind::Int(n), start))
            }
            TokenKind::Decimal(s) => {
                self.bump();
                Ok(self.mk(ExprKind::Decimal(s), start))
            }
            TokenKind::Str(s) => {
                self.bump();
                Ok(self.mk(ExprKind::Str(s), start))
            }
            TokenKind::Date { year, month, day } => {
                self.bump();
                Ok(self.mk(ExprKind::Date { year, month, day }, start))
            }
            TokenKind::Colon => self.parse_field_path(start),
            TokenKind::LParen => {
                self.bump();
                let mut inner = self.parse_expr(0)?;
                let end = self.expect(&TokenKind::RParen, "to close the expression")?;
                inner.span = start.to(end.span);
                Ok(inner)
            }
            TokenKind::KwNew => self.parse_new(start),

            // prefix operators
            TokenKind::Tilde => {
                self.bump();
                let operand = self.parse_expr(RBP_INVERSE)?;
                Ok(self.mk_to(ExprKind::Inverse(Box::new(operand)), start))
            }
            TokenKind::KwDistinct => {
                self.bump();
                let operand = self.parse_expr(RBP_DISTINCT)?;
                Ok(self.mk_to(ExprKind::Distinct(Box::new(operand)), start))
            }
            // prefix (filter-position) comparisons: `> 30`, `= @west`
            TokenKind::Eq | TokenKind::Lt | TokenKind::Gt | TokenKind::Le | TokenKind::Ge => {
                let op = self.cmp_op();
                self.bump();
                let rhs = self.parse_expr(RBP_CMP_PREFIX)?;
                Ok(self.mk_to(
                    ExprKind::Compare {
                        op,
                        lhs: None,
                        rhs: Box::new(rhs),
                    },
                    start,
                ))
            }
            TokenKind::KwIn => {
                self.bump();
                let rhs = self.parse_expr(RBP_CMP_PREFIX)?;
                Ok(self.mk_to(
                    ExprKind::In {
                        lhs: None,
                        rhs: Box::new(rhs),
                    },
                    start,
                ))
            }

            other => self.error(format!("expected an expression, found {}", other.describe())),
        }
    }

    fn parse_infix(&mut self, lhs: Expr, lbp: u8) -> PResult<Expr> {
        let start = lhs.span;
        match self.peek().clone() {
            TokenKind::Comma => self.binary(lhs, lbp, start, ExprKind::Fork),
            TokenKind::Plus => self.binary(lhs, lbp, start, ExprKind::Union),
            TokenKind::Amp => self.binary(lhs, lbp, start, ExprKind::Intersect),
            TokenKind::KwExcept => self.binary(lhs, lbp, start, ExprKind::Except),
            TokenKind::KwAntijoin => self.binary(lhs, lbp, start, ExprKind::Antijoin),
            TokenKind::KwWhere => self.binary(lhs, lbp, start, ExprKind::Where),
            TokenKind::KwBy => self.binary(lhs, lbp, start, ExprKind::By),
            TokenKind::Star => self.binary(lhs, lbp, start, ExprKind::Mul),
            TokenKind::BarBar => self.binary(lhs, lbp, start, ExprKind::Concat),
            TokenKind::Dot => self.binary(lhs, lbp, start, ExprKind::Compose),
            // `E:field` is sugar for `E . :field` — naming an attribute
            // relation directly off an entity/expression without a dot.
            TokenKind::Colon => {
                let field_start = self.span();
                let field = self.parse_field_path(field_start)?;
                Ok(self.mk_to(ExprKind::Compose(Box::new(lhs), Box::new(field)), start))
            }

            TokenKind::Eq | TokenKind::Lt | TokenKind::Gt | TokenKind::Le | TokenKind::Ge => {
                let op = self.cmp_op();
                self.bump();
                let rhs = self.parse_expr(lbp)?;
                Ok(self.mk_to(
                    ExprKind::Compare {
                        op,
                        lhs: Some(Box::new(lhs)),
                        rhs: Box::new(rhs),
                    },
                    start,
                ))
            }
            TokenKind::KwIn => {
                self.bump();
                let rhs = self.parse_expr(lbp)?;
                Ok(self.mk_to(
                    ExprKind::In {
                        lhs: Some(Box::new(lhs)),
                        rhs: Box::new(rhs),
                    },
                    start,
                ))
            }

            TokenKind::LBracket => {
                self.bump();
                let inner = self.parse_expr(0)?;
                self.expect(&TokenKind::RBracket, "to close the restriction `[...]`")?;
                Ok(self.mk_to(
                    ExprKind::Restrict(Box::new(lhs), Box::new(inner)),
                    start,
                ))
            }
            TokenKind::LParen => self.parse_call(lhs, start),

            other => self.error(format!("unexpected {} in expression", other.describe())),
        }
    }

    fn binary(
        &mut self,
        lhs: Expr,
        lbp: u8,
        start: Span,
        build: fn(Box<Expr>, Box<Expr>) -> ExprKind,
    ) -> PResult<Expr> {
        self.bump(); // operator
        let rhs = self.parse_expr(lbp)?;
        Ok(self.mk_to(build(Box::new(lhs), Box::new(rhs)), start))
    }

    fn parse_call(&mut self, callee: Expr, start: Span) -> PResult<Expr> {
        let ExprKind::Ident(func) = callee.kind else {
            self.diagnostics.push(Diagnostic::error(
                callee.span,
                "only a named function (e.g. `sum`, `count`) can be called",
            ));
            return Err(Bail);
        };
        self.bump(); // `(`
        let mut args = Vec::new();
        if !matches!(self.peek(), TokenKind::RParen) {
            loop {
                args.push(self.parse_expr(BP_ARG)?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "to close the argument list")?;
        Ok(self.mk_to(ExprKind::Call { func, args }, start))
    }

    fn parse_field_path(&mut self, start: Span) -> PResult<Expr> {
        self.bump(); // `:`
        let (first, _) = self.expect_ident("a field name after `:`")?;
        let mut parts = vec![first];
        // Greedily consume `.field` hops (only when a `.` is followed by an identifier).
        while matches!(self.peek(), TokenKind::Dot)
            && matches!(self.peek_at(1), TokenKind::Ident(_))
        {
            self.bump(); // `.`
            let (part, _) = self.expect_ident("a field name after `.`")?;
            parts.push(part);
        }
        Ok(self.mk_to(ExprKind::FieldPath(parts), start))
    }

    fn parse_new(&mut self, start: Span) -> PResult<Expr> {
        self.bump(); // `new`
        let (entity, _) = self.expect_ident("an entity name after `new`")?;
        self.expect(&TokenKind::LBrace, "to open the creation body")?;
        let mut fields = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let field_start = self.span();
            let (name, _) = self.expect_ident("a field name")?;
            self.expect(&TokenKind::Colon, "after the field name")?;
            let value = self.parse_expr(BP_ARG)?;
            let span = field_start.to(self.prev_span());
            fields.push(FieldInit { name, value, span });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "to close the creation body")?;
        Ok(self.mk_to(ExprKind::New { entity, fields }, start))
    }

    // --- helpers ----------------------------------------------------------

    fn cmp_op(&self) -> CmpOp {
        match self.peek() {
            TokenKind::Eq => CmpOp::Eq,
            TokenKind::Lt => CmpOp::Lt,
            TokenKind::Gt => CmpOp::Gt,
            TokenKind::Le => CmpOp::Le,
            TokenKind::Ge => CmpOp::Ge,
            _ => unreachable!("cmp_op called on non-comparison token"),
        }
    }

    /// Build an expression spanning exactly the given start token.
    fn mk(&self, kind: ExprKind, start: Span) -> Expr {
        Expr { kind, span: start }
    }

    /// Build an expression spanning from `start` through the previous token.
    fn mk_to(&self, kind: ExprKind, start: Span) -> Expr {
        Expr {
            kind,
            span: start.to(self.prev_span()),
        }
    }
}
