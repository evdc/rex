//! View desugaring (§M5): expand each `view` into ordinary `let`s plus a
//! [`ShapeProgram`] the compiler emits JS from.
//!
//! The narrow core never learns about UI. A `view` is sugar: its nesting
//! becomes composite-keyed membership relations, its `order by` an order
//! relation, its bound attributes attribute relations — all auto-derived
//! `let`s the existing checker elaborates. What can't be a relation (the DOM
//! skeleton, event wiring) becomes the ShapeIR/HandlerIR, consumed by codegen.
//!
//! The load-bearing rule (see plan): binders are second-class — they denote
//! row keys, never relations. A binder appears only as the RHS of a membership
//! conjunct (`:list == List`), as a value in a mutation, or as the implicit
//! subject of `:field` paths in its own body. A handler may reference its own
//! level's binder (the row it renders) and its params — enclosing binders are
//! rejected, so every generated listener closes over exactly the key the
//! shaper hands its template.

use super::shape_ir::*;
use super::typed::Lit;
use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;

pub struct Desugared {
    pub program: Program,
    pub shapes: ShapeProgram,
    pub diagnostics: Vec<Diagnostic>,
}

/// Expand every `view`/`state` statement, returning the program with them
/// replaced by generated `let`s/entities plus the collected ShapeIR.
pub fn desugar(program: &Program) -> Desugared {
    // Collect each entity's field names and declared types (including
    // `rel`-injected fields) up front, so a `view` — processed in the same
    // pass below, regardless of whether it appears before or after the
    // entities it renders — can (a) check a bare element tag against them
    // (see `LevelWalk::element`) and (b) pick the right wire `Encoding` for a
    // `:field` bind (see `LevelWalk::attr_view`).
    let mut entity_fields: std::collections::HashMap<String, std::collections::HashMap<String, Type>> =
        Default::default();
    for stmt in &program.stmts {
        if let Stmt::Entity(e) = stmt {
            let fields = entity_fields.entry(e.name.clone()).or_default();
            for f in &e.fields {
                fields.insert(f.name.clone(), f.ty.clone());
            }
        }
        if let Stmt::Rel(r) = stmt {
            entity_fields.entry(r.from.clone()).or_default().insert(
                r.name.clone(),
                Type { kind: TypeKind::Named(r.to.clone()), span: r.span },
            );
        }
    }
    let mut d = Desugar {
        stmts: Vec::new(),
        shapes: ShapeProgram::default(),
        diagnostics: Vec::new(),
        handler_seq: 0,
        entity_fields,
    };
    // Pass 1: collect `rel` decls so each becomes a functional field on its
    // source entity (grouped by entity name), regardless of decl order.
    let mut rel_fields: std::collections::HashMap<String, Vec<FieldDecl>> = Default::default();
    for stmt in &program.stmts {
        if let Stmt::Rel(r) = stmt {
            rel_fields.entry(r.from.clone()).or_default().push(FieldDecl {
                name: r.name.clone(),
                ty: Type { kind: TypeKind::Named(r.to.clone()), span: r.span },
                span: r.span,
            });
        }
    }
    // Pass 2: emit entities (with rel-fields injected), `let`s for each rel, and
    // desugared views/state.
    for stmt in &program.stmts {
        match stmt {
            Stmt::View(v) => d.view(v),
            Stmt::State(s) => d.diagnostics.push(Diagnostic::error(
                s.span,
                "`state` is not supported yet".to_string(),
            )),
            Stmt::Rel(r) => {
                // `rel R(A, B)` -> `let R = A . :R` (A's identity, then the
                // injected field). The field itself is added to A's entity decl.
                let body = compose(ident(&r.from, r.span), field_path(std::slice::from_ref(&r.name), r.span), r.span);
                d.stmts.push(Stmt::Let(LetDecl {
                    name: Some(r.name.clone()),
                    ty: None,
                    body,
                    recursive: false,
                    span: r.span,
                }));
            }
            Stmt::Entity(e) => {
                let mut e = e.clone();
                if let Some(extra) = rel_fields.get(&e.name) {
                    e.fields.extend(extra.iter().cloned());
                }
                d.stmts.push(Stmt::Entity(e));
            }
            other => d.stmts.push(other.clone()),
        }
    }
    Desugared {
        program: Program { stmts: d.stmts },
        shapes: d.shapes,
        diagnostics: d.diagnostics,
    }
}

struct Desugar {
    stmts: Vec<Stmt>,
    shapes: ShapeProgram,
    diagnostics: Vec<Diagnostic>,
    handler_seq: usize,
    /// Each entity's field names and declared types (declared + `rel`-injected).
    /// Used to (a) catch a bare identifier in an element body that names a
    /// field but was meant as a bind (`{ cnt }` instead of `{ :cnt }`), and
    /// (b) pick the wire `Encoding` for a `:field` bind.
    entity_fields: std::collections::HashMap<String, std::collections::HashMap<String, Type>>,
}

