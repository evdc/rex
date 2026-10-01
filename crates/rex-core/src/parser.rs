//! Hand-written parser: `Vec<Token>` -> `Program`.
//!
//! Statements are parsed by recursive descent; expressions by a Pratt (binding
//! power) parser. Diagnostics accumulate; on error the statement loop recovers
//! by skipping to the next statement keyword.
//!
//! Precedence (loosest -> tightest), as binding powers:
//!   fork `,`(10) < union `|`/except/antijoin(20) < intersect `&`(25) < where/by(30)
//!   < comparison/`in`(40) < `+` `-` `++`(46) < `*` `/` `%`(50)
//!   < compose `.`(60) < prefix `~`(70)/`distinct`(35)/`not`(35)
//!   < postfix `[]` and call `()`(90)
//! A leading field path (`.a.b.c`) is a lexically greedy primary, so hops
//! inside a path always bind tighter than any operator; `x.f` is ordinary
//! compose with an identifier the checker resolves as a field.

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
const BP_SET: u8 = 20; // union, except, antijoin
const BP_INTERSECT: u8 = 25; // `&` binds tighter than `|`, as in logic
const BP_WHERE_BY: u8 = 30;
const BP_CMP: u8 = 40; // comparisons and `in`
const BP_ADD: u8 = 46; // `+`, `-`, `++`
const BP_MUL: u8 = 50; // `*`, `/`, `%`
const BP_COMPOSE: u8 = 60;
const BP_POSTFIX: u8 = 90; // `[]` restrict, call `()`

// Right binding powers for prefix operators.
const RBP_DISTINCT: u8 = 35;
const RBP_NOT: u8 = 35;
const RBP_CMP_PREFIX: u8 = 45;
const RBP_INVERSE: u8 = 70;

/// Minimum binding power for a call/`new` argument or field value: above fork so
/// that a top-level `,` separates arguments rather than building a fork.
const BP_ARG: u8 = BP_FORK;

/// How deeply a program may nest: parentheses, prefix operators, the length
/// of an operator chain (`a | b | c …` is a tree as deep as it is long),
/// elements within elements. Every later pass — the checker, lowering, the
/// evaluator, even dropping the tree — recurses over the AST, so depth is
/// stack, and stack is small where Rex runs (1 MB in the browser). Past this
/// the parser reports an error instead of letting a later pass overflow.
pub const MAX_NESTING: usize = 128;

