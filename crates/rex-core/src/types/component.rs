//! Component expansion (S-61): `view Name(p: E) = <element>` is a template,
//! and a call `Name(x)` — as a `select` body or as an element child — is
//! replaced, before any level is lowered, by the template's element with each
//! param renamed to the binder passed for it.
//!
//! This is macro-style inline expansion, so the rest of desugaring never
//! learns components exist: the expanded element's binders are still keys
//! (§2.5), its `select`s become ordinary nested levels named from the call
//! site, and generated code is the same as if the element had been written
//! out by hand. A `Name(args) { … }` block lands at the template's single
//! `children` slot.
//!
//! The expander tracks the `(binder, entity)` scope itself because an
//! argument is checked against the type of the binder it names, and a call may
//! sit inside any number of nested `select`s.

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

/// The hidden entity field and read view behind `local <name>` in component
/// `comp` (S-62): one name for both, since the view is `Entity . field` with
/// the default applied. `#` keeps it out of user code.
pub fn local_view(comp: &str, name: &str) -> String {
    format!("local#{comp}#{name}")
}

/// The hidden event a `set <name> = v` in a DOM handler of `comp` dispatches.
pub fn local_event(comp: &str, name: &str) -> String {
    format!("local#{comp}#{name}#set")
}

/// The `(binder, entity)` pairs in scope at a point in a view, outermost first.
pub type Scope = Vec<(String, String)>;

pub struct Expander<'a> {
    /// Every view with an element body, by name — the callable templates.
    comps: &'a HashMap<String, &'a ViewDecl>,
    /// A `select` source name -> the entity it ranges over (an entity itself,
    /// or a `let` declared `: Entity`).
    resolve: &'a dyn Fn(&str) -> Option<String>,
    pub diagnostics: Vec<Diagnostic>,
    /// Components being expanded right now, for the recursion check.
    stack: Vec<String>,
    /// Expansions so far, so each one's own binders get distinct names.
    expansions: usize,
}

impl<'a> Expander<'a> {
    pub fn new(comps: &'a HashMap<String, &'a ViewDecl>, resolve: &'a dyn Fn(&str) -> Option<String>) -> Self {
        Expander { comps, resolve, diagnostics: Vec::new(), stack: Vec::new(), expansions: 0 }
    }

    fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diagnostics.push(Diagnostic::error(span, msg.into()));
    }

    /// Expand every component call under `el`, whose own row scope is `scope`.
    pub fn element(&mut self, el: &ElementExpr, scope: &mut Scope) -> ElementExpr {
        let mut out = el.clone();
        out.children = self.contents(&el.children, scope);
        out
    }

    pub fn select(&mut self, sel: &SelectExpr, scope: &mut Scope) -> SelectExpr {
        let entity = (self.resolve)(&sel.entity).unwrap_or_else(|| sel.entity.clone());
        let binder = sel.binder.clone().unwrap_or_else(|| sel.entity.clone());
        scope.push((binder, entity));
        let mut out = sel.clone();
        out.body = match &sel.body {
            Content::Component { name, args, children, span } => {
                match self.call(name, args, children.as_deref(), *span, scope) {
                    Some(el) => Content::Element(el),
                    None => sel.body.clone(),
                }
            }
            Content::Element(el) => Content::Element(self.element(el, scope)),
            other => other.clone(),
        };
        scope.pop();
        out
    }

    fn contents(&mut self, cs: &[Content], scope: &mut Scope) -> Vec<Content> {
        let mut out = Vec::new();
        for c in cs {
            match c {
                Content::Element(e) => out.push(Content::Element(self.element(e, scope))),
                Content::Select(s) => out.push(Content::Select(Box::new(self.select(s, scope)))),
                Content::If { cond, children, span } => out.push(Content::If {
                    cond: cond.clone(),
                    children: self.contents(children, scope),
                    span: *span,
                }),
                Content::Component { name, args, children, span } => {
                    if let Some(el) = self.call(name, args, children.as_deref(), *span, scope) {
                        out.push(Content::Element(el));
                    }
                }
                Content::Text(_) | Content::Bind(_) | Content::ChildrenSlot(_) => out.push(c.clone()),
            }
        }
        out
    }

    /// Expand one call to its template's element, or `None` (with a
    /// diagnostic) if the call is malformed.
    fn call(
        &mut self,
        name: &str,
        args: &[Expr],
        block: Option<&[Content]>,
        span: Span,
        scope: &mut Scope,
    ) -> Option<ElementExpr> {
        let Some(comp) = self.comps.get(name).copied() else {
            self.error(span, format!("unknown component `{name}`; declare it with `view {name}(p: Entity) = …`"));
            return None;
        };
        if self.stack.iter().any(|s| s == name) {
            let mut cycle = self.stack.clone();
            cycle.push(name.to_string());
            self.error(span, format!("components cannot be recursive: {}", cycle.join(" -> ")));
            return None;
        }
        let ViewBody::Element(template) = &comp.body else {
            self.error(span, format!("component `{name}` must have an element body, not a `select`"));
            return None;
        };
        if args.len() != comp.params.len() {
            self.error(
                span,
                format!("component `{name}` takes {} argument(s) but the call passes {}", comp.params.len(), args.len()),
            );
            return None;
        }

        // Positional binder substitution, each argument checked against its
        // param's declared entity.
        let mut rename: HashMap<String, Expr> = HashMap::new();
        for (a, p) in args.iter().zip(&comp.params) {
            let ExprKind::Ident(binder) = &a.kind else {
                self.error(a.span, "a component argument must be a row binder in scope");
                return None;
            };
            let Some((_, entity)) = scope.iter().rev().find(|(b, _)| b == binder) else {
                self.error(a.span, format!("unknown binder `{binder}`: a component argument must be a row binder in scope"));
                return None;
            };
            let want = match &p.ty.kind {
                TypeKind::Named(n) => n.strip_suffix("ID").unwrap_or(n).to_string(),
                _ => {
                    self.error(p.span, format!("component parameter `{}` must be entity-typed", p.name));
                    return None;
                }
            };
            let declared = match &p.ty.kind {
                TypeKind::Named(n) => n.clone(),
                _ => unreachable!(),
            };
            if *entity != want && *entity != declared {
                self.error(
                    a.span,
                    format!("component `{name}`: `{}` expects a `{declared}`, but `{binder}` is a `{entity}`", p.name),
                );
                return None;
            }
            rename.insert(p.name.clone(), a.clone());
        }

        // The call's block is written in the caller's scope: expand it there,
        // before this component is on the stack, so `Card { Card { … } }` is
        // nesting, not recursion.
        let kids = block.map(|b| self.contents(b, scope));

        // `local` state (S-62) is keyed by the first param's row. A bare
        // `name` in the body reads it *at the argument's row*, and a `set`
        // becomes a dispatch of the local's hidden event for that row.
        let mut sets: HashMap<String, String> = HashMap::new();
        if !comp.locals.is_empty() {
            let Some(key) = args.first() else {
                self.error(comp.span, format!("component `{name}` has `local` state but no parameter to key it by"));
                return None;
            };
            for l in &comp.locals {
                let view = ident_expr(&local_view(name, &l.name), key.span);
                rename.insert(
                    l.name.clone(),
                    Expr { kind: ExprKind::Compose(Box::new(key.clone()), Box::new(view)), span: key.span },
                );
                sets.insert(l.name.clone(), local_event(name, &l.name));
            }
        }
        // Hygiene: the template's own row binders (`Item as x`) get fresh
        // names, so they can neither collide with the caller's binders nor
        // capture a name in the call's block.
        self.expansions += 1;
        let mut binders = Vec::new();
        select_binders(&template.children, &mut binders);
        for b in binders {
            if rename.contains_key(&b) {
                self.error(comp.span, format!("in component `{name}`, binder `{b}` shadows a parameter or `local`"));
                return None;
            }
            let fresh = format!("{b}#{name}#{}", self.expansions);
            rename.insert(b, ident_expr(&fresh, span));
        }
        let mut body = Renamer { map: &rename, sets: (!sets.is_empty()).then(|| (&sets, args.first().unwrap())) }
            .element(template);
        rebind(&mut body.children, &rename);
        let slots = count_slots(&body.children);
        if slots > 1 {
            self.error(comp.span, format!("component `{name}` has more than one `children` slot"));
            return None;
        }
        match (&kids, slots) {
            (Some(_), 0) => {
                self.error(span, format!("component `{name}` has no `children` slot, but the call passes a block"));
                return None;
            }
            (kids, _) => fill_slot(&mut body.children, kids.as_deref().unwrap_or(&[])),
        }

        self.stack.push(name.to_string());
        let out = self.element(&body, scope);
        self.stack.pop();
        Some(out)
    }
}

