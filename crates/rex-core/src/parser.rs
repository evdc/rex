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

    /// Is the cursor on a contextual keyword (a bare identifier `word`)?
    /// The view surface reuses ordinary identifiers (`order`, `select`, `on`,
    /// `delete`) as keywords only inside view bodies, so the base expression
    /// language keeps them usable as field/entity names.
    fn at_kw(&self, word: &str) -> bool {
        matches!(self.peek(), TokenKind::Ident(s) if s == word)
    }

    fn eat_kw(&mut self, word: &str) -> bool {
        if self.at_kw(word) {
            self.bump();
            true
        } else {
            false
        }
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
        while !self.at_eof()
            && !matches!(
                self.peek(),
                TokenKind::KwLet
                    | TokenKind::KwEntity
                    | TokenKind::KwView
                    | TokenKind::KwState
                    | TokenKind::KwRel
            )
        {
            self.bump();
        }
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        match self.peek() {
            TokenKind::KwEntity => self.parse_entity().map(Stmt::Entity),
            TokenKind::KwLet => self.parse_let().map(Stmt::Let),
            TokenKind::KwView => self.parse_view().map(Stmt::View),
            TokenKind::KwState => self.parse_state().map(Stmt::State),
            TokenKind::KwRel => self.parse_rel().map(Stmt::Rel),
            other => self.error(format!(
                "expected a statement (`entity`, `rel`, `let`, `view`, or `state`), found {}",
                other.describe()
            )),
        }
    }

    // --- views ------------------------------------------------------------

    fn parse_view(&mut self) -> PResult<ViewDecl> {
        let start = self.span();
        self.bump(); // `view`
        let (name, _) = self.expect_ident("a view name after `view`")?;
        self.expect(&TokenKind::Eq, "before the view body")?;
        let body = self.parse_select()?;
        Ok(ViewDecl {
            name,
            body,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_state(&mut self) -> PResult<StateDecl> {
        let start = self.span();
        self.bump(); // `state`
        let (name, _) = self.expect_ident("a state name after `state`")?;
        self.expect(&TokenKind::Colon, "before the state type")?;
        let ty = self.parse_type()?;
        self.expect(&TokenKind::Eq, "before the state default")?;
        let default = self.parse_expr(0)?;
        Ok(StateDecl {
            name,
            ty,
            default,
            span: start.to(self.prev_span()),
        })
    }

    /// `Entity [where p (& p)*] [order by :path] select <element>`.
    fn parse_select(&mut self) -> PResult<SelectExpr> {
        let start = self.span();
        let (entity, _) = self.expect_ident("an entity name to start a `select`")?;
        // Optional `as l` row-binder alias (disambiguates the row from the entity).
        let binder = if self.eat_kw("as") {
            Some(self.expect_ident("a binder name after `as`")?.0)
        } else {
            None
        };
        let mut wheres = Vec::new();
        if self.eat(&TokenKind::KwWhere) {
            loop {
                wheres.push(self.parse_expr(BP_WHERE_BY)?);
                if !self.eat(&TokenKind::Amp) {
                    break;
                }
            }
        }
        let order_by = if self.eat_kw("order") {
            self.expect(&TokenKind::KwBy, "after `order`")?;
            Some(self.parse_field_parts()?)
        } else {
            None
        };
        if !self.eat_kw("select") {
            return self.error(format!(
                "expected `select` before the view element, found {}",
                self.peek().describe()
            ));
        }
        let body = self.parse_element()?;
        Ok(SelectExpr {
            entity,
            binder,
            wheres,
            order_by,
            body,
            span: start.to(self.prev_span()),
        })
    }

    /// A bare field path `:a.b` returning just its parts (for `order by`).
    fn parse_field_parts(&mut self) -> PResult<Vec<String>> {
        self.expect(&TokenKind::Colon, "a `:field` path")?;
        let (first, _) = self.expect_ident("a field name after `:`")?;
        let mut parts = vec![first];
        while matches!(self.peek(), TokenKind::Dot)
            && matches!(self.peek_at(1), TokenKind::Ident(_))
        {
            self.bump();
            let (part, _) = self.expect_ident("a field name after `.`")?;
            parts.push(part);
        }
        Ok(parts)
    }

    /// `tag ('.' class)* item* block?` where an item is an attr, bare modifier,
    /// handler, or text, and the block holds nested content.
    fn parse_element(&mut self) -> PResult<ElementExpr> {
        let start = self.span();
        let (tag, _) = self.expect_ident("an element tag")?;
        let mut classes = Vec::new();
        while self.eat(&TokenKind::Dot) {
            let (c, _) = self.expect_ident("a class name after `.`")?;
            classes.push(c);
        }
        // Bare-ident modifiers appear only here, right after the tag/classes
        // (`section.list dropTarget`, `div.card draggable`). Restricting them to
        // this position keeps a later bare ident (a sibling tag) from being
        // swallowed by an unbraced element like `input value=… on change=…`.
        let mut modifiers = Vec::new();
        while let TokenKind::Ident(name) = self.peek().clone() {
            let is_modifier = name != "on"
                && !matches!(self.peek_at(1), TokenKind::Eq)
                && !(name == "class" && matches!(self.peek_at(1), TokenKind::Dot));
            if !is_modifier {
                break;
            }
            self.bump();
            modifiers.push(name);
        }

        let mut attrs = Vec::new();
        let mut handlers = Vec::new();
        let mut children = Vec::new();
        loop {
            match self.peek().clone() {
                TokenKind::Str(s) => {
                    self.bump();
                    children.push(Content::Text(s));
                }
                TokenKind::LBrace => {
                    self.parse_element_block(&mut handlers, &mut children)?;
                    break; // the block is always the last part of an element
                }
                TokenKind::Ident(name) if name == "on" => {
                    handlers.push(self.parse_handler()?);
                }
                TokenKind::Ident(name) if name == "class" && matches!(self.peek_at(1), TokenKind::Dot) => {
                    attrs.push(self.parse_class_attr()?);
                }
                TokenKind::Ident(name) if matches!(self.peek_at(1), TokenKind::Eq) => {
                    attrs.push(self.parse_attr(name)?);
                }
                // A bare ident here is the next sibling's tag — this element ends.
                _ => break,
            }
        }
        Ok(ElementExpr {
            tag,
            classes,
            modifiers,
            attrs,
            handlers,
            children,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_attr(&mut self, name: String) -> PResult<AttrBind> {
        let start = self.span();
        self.bump(); // name ident
        self.bump(); // `=`
        let value = self.parse_attr_value()?;
        Ok(AttrBind {
            name,
            value,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_class_attr(&mut self) -> PResult<AttrBind> {
        let start = self.span();
        self.bump(); // `class`
        self.bump(); // `.`
        let (cls, _) = self.expect_ident("a class name after `class.`")?;
        self.expect(&TokenKind::Eq, "after the class toggle name")?;
        let value = self.parse_attr_value()?;
        Ok(AttrBind {
            name: format!("class.{cls}"),
            value,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_attr_value(&mut self) -> PResult<AttrValue> {
        match self.peek().clone() {
            TokenKind::Str(s) => {
                self.bump();
                Ok(AttrValue::Static(s))
            }
            TokenKind::Colon => Ok(AttrValue::Bind(self.parse_field_parts()?)),
            other => self.error(format!(
                "expected a string or `:field` for an attribute value, found {}",
                other.describe()
            )),
        }
    }

    /// The `{ ... }` block: nested elements, selects, `:field` text, static
    /// text, or handlers on the enclosing element.
    fn parse_element_block(
        &mut self,
        handlers: &mut Vec<HandlerDecl>,
        children: &mut Vec<Content>,
    ) -> PResult<()> {
        self.bump(); // `{`
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            match self.peek().clone() {
                TokenKind::Ident(name) if name == "on" => {
                    handlers.push(self.parse_handler()?);
                }
                TokenKind::Colon => children.push(Content::Bind(self.parse_field_parts()?)),
                TokenKind::Str(s) => {
                    self.bump();
                    children.push(Content::Text(s));
                }
                TokenKind::Ident(_) if self.looks_like_select() => {
                    children.push(Content::Select(Box::new(self.parse_select()?)));
                }
                TokenKind::Ident(_) => {
                    children.push(Content::Element(self.parse_element()?));
                }
                other => {
                    return self.error(format!("unexpected {} in element body", other.describe()));
                }
            }
        }
        self.expect(&TokenKind::RBrace, "to close the element body")?;
        Ok(())
    }

    /// A content ident begins a `select` (not a plain element) when it is
    /// immediately followed by `as` (an alias), `where`, `order`, or `select`.
    fn looks_like_select(&self) -> bool {
        matches!(self.peek_at(1), TokenKind::KwWhere)
            || matches!(self.peek_at(1), TokenKind::Ident(s) if s == "as" || s == "order" || s == "select")
    }

    /// `on event('.'mod)* ['(' params ')'] '=>' mutation (';' mutation)*`.
    fn parse_handler(&mut self) -> PResult<HandlerDecl> {
        let start = self.span();
        self.bump(); // `on` (contextual keyword)
        let (event, _) = self.expect_ident("a DOM event name after `on`")?;
        let mut modifiers = Vec::new();
        while self.eat(&TokenKind::Dot) {
            let (m, _) = self.expect_ident("an event modifier after `.`")?;
            modifiers.push(m);
        }
        let mut params = Vec::new();
        if self.eat(&TokenKind::LParen) {
            if !matches!(self.peek(), TokenKind::RParen) {
                loop {
                    params.push(self.parse_handler_param()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect(&TokenKind::RParen, "to close the handler parameters")?;
        }
        self.expect(&TokenKind::FatArrow, "before the handler body")?;
        let mut body = vec![self.parse_mutation()?];
        while self.eat(&TokenKind::Semi) {
            body.push(self.parse_mutation()?);
        }
        Ok(HandlerDecl {
            event,
            modifiers,
            params,
            body,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_handler_param(&mut self) -> PResult<HandlerParam> {
        let start = self.span();
        let (name, _) = self.expect_ident("a handler parameter name")?;
        self.expect(&TokenKind::Colon, "after the parameter name")?;
        let ty = self.parse_type()?;
        self.expect(&TokenKind::Eq, "before the parameter extractor")?;
        let extractor = self.parse_extractor()?;
        Ok(HandlerParam {
            name,
            ty,
            extractor,
            span: start.to(self.prev_span()),
        })
    }

    fn parse_extractor(&mut self) -> PResult<Extractor> {
        let (name, span) = self.expect_ident("an extractor (value, checked, drag, ...)")?;
        let arg = if self.eat(&TokenKind::LParen) {
            let a = match self.peek().clone() {
                TokenKind::Str(s) => {
                    self.bump();
                    s
                }
                TokenKind::Ident(s) => {
                    self.bump();
                    s
                }
                other => {
                    return self
                        .error(format!("expected an extractor argument, found {}", other.describe()))
                }
            };
            self.expect(&TokenKind::RParen, "to close the extractor argument")?;
            Some(a)
        } else {
            None
        };
        match (name.as_str(), arg) {
            ("value", None) => Ok(Extractor::Value),
            ("checked", None) => Ok(Extractor::Checked),
            ("drag", Some(a)) => Ok(Extractor::Drag(a)),
            ("dropPos", Some(a)) => Ok(Extractor::DropPos(a)),
            ("endOf", Some(a)) => Ok(Extractor::EndOf(a)),
            ("prompt", Some(a)) => Ok(Extractor::Prompt(a)),
            (other, _) => {
                self.diagnostics.push(Diagnostic::error(
                    span,
                    format!("unknown extractor `{other}`"),
                ));
                Err(Bail)
            }
        }
    }

    fn parse_mutation(&mut self) -> PResult<Mutation> {
        let start = self.span();
        if self.at_kw("delete") {
            self.bump();
            let (target, _) = self.expect_ident("a binder to `delete`")?;
            return Ok(Mutation::Delete {
                target,
                span: start.to(self.prev_span()),
            });
        }
        match self.peek().clone() {
            TokenKind::KwNew => {
                let e = self.parse_new(start)?;
                let ExprKind::New { entity, fields } = e.kind else {
                    unreachable!("parse_new yields New")
                };
                Ok(Mutation::New {
                    entity,
                    fields,
                    span: start.to(self.prev_span()),
                })
            }
            // `:field := value` — set a field of `self`.
            TokenKind::Colon => {
                let field = self.parse_field_parts()?;
                self.expect(&TokenKind::ColonEq, "in a field assignment")?;
                let value = self.parse_expr(BP_ARG)?;
                Ok(Mutation::Set {
                    binder: None,
                    field,
                    value,
                    span: start.to(self.prev_span()),
                })
            }
            // `binder:field := value`.
            TokenKind::Ident(binder) => {
                self.bump();
                let field = self.parse_field_parts()?;
                self.expect(&TokenKind::ColonEq, "in a field assignment")?;
                let value = self.parse_expr(BP_ARG)?;
                Ok(Mutation::Set {
                    binder: Some(binder),
                    field,
                    value,
                    span: start.to(self.prev_span()),
                })
            }
            other => self.error(format!("expected a mutation, found {}", other.describe())),
        }
    }

    /// `rel Name(From, To)` — a named binary relation `From -> To`.
    fn parse_rel(&mut self) -> PResult<RelDecl> {
        let start = self.span();
        self.bump(); // `rel`
        let (name, _) = self.expect_ident("a relation name after `rel`")?;
        self.expect(&TokenKind::LParen, "after the relation name")?;
        let (from, _) = self.expect_ident("the source entity")?;
        self.expect(&TokenKind::Comma, "between the relation's two entities")?;
        let (to, _) = self.expect_ident("the target entity")?;
        let end = self.expect(&TokenKind::RParen, "to close the relation")?;
        Ok(RelDecl {
            name,
            from,
            to,
            span: start.to(end.span),
        })
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
        let recursive = self.eat(&TokenKind::KwRecursive);
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
            recursive,
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
            kind @ (TokenKind::KwFst | TokenKind::KwSnd) => {
                let side = if kind == TokenKind::KwFst {
                    crate::ast::ProjSide::Fst
                } else {
                    crate::ast::ProjSide::Snd
                };
                self.bump();
                let operand = self.parse_expr(RBP_INVERSE)?;
                Ok(self.mk_to(ExprKind::Proj(side, Box::new(operand)), start))
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