impl Desugar {
    fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diagnostics.push(Diagnostic::error(span, msg.into()));
    }

    fn view(&mut self, v: &ViewDecl) {
        let root_name = format!("{}#{}", v.name, v.body.entity.to_lowercase());
        if let Some(level) = self.level(&v.body, &root_name, None) {
            self.shapes.views.push(level);
        }
    }

    /// Lower one nesting level (a `select`), emitting its membership/order/attr
    /// `let`s and recursing into nested selects.
    fn level(
        &mut self,
        sel: &SelectExpr,
        name: &str,
        parent: Option<&str>,
    ) -> Option<ShapeLevel> {
        let e = &sel.entity;
        // The row binder in scope: the explicit `as l` alias, or the entity name.
        let binder = sel.binder.clone().unwrap_or_else(|| e.clone());
        let span = sel.span;

        // Split the `where` conjuncts into the membership relation (`:f = P` or
        // a named `rel R = P`) and ordinary domain restrictions.
        let mut membership_rel: Option<Expr> = None;
        let mut restrictions: Vec<Expr> = Vec::new();
        for w in &sel.wheres {
            if let Some(rel) = self.as_membership(w, parent) {
                if membership_rel.is_some() {
                    self.error(w.span, "a nested `select` may have only one membership `where`");
                }
                membership_rel = Some(rel);
            } else {
                restrictions.push(w.clone());
            }
        }
        if parent.is_some() && membership_rel.is_none() {
            self.error(
                span,
                format!("nested `select {e}` needs a membership `where` relating a field to the enclosing binder, e.g. `where :field == Parent`"),
            );
            return None;
        }

        // Membership expr: root -> `E [where ..]`; child -> `E [where ..] . :f`.
        let mut base = ident(e, span);
        for r in &restrictions {
            base = Expr {
                kind: ExprKind::Where(Box::new(base), Box::new(r.clone())),
                span,
            };
        }
        let membership_expr = match &membership_rel {
            None => base, // root: `E [where ..]`
            Some(rel) => compose(base, rel.clone(), span),
        };
        self.emit_let(name, membership_expr);

        // Order view.
        let order_view = sel.order_by.as_ref().map(|path| {
            let vname = format!("{name}#order");
            self.emit_let(&vname, compose(ident(e, span), field_path(path, span), span));
            vname
        });
        let order_field = sel.order_by.as_ref().map(|p| p.join("."));

        // Walk the element into a template + bindings + events + child levels.
        let mut lw = LevelWalk {
            d: self,
            entity: e,
            binder: &binder,
            level_name: name,
            attrs: Vec::new(),
            events: Vec::new(),
            children: Vec::new(),
        };
        let template = lw.element(&sel.body, &[]);
        let attrs = std::mem::take(&mut lw.attrs);
        let events = std::mem::take(&mut lw.events);
        let children = std::mem::take(&mut lw.children);

        Some(ShapeLevel {
            name: name.to_string(),
            entity: e.clone(),
            membership_view: name.to_string(),
            order_view,
            order_field,
            template,
            attrs,
            events,
            children,
        })
    }

    /// If `w` is a membership conjunct `<childrel> = Parent` — where the LHS is
    /// a field path `:list` or a named relation `CardList`, both child->parent —
    /// return the LHS relation expression to compose after the base.
    fn as_membership(&self, w: &Expr, parent: Option<&str>) -> Option<Expr> {
        let ExprKind::Compare { op: CmpOp::Eq, lhs: Some(lhs), rhs } = &w.kind else {
            return None;
        };
        let ExprKind::Ident(name) = &rhs.kind else { return None };
        if parent != Some(name.as_str()) {
            return None;
        }
        match &lhs.kind {
            ExprKind::FieldPath(_) | ExprKind::Ident(_) => Some((**lhs).clone()),
            _ => None,
        }
    }

    fn emit_let(&mut self, name: &str, body: Expr) {
        self.stmts.push(Stmt::Let(LetDecl {
            name: Some(name.to_string()),
            ty: None,
            body,
            recursive: false,
            span: Span::point(0),
        }));
    }
}

/// Walks one level's element, accumulating the template skeleton, the flat
/// attr/event bindings (with child-index paths), and nested child levels.
struct LevelWalk<'a> {
    d: &'a mut Desugar,
    entity: &'a str,
    /// The row binder name in scope (alias or entity name) — what membership
    /// conjuncts of nested selects and handler bodies reference.
    binder: &'a str,
    level_name: &'a str,
    attrs: Vec<AttrBinding>,
    events: Vec<EventBinding>,
    children: Vec<ShapeLevel>,
}