fn count_slots(cs: &[Content]) -> usize {
    cs.iter()
        .map(|c| match c {
            Content::ChildrenSlot(_) => 1,
            Content::Element(e) => count_slots(&e.children),
            Content::If { children, .. } => count_slots(children),
            Content::Select(s) => count_slots(std::slice::from_ref(&s.body)),
            _ => 0,
        })
        .sum()
}

/// Replace the (at most one) `children` slot in `cs` with `kids`.
fn fill_slot(cs: &mut Vec<Content>, kids: &[Content]) {
    let mut out = Vec::with_capacity(cs.len());
    for c in cs.drain(..) {
        match c {
            Content::ChildrenSlot(_) => out.extend(kids.iter().cloned()),
            Content::Element(mut e) => {
                fill_slot(&mut e.children, kids);
                out.push(Content::Element(e));
            }
            Content::If { cond, mut children, span } => {
                fill_slot(&mut children, kids);
                out.push(Content::If { cond, children, span });
            }
            Content::Select(mut s) => {
                if let Content::Element(e) = &mut s.body {
                    fill_slot(&mut e.children, kids);
                }
                out.push(Content::Select(s));
            }
            other => out.push(other),
        }
    }
    *cs = out;
}

/// The explicit `as x` binders of every `select` in `cs`.
fn select_binders(cs: &[Content], out: &mut Vec<String>) {
    for c in cs {
        match c {
            Content::Element(e) => select_binders(&e.children, out),
            Content::If { children, .. } => select_binders(children, out),
            Content::Select(s) => {
                if let Some(b) = &s.binder
                    && !out.contains(b)
                {
                    out.push(b.clone());
                }
                select_binders(std::slice::from_ref(&s.body), out);
            }
            _ => {}
        }
    }
}

/// Rename each `select`'s own `as x` binder per `map` (references to it are
/// renamed by [`Renamer`]).
fn rebind(cs: &mut [Content], map: &HashMap<String, Expr>) {
    for c in cs {
        match c {
            Content::Element(e) => rebind(&mut e.children, map),
            Content::If { children, .. } => rebind(children, map),
            Content::Select(s) => {
                if let Some(Expr { kind: ExprKind::Ident(fresh), .. }) = s.binder.as_ref().and_then(|b| map.get(b)) {
                    s.binder = Some(fresh.clone());
                }
                rebind(std::slice::from_mut(&mut s.body), map);
            }
            _ => {}
        }
    }
}

/// Substitutes a replacement expression for each bare `Ident` named in `map`
/// — the template's param names -> the caller's binders (S-61), or a level's
/// binders -> the relations they denote (S-60). It never touches the field on
/// the right of a `.` (`x.text` never substitutes `text`).
pub struct Renamer<'a> {
    pub map: &'a HashMap<String, Expr>,
    /// `set <local> = v` -> the local's event name, and the row key it is set
    /// at (S-62). `None` outside a component with `local`s.
    pub sets: Option<(&'a HashMap<String, String>, &'a Expr)>,
}

fn ident_expr(name: &str, span: Span) -> Expr {
    Expr { kind: ExprKind::Ident(name.to_string()), span }
}

