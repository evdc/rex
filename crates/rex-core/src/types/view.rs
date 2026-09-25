//! View desugaring (§M5): expand each `view` into ordinary `let`s plus a
//! [`ShapeProgram`] the compiler emits JS from.
//!
//! The narrow core never learns about UI. A `view` is sugar: its nesting
//! becomes composite-keyed membership relations, its `order by` an order
//! relation, its bound attributes attribute relations — all auto-derived
//! `let`s the existing checker elaborates. What can't be a relation (the DOM
//! skeleton, event wiring) becomes the ShapeIR/EventIR, consumed by codegen.
//!
//! The load-bearing rule (see plan): binders are second-class — they denote
//! row keys, never relations. A binder appears only as the RHS of a membership
//! conjunct (`.list = l`), as a `do` argument, or as the implicit subject of
//! `.field` paths in its own body. A DOM handler never mutates directly: it
//! `do`es named events, passing its own level's binder, any enclosing
//! binder (the shaper hands each template its ancestor keys), and its
//! extracted params. Named events (`event E(…)` + `on E(…)`) own the checked
//! mutation bodies (the EventIR).

use super::shape_ir::*;
use super::typed::{ArithKind, Lit};
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
        attr_seq: 0,
        entity_fields,
        event_spans: Default::default(),
        hidden_lets: Vec::new(),
        hidden_seq: 0,
    };
    // Pass 0: `event` declarations, then their `on` handlers (order-free, so a
    // view or a handler may `do` an event declared later in the file).
    for stmt in &program.stmts {
        if let Stmt::Event(e) = stmt {
            d.event_decl(e);
        }
    }
    for stmt in &program.stmts {
        if let Stmt::On(o) = stmt {
            d.on_decl(o);
        }
    }
    d.check_do_graph();
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
                "`state` is not supported yet (MVP-PLAN S-51)".to_string(),
            )),
            // Consumed in pass 0.
            Stmt::Event(_) | Stmt::On(_) => {}
            Stmt::Type(t) => d.diagnostics.push(Diagnostic::error(
                t.span,
                "`type` is not supported yet (MVP-PLAN S-50)".to_string(),
            )),
            Stmt::Import(i) => d.diagnostics.push(Diagnostic::error(
                i.span,
                "`import js` is not supported yet (MVP-PLAN S-42)".to_string(),
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
    // Hidden bulk-target keyset views (S-41) go last: every entity they
    // could read is already in `d.stmts` by now, regardless of whether the
    // `on` handler that produced them appeared before or after it in source.
    d.stmts.append(&mut d.hidden_lets);
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
    /// Sequence for auto-named attribute views of non-path binds.
    attr_seq: usize,
    /// Each entity's field names and declared types (declared + `rel`-injected).
    /// Used to (a) catch a bare identifier in an element body that names a
    /// field but was meant as a bind (`{ cnt }` instead of `{ :cnt }`), and
    /// (b) pick the wire `Encoding` for a `:field` bind.
    entity_fields: std::collections::HashMap<String, std::collections::HashMap<String, Type>>,
    /// Event name -> (decl span, `on` span if any); for diagnostics.
    event_spans: std::collections::HashMap<String, (Span, Option<Span>)>,
    /// Hidden keyset `let`s synthesized for an arg-free `where`-targeted bulk
    /// mutation (S-41). Appended to `stmts` only after every real statement
    /// has been processed (see `desugar`), so a hidden view — built in Pass 0
    /// while `on` handlers are checked — never lands before the entity it
    /// reads, regardless of source order.
    hidden_lets: Vec<Stmt>,
    /// Sequence for hidden keyset view names (`on#E#k`).
    hidden_seq: usize,
}


/// A name a handler body may reference, with what it denotes.
type Scope = Vec<(String, ParamTy)>;

/// A mutation-value expression's type (MVP-PLAN S-40): coarser than the
/// checker's `ValueTy` (no `SortId`s exist yet — view desugaring runs before
/// the checker mints sorts) but enough to validate arithmetic/comparison
/// operands, field targets, and (for `not`) the atom pair to flip between.
#[derive(Clone, Debug, PartialEq)]
enum ValTy {
    Int,
    Money,
    Text,
    Date,
    /// An entity id, by entity name.
    Id(String),
    /// An atom (co)product; every alternative, in declared order. Empty for
    /// an opaque atom type (`Bool`, pending its own sugar, MVP-PLAN §2.9) —
    /// `not` on one of those is rejected rather than guessed at.
    Atoms(Vec<String>),
}

impl ValTy {
    fn is_numeric(&self) -> bool {
        matches!(self, ValTy::Int | ValTy::Money)
    }

    fn encoding(&self) -> Encoding {
        match self {
            ValTy::Int => Encoding::Int,
            ValTy::Money => Encoding::Money,
            ValTy::Text | ValTy::Date => Encoding::Text,
            ValTy::Id(_) => Encoding::Id,
            ValTy::Atoms(_) => Encoding::Atom,
        }
    }
}

fn show_valty(t: &ValTy) -> String {
    match t {
        ValTy::Int => "Int".to_string(),
        ValTy::Money => "Money".to_string(),
        ValTy::Text => "Text".to_string(),
        ValTy::Date => "Date".to_string(),
        ValTy::Id(e) => e.clone(),
        ValTy::Atoms(atoms) if atoms.is_empty() => "an atom".to_string(),
        ValTy::Atoms(atoms) => {
            format!("{{{}}}", atoms.iter().map(|a| format!("@{a}")).collect::<Vec<_>>().join(" | "))
        }
    }
}

/// A scope param's (wire-level) `ParamTy` widened to a `ValTy`. Lossy for an
/// atom-typed param (the wire format doesn't carry the alternative names),
/// which only matters if `not` is applied directly to a bare param rather
/// than a field access — rare, and rejected cleanly (`Atoms(vec![])`) rather
/// than silently wrong.
fn scope_val_ty(pty: &ParamTy) -> ValTy {
    match pty {
        ParamTy::Id(e) => ValTy::Id(e.clone()),
        ParamTy::Scalar(Encoding::Int) => ValTy::Int,
        ParamTy::Scalar(Encoding::Money) => ValTy::Money,
        ParamTy::Scalar(Encoding::Text) => ValTy::Text,
        ParamTy::Scalar(Encoding::Atom) => ValTy::Atoms(Vec::new()),
        ParamTy::Scalar(Encoding::Id) | ParamTy::Rel(..) => ValTy::Atoms(Vec::new()),
    }
}

/// Whether `e` references any name in `scope` (an event param) — decides
/// whether a bulk mutation's `where` predicate (S-41) can be precomputed as
/// a static keyset view or must be scanned per dispatch instead.
/// Conservative: a shape this walk doesn't recognize counts as dependent,
/// never as arg-free — the worst outcome is an avoidable scan, never a wrong
/// materialized view.
fn expr_refs_scope(e: &Expr, scope: &Scope) -> bool {
    match &e.kind {
        ExprKind::Ident(name) => scope.iter().any(|(n, _)| n == name),
        ExprKind::FieldPath(_)
        | ExprKind::Atom(_)
        | ExprKind::Int(_)
        | ExprKind::Decimal(_)
        | ExprKind::Str(_)
        | ExprKind::Date { .. }
        | ExprKind::Id => false,
        ExprKind::Not(a) => expr_refs_scope(a, scope),
        ExprKind::Add(a, b)
        | ExprKind::Sub(a, b)
        | ExprKind::Mul(a, b)
        | ExprKind::Div(a, b)
        | ExprKind::Mod(a, b)
        | ExprKind::Concat(a, b)
        | ExprKind::Compose(a, b) => expr_refs_scope(a, scope) || expr_refs_scope(b, scope),
        ExprKind::Compare { lhs, rhs, .. } | ExprKind::In { lhs, rhs } => {
            lhs.as_deref().is_some_and(|l| expr_refs_scope(l, scope)) || expr_refs_scope(rhs, scope)
        }
        _ => true,
    }
}

/// Rewrite every `.a.b.c` (an implicit-subject field path) in `e` to
/// `row.a.b.c`: turns "the row under test" from an ambient default into an
/// explicit param `val_expr` already knows how to check field hops on — the
/// row a bulk mutation's per-dispatch scan (S-41) evaluates the predicate
/// against. Leaves everything outside `val_expr`'s own grammar (literals,
/// `Ident`, `Compose`, `Not`, arithmetic, `Compare`) untouched; anything else
/// is rejected by `val_expr` itself, so there's nothing to rewrite in it.
fn rewrite_row_refs(e: &Expr, row: &str) -> Expr {
    let kind = match &e.kind {
        ExprKind::FieldPath(parts) => {
            let mut acc = ident(row, e.span);
            for p in parts {
                acc = compose(acc, ident(p, e.span), e.span);
            }
            return acc;
        }
        ExprKind::Not(a) => ExprKind::Not(Box::new(rewrite_row_refs(a, row))),
        ExprKind::Add(a, b) => ExprKind::Add(Box::new(rewrite_row_refs(a, row)), Box::new(rewrite_row_refs(b, row))),
        ExprKind::Sub(a, b) => ExprKind::Sub(Box::new(rewrite_row_refs(a, row)), Box::new(rewrite_row_refs(b, row))),
        ExprKind::Div(a, b) => ExprKind::Div(Box::new(rewrite_row_refs(a, row)), Box::new(rewrite_row_refs(b, row))),
        ExprKind::Mod(a, b) => ExprKind::Mod(Box::new(rewrite_row_refs(a, row)), Box::new(rewrite_row_refs(b, row))),
        ExprKind::Compose(a, b) => {
            ExprKind::Compose(Box::new(rewrite_row_refs(a, row)), Box::new(rewrite_row_refs(b, row)))
        }
        ExprKind::Compare { op, lhs, rhs } => ExprKind::Compare {
            op: *op,
            lhs: lhs.as_deref().map(|l| Box::new(rewrite_row_refs(l, row))),
            rhs: Box::new(rewrite_row_refs(rhs, row)),
        },
        _ => e.kind.clone(),
    };
    Expr { kind, span: e.span }
}

/// Whether a value of type `got` may be written where `want` is declared:
/// exact match, `Int` widening to `Money`, or an atom subset.
fn valty_assignable(want: &ValTy, got: &ValTy) -> bool {
    if want == got {
        return true;
    }
    match (want, got) {
        (ValTy::Money, ValTy::Int) => true,
        (ValTy::Atoms(w), ValTy::Atoms(g)) => !g.is_empty() && g.iter().all(|a| w.contains(a)),
        _ => false,
    }
}

impl Desugar {
    fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diagnostics.push(Diagnostic::error(span, msg.into()));
    }

    // --- events -----------------------------------------------------------

    fn event_decl(&mut self, e: &EventDecl) {
        if self.event_spans.contains_key(&e.name) {
            self.error(e.span, format!("event `{}` is declared twice", e.name));
            return;
        }
        let mut params = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for p in &e.params {
            if !seen.insert(p.name.clone()) {
                self.error(p.span, format!("duplicate parameter `{}`", p.name));
            }
            match self.param_ty(&p.ty) {
                Some(ty) => params.push(EventParam { name: p.name.clone(), ty }),
                None => return,
            }
        }
        self.event_spans.insert(e.name.clone(), (e.span, None));
        self.shapes.events.push(EventDef { name: e.name.clone(), params, body: Vec::new() });
    }

    fn on_decl(&mut self, o: &OnDecl) {
        let Some(idx) = self.shapes.events.iter().position(|e| e.name == o.event) else {
            self.error(o.span, format!("`on {}`: no such event; declare it with `event {}(…)`", o.event, o.event));
            return;
        };
        if self.event_spans[&o.event].1.is_some() {
            self.error(o.span, format!("event `{}` already has an `on` handler", o.event));
            return;
        }
        self.event_spans.get_mut(&o.event).unwrap().1 = Some(o.span);
        let decl_params = self.shapes.events[idx].params.clone();
        if o.params.len() != decl_params.len() {
            self.error(
                o.span,
                format!(
                    "`on {}` binds {} parameter(s) but the event declares {}",
                    o.event,
                    o.params.len(),
                    decl_params.len()
                ),
            );
            return;
        }
        let scope: Scope = o
            .params
            .iter()
            .zip(&decl_params)
            .map(|(n, p)| (n.clone(), p.ty.clone()))
            .collect();
        let mut body = Vec::new();
        for m in &o.body {
            match self.mutation(m, &scope, None, &o.event) {
                Some(ir) => body.push(ir),
                None => return,
            }
        }
        self.shapes.events[idx].body = body;
    }

    /// Reject a cycle in the static synchronous-`do` graph (§5 decision 2:
    /// a `do` inside an `on` body joins the caller's transaction, so a cycle
    /// would never terminate).
    fn check_do_graph(&mut self) {
        let names: Vec<String> = self.shapes.events.iter().map(|e| e.name.clone()).collect();
        for name in names {
            let mut stack = vec![name.clone()];
            if let Some(cycle) = self.find_cycle(&name, &mut stack) {
                let span = self.event_spans[&name].1.unwrap_or(self.event_spans[&name].0);
                self.error(span, format!("synchronous `do` cycle: {}", cycle.join(" -> ")));
                return;
            }
        }
    }

    fn find_cycle(&self, at: &str, stack: &mut Vec<String>) -> Option<Vec<String>> {
        let def = self.shapes.events.iter().find(|e| e.name == at)?;
        for m in &def.body {
            if let MutationIR::Do { event, .. } = m {
                if let Some(pos) = stack.iter().position(|s| s == event) {
                    let mut cycle = stack[pos..].to_vec();
                    cycle.push(event.clone());
                    return Some(cycle);
                }
                stack.push(event.clone());
                if let Some(c) = self.find_cycle(event, stack) {
                    return Some(c);
                }
                stack.pop();
            }
        }
        None
    }

    /// The wire shape of a declared parameter type.
    fn param_ty(&mut self, ty: &Type) -> Option<ParamTy> {
        match &ty.kind {
            TypeKind::Named(n) => match n.as_str() {
                "Int" | "Money" | "Text" | "Date" | "Bool" => Some(ParamTy::Scalar(encoding_of_type(ty))),
                _ if self.entity_fields.contains_key(n) => Some(ParamTy::Id(n.clone())),
                // The sort-name spelling `ListID` names the same entity.
                _ if n.strip_suffix("ID").is_some_and(|e| self.entity_fields.contains_key(e)) => {
                    Some(ParamTy::Id(n.strip_suffix("ID").unwrap().to_string()))
                }
                _ => {
                    self.error(ty.span, format!("unknown type `{n}`"));
                    None
                }
            },
            TypeKind::AtomSingleton(_) | TypeKind::Coproduct(_) => Some(ParamTy::Scalar(Encoding::Atom)),
            TypeKind::Arrow(a, b) => match (&a.kind, &b.kind) {
                (TypeKind::Named(x), TypeKind::Named(y)) => Some(ParamTy::Rel(x.clone(), y.clone())),
                _ => {
                    self.error(ty.span, "a relation-typed parameter must be `A -> B` with named types");
                    None
                }
            },
            TypeKind::Product(..) => {
                self.error(ty.span, "product-typed event parameters are not supported");
                None
            }
        }
    }

    /// Lower one handler-body statement against `scope`. `implicit` is the
    /// binder a bare `.f := v` targets (a DOM handler's own row); `None` in
    /// an `on` body, where every target must be named.
    fn mutation(&mut self, m: &HStmt, scope: &Scope, implicit: Option<&str>, event: &str) -> Option<MutationIR> {
        let span = m.span();
        match m {
            HStmt::Assign { binder, field, value, .. } => {
                let Some(target_name) = binder.clone().or_else(|| implicit.map(str::to_string)) else {
                    self.error(span, "`.f := v` needs a target: write `x.f := v` for a parameter `x`");
                    return None;
                };
                let entity = self.target_entity(&target_name, scope, span)?;
                let (val, ty) = self.val(value, scope)?;
                self.check_field_value(&entity, field, &ty, value.span)?;
                Some(MutationIR::Set {
                    target: Target::One(Ref(target_name)),
                    entity,
                    updates: vec![(field.clone(), val)],
                })
            }
            HStmt::Update { target, sets, .. } => {
                let (bulk_target, entity) = self.mutation_target(target, scope, event)?;
                let mut updates = Vec::new();
                for f in sets {
                    let (v, ty) = self.val(&f.value, scope)?;
                    self.check_field_value(&entity, &f.name, &ty, f.value.span)?;
                    updates.push((f.name.clone(), v));
                }
                Some(MutationIR::Set { target: bulk_target, entity, updates })
            }
            HStmt::Delete { target, .. } => {
                // `delete Entity` (no predicate at all): retract the whole
                // identity relation in one transaction (S-41 subtask 3) —
                // distinct from a `where`-targeted bulk delete below.
                if let ExprKind::Ident(name) = &target.kind
                    && scope.iter().all(|(n, _)| n != name)
                {
                    if !self.entity_fields.contains_key(name) {
                        self.error(target.span, format!("unknown parameter or entity `{name}` in mutation"));
                        return None;
                    }
                    return Some(MutationIR::Delete { target: Target::All { entity: name.clone() } });
                }
                let (bulk_target, _entity) = self.mutation_target(target, scope, event)?;
                Some(MutationIR::Delete { target: bulk_target })
            }
            HStmt::New { bind, entity, from, fields, .. } => {
                if bind.is_some() {
                    self.error(span, "`let x = new …` in a handler is not supported yet (MVP-PLAN S-40)");
                    return None;
                }
                if !self.entity_fields.contains_key(entity) {
                    self.error(span, format!("unknown entity `{entity}`"));
                    return None;
                }
                match from {
                    None => {
                        let mut fs = Vec::new();
                        for f in fields {
                            let (v, ty) = self.val(&f.value, scope)?;
                            self.check_field_value(entity, &f.name, &ty, f.value.span)?;
                            fs.push((f.name.clone(), v));
                        }
                        Some(MutationIR::Insert { entity: entity.clone(), fields: fs })
                    }
                    Some(from) => self.new_from(entity, from, fields, scope, span),
                }
            }
            HStmt::Do { event, args, .. } => {
                let params = self.event_params(event, args.len(), span)?;
                let mut vals = Vec::new();
                for (a, p) in args.iter().zip(&params) {
                    let (v, ty) = self.val(a, scope)?;
                    self.check_arg(&ty, &p.ty, a.span, event, &p.name)?;
                    vals.push(v);
                }
                Some(MutationIR::Do { event: event.clone(), args: vals })
            }
            HStmt::Set { .. } => {
                self.error(span, "`set` is not supported yet (MVP-PLAN S-51)");
                None
            }
            HStmt::Clear { .. } | HStmt::Focus { .. } => {
                self.error(span, "`clear`/`focus` are DOM actions; they are not allowed in an `on` body");
                None
            }
        }
    }

    /// Check an `update`/`delete` target: a bound param (a single row) or
    /// `Entity where P` (a bulk keyset, S-41). Returns the checked
    /// [`Target`] together with the entity it ranges over.
    fn mutation_target(&mut self, target: &Expr, scope: &Scope, event: &str) -> Option<(Target, String)> {
        match &target.kind {
            ExprKind::Ident(name) => {
                let entity = self.target_entity(name, scope, target.span)?;
                Some((Target::One(Ref(name.clone())), entity))
            }
            ExprKind::Where(base, pred) => {
                let ExprKind::Ident(entity) = &base.kind else {
                    self.error(base.span, "a bulk mutation target must be `Entity where predicate`");
                    return None;
                };
                if !self.entity_fields.contains_key(entity) {
                    self.error(base.span, format!("unknown entity `{entity}`"));
                    return None;
                }
                if expr_refs_scope(pred, scope) {
                    // Arg-dependent: can't precompute, so it's a per-dispatch
                    // scan over the whole entity (subtask 2) — the predicate
                    // is checked as a point-evaluation `ValExpr`, with a
                    // `.field` path rewritten to read the row under test.
                    let mut pscope = scope.clone();
                    pscope.push((ROW_SELF.to_string(), ParamTy::Id(entity.clone())));
                    let rewritten = rewrite_row_refs(pred, ROW_SELF);
                    let (ve, ty) = self.val_expr(&rewritten, &pscope)?;
                    if ty != ValTy::Atoms(vec!["true".to_string(), "false".to_string()]) {
                        self.error(pred.span, "a bulk mutation's `where` predicate must be a comparison");
                        return None;
                    }
                    Some((Target::Scan { entity: entity.clone(), pred: ve }, entity.clone()))
                } else {
                    // Arg-free: desugar to a hidden keyset `let` (subtask 1),
                    // typed and materialized by the ordinary checker/engine —
                    // dispatch just reads its rows, never scans.
                    self.hidden_seq += 1;
                    let view = format!("on#{event}#{}", self.hidden_seq);
                    self.hidden_lets.push(Stmt::Let(LetDecl {
                        name: Some(view.clone()),
                        ty: None,
                        body: target.clone(),
                        recursive: false,
                        span: target.span,
                    }));
                    Some((Target::View { view, entity: entity.clone() }, entity.clone()))
                }
            }
            _ => {
                self.error(target.span, "a mutation target must be a parameter or `Entity where predicate`");
                None
            }
        }
    }

    /// Check `new Entity from source as (key, value) { … }` (S-42): `source`
    /// must be a relation-typed event param; each field initializer may
    /// reference the per-row `key`/`value` binder directly, or be an
    /// ordinary checked mutation value shared by every minted row. One
    /// transaction mints a row per source row, in key order —
    /// `events.rs`'s `MutationIR::InsertFrom` expansion.
    fn new_from(&mut self, entity: &str, from: &FromClause, fields: &[FieldInit], scope: &Scope, span: Span) -> Option<MutationIR> {
        let ExprKind::Ident(param) = &from.source.kind else {
            self.error(from.source.span, "`new … from` needs a relation-typed event parameter");
            return None;
        };
        let Some((_, pty)) = scope.iter().find(|(n, _)| n == param) else {
            self.error(from.source.span, format!("unknown parameter `{param}`"));
            return None;
        };
        let ParamTy::Rel(key_ty, val_ty) = pty.clone() else {
            self.error(from.source.span, format!("`{param}` is not a relation-typed parameter"));
            return None;
        };
        let key_vt = self.named_val_ty(&key_ty, span)?;
        let val_vt = self.named_val_ty(&val_ty, span)?;
        let mut fs = Vec::new();
        for f in fields {
            let (field_val, ty) = match &f.value.kind {
                ExprKind::Ident(n) if *n == from.key => (FromField::Key, key_vt.clone()),
                ExprKind::Ident(n) if *n == from.value => (FromField::Value, val_vt.clone()),
                _ => {
                    let (v, ty) = self.val(&f.value, scope)?;
                    (FromField::Val(v), ty)
                }
            };
            self.check_field_value(entity, &f.name, &ty, f.value.span)?;
            fs.push((f.name.clone(), field_val));
        }
        Some(MutationIR::InsertFrom { entity: entity.to_string(), param: param.clone(), fields: fs })
    }

    /// A `ParamTy::Rel` side's name (an entity or a scalar type name)
    /// resolved to a mutation-value `ValTy`, reusing `resolve_val_ty`'s name
    /// resolution against a synthetic `Type` — there's no source `Type` here,
    /// the param was already checked once when the event was declared.
    fn named_val_ty(&mut self, name: &str, span: Span) -> Option<ValTy> {
        self.resolve_val_ty(&Type { kind: TypeKind::Named(name.to_string()), span })
    }

    /// The declared params of `event`, checking the call's arity.
    fn event_params(&mut self, event: &str, nargs: usize, span: Span) -> Option<Vec<EventParam>> {
        let Some(def) = self.shapes.events.iter().find(|e| e.name == event) else {
            self.error(span, format!("`do {event}`: no such event"));
            return None;
        };
        if def.params.len() != nargs {
            self.error(
                span,
                format!("`do {event}` passes {nargs} argument(s) but the event declares {}", def.params.len()),
            );
            return None;
        }
        Some(def.params.clone())
    }

    /// Check one `do` argument against the callee's declared param type.
    fn check_arg(&mut self, got: &ValTy, want: &ParamTy, span: Span, event: &str, pname: &str) -> Option<()> {
        let ok = match (want, got) {
            (ParamTy::Id(e), ValTy::Id(g)) => e == g,
            (ParamTy::Scalar(enc), got) => *enc == got.encoding(),
            _ => false,
        };
        if !ok {
            self.error(span, format!("`do {event}`: argument for `{pname}` has the wrong type"));
            return None;
        }
        Some(())
    }

    /// Check a field initializer/update against the entity's declared field
    /// type (the checker never sees handler bodies, so this is their typing).
    fn check_field_value(&mut self, entity: &str, field: &str, got: &ValTy, span: Span) -> Option<()> {
        let Some(ty) = self.entity_fields.get(entity).and_then(|f| f.get(field)).cloned() else {
            self.error(span, format!("no field `{field}` on `{entity}`"));
            return None;
        };
        let want = self.resolve_val_ty(&ty)?;
        if !valty_assignable(&want, got) {
            self.error(span, format!("field `{field}` of `{entity}` expects `{}`", show_type(&ty)));
            return None;
        }
        Some(())
    }

    /// The entity of a mutation target named in `scope`.
    fn target_entity(&mut self, name: &str, scope: &Scope, span: Span) -> Option<String> {
        match scope.iter().find(|(n, _)| n == name) {
            Some((_, ParamTy::Id(e))) => Some(e.clone()),
            Some(_) => {
                self.error(span, format!("`{name}` is not an entity id and cannot be a mutation target"));
                None
            }
            None => {
                self.error(span, format!("unknown parameter `{name}` in mutation"));
                None
            }
        }
    }

    /// The wire type (§ old `param_ty`) resolved to a mutation-value `ValTy`:
    /// entity ids by name (no `SortId`s exist yet — desugaring runs before
    /// the checker mints sorts) and atom (co)products with every alternative
    /// named, so `not` (S-40) knows the other one.
    fn resolve_val_ty(&mut self, ty: &Type) -> Option<ValTy> {
        match &ty.kind {
            TypeKind::Named(n) => match n.as_str() {
                "Int" => Some(ValTy::Int),
                "Money" => Some(ValTy::Money),
                "Text" => Some(ValTy::Text),
                "Date" => Some(ValTy::Date),
                // `Bool` sugar (MVP-PLAN §2.9/decision 3) isn't implemented
                // yet, so it's an opaque atom: `not` on one is rejected
                // rather than guessing which atom means "true".
                "Bool" => Some(ValTy::Atoms(Vec::new())),
                _ if self.entity_fields.contains_key(n) => Some(ValTy::Id(n.clone())),
                _ if n.strip_suffix("ID").is_some_and(|e| self.entity_fields.contains_key(e)) => {
                    Some(ValTy::Id(n.strip_suffix("ID").unwrap().to_string()))
                }
                _ => {
                    self.error(ty.span, format!("unknown type `{n}`"));
                    None
                }
            },
            TypeKind::AtomSingleton(a) => Some(ValTy::Atoms(vec![a.clone()])),
            TypeKind::Coproduct(elems) => {
                let mut atoms = Vec::new();
                for e in elems {
                    match &e.kind {
                        TypeKind::AtomSingleton(a) => atoms.push(a.clone()),
                        _ => {
                            self.error(e.span, "a coproduct's members must all be atoms");
                            return None;
                        }
                    }
                }
                Some(ValTy::Atoms(atoms))
            }
            TypeKind::Arrow(..) => {
                self.error(ty.span, "expected a value type here, found a relation type `->`");
                None
            }
            TypeKind::Product(..) => {
                self.error(ty.span, "product types are not supported in a mutation value yet");
                None
            }
        }
    }

    /// Check a mutation value: a literal or a bare parameter keeps the
    /// simple `ValRef` shape; anything else is a checked expression
    /// (`ValRef::Expr`, S-40).
    fn val(&mut self, e: &Expr, scope: &Scope) -> Option<(ValRef, ValTy)> {
        match &e.kind {
            ExprKind::Str(s) => Some((ValRef::Lit(Lit::Str(s.clone())), ValTy::Text)),
            ExprKind::Int(n) => Some((ValRef::Lit(Lit::Int(*n)), ValTy::Int)),
            ExprKind::Decimal(s) => Some((ValRef::Lit(Lit::Decimal(s.clone())), ValTy::Money)),
            ExprKind::Atom(a) => Some((ValRef::Lit(Lit::Atom(a.clone())), ValTy::Atoms(vec![a.clone()]))),
            ExprKind::Date { year, month, day } => Some((
                ValRef::Lit(Lit::Date { year: *year, month: *month, day: *day }),
                ValTy::Date,
            )),
            ExprKind::Ident(name) => match scope.iter().find(|(n, _)| n == name) {
                Some((_, pty)) => Some((ValRef::Arg(name.clone()), scope_val_ty(pty))),
                None => {
                    self.error(e.span, format!("unknown parameter `{name}` in mutation value"));
                    None
                }
            },
            _ => {
                let (ve, ty) = self.val_expr(e, scope)?;
                Some((ValRef::Expr(ve), ty))
            }
        }
    }

    /// A compound mutation-value expression (MVP-PLAN S-40, §2.10: "Rex
    /// expressions evaluated at a key"). Every leaf is a literal or a
    /// handler param, so this is a point-evaluation sublanguage: evaluating
    /// one always yields exactly one value, never a relation — it does not
    /// reuse `check_rel`, which types the general relational language
    /// (ambient domains ranging over whole entities).
    fn val_expr(&mut self, e: &Expr, scope: &Scope) -> Option<(ValExpr, ValTy)> {
        match &e.kind {
            ExprKind::Str(s) => Some((ValExpr::Lit(Lit::Str(s.clone())), ValTy::Text)),
            ExprKind::Int(n) => Some((ValExpr::Lit(Lit::Int(*n)), ValTy::Int)),
            ExprKind::Decimal(s) => Some((ValExpr::Lit(Lit::Decimal(s.clone())), ValTy::Money)),
            ExprKind::Atom(a) => Some((ValExpr::Lit(Lit::Atom(a.clone())), ValTy::Atoms(vec![a.clone()]))),
            ExprKind::Date { year, month, day } => Some((
                ValExpr::Lit(Lit::Date { year: *year, month: *month, day: *day }),
                ValTy::Date,
            )),
            ExprKind::Ident(name) => match scope.iter().find(|(n, _)| n == name) {
                Some((_, pty)) => Some((ValExpr::Param(name.clone()), scope_val_ty(pty))),
                None => {
                    self.error(e.span, format!("unknown parameter `{name}`"));
                    None
                }
            },
            ExprKind::Compose(a, b) => {
                let (va, ta) = self.val_expr(a, scope)?;
                let ExprKind::Ident(field) = &b.kind else {
                    self.error(b.span, "expected a field name after `.`");
                    return None;
                };
                let ValTy::Id(entity) = &ta else {
                    self.error(a.span, format!("`.{field}` needs an entity value on its left, found `{}`", show_valty(&ta)));
                    return None;
                };
                let Some(fty) = self.entity_fields.get(entity).and_then(|f| f.get(field)).cloned() else {
                    self.error(b.span, format!("no field `{field}` on `{entity}`"));
                    return None;
                };
                let vt = self.resolve_val_ty(&fty)?;
                Some((ValExpr::Field(Box::new(va), field.clone()), vt))
            }
            ExprKind::Not(inner) => {
                let (vi, ti) = self.val_expr(inner, scope)?;
                let ValTy::Atoms(atoms) = &ti else {
                    self.error(inner.span, format!("`not` needs a two-valued (boolean-like) operand, found `{}`", show_valty(&ti)));
                    return None;
                };
                if atoms.len() != 2 {
                    self.error(inner.span, format!("`not` needs exactly two alternatives, found {}", atoms.len()));
                    return None;
                }
                let pair = [atoms[0].clone(), atoms[1].clone()];
                Some((ValExpr::Not(Box::new(vi), pair), ti))
            }
            ExprKind::Add(a, b) => self.val_arith(a, b, scope, ArithKind::Add, false),
            ExprKind::Sub(a, b) => self.val_arith(a, b, scope, ArithKind::Sub, false),
            ExprKind::Div(a, b) => self.val_arith(a, b, scope, ArithKind::Div, true),
            ExprKind::Mod(a, b) => self.val_arith(a, b, scope, ArithKind::Mod, true),
            ExprKind::Compare { op, lhs: Some(lhs), rhs } => {
                let (va, ta) = self.val_expr(lhs, scope)?;
                let (vb, tb) = self.val_expr(rhs, scope)?;
                if !(ta.is_numeric() && tb.is_numeric()) && ta != tb {
                    self.error(e.span, format!("cannot compare `{}` with `{}`", show_valty(&ta), show_valty(&tb)));
                    return None;
                }
                Some((
                    ValExpr::Compare(*op, Box::new(va), Box::new(vb)),
                    ValTy::Atoms(vec!["true".to_string(), "false".to_string()]),
                ))
            }
            _ => {
                self.error(e.span, "this expression is not supported in a mutation value yet (MVP-PLAN S-40/S-52)");
                None
            }
        }
    }

    fn val_arith(
        &mut self,
        a: &Expr,
        b: &Expr,
        scope: &Scope,
        kind: ArithKind,
        int_only: bool,
    ) -> Option<(ValExpr, ValTy)> {
        let (va, ta) = self.val_expr(a, scope)?;
        let (vb, tb) = self.val_expr(b, scope)?;
        if !ta.is_numeric() || !tb.is_numeric() {
            self.error(a.span, format!("arithmetic needs numeric operands, got `{}` and `{}`", show_valty(&ta), show_valty(&tb)));
            return None;
        }
        if int_only && (ta != ValTy::Int || tb != ValTy::Int) {
            self.error(a.span, "`/`/`%` need `Int` operands");
            return None;
        }
        let money = ta == ValTy::Money || tb == ValTy::Money;
        let result = if money { ValTy::Money } else { ValTy::Int };
        Some((ValExpr::Arith(kind, Box::new(va), Box::new(vb), money), result))
    }

    fn view(&mut self, v: &ViewDecl) {
        if !v.params.is_empty() {
            self.error(v.span, "components (`view Name(params)`) are not supported yet (MVP-PLAN S-61)");
            return;
        }
        if let Some(l) = v.locals.first() {
            self.error(l.span, "`local` state is not supported yet (MVP-PLAN S-62)");
            return;
        }
        match &v.body {
            ViewBody::Select(sel) => {
                let root_name = format!("{}#{}", v.name, sel.entity.to_lowercase());
                if let Some(level) = self.level(sel, &root_name, &[]) {
                    self.shapes.views.push(level);
                }
            }
            ViewBody::Element(e) => self.error(
                e.span,
                "a view whose body is a bare element (an implicit `Unit` root) is not supported yet (MVP-PLAN S-53)",
            ),
        }
    }

    /// Lower one nesting level (a `select`), emitting its membership/order/attr
    /// `let`s and recursing into nested selects. `ancestors` are the enclosing
    /// levels' `(binder, entity)` pairs, outermost first; the last is the
    /// parent whose binder a membership `where` must reference.
    fn level(
        &mut self,
        sel: &SelectExpr,
        name: &str,
        ancestors: &[(String, String)],
    ) -> Option<ShapeLevel> {
        let e = &sel.entity;
        // The row binder in scope: the explicit `as l` alias, or the entity name.
        let binder = sel.binder.clone().unwrap_or_else(|| e.clone());
        let span = sel.span;
        let parent = ancestors.last().map(|(b, _)| b.as_str());
        if ancestors.iter().any(|(b, _)| b == &binder) {
            self.error(span, format!("binder `{binder}` shadows an enclosing level's binder"));
            return None;
        }

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
                format!("nested `select {e}` needs a membership `where` relating a field to the enclosing binder, e.g. `where .field = parent`"),
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

        // Order view: `E . <order expr>` (a field path or any co-keyed expression).
        let order_view = sel.order_by.as_ref().map(|o| {
            if o.desc {
                self.error(o.expr.span, "`order by … desc` is not supported yet (MVP-PLAN S-70)");
            }
            let vname = format!("{name}#order");
            self.emit_let(&vname, compose(ident(e, span), o.expr.clone(), span));
            vname
        });
        let order_field = sel.order_by.as_ref().and_then(|o| match &o.expr.kind {
            ExprKind::FieldPath(p) => Some(p.join(".")),
            _ => None,
        });

        // Child levels by binder, so `endOf(c)` / `dropPos(c, x)` in this
        // level's handlers can name the level they range over.
        let Content::Element(body) = &sel.body else {
            self.error(span, "a `select` whose body is a component call is not supported yet (MVP-PLAN S-61)");
            return None;
        };
        let mut child_levels = std::collections::HashMap::new();
        collect_child_levels(&body.children, name, &mut child_levels);

        // Walk the element into a template + bindings + events + child levels.
        let mut lw = LevelWalk {
            d: self,
            entity: e,
            binder: &binder,
            ancestors,
            level_name: name,
            child_levels,
            attrs: Vec::new(),
            events: Vec::new(),
            children: Vec::new(),
        };
        let template = lw.element(body, &[]);
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
    /// Enclosing levels' `(binder, entity)`, outermost first.
    ancestors: &'a [(String, String)],
    level_name: &'a str,
    /// Binder -> level name of every `select` nested directly in this level.
    child_levels: std::collections::HashMap<String, String>,
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
        let mut classes = Vec::new();
        for a in &el.attrs {
            match &a.value {
                // `class="a b"` is the static class list (the Tpl keeps it apart
                // from other attributes so codegen sets `className` once).
                AttrValue::Static(s) if a.name == "class" => {
                    classes.extend(s.split_whitespace().map(str::to_string));
                }
                AttrValue::Static(s) => static_attrs.push((a.name.clone(), s.clone())),
                AttrValue::Bind(expr) => {
                    let kind = if let Some(cls) = a.name.strip_prefix("class.") {
                        BindKind::Class(cls.to_string())
                    } else {
                        BindKind::Prop(a.name.clone())
                    };
                    let (view, encoding) = self.bind_view(expr);
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
                Content::Bind(expr) => {
                    // Bind this element's textContent; not a static child node.
                    let (view, encoding) = self.bind_view(expr);
                    self.attrs.push(AttrBinding {
                        view,
                        path: path.to_vec(),
                        kind: BindKind::Text,
                        encoding,
                    });
                }
                Content::Element(child) => {
                    let mut cp = path.to_vec();
                    cp.push(tpl_children.len());
                    tpl_children.push(self.element(child, &cp));
                }
                Content::If { span, .. } => {
                    self.d.error(*span, "`if (…) { … }` in a view is not supported yet (MVP-PLAN S-53)");
                }
                Content::Component { span, .. } => {
                    self.d.error(*span, "component calls are not supported yet (MVP-PLAN S-61)");
                }
                Content::ChildrenSlot(span) => {
                    self.d.error(*span, "`children` slots are not supported yet (MVP-PLAN S-61)");
                }
                Content::Select(sel) => {
                    let child_name = format!("{}#{}", self.level_name, sel.entity.to_lowercase());
                    // The child's membership `where .f = <binder>` references THIS
                    // level's binder (alias or entity name), the last ancestor.
                    let mut chain = self.ancestors.to_vec();
                    chain.push((self.binder.to_string(), self.entity.to_string()));
                    if let Some(level) = self.d.level(sel, &child_name, &chain) {
                        self.children.push(level);
                    }
                }
            }
        }
        Tpl::Elem {
            tag: el.tag.clone(),
            classes,
            modifiers: el.modifiers.clone(),
            attrs: static_attrs,
            children: tpl_children,
        }
    }

    /// The attribute view for a bind expression: a `.field` path keeps its
    /// typed encoding; any other co-keyed expression gets an auto-named view
    /// and (until S-60 types it) a `Text` encoding.
    fn bind_view(&mut self, expr: &Expr) -> (String, Encoding) {
        if let ExprKind::FieldPath(parts) = &expr.kind {
            return (self.attr_view(parts), self.field_encoding(parts));
        }
        self.d.attr_seq += 1;
        let name = format!("{}#bind{}", self.level_name, self.d.attr_seq);
        self.d.emit_let(
            &name,
            compose(ident(self.entity, Span::point(0)), expr.clone(), Span::point(0)),
        );
        (name, Encoding::Text)
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

    /// Lower a DOM handler into an [`EventBinding`]: extract params, then
    /// `do` named events with this level's key, any enclosing level's key,
    /// params or literals as arguments; `focus`/`clear` become UI actions.
    /// Returns `None` on error.
    fn handler(&mut self, h: &HandlerDecl, path: &[usize]) -> Option<EventBinding> {
        // What a `do` argument may name, and how the listener gets it.
        let mut scope: Vec<(String, ParamTy, ArgRef)> = Vec::new();
        let depth = self.ancestors.len();
        for (i, (b, e)) in self.ancestors.iter().enumerate() {
            scope.push((b.clone(), ParamTy::Id(e.clone()), ArgRef::Ancestor(depth - i)));
        }
        scope.push((self.binder.to_string(), ParamTy::Id(self.entity.to_string()), ArgRef::SelfKey));

        let mut params = Vec::new();
        for p in &h.params {
            let ty = match &p.ty {
                Some(t) => t.clone(),
                None => match infer_param_type(&p.extractor, p.span) {
                    Some(t) => t,
                    None => {
                        self.d.error(p.span, format!("parameter `{}` needs a type annotation", p.name));
                        return None;
                    }
                },
            };
            let pty = self.d.param_ty(&ty)?;
            let extractor = self.resolve_extractor(&p.extractor, p.span)?;
            params.push(ArgSpec { name: p.name.clone(), encoding: pty.encoding(), extractor });
            scope.push((p.name.clone(), pty, ArgRef::Param(p.name.clone())));
        }

        let mut dispatches = Vec::new();
        let mut actions = Vec::new();
        for stmt in &h.body {
            let span = stmt.span();
            match stmt {
                HStmt::Do { event, args, .. } => {
                    if !actions.is_empty() {
                        self.d.error(span, "`do` must come before `focus`/`clear` in a DOM handler");
                        return None;
                    }
                    let decl = self.d.event_params(event, args.len(), span)?;
                    let mut bound = Vec::new();
                    for (a, p) in args.iter().zip(&decl) {
                        let (val, got) = match &a.kind {
                            ExprKind::Ident(n) => match scope.iter().find(|(s, _, _)| s == n) {
                                Some((_, t, r)) => (r.clone(), t.clone()),
                                None => {
                                    self.d.error(a.span, format!("unknown name `{n}`: not a binder in scope or a handler param"));
                                    return None;
                                }
                            },
                            ExprKind::Str(x) => (ArgRef::Lit(Lit::Str(x.clone())), ParamTy::Scalar(Encoding::Text)),
                            ExprKind::Int(x) => (ArgRef::Lit(Lit::Int(*x)), ParamTy::Scalar(Encoding::Int)),
                            ExprKind::Decimal(x) => (ArgRef::Lit(Lit::Decimal(x.clone())), ParamTy::Scalar(Encoding::Money)),
                            ExprKind::Atom(x) => (ArgRef::Lit(Lit::Atom(x.clone())), ParamTy::Scalar(Encoding::Atom)),
                            _ => {
                                self.d.error(a.span, "`do` arguments in a DOM handler may be binders, params or literals");
                                return None;
                            }
                        };
                        if got != p.ty {
                            self.d.error(a.span, format!("`do {event}`: argument for `{}` has the wrong type", p.name));
                            return None;
                        }
                        bound.push((p.name.clone(), val));
                    }
                    dispatches.push(Dispatch { event: event.clone(), args: bound });
                }
                HStmt::Focus { target: FocusTarget::Level(b), .. } => match self.child_levels.get(b) {
                    Some(level) => actions.push(UiAction::FocusNew { level: level.clone() }),
                    None => {
                        self.d.error(span, format!("`focus({b})`: `{b}` is not the binder of a `select` nested in this level"));
                        return None;
                    }
                },
                HStmt::Focus { target: FocusTarget::Class(c), .. } => actions.push(UiAction::FocusClass(c.clone())),
                HStmt::Clear { .. } => actions.push(UiAction::Clear),
                HStmt::Set { .. } => {
                    self.d.error(span, "`set` is not supported yet (MVP-PLAN S-51)");
                    return None;
                }
                HStmt::New { .. } | HStmt::Assign { .. } | HStmt::Update { .. } | HStmt::Delete { .. } => {
                    self.d.error(span, "a DOM handler may not mutate directly: declare an `event` with an `on` body and `do` it (MVP-PLAN S-20)");
                    return None;
                }
            }
        }

        Some(EventBinding {
            path: path.to_vec(),
            dom_event: h.event.clone(),
            modifiers: h.modifiers.clone(),
            params,
            dispatches,
            actions,
        })
    }

    /// Resolve the surface extractor's level binders (`endOf(c)`) to level
    /// names, so codegen never re-derives naming.
    fn resolve_extractor(&mut self, x: &Extractor, span: Span) -> Option<Extractor> {
        let level = |this: &mut Self, binder: &str| -> Option<String> {
            match this.child_levels.get(binder) {
                Some(l) => Some(l.clone()),
                None => {
                    this.d.error(
                        span,
                        format!("`{binder}` is not the binder of a `select` nested in this level"),
                    );
                    None
                }
            }
        };
        Some(match x {
            Extractor::EndOf(b) => Extractor::EndOf(level(self, b)?),
            Extractor::DropPos { level: b, exclude } => Extractor::DropPos {
                level: level(self, b)?,
                exclude: exclude.clone(),
            },
            Extractor::Js { .. } => {
                self.d.error(span, "JS extractors (`import js`) are not supported yet (MVP-PLAN S-42)");
                return None;
            }
            other => other.clone(),
        })
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

/// The type a DOM handler param has when its extractor fixes it (`value`
/// is Text, `drag(E)` is `E`, …); `None` when an annotation is required.
fn infer_param_type(x: &Extractor, span: Span) -> Option<Type> {
    let named = |n: &str| Type { kind: TypeKind::Named(n.to_string()), span };
    Some(match x {
        Extractor::Value | Extractor::EndOf(_) | Extractor::DropPos { .. } => named("Text"),
        Extractor::Checked => named("Bool"),
        Extractor::Drag(e) => named(e),
        Extractor::Js { .. } => return None,
    })
}

/// Binder -> level name for every `select` nested directly under `contents`
/// (descending through plain elements, not through nested selects).
fn collect_child_levels(
    contents: &[Content],
    level_name: &str,
    out: &mut std::collections::HashMap<String, String>,
) {
    for c in contents {
        match c {
            Content::Select(sel) => {
                let binder = sel.binder.clone().unwrap_or_else(|| sel.entity.clone());
                out.insert(binder, format!("{level_name}#{}", sel.entity.to_lowercase()));
            }
            Content::Element(e) => collect_child_levels(&e.children, level_name, out),
            Content::If { children, .. } => collect_child_levels(children, level_name, out),
            Content::Component { children: Some(kids), .. } => {
                collect_child_levels(kids, level_name, out)
            }
            _ => {}
        }
    }
}

fn show_type(ty: &Type) -> String {
    match &ty.kind {
        TypeKind::Named(n) => n.clone(),
        TypeKind::AtomSingleton(a) => format!("@{a}"),
        TypeKind::Arrow(a, b) => format!("{} -> {}", show_type(a), show_type(b)),
        TypeKind::Product(a, b) => format!("{} * {}", show_type(a), show_type(b)),
        TypeKind::Coproduct(ts) => format!("{{{}}}", ts.iter().map(show_type).collect::<Vec<_>>().join(" | ")),
    }
}

fn encoding_of_type(ty: &Type) -> Encoding {
    match &ty.kind {
        TypeKind::Named(n) => match n.as_str() {
            "Int" => Encoding::Int,
            "Money" => Encoding::Money,
            "Text" => Encoding::Text,
            "Bool" => Encoding::Atom,
            _ => Encoding::Id, // an entity type
        },
        TypeKind::AtomSingleton(_) | TypeKind::Coproduct(_) => Encoding::Atom,
        _ => Encoding::Text,
    }
}

fn entity_of_type(ty: &Type) -> Option<String> {
    match &ty.kind {
        TypeKind::Named(n) if !matches!(n.as_str(), "Int" | "Money" | "Text" | "Date" | "Unit" | "Bool") => {
            Some(n.clone())
        }
        _ => None,
    }
}