/// What an `if`/`match` costs against [`MAX_NESTING`]: each desugars to
/// several nested core forms (gates, an `except`, a union).
const BRANCH_NESTING: usize = 4;

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    /// Nesting of the construct being parsed, against [`MAX_NESTING`].
    depth: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>, diagnostics: Vec<Diagnostic>) -> Parser {
        Parser {
            tokens,
            pos: 0,
            diagnostics,
            depth: 0,
        }
    }

    /// Go `levels` deeper, or report that the program nests too far. The
    /// caller restores `depth` when its construct ends.
    fn descend(&mut self, levels: usize) -> PResult<()> {
        self.depth += levels;
        if self.depth > MAX_NESTING {
            return self.error(format!(
                "this nests more than {MAX_NESTING} levels deep; name a part of it with `let` (or a component) and refer to that"
            ));
        }
        Ok(())
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
    /// The surface reuses ordinary identifiers (`order`, `select`, `as`,
    /// `update`, `delete`, `set`, `do`, `local`, `desc`, `children`) as
    /// keywords only in specific positions, so the base expression language
    /// keeps them usable as field/entity names.
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

    fn expect_kw(&mut self, word: &str, context: &str) -> PResult<()> {
        if self.eat_kw(word) {
            Ok(())
        } else {
            self.error(format!(
                "expected `{word}` {context}, found {}",
                self.peek().describe()
            ))
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
            self.depth = 0;
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
                    | TokenKind::KwEvent
                    | TokenKind::KwOn
                    | TokenKind::KwType
                    | TokenKind::KwImport
            )
        {
            self.bump();
        }
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        match self.peek() {
            TokenKind::KwEntity => self.parse_entity().map(Stmt::Entity),
            TokenKind::KwLet => self.parse_let().map(Stmt::Let),
            TokenKind::KwView => self.parse_view().map(|v| Stmt::View(Box::new(v))),
            TokenKind::KwState => self.parse_state().map(Stmt::State),
            TokenKind::KwRel => self.parse_rel().map(Stmt::Rel),
            TokenKind::KwEvent => self.parse_event().map(Stmt::Event),
            TokenKind::KwOn => self.parse_on().map(Stmt::On),
            TokenKind::KwType => self.parse_type_decl().map(Stmt::Type),
            TokenKind::KwImport => self.parse_import().map(Stmt::Import),
            other => self.error(format!(
                "expected a statement (`entity`, `rel`, `let`, `type`, `state`, `event`, `on`, `view`, or `import`), found {}",
                other.describe()
            )),
        }
    }

    // --- events, types, imports ------------------------------------------

    /// `event Name(p: T, ...)`.
    fn parse_event(&mut self) -> PResult<EventDecl> {
        let start = self.span();
        self.bump(); // `event`
        let (name, _) = self.expect_ident("an event name after `event`")?;
        let params = self.parse_typed_params("event")?;
        Ok(EventDecl {
            name,
            params,
            span: start.to(self.prev_span()),
        })
    }

    /// `( name: T, ... )` — typed parameter list for events and components.
    fn parse_typed_params(&mut self, what: &str) -> PResult<Vec<Param>> {
        self.expect(&TokenKind::LParen, &format!("after the {what} name"))?;
        let mut params = Vec::new();
        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
            let pstart = self.span();
            let (pname, _) = self.expect_ident("a parameter name")?;
            self.expect(&TokenKind::Colon, "after the parameter name")?;
            let ty = self.parse_type()?;
            params.push(Param {
                name: pname,
                ty,
                span: pstart.to(self.prev_span()),
            });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "to close the parameter list")?;
        Ok(params)
    }

    /// `on Name(p, ...) { stmts }` / `=> stmt`.
    fn parse_on(&mut self) -> PResult<OnDecl> {
        let start = self.span();
        self.bump(); // `on`
        let (event, _) = self.expect_ident("an event name after `on`")?;
        self.expect(&TokenKind::LParen, "after the event name")?;
        let mut params = Vec::new();
        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
            params.push(self.expect_ident("a parameter name")?.0);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "to close the parameter list")?;
        let body = self.parse_handler_body()?;
        Ok(OnDecl {
            event,
            params,
            body,
            span: start.to(self.prev_span()),
        })
    }

    /// `{ stmt* }` or `=> stmt`.
    fn parse_handler_body(&mut self) -> PResult<Vec<HStmt>> {
        if self.eat(&TokenKind::FatArrow) {
            return Ok(vec![self.parse_hstmt()?]);
        }
        self.expect(&TokenKind::LBrace, "or `=>` to open the handler body")?;
        let mut body = Vec::new();
        loop {
            while self.eat(&TokenKind::Semi) {}
            if matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
                break;
            }
            body.push(self.parse_hstmt()?);
        }
        self.expect(&TokenKind::RBrace, "to close the handler body")?;
        Ok(body)
    }

    /// `type Name = A | B | C`.
    fn parse_type_decl(&mut self) -> PResult<TypeDecl> {
        let start = self.span();
        self.bump(); // `type`
        let (name, _) = self.expect_ident("a type name after `type`")?;
        self.expect(&TokenKind::Eq, "after the type name")?;
        let mut ctors = vec![self.expect_ident("a constructor name")?.0];
        while self.eat(&TokenKind::Bar) {
            ctors.push(self.expect_ident("a constructor name after `|`")?.0);
        }
        Ok(TypeDecl {
            name,
            ctors,
            span: start.to(self.prev_span()),
        })
    }

    /// `import js "./path.js" as alias`.
    fn parse_import(&mut self) -> PResult<ImportDecl> {
        let start = self.span();
        self.bump(); // `import`
        self.expect_kw("js", "after `import`")?;
        let path = match self.peek().clone() {
            TokenKind::Str(s) => {
                self.bump();
                s
            }
            other => {
                return self.error(format!(
                    "expected a module path string after `import js`, found {}",
                    other.describe()
                ))
            }
        };
        self.expect_kw("as", "after the module path")?;
        let (alias, _) = self.expect_ident("a module alias after `as`")?;
        Ok(ImportDecl {
            path,
            alias,
            span: start.to(self.prev_span()),
        })
    }

    // --- handler statements ----------------------------------------------

    fn parse_hstmt(&mut self) -> PResult<HStmt> {
        let start = self.span();
        match self.peek().clone() {
            TokenKind::KwLet => {
                self.bump();
                let (bind, _) = self.expect_ident("a binding name after `let`")?;
                self.expect(&TokenKind::Eq, "before the `new` expression")?;
                if !matches!(self.peek(), TokenKind::KwNew) {
                    return self.error("only `let x = new E { … }` is allowed in a handler body");
                }
                self.parse_new_stmt(Some(bind), start)
            }
            TokenKind::KwNew => self.parse_new_stmt(None, start),
            TokenKind::Ident(w) if w == "update" => {
                self.bump();
                let target = self.parse_expr(0)?;
                let sets = self.parse_field_inits("update")?;
                Ok(HStmt::Update {
                    target,
                    sets,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Ident(w) if w == "delete" => {
                self.bump();
                let target = self.parse_expr(0)?;
                Ok(HStmt::Delete {
                    target,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Ident(w) if w == "set" => {
                self.bump();
                let (name, _) = self.expect_ident("a state or local name after `set`")?;
                self.expect(&TokenKind::Eq, "after the name in `set`")?;
                let value = self.parse_expr(0)?;
                Ok(HStmt::Set {
                    name,
                    value,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Ident(w) if w == "do" => {
                self.bump();
                let (event, _) = self.expect_ident("an event name after `do`")?;
                self.expect(&TokenKind::LParen, "after the event name")?;
                let args = self.parse_args()?;
                Ok(HStmt::Do {
                    event,
                    args,
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Ident(w) if w == "clear" => {
                self.bump();
                Ok(HStmt::Clear {
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Ident(w) if w == "revert" => {
                self.bump();
                Ok(HStmt::Revert {
                    span: start.to(self.prev_span()),
                })
            }
            TokenKind::Ident(w) if w == "focus" => {
                self.bump();
                self.expect(&TokenKind::LParen, "after `focus`")?;
                let target = if self.eat(&TokenKind::Dot) {
                    FocusTarget::Class(self.expect_ident("a class name after `.`")?.0)
                } else {
                    FocusTarget::Level(self.expect_ident("a level binder to focus")?.0)
                };
                self.expect(&TokenKind::RParen, "to close `focus(...)`")?;
                Ok(HStmt::Focus {
                    target,
                    span: start.to(self.prev_span()),
                })
            }
            // `.f := e` — assign a field of the level's own row.
            TokenKind::Dot => {
                self.bump();
                let (field, _) = self.expect_ident("a field name after `.`")?;
                self.expect(&TokenKind::ColonEq, "in a field assignment")?;
                let value = self.parse_expr(0)?;
                Ok(HStmt::Assign {
                    binder: None,
                    field,
                    value,
                    span: start.to(self.prev_span()),
                })
            }
            // `x.f := e`.
            TokenKind::Ident(binder)
                if matches!(self.peek_at(1), TokenKind::Dot)
                    && matches!(self.peek_at(2), TokenKind::Ident(_))
                    && matches!(self.peek_at(3), TokenKind::ColonEq) =>
            {
                self.bump();
                self.bump(); // `.`
                let (field, _) = self.expect_ident("a field name")?;
                self.bump(); // `:=`
                let value = self.parse_expr(0)?;
                Ok(HStmt::Assign {
                    binder: Some(binder),
                    field,
                    value,
                    span: start.to(self.prev_span()),
                })
            }
            other => self.error(format!(
                "expected a handler statement (`new`, `update`, `delete`, `set`, `do`, `x.f := …`, `clear`, `focus`), found {}",
                other.describe()
            )),
        }
    }

    /// `new E [from R as (k, v)] { f: e, … }` as a statement.
    fn parse_new_stmt(&mut self, bind: Option<String>, start: Span) -> PResult<HStmt> {
        self.bump(); // `new`
        let (entity, _) = self.expect_ident("an entity name after `new`")?;
        let from = if self.eat(&TokenKind::KwFrom) {
            let source = self.parse_expr(0)?;
            self.expect_kw("as", "after the `from` relation")?;
            self.expect(&TokenKind::LParen, "after `as`")?;
            let (key, _) = self.expect_ident("a key binder")?;
            self.expect(&TokenKind::Comma, "between the key and value binders")?;
            let (value, _) = self.expect_ident("a value binder")?;
            self.expect(&TokenKind::RParen, "to close the binders")?;
            Some(FromClause { source, key, value })
        } else {
            None
        };
        let fields = self.parse_field_inits("creation")?;
        Ok(HStmt::New {
            bind,
            entity,
            from,
            fields,
            span: start.to(self.prev_span()),
        })
    }

    /// `{ f: e, … }` — field initialisers; commas optional between lines.
    fn parse_field_inits(&mut self, what: &str) -> PResult<Vec<FieldInit>> {
        self.expect(&TokenKind::LBrace, &format!("to open the {what} body"))?;
        let mut fields = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let field_start = self.span();
            let (name, _) = self.expect_ident("a field name")?;
            self.expect(&TokenKind::Colon, "after the field name")?;
            let value = self.parse_expr(BP_ARG)?;
            let span = field_start.to(self.prev_span());
            fields.push(FieldInit { name, value, span });
            self.eat(&TokenKind::Comma);
        }
        self.expect(&TokenKind::RBrace, &format!("to close the {what} body"))?;
        Ok(fields)
    }

    /// Comma-separated argument expressions up to and including `)`.
    fn parse_args(&mut self) -> PResult<Vec<Expr>> {
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
        Ok(args)
    }

    // --- views ------------------------------------------------------------

    /// `view Name [(params)] = [local …]* (select | element)`.
    fn parse_view(&mut self) -> PResult<ViewDecl> {
        let start = self.span();
        self.bump(); // `view`
        let (name, _) = self.expect_ident("a view name after `view`")?;
        let params = if matches!(self.peek(), TokenKind::LParen) {
            self.parse_typed_params("view")?
        } else {
            Vec::new()
        };
        self.expect(&TokenKind::Eq, "before the view body")?;
        let mut locals = Vec::new();
        while self.at_kw("local") {
            let lstart = self.span();
            self.bump();
            let (lname, _) = self.expect_ident("a local name after `local`")?;
            let ty = if self.eat(&TokenKind::Colon) {
                Some(self.parse_type()?)
            } else {
                None
            };
            self.expect(&TokenKind::Eq, "before the local's default")?;
            let default = self.parse_expr(0)?;
            locals.push(LocalDecl {
                name: lname,
                ty,
                default,
                span: lstart.to(self.prev_span()),
            });
        }
        let body = if self.looks_like_select() {
            ViewBody::Select(self.parse_select()?)
        } else {
            ViewBody::Element(self.parse_element()?)
        };
        Ok(ViewDecl {
            name,
            params,
            locals,
            body,
            span: start.to(self.prev_span()),
        })
    }

    /// `state name : T [= default]`.
    fn parse_state(&mut self) -> PResult<StateDecl> {
        let start = self.span();
        self.bump(); // `state`
        let (name, _) = self.expect_ident("a state name after `state`")?;
        self.expect(&TokenKind::Colon, "before the state type")?;
        let ty = self.parse_type()?;
        let default = if self.eat(&TokenKind::Eq) {
            Some(self.parse_expr(0)?)
        } else {
            None
        };
        Ok(StateDecl {
            name,
            ty,
            default,
            span: start.to(self.prev_span()),
        })
    }

    /// `R [as x] [where p (& p)*] [order by e [desc]] select <element>`.
    fn parse_select(&mut self) -> PResult<SelectExpr> {
        let start = self.span();
        let (entity, _) = self.expect_ident("a relation name to start a `select`")?;
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
            let expr = self.parse_expr(BP_WHERE_BY)?;
            let desc = self.eat_kw("desc");
            Some(OrderBy { expr, desc })
        } else {
            None
        };
        self.expect_kw("select", "before the view element")?;
        let body = self.parse_select_body()?;
        Ok(SelectExpr {
            entity,
            binder,
            wheres,
            order_by,
            body,
            span: start.to(self.prev_span()),
        })
    }

    /// The per-row body of a `select`: an element or a component call.
    fn parse_select_body(&mut self) -> PResult<Content> {
        let start = self.span();
        if let TokenKind::Ident(name) = self.peek().clone()
            && is_component_name(&name)
            && matches!(self.peek_at(1), TokenKind::LParen)
        {
            self.bump();
            self.bump(); // `(`
            let args = self.parse_args()?;
            let block = if self.eat(&TokenKind::LBrace) {
                let mut inner = Vec::new();
                self.parse_children(&mut inner)?;
                self.expect(&TokenKind::RBrace, "to close the component block")?;
                Some(inner)
            } else {
                None
            };
            return Ok(Content::Component {
                name,
                args,
                children: block,
                span: start.to(self.prev_span()),
            });
        }
        Ok(Content::Element(self.parse_element()?))
    }

    /// `tag [( prop* )] ["text"] [{ child* }]`.
    fn parse_element(&mut self) -> PResult<ElementExpr> {
        let outer = self.depth;
        let element = self.descend(1).and_then(|()| self.parse_element_at());
        self.depth = outer;
        element
    }

    fn parse_element_at(&mut self) -> PResult<ElementExpr> {
        let start = self.span();
        let (tag, _) = self.expect_ident("an element tag")?;
        let mut modifiers = Vec::new();
        let mut attrs = Vec::new();
        let mut handlers = Vec::new();
        let mut children = Vec::new();
        if self.eat(&TokenKind::LParen) {
            while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                match self.peek().clone() {
                    TokenKind::KwOn => handlers.push(self.parse_handler()?),
                    TokenKind::Ident(name)
                        if name == "class" && matches!(self.peek_at(1), TokenKind::Dot) =>
                    {
                        attrs.push(self.parse_class_attr()?);
                    }
                    // Attribute names are HTML names: keywords (`id`, `type`)
                    // and hyphenated names (`aria-hidden`) are fine here.
                    kind if self.attr_name_ahead() => {
                        let _ = kind;
                        attrs.push(self.parse_attr()?);
                    }
                    TokenKind::Ident(name) => {
                        self.bump();
                        modifiers.push(name);
                    }
                    other => {
                        return self.error(format!(
                            "expected an attribute, modifier, or `on` handler in the element's properties, found {}",
                            other.describe()
                        ));
                    }
                }
            }
            self.expect(&TokenKind::RParen, "to close the element's properties")?;
        }
        if let TokenKind::Str(s) = self.peek().clone() {
            self.bump();
            children.push(Content::Text(s));
        }
        if self.eat(&TokenKind::LBrace) {
            self.parse_children(&mut children)?;
            self.expect(&TokenKind::RBrace, "to close the element body")?;
        }
        Ok(ElementExpr {
            tag,
            modifiers,
            attrs,
            handlers,
            children,
            span: start.to(self.prev_span()),
        })
    }

    /// Is the cursor on `name =` where `name` is an identifier or keyword,
    /// possibly hyphenated (`aria-hidden`)?
    fn attr_name_ahead(&self) -> bool {
        let word = |k: &TokenKind| matches!(k, TokenKind::Ident(_)) || k.keyword_word().is_some();
        if !word(self.peek()) {
            return false;
        }
        let mut i = 1;
        while matches!(self.peek_at(i), TokenKind::Minus) && word(self.peek_at(i + 1)) {
            i += 2;
        }
        matches!(self.peek_at(i), TokenKind::Eq)
    }

    fn parse_attr(&mut self) -> PResult<AttrBind> {
        let start = self.span();
        let mut name = self.attr_name_word();
        while self.eat(&TokenKind::Minus) {
            name.push('-');
            name.push_str(&self.attr_name_word());
        }
        self.bump(); // `=`
        let value = self.parse_attr_value()?;
        Ok(AttrBind {
            name,
            value,
            span: start.to(self.prev_span()),
        })
    }

    /// Consume an identifier-or-keyword token as an attribute-name word.
    fn attr_name_word(&mut self) -> String {
        let tok = self.bump();
        match &tok.kind {
            TokenKind::Ident(s) => s.clone(),
            other => other.keyword_word().unwrap_or("").to_string(),
        }
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
            _ => Ok(AttrValue::Bind(self.parse_expr(BP_ARG)?)),
        }
    }

    /// The children of an element body, up to (not including) the closing `}`.
    fn parse_children(&mut self, children: &mut Vec<Content>) -> PResult<()> {
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let start = self.span();
            match self.peek().clone() {
                // A string followed by `++` starts a concat bind, not static text.
                TokenKind::Str(_) if matches!(self.peek_at(1), TokenKind::PlusPlus) => {
                    children.push(Content::Bind(self.parse_expr(0)?));
                }
                TokenKind::Str(s) => {
                    self.bump();
                    children.push(Content::Text(s));
                }
                TokenKind::KwIf => {
                    self.bump();
                    self.expect(&TokenKind::LParen, "after `if` in a view")?;
                    let cond = self.parse_expr(0)?;
                    self.expect(&TokenKind::RParen, "to close the `if` condition")?;
                    self.expect(&TokenKind::LBrace, "to open the `if` body")?;
                    let mut inner = Vec::new();
                    // An `if` body may hold another `if`: nesting, like an
                    // element's. (An error leaves `depth` to the enclosing
                    // element or statement to restore.)
                    let outer = self.depth;
                    self.descend(1)?;
                    self.parse_children(&mut inner)?;
                    self.depth = outer;
                    self.expect(&TokenKind::RBrace, "to close the `if` body")?;
                    children.push(Content::If {
                        cond,
                        children: inner,
                        span: start.to(self.prev_span()),
                    });
                }
                TokenKind::Ident(w) if w == "children" && !matches!(self.peek_at(1), TokenKind::LParen | TokenKind::LBrace) => {
                    self.bump();
                    children.push(Content::ChildrenSlot(start));
                }
                TokenKind::Ident(_) if self.looks_like_select() => {
                    children.push(Content::Select(Box::new(self.parse_select()?)));
                }
                TokenKind::Ident(name)
                    if is_component_name(&name) && matches!(self.peek_at(1), TokenKind::LParen) =>
                {
                    self.bump();
                    self.bump(); // `(`
                    let args = self.parse_args()?;
                    let block = if self.eat(&TokenKind::LBrace) {
                        let mut inner = Vec::new();
                        self.parse_children(&mut inner)?;
                        self.expect(&TokenKind::RBrace, "to close the component block")?;
                        Some(inner)
                    } else {
                        None
                    };
                    children.push(Content::Component {
                        name,
                        args,
                        children: block,
                        span: start.to(self.prev_span()),
                    });
                }
                TokenKind::Ident(_)
                    if matches!(
                        self.peek_at(1),
                        TokenKind::LParen | TokenKind::LBrace | TokenKind::Str(_)
                    ) =>
                {
                    children.push(Content::Element(self.parse_element()?));
                }
                // Anything else is a bind expression: `.text`, `x.f`, `name`, `(expr)`.
                _ => children.push(Content::Bind(self.parse_expr(0)?)),
            }
        }
        Ok(())
    }

    /// A content ident begins a `select` (not a plain element) when it is
    /// immediately followed by `as` (an alias), `where`, `order`, or `select`.
    fn looks_like_select(&self) -> bool {
        matches!(self.peek(), TokenKind::Ident(_))
            && (matches!(self.peek_at(1), TokenKind::KwWhere)
                || matches!(self.peek_at(1), TokenKind::Ident(s) if s == "as" || s == "order" || s == "select"))
    }

    /// `on event('.'mod)* ['(' params ')'] ( '{' stmts '}' | '=>' stmt )`.
    fn parse_handler(&mut self) -> PResult<HandlerDecl> {
        let start = self.span();
        self.bump(); // `on`
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
        let body = self.parse_handler_body()?;
        Ok(HandlerDecl {
            event,
            modifiers,
            params,
            body,
            span: start.to(self.prev_span()),
        })
    }

    /// `name [: T] = extractor`.
    fn parse_handler_param(&mut self) -> PResult<HandlerParam> {
        let start = self.span();
        let (name, _) = self.expect_ident("a handler parameter name")?;
        let ty = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
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
        // `module.fn(args)` — a JS escape hatch.
        if self.eat(&TokenKind::Dot) {
            let (func, _) = self.expect_ident("a function name after the module alias")?;
            self.expect(&TokenKind::LParen, "after the JS function name")?;
            let args = self.parse_args()?;
            return Ok(Extractor::Js {
                module: name,
                func,
                args,
            });
        }
        let mut args = Vec::new();
        if self.eat(&TokenKind::LParen) {
            while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                args.push(self.expect_ident("an extractor argument")?.0);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::RParen, "to close the extractor argument")?;
        }
        let bad = |this: &mut Parser, msg: String| -> PResult<Extractor> {
            this.diagnostics.push(Diagnostic::error(span, msg));
            Err(Bail)
        };
        match (name.as_str(), args.len()) {
            ("value", 0) => Ok(Extractor::Value),
            ("checked", 0) => Ok(Extractor::Checked),
            ("drag", 1) => Ok(Extractor::Drag(args.remove(0))),
            ("endOf", 1) => Ok(Extractor::EndOf(args.remove(0))),
            ("dropPos", 2) => {
                let exclude = args.remove(1);
                let level = args.remove(0);
                Ok(Extractor::DropPos { level, exclude })
            }
            ("value" | "checked" | "drag" | "endOf" | "dropPos", n) => {
                bad(self, format!("extractor `{name}` takes a different number of arguments (got {n})"))
            }
            (other, _) => bad(self, format!("unknown extractor `{other}`")),
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
            self.eat(&TokenKind::Comma);
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
        let outer = self.depth;
        let ty = self.descend(1).and_then(|()| self.parse_type_at());
        self.depth = outer;
        ty
    }

    fn parse_type_at(&mut self) -> PResult<Type> {
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
            self.descend(1)?;
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

    /// `{ @a | @b | ... }`.
    fn parse_coproduct_type(&mut self, start: Span) -> PResult<Type> {
        self.bump(); // `{`
        let mut elems = Vec::new();
        elems.push(self.parse_atom_type()?);
        while self.eat(&TokenKind::Bar) {
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
        let outer = self.depth;
        let expr = self.parse_expr_at(min_bp);
        self.depth = outer;
        expr
    }

    fn parse_expr_at(&mut self, min_bp: u8) -> PResult<Expr> {
        self.descend(1)?;
        let mut lhs = self.parse_prefix()?;
        while let Some(lbp) = self.infix_bp() {
            if lbp <= min_bp {
                break;
            }
            // Each operator applied here wraps `lhs` one level deeper.
            self.descend(1)?;
            lhs = self.parse_infix(lhs, lbp)?;
        }
        Ok(lhs)
    }

    /// Binding power of the current token as an infix/postfix operator.
    fn infix_bp(&self) -> Option<u8> {
        Some(match self.peek() {
            TokenKind::Comma => BP_FORK,
            TokenKind::Bar | TokenKind::KwExcept | TokenKind::KwAntijoin => BP_SET,
            TokenKind::Amp => BP_INTERSECT,
            TokenKind::KwWhere | TokenKind::KwBy => BP_WHERE_BY,
            TokenKind::Eq
            | TokenKind::Ne
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::Le
            | TokenKind::Ge
            | TokenKind::KwIn => BP_CMP,
            TokenKind::Plus | TokenKind::Minus | TokenKind::PlusPlus => BP_ADD,
            TokenKind::Star | TokenKind::Slash | TokenKind::Percent => BP_MUL,
            TokenKind::Dot => BP_COMPOSE,
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
            // Negative numeric literals.
            TokenKind::Minus => {
                self.bump();
                match self.peek().clone() {
                    TokenKind::Int(n) => {
                        self.bump();
                        Ok(self.mk_to(ExprKind::Int(-n), start))
                    }
                    TokenKind::Decimal(s) => {
                        self.bump();
                        Ok(self.mk_to(ExprKind::Decimal(format!("-{s}")), start))
                    }
                    other => self.error(format!(
                        "expected a numeric literal after unary `-`, found {}",
                        other.describe()
                    )),
                }
            }
            TokenKind::Dot => self.parse_field_path(start),
            TokenKind::LParen => {
                self.bump();
                let mut inner = self.parse_expr(0)?;
                let end = self.expect(&TokenKind::RParen, "to close the expression")?;
                inner.span = start.to(end.span);
                Ok(inner)
            }
            TokenKind::KwNew => self.parse_new(start),
            TokenKind::KwMatch => self.parse_match(start),
            TokenKind::KwIf => {
                self.descend(BRANCH_NESTING)?;
                self.bump();
                let cond = self.parse_expr(0)?;
                self.expect(&TokenKind::KwThen, "after the `if` condition")?;
                let then = self.parse_expr(0)?;
                self.expect(&TokenKind::KwElse, "after the `then` branch")?;
                let els = self.parse_expr(0)?;
                Ok(self.mk_to(
                    ExprKind::If {
                        cond: Box::new(cond),
                        then: Box::new(then),
                        els: Box::new(els),
                    },
                    start,
                ))
            }

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
            TokenKind::KwNot => {
                self.bump();
                let operand = self.parse_expr(RBP_NOT)?;
                Ok(self.mk_to(ExprKind::Not(Box::new(operand)), start))
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
            TokenKind::Eq
            | TokenKind::Ne
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::Le
            | TokenKind::Ge => {
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
            TokenKind::Bar => self.binary(lhs, lbp, start, ExprKind::Union),
            TokenKind::Amp => self.binary(lhs, lbp, start, ExprKind::Intersect),
            TokenKind::KwExcept => self.binary(lhs, lbp, start, ExprKind::Except),
            TokenKind::KwAntijoin => self.binary(lhs, lbp, start, ExprKind::Antijoin),
            TokenKind::KwWhere => self.binary(lhs, lbp, start, ExprKind::Where),
            TokenKind::KwBy => self.binary(lhs, lbp, start, ExprKind::By),
            TokenKind::Plus => self.binary(lhs, lbp, start, ExprKind::Add),
            TokenKind::Minus => self.binary(lhs, lbp, start, ExprKind::Sub),
            TokenKind::PlusPlus => self.binary(lhs, lbp, start, ExprKind::Concat),
            TokenKind::Star => self.binary(lhs, lbp, start, ExprKind::Mul),
            TokenKind::Slash => self.binary(lhs, lbp, start, ExprKind::Div),
            TokenKind::Percent => self.binary(lhs, lbp, start, ExprKind::Mod),
            TokenKind::Dot => self.binary(lhs, lbp, start, ExprKind::Compose),

            TokenKind::Eq
            | TokenKind::Ne
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::Le
            | TokenKind::Ge => {
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
        let args = self.parse_args()?;
        Ok(self.mk_to(ExprKind::Call { func, args }, start))
    }

    /// `.a.b.c` — greedy: every `.ident` hop is part of the path.
    fn parse_field_path(&mut self, start: Span) -> PResult<Expr> {
        let mut parts = Vec::new();
        while matches!(self.peek(), TokenKind::Dot)
            && matches!(self.peek_at(1), TokenKind::Ident(_))
        {
            self.bump(); // `.`
            let (part, _) = self.expect_ident("a field name after `.`")?;
            parts.push(part);
        }
        if parts.is_empty() {
            return self.error("expected a field name after `.`");
        }
        Ok(self.mk_to(ExprKind::FieldPath(parts), start))
    }

    /// `match e { pat => expr, … }` — arms separated by commas or newlines.
    fn parse_match(&mut self, start: Span) -> PResult<Expr> {
        self.descend(BRANCH_NESTING)?;
        self.bump(); // `match`
        let scrutinee = self.parse_expr(0)?;
        self.expect(&TokenKind::LBrace, "to open the `match` arms")?;
        let mut arms = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let astart = self.span();
            let pat = match self.peek().clone() {
                TokenKind::Ident(s) if s == "_" => {
                    self.bump();
                    Pattern::Wildcard
                }
                TokenKind::Ident(s) => {
                    self.bump();
                    Pattern::Ident(s)
                }
                TokenKind::Atom(a) => {
                    self.bump();
                    Pattern::Atom(a)
                }
                TokenKind::Int(n) => {
                    self.bump();
                    Pattern::Int(n)
                }
                TokenKind::Str(s) => {
                    self.bump();
                    Pattern::Str(s)
                }
                other => {
                    return self.error(format!("expected a `match` pattern, found {}", other.describe()))
                }
            };
            self.expect(&TokenKind::FatArrow, "after the `match` pattern")?;
            let body = self.parse_expr(BP_ARG)?;
            arms.push(MatchArm {
                pat,
                body,
                span: astart.to(self.prev_span()),
            });
            self.eat(&TokenKind::Comma);
        }
        self.expect(&TokenKind::RBrace, "to close the `match` arms")?;
        Ok(self.mk_to(
            ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            start,
        ))
    }

    fn parse_new(&mut self, start: Span) -> PResult<Expr> {
        self.bump(); // `new`
        let (entity, _) = self.expect_ident("an entity name after `new`")?;
        let fields = self.parse_field_inits("creation")?;
        Ok(self.mk_to(ExprKind::New { entity, fields }, start))
    }

    // --- helpers ----------------------------------------------------------

    fn cmp_op(&self) -> CmpOp {
        match self.peek() {
            TokenKind::Eq => CmpOp::Eq,
            TokenKind::Ne => CmpOp::Ne,
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

/// Component calls are capitalised (`TodoItem(t)`); element tags are not.
fn is_component_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}