impl Renamer<'_> {
    /// A name in a position that can only hold a binder name (`focus(c)`).
    fn name(&self, n: &str) -> String {
        match self.map.get(n) {
            Some(Expr { kind: ExprKind::Ident(x), .. }) => x.clone(),
            _ => n.to_string(),
        }
    }

    fn boxed(&self, e: &Expr) -> Box<Expr> {
        Box::new(self.expr(e))
    }

    fn opt(&self, e: &Option<Box<Expr>>) -> Option<Box<Expr>> {
        e.as_ref().map(|e| self.boxed(e))
    }

    pub fn expr(&self, e: &Expr) -> Expr {
        use ExprKind::*;
        let kind = match &e.kind {
            Ident(n) => match self.map.get(n) {
                Some(rep) => return rep.clone(),
                None => Ident(n.clone()),
            },
            Id | FieldPath(_) | Atom(_) | Int(_) | Decimal(_) | Str(_) | Date { .. } => e.kind.clone(),
            Compose(a, b) => Compose(
                self.boxed(a),
                match &b.kind {
                    Ident(_) => b.clone(),
                    _ => self.boxed(b),
                },
            ),
            Fork(a, b) => Fork(self.boxed(a), self.boxed(b)),
            Union(a, b) => Union(self.boxed(a), self.boxed(b)),
            Intersect(a, b) => Intersect(self.boxed(a), self.boxed(b)),
            Restrict(a, b) => Restrict(self.boxed(a), self.boxed(b)),
            Inverse(a) => Inverse(self.boxed(a)),
            Distinct(a) => Distinct(self.boxed(a)),
            Proj(s, a) => Proj(*s, self.boxed(a)),
            Where(a, b) => Where(self.boxed(a), self.boxed(b)),
            By(a, b) => By(self.boxed(a), self.boxed(b)),
            Except(a, b) => Except(self.boxed(a), self.boxed(b)),
            Antijoin(a, b) => Antijoin(self.boxed(a), self.boxed(b)),
            Not(a) => Not(self.boxed(a)),
            Add(a, b) => Add(self.boxed(a), self.boxed(b)),
            Sub(a, b) => Sub(self.boxed(a), self.boxed(b)),
            Mul(a, b) => Mul(self.boxed(a), self.boxed(b)),
            Div(a, b) => Div(self.boxed(a), self.boxed(b)),
            Mod(a, b) => Mod(self.boxed(a), self.boxed(b)),
            Concat(a, b) => Concat(self.boxed(a), self.boxed(b)),
            Compare { op, lhs, rhs } => Compare { op: *op, lhs: self.opt(lhs), rhs: self.boxed(rhs) },
            In { lhs, rhs } => In { lhs: self.opt(lhs), rhs: self.boxed(rhs) },
            Match { scrutinee, arms } => Match {
                scrutinee: self.boxed(scrutinee),
                arms: arms
                    .iter()
                    .map(|a| MatchArm { pat: a.pat.clone(), body: self.expr(&a.body), span: a.span })
                    .collect(),
            },
            If { cond, then, els } => If { cond: self.boxed(cond), then: self.boxed(then), els: self.boxed(els) },
            Call { func, args } => Call { func: func.clone(), args: args.iter().map(|a| self.expr(a)).collect() },
            New { entity, fields } => New { entity: entity.clone(), fields: self.fields(fields) },
        };
        Expr { kind, span: e.span }
    }

    fn fields(&self, fs: &[FieldInit]) -> Vec<FieldInit> {
        fs.iter().map(|f| FieldInit { name: f.name.clone(), value: self.expr(&f.value), span: f.span }).collect()
    }

    fn extractor(&self, x: &Extractor) -> Extractor {
        match x {
            Extractor::DropPos { level, exclude } => {
                Extractor::DropPos { level: self.name(level), exclude: exclude.clone() }
            }
            Extractor::EndOf(b) => Extractor::EndOf(self.name(b)),
            Extractor::Js { module, func, args } => Extractor::Js {
                module: module.clone(),
                func: func.clone(),
                args: args.iter().map(|a| self.expr(a)).collect(),
            },
            Extractor::Value | Extractor::Checked | Extractor::Drag(_) => x.clone(),
        }
    }

    fn stmt(&self, s: &HStmt) -> HStmt {
        match s {
            HStmt::New { bind, entity, from, fields, span } => HStmt::New {
                bind: bind.clone(),
                entity: entity.clone(),
                from: from.as_ref().map(|f| FromClause {
                    source: self.expr(&f.source),
                    key: f.key.clone(),
                    value: f.value.clone(),
                }),
                fields: self.fields(fields),
                span: *span,
            },
            HStmt::Assign { binder, field, value, span } => HStmt::Assign {
                binder: binder.as_ref().map(|b| self.name(b)),
                field: field.clone(),
                value: self.expr(value),
                span: *span,
            },
            HStmt::Update { target, sets, span } => {
                HStmt::Update { target: self.expr(target), sets: self.fields(sets), span: *span }
            }
            HStmt::Delete { target, span } => HStmt::Delete { target: self.expr(target), span: *span },
            HStmt::Set { name, value, span } => match self.sets.and_then(|(s, key)| s.get(name).map(|ev| (ev, key))) {
                // A component `local`: an implicit, logged event (S-62).
                Some((event, key)) => HStmt::Do {
                    event: event.clone(),
                    args: vec![key.clone(), self.expr(value)],
                    span: *span,
                },
                None => HStmt::Set { name: name.clone(), value: self.expr(value), span: *span },
            },
            HStmt::Do { event, args, span } => {
                HStmt::Do { event: event.clone(), args: args.iter().map(|a| self.expr(a)).collect(), span: *span }
            }
            HStmt::Focus { target: FocusTarget::Level(b), span } => {
                HStmt::Focus { target: FocusTarget::Level(self.name(b)), span: *span }
            }
            HStmt::Clear { .. } | HStmt::Focus { .. } => s.clone(),
        }
    }

    fn handler(&self, h: &HandlerDecl) -> HandlerDecl {
        HandlerDecl {
            event: h.event.clone(),
            modifiers: h.modifiers.clone(),
            params: h
                .params
                .iter()
                .map(|p| HandlerParam {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    extractor: self.extractor(&p.extractor),
                    span: p.span,
                })
                .collect(),
            body: h.body.iter().map(|s| self.stmt(s)).collect(),
            span: h.span,
        }
    }

    fn content(&self, c: &Content) -> Content {
        match c {
            Content::Element(e) => Content::Element(self.element(e)),
            Content::Select(s) => Content::Select(Box::new(SelectExpr {
                entity: s.entity.clone(),
                binder: s.binder.clone(),
                wheres: s.wheres.iter().map(|w| self.expr(w)).collect(),
                order_by: s.order_by.as_ref().map(|o| OrderBy { expr: self.expr(&o.expr), desc: o.desc }),
                body: self.content(&s.body),
                span: s.span,
            })),
            Content::Text(_) | Content::ChildrenSlot(_) => c.clone(),
            Content::Bind(e) => Content::Bind(self.expr(e)),
            Content::If { cond, children, span } => Content::If {
                cond: self.expr(cond),
                children: children.iter().map(|c| self.content(c)).collect(),
                span: *span,
            },
            Content::Component { name, args, children, span } => Content::Component {
                name: name.clone(),
                args: args.iter().map(|a| self.expr(a)).collect(),
                children: children.as_ref().map(|k| k.iter().map(|c| self.content(c)).collect()),
                span: *span,
            },
        }
    }

    pub fn element(&self, e: &ElementExpr) -> ElementExpr {
        ElementExpr {
            tag: e.tag.clone(),
            modifiers: e.modifiers.clone(),
            attrs: e
                .attrs
                .iter()
                .map(|a| AttrBind {
                    name: a.name.clone(),
                    value: match &a.value {
                        AttrValue::Bind(x) => AttrValue::Bind(self.expr(x)),
                        s => s.clone(),
                    },
                    span: a.span,
                })
                .collect(),
            handlers: e.handlers.iter().map(|h| self.handler(h)).collect(),
            children: e.children.iter().map(|c| self.content(c)).collect(),
            span: e.span,
        }
    }
}