impl LevelWalk<'_> {
    /// Lower an element into a `Tpl`, registering its bindings/handlers at
    /// child-index `path` and its nested selects as child levels.
    fn element(&mut self, el: &ElementExpr, path: &[usize]) -> Tpl {
        // Attribute bindings on this element.
        let mut static_attrs = Vec::new();
        for a in &el.attrs {
            match &a.value {
                AttrValue::Static(s) => static_attrs.push((a.name.clone(), s.clone())),
                AttrValue::Bind(field) => {
                    let kind = if let Some(cls) = a.name.strip_prefix("class.") {
                        BindKind::Class(cls.to_string())
                    } else {
                        BindKind::Prop(a.name.clone())
                    };
                    let encoding = self.field_encoding(field);
                    let view = self.attr_view(field);
                    self.attrs.push(AttrBinding { view, path: path.to_vec(), kind, encoding });
                }
            }
        }
        // Event handlers on this element.
        for h in &el.handlers {
            if let Some(ev) = self.handler(h, path) {
                self.events.push(ev);
            }
        }
        // Children: static text, dynamic text binds, nested elements, nested
        // selects (which become child levels and produce no static node).
        let mut tpl_children = Vec::new();
        for c in &el.children {
            match c {
                Content::Text(s) => tpl_children.push(Tpl::Static(s.clone())),
                Content::Bind(field) => {
                    // Bind this element's textContent; not a static child node.
                    let encoding = self.field_encoding(field);
                    let view = self.attr_view(field);
                    self.attrs.push(AttrBinding {
                        view,
                        path: path.to_vec(),
                        kind: BindKind::Text,
                        encoding,
                    });
                }
                Content::Element(child) => {
                    // A childless, attribute-less bare tag that happens to
                    // name a field of the enclosing entity is almost always
                    // a forgotten `:` (`{ cnt }` meant `{ :cnt }`), not a
                    // deliberate empty element — reject it instead of
                    // silently emitting `<cnt>`.
                    if child.classes.is_empty()
                        && child.modifiers.is_empty()
                        && child.attrs.is_empty()
                        && child.handlers.is_empty()
                        && child.children.is_empty()
                        && self
                            .d
                            .entity_fields
                            .get(self.entity)
                            .is_some_and(|fields| fields.contains_key(&child.tag))
                    {
                        self.d.error(
                            child.span,
                            format!(
                                "unknown element `{}`; did you mean `{{ :{} }}`?",
                                child.tag, child.tag
                            ),
                        );
                    }
                    let mut cp = path.to_vec();
                    cp.push(tpl_children.len());
                    tpl_children.push(self.element(child, &cp));
                }
                Content::Select(sel) => {
                    let child_name = format!("{}#{}", self.level_name, sel.entity.to_lowercase());
                    // The child's membership `where :f = <binder>` references THIS
                    // level's binder (alias or entity name), so pass it as parent.
                    if let Some(level) =
                        self.d.level(sel, &child_name, Some(self.binder))
                    {
                        self.children.push(level);
                    }
                }
            }
        }
        Tpl::Elem {
            tag: el.tag.clone(),
            classes: el.classes.clone(),
            modifiers: el.modifiers.clone(),
            attrs: static_attrs,
            children: tpl_children,
        }
    }

    fn attr_view(&mut self, field: &[String]) -> String {
        let name = format!("{}#{}", self.level_name, field.join("."));
        self.d
            .emit_let(&name, compose(ident(self.entity, Span::point(0)), field_path(field, Span::point(0)), Span::point(0)));
        name
    }

    /// The wire `Encoding` a `:field` (or `:a.b`) path resolves to, walking
    /// through entity-typed hops via the declared field types collected in
    /// `Desugar::entity_fields`. Falls back to `Text` for a path the checker
    /// hasn't type-checked yet (unknown entity/field) — the checker rejects
    /// those independently, so this is a best-effort codegen hint, not a
    /// source of truth.
    fn field_encoding(&self, field: &[String]) -> Encoding {
        let mut entity = self.entity.to_string();
        let mut ty: Option<&Type> = None;
        for seg in field {
            let Some(t) = self.d.entity_fields.get(&entity).and_then(|f| f.get(seg)) else {
                return Encoding::Text;
            };
            ty = Some(t);
            match entity_of_type(t) {
                Some(next) => entity = next,
                None => break,
            }
        }
        ty.map(encoding_of_type).unwrap_or(Encoding::Text)
    }

    /// Lower an inline handler into an [`EventBinding`], recording its
    /// [`HandlerDef`]. Returns `None` on error.
    fn handler(&mut self, h: &HandlerDecl, path: &[usize]) -> Option<EventBinding> {
        self.d.handler_seq += 1;
        let name = format!("{}@{}{}", self.level_name, h.event, self.d.handler_seq);

        // Scope: the level's own binder (self) plus params. The binder maps to
        // this level's entity so `delete l` / `new Card { list: l }` type-check.
        let self_binder = self.binder.to_string();
        let mut scope: Vec<(String, String)> = vec![(self_binder.clone(), self.entity.to_string())];
        let mut params = Vec::new();
        let mut args = Vec::new();
        for p in &h.params {
            let enc = encoding_of_type(&p.ty);
            let entity = entity_of_type(&p.ty);
            scope.push((p.name.clone(), entity.unwrap_or_default()));
            params.push((p.name.clone(), enc));
            args.push(ArgSpec {
                name: p.name.clone(),
                encoding: enc,
                extractor: p.extractor.clone(),
            });
        }

        let mut body = Vec::new();
        for m in &h.body {
            body.push(self.mutation(m, &scope)?);
        }

        self.d.shapes.handlers.push(HandlerDef {
            name: name.clone(),
            binders: vec![(self_binder, Encoding::Id)],
            params,
            body,
        });

        Some(EventBinding {
            path: path.to_vec(),
            dom_event: h.event.clone(),
            modifiers: h.modifiers.clone(),
            handler: name,
            args,
        })
    }

    fn mutation(&mut self, m: &Mutation, scope: &[(String, String)]) -> Option<MutationIR> {
        let lookup = |name: &str| scope.iter().find(|(n, _)| n == name).map(|(_, e)| e.clone());
        match m {
            Mutation::Set { binder, field, value, span } => {
                let target_name = binder.clone().unwrap_or_else(|| self.binder.to_string());
                let Some(entity) = lookup(&target_name) else {
                    self.d.error(*span, format!("unknown binder `{target_name}` in mutation"));
                    return None;
                };
                if field.len() != 1 {
                    self.d.error(*span, "only single-field assignment is supported");
                    return None;
                }
                let val = self.val(value, scope)?;
                Some(MutationIR::Set {
                    target: Ref(target_name),
                    entity,
                    updates: vec![(field[0].clone(), val)],
                })
            }
            Mutation::Delete { target, span } => {
                if lookup(target).is_none() {
                    self.d.error(*span, format!("unknown binder `{target}` in `delete`"));
                    return None;
                }
                Some(MutationIR::Delete { target: Ref(target.clone()) })
            }
            Mutation::New { entity, fields, span: _ } => {
                let mut fs = Vec::new();
                for f in fields {
                    fs.push((f.name.clone(), self.val(&f.value, scope)?));
                }
                Some(MutationIR::Insert { entity: entity.clone(), fields: fs })
            }
        }
    }

    fn val(&mut self, e: &Expr, scope: &[(String, String)]) -> Option<ValRef> {
        match &e.kind {
            ExprKind::Str(s) => Some(ValRef::Lit(Lit::Str(s.clone()))),
            ExprKind::Int(n) => Some(ValRef::Lit(Lit::Int(*n))),
            ExprKind::Decimal(s) => Some(ValRef::Lit(Lit::Decimal(s.clone()))),
            ExprKind::Atom(a) => Some(ValRef::Lit(Lit::Atom(a.clone()))),
            ExprKind::Date { year, month, day } => Some(ValRef::Lit(Lit::Date {
                year: *year,
                month: *month,
                day: *day,
            })),
            ExprKind::Ident(name) => {
                if scope.iter().any(|(n, _)| n == name) {
                    Some(ValRef::Arg(name.clone()))
                } else {
                    self.d.error(e.span, format!("unknown binder/param `{name}` in mutation value"));
                    None
                }
            }
            _ => {
                self.d.error(e.span, "unsupported mutation value");
                None
            }
        }
    }
}

// --- surface-AST constructors --------------------------------------------

fn ident(name: &str, span: Span) -> Expr {
    Expr { kind: ExprKind::Ident(name.to_string()), span }
}

fn field_path(parts: &[String], span: Span) -> Expr {
    Expr { kind: ExprKind::FieldPath(parts.to_vec()), span }
}

fn compose(a: Expr, b: Expr, span: Span) -> Expr {
    Expr { kind: ExprKind::Compose(Box::new(a), Box::new(b)), span }
}

fn encoding_of_type(ty: &Type) -> Encoding {
    match &ty.kind {
        TypeKind::Named(n) => match n.as_str() {
            "Int" => Encoding::Int,
            "Money" => Encoding::Money,
            "Text" => Encoding::Text,
            _ => Encoding::Id, // an entity type
        },
        TypeKind::AtomSingleton(_) | TypeKind::Coproduct(_) => Encoding::Atom,
        _ => Encoding::Text,
    }
}

fn entity_of_type(ty: &Type) -> Option<String> {
    match &ty.kind {
        TypeKind::Named(n) if !matches!(n.as_str(), "Int" | "Money" | "Text" | "Date" | "Unit") => {
            Some(n.clone())
        }
        _ => None,
    }
}
