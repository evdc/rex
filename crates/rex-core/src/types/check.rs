//! The bidirectional type checker / field elaborator (§4, §3.2, §5).
//!
//! A single type-directed walk validates the program *and* produces the
//! elaborated [`TProgram`]: field paths resolved to `(sort, field)` hops,
//! identifiers resolved to their kind, and filter-position built-ins lowered to
//! `Filter` nodes. The elaborated program is returned only if there are no
//! errors.
//!
//! Entities are registered in two passes (so field types may forward-reference
//! any entity's sort); then `let`s are checked in order. Field paths resolve
//! their leading field in the *ambient domain* — seeded by a `let`'s declared
//! domain and threaded through composition (§4). Diagnostics accumulate; a
//! failed sub-expression yields `Err(Bail)`.

use super::env::{Binding, Env};
use super::ty::{RelTy, SortId, ValueTy};
use super::typed::*;
use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;

pub struct CheckResult {
    pub env: Env,
    pub diagnostics: Vec<Diagnostic>,
    /// The elaborated program — `Some` iff there were no errors.
    pub elaborated: Option<TProgram>,
    /// The UI IR desugared from `view`/`state` statements (§M5). Empty for
    /// programs with no views.
    pub shapes: super::shape_ir::ShapeProgram,
}

pub fn check(program: &Program) -> CheckResult {
    // Desugar `view`/`state` into ordinary entities + `let`s (plus the UI IR)
    // before type-checking; the checker only ever sees the core language.
    let desugared = super::view::desugar(program);
    let program = &desugared.program;
    let mut shapes = desugared.shapes;

    let mut cx = Checker {
        env: Env::new(),
        diagnostics: desugared.diagnostics,
        rec_names: Default::default(),
    };
    let stmts = cx.run(program);
    let prog = TProgram { stmts };
    // A bind over an arbitrary co-keyed expression (`span { cards }`) gets
    // its wire encoding from the elaborated attribute view's codomain — the
    // desugarer only knows the types of `.field` paths.
    for view in &mut shapes.views {
        type_binds(view, &cx.env);
    }
    // Groundedness (§9.1) runs only on a clean type-check, since the elaborated AST
    // is well-formed only then. A rejection here nulls `elaborated`, so every
    // downstream caller refuses to evaluate the program. Warnings (the §8
    // incrementality-cliff marks) never block elaboration.
    let has_errors =
        |ds: &[Diagnostic]| ds.iter().any(|d| d.severity == crate::diagnostic::Severity::Error);
    if !has_errors(&cx.diagnostics) {
        cx.diagnostics
            .extend(super::ground::check_groundedness(&prog, &cx.env));
        cx.diagnostics
            .extend(super::strat::check_stratification(&prog));
    }
    let elaborated = if has_errors(&cx.diagnostics) {
        None
    } else {
        Some(prog)
    };
    CheckResult {
        env: cx.env,
        diagnostics: cx.diagnostics,
        elaborated,
        shapes,
    }
}

struct Bail;
type TResult<T> = Result<T, Bail>;

fn type_binds(level: &mut super::shape_ir::ShapeLevel, env: &Env) {
    use super::shape_ir::Encoding;
    for a in &mut level.attrs {
        if let Some(Binding::Rel(rt)) = env.binding(&a.view) {
            a.encoding = match &rt.to {
                ValueTy::Int => Encoding::Int,
                ValueTy::Money => Encoding::Money,
                ValueTy::Text | ValueTy::Date | ValueTy::Unit => Encoding::Text,
                ValueTy::Id(_) => Encoding::Id,
                ValueTy::Atom(_) | ValueTy::Coproduct(_) => Encoding::Atom,
                ValueTy::Product(..) => Encoding::Text,
            };
        }
    }
    for child in &mut level.children {
        type_binds(child, env);
    }
}

struct Checker {
    env: Env,
    diagnostics: Vec<Diagnostic>,
    /// Names of the recursion group currently being checked: identifiers
    /// matching these elaborate to `RecVar`, not `View`. Empty outside groups.
    rec_names: std::collections::HashSet<String>,
}

impl Checker {
    fn error<T>(&mut self, span: Span, message: impl Into<String>) -> TResult<T> {
        self.diagnostics.push(Diagnostic::error(span, message.into()));
        Err(Bail)
    }

    fn run(&mut self, program: &Program) -> Vec<TStmt> {
        // Pass 1: mint a sort for every entity.
        for stmt in &program.stmts {
            if let Stmt::Entity(e) = stmt {
                if self.env.is_entity(&e.name) {
                    self.diagnostics.push(Diagnostic::error(
                        e.span,
                        format!("entity `{}` is already defined", e.name),
                    ));
                } else {
                    self.env.mint_sort(&e.name);
                }
            }
        }
        // Pass 2: resolve field types (may reference any entity's sort).
        for stmt in &program.stmts {
            if let Stmt::Entity(e) = stmt {
                let Some(sort) = self.env.entity_sort(&e.name) else {
                    continue;
                };
                for field in &e.fields {
                    if let Ok(ty) = self.resolve_value_type(&field.ty) {
                        self.env.add_field(sort, &field.name, ty);
                    }
                }
            }
        }
        // Pass 3: check & elaborate `let`s in order. Consecutive recursive
        // lets form one fixpoint group (§8); any other statement ends it.
        let mut stmts = Vec::new();
        let mut i = 0;
        while i < program.stmts.len() {
            match &program.stmts[i] {
                Stmt::Let(l) if l.recursive => {
                    let mut group = vec![l];
                    while let Some(Stmt::Let(next)) = program.stmts.get(i + group.len())
                        && next.recursive
                    {
                        group.push(next);
                    }
                    i += group.len();
                    if let Ok(ts) = self.check_rec_group(&group) {
                        stmts.push(ts);
                    }
                }
                Stmt::Let(l) => {
                    i += 1;
                    if let Ok(ts) = self.check_let(l) {
                        stmts.push(ts);
                    }
                }
                Stmt::Entity(_) => i += 1,
                // Views, state, and rels are desugared to entities + lets in a
                // pre-pass (see [`super::view`]); by the time the checker runs
                // they appear as ordinary statements, so any surviving
                // `View`/`State`/`Rel` here is a no-op.
                Stmt::View(_)
                | Stmt::State(_)
                | Stmt::Rel(_)
                | Stmt::Event(_)
                | Stmt::On(_)
                | Stmt::Type(_)
                | Stmt::Import(_) => i += 1,
            }
        }
        stmts
    }

    // --- type-annotation resolution --------------------------------------

    fn resolve_value_type(&mut self, ty: &Type) -> TResult<ValueTy> {
        match &ty.kind {
            TypeKind::Named(name) => match name.as_str() {
                "Unit" => Ok(ValueTy::Unit),
                "Int" => Ok(ValueTy::Int),
                "Money" => Ok(ValueTy::Money),
                "Text" => Ok(ValueTy::Text),
                "Date" => Ok(ValueTy::Date),
                _ => match self.env.resolve_sort(name) {
                    Some(sort) => Ok(ValueTy::Id(sort)),
                    None => self.error(ty.span, format!("unknown type `{name}`")),
                },
            },
            TypeKind::AtomSingleton(a) => Ok(ValueTy::Atom(a.clone())),
            TypeKind::Product(a, b) => {
                let a = self.resolve_value_type(a)?;
                let b = self.resolve_value_type(b)?;
                Ok(ValueTy::Product(Box::new(a), Box::new(b)))
            }
            TypeKind::Coproduct(elems) => {
                let mut out = Vec::new();
                for e in elems {
                    out.push(self.resolve_value_type(e)?);
                }
                Ok(ValueTy::Coproduct(out))
            }
            TypeKind::Arrow(..) => {
                self.error(ty.span, "expected a value type here, found a relation type `->`")
            }
        }
    }

    fn resolve_rel_type(&mut self, ty: &Type) -> TResult<RelTy> {
        match &ty.kind {
            TypeKind::Arrow(a, b) => {
                let from = self.resolve_value_type(a)?;
                let to = self.resolve_value_type(b)?;
                Ok(RelTy::new(from, to))
            }
            _ => Ok(RelTy::coreflexive(self.resolve_value_type(ty)?)),
        }
    }

    // --- statements -------------------------------------------------------

    fn check_let(&mut self, l: &LetDecl) -> TResult<TStmt> {
        // `let x = new E { .. }` binds a value (an entity id, §4).
        if let ExprKind::New { entity, fields } = &l.body.kind {
            let (sort, tfields) = self.check_new(entity, fields, l.body.span)?;
            if let Some(name) = &l.name {
                self.env.bind(name, Binding::Value(ValueTy::Id(sort)));
            }
            return Ok(TStmt::New {
                name: l.name.clone(),
                sort,
                fields: tfields,
            });
        }

        let declared = match &l.ty {
            Some(ty) => Some(self.resolve_rel_type(ty)?),
            None => None,
        };
        let dom = declared.as_ref().map(|d| d.from.clone());
        let body = self.check_rel(&l.body, dom)?;

        if let Some(declared) = &declared
            && !self.rel_matches(declared, &body.ty)
        {
            return self.error(
                l.body.span,
                format!(
                    "type mismatch: `{}` is declared `{}` but its body is `{}`",
                    l.name.as_deref().unwrap_or("_"),
                    self.env.show_rel(declared),
                    self.env.show_rel(&body.ty),
                ),
            );
        }

        if let Some(name) = &l.name {
            let ty = declared.unwrap_or_else(|| body.ty.clone());
            self.env.bind(name, Binding::Rel(ty));
        }
        Ok(TStmt::Let {
            name: l.name.clone(),
            body,
        })
    }

    /// Check one recursion group. All member names are bound (from their
    /// mandatory annotations) *before* any body is checked, so each body may
    /// reference every member; those references elaborate to `RecVar`.
    fn check_rec_group(&mut self, group: &[&LetDecl]) -> TResult<TStmt> {
        let mut members: Vec<(String, RelTy, &LetDecl)> = Vec::new();
        for l in group {
            if matches!(l.body.kind, ExprKind::New { .. }) {
                let _: TResult<()> =
                    self.error(l.span, "`new` cannot be `recursive` — it creates data, not a view");
                continue;
            }
            let Some(name) = l.name.clone() else {
                let _: TResult<()> =
                    self.error(l.span, "a recursive `let` needs a name for the self-reference");
                continue;
            };
            let Some(ty) = &l.ty else {
                let _: TResult<()> = self.error(
                    l.span,
                    format!("recursive view `{name}` needs a type annotation to seed the self-reference"),
                );
                continue;
            };
            let Ok(declared) = self.resolve_rel_type(ty) else {
                continue;
            };
            // The group's fixpoint state is keyed by name in both backends, so
            // duplicates would silently collapse — reject them outright.
            if self.rec_names.contains(&name) {
                let _: TResult<()> = self.error(
                    l.span,
                    format!("duplicate recursive binding `{name}` in this group"),
                );
                continue;
            }
            self.env.bind(&name, Binding::Rel(declared.clone()));
            self.rec_names.insert(name.clone());
            members.push((name, declared, l));
        }

        let complete = members.len() == group.len();
        let mut bindings = Vec::new();
        for (name, declared, l) in &members {
            let Ok(body) = self.check_rel(&l.body, Some(declared.from.clone())) else {
                continue;
            };
            if !self.rel_matches(declared, &body.ty) {
                let msg = format!(
                    "type mismatch: `{name}` is declared `{}` but its body is `{}`",
                    self.env.show_rel(declared),
                    self.env.show_rel(&body.ty),
                );
                let _: TResult<()> = self.error(l.body.span, msg);
                continue;
            }
            bindings.push((name.clone(), body));
        }
        for (name, _, _) in &members {
            self.rec_names.remove(name);
        }

        if complete && bindings.len() == members.len() {
            Ok(TStmt::LetRec { bindings })
        } else {
            Err(Bail)
        }
    }

    fn check_new(
        &mut self,
        entity: &str,
        fields: &[FieldInit],
        span: Span,
    ) -> TResult<(SortId, Vec<(String, TValue)>)> {
        let Some(sort) = self.env.entity_sort(entity) else {
            return self.error(span, format!("`new` on unknown entity `{entity}`"));
        };
        let mut tfields = Vec::new();
        for init in fields {
            let Some(expected) = self.env.field_ty(sort, &init.name).cloned() else {
                self.diagnostics.push(Diagnostic::error(
                    init.span,
                    format!("entity `{entity}` has no field `{}`", init.name),
                ));
                continue;
            };
            if let Ok((got, tvalue)) = self.check_value(&init.value) {
                if self.value_assignable(&expected, &got) {
                    tfields.push((init.name.clone(), tvalue));
                } else {
                    self.diagnostics.push(Diagnostic::error(
                        init.value.span,
                        format!(
                            "field `{}` expects `{}` but got `{}`",
                            init.name,
                            self.env.show(&expected),
                            self.env.show(&got),
                        ),
                    ));
                }
            }
        }
        Ok((sort, tfields))
    }

    /// Check an expression in *value* position (a `new` field initializer).
    fn check_value(&mut self, expr: &Expr) -> TResult<(ValueTy, TValue)> {
        match &expr.kind {
            ExprKind::Int(n) => Ok((ValueTy::Int, TValue::Lit(Lit::Int(*n)))),
            ExprKind::Decimal(s) => Ok((ValueTy::Money, TValue::Lit(Lit::Decimal(s.clone())))),
            ExprKind::Str(s) => Ok((ValueTy::Text, TValue::Lit(Lit::Str(s.clone())))),
            ExprKind::Date { year, month, day } => Ok((
                ValueTy::Date,
                TValue::Lit(Lit::Date {
                    year: *year,
                    month: *month,
                    day: *day,
                }),
            )),
            ExprKind::Atom(a) => Ok((ValueTy::Atom(a.clone()), TValue::Lit(Lit::Atom(a.clone())))),
            ExprKind::Ident(name) => match self.env.binding(name) {
                Some(Binding::Value(vt)) => Ok((vt.clone(), TValue::Ref(name.clone()))),
                Some(Binding::Rel(_)) => {
                    self.error(expr.span, format!("`{name}` is a view, not a value"))
                }
                None => self.error(expr.span, format!("unknown name `{name}`")),
            },
            _ => self.error(expr.span, "expected a value here"),
        }
    }

    // --- relational expressions ------------------------------------------

    /// Check and elaborate `expr`. `dom` is the ambient left-column type (the
    /// key things are co-keyed on), seeded by the enclosing `let`'s domain and
    /// threaded through composition.
    fn check_rel(&mut self, expr: &Expr, dom: Option<ValueTy>) -> TResult<TExpr> {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Id => {
                let Some(d) = dom else {
                    return self.error(span, "`id` needs a known domain (add a type annotation)");
                };
                let sort = self.as_sort(span, &d, "`id`")?;
                Ok(TExpr::new(TExprKind::Identity(sort), RelTy::coreflexive(d), span))
            }

            // `unit : X -> Unit` (S-50): the constant relation onto the one
            // point of `Unit`, grounded by the ambient domain exactly like the
            // literal constants below. It is a built-in name rather than a
            // keyword, so it only wins when nothing else has bound it.
            ExprKind::Ident(name) if name == UNIT && self.env.binding(name).is_none() => {
                let Some(d) = dom else {
                    return self.error(span, "`unit` needs a known domain (add a type annotation)");
                };
                let sort = self.as_sort(span, &d, "`unit`")?;
                Ok(TExpr::new(
                    TExprKind::Const { lit: Lit::Unit, dom: sort },
                    RelTy::new(d, ValueTy::Unit),
                    span,
                ))
            }

            ExprKind::Ident(name) => self.check_ident(name, span),

            ExprKind::FieldPath(parts) => self.check_field_path(parts, span, dom),

            // An atom literal grounds to a constant relation over an entity
            // domain (like the other literals below), so `:f = @atom` at an
            // entity-keyed ambient domain co-keys with `:f`. Outside an
            // entity domain (e.g. a top-level `{@a}`-typed `let`, or an `in`
            // set's element type) it stays its own standalone coreflexive.
            ExprKind::Atom(a) => match &dom {
                Some(ValueTy::Id(sort)) => Ok(TExpr::new(
                    TExprKind::Const { lit: Lit::Atom(a.clone()), dom: *sort },
                    RelTy::new(ValueTy::Id(*sort), ValueTy::Atom(a.clone())),
                    span,
                )),
                _ => Ok(TExpr::new(
                    TExprKind::Atom(a.clone()),
                    RelTy::coreflexive(ValueTy::Atom(a.clone())),
                    span,
                )),
            },

            ExprKind::Int(_) => self.constant(expr, dom, ValueTy::Int),
            ExprKind::Decimal(_) => self.constant(expr, dom, ValueTy::Money),
            ExprKind::Str(_) => self.constant(expr, dom, ValueTy::Text),
            ExprKind::Date { .. } => self.constant(expr, dom, ValueTy::Date),

            ExprKind::Compose(a, b) => {
                let ta = self.check_rel(a, dom)?;
                // `x.f`: an identifier after `.` resolves as a field of the
                // left side's codomain entity before it resolves as a name —
                // a field is a relation you join with (SYNTAX v1 rule 1).
                let field_hop = match (&b.kind, &ta.ty.to) {
                    (ExprKind::Ident(n), ValueTy::Id(sort)) if self.env.field_ty(*sort, n).is_some() => {
                        Some(vec![n.clone()])
                    }
                    _ => None,
                };
                let tb = match field_hop {
                    Some(parts) => self.check_field_path(&parts, b.span, Some(ta.ty.to.clone()))?,
                    None => self.check_rel(b, Some(ta.ty.to.clone()))?,
                };
                self.expect_join(span, &ta.ty.to, &tb.ty.from, "composition `.`")?;
                let ty = RelTy::new(ta.ty.from.clone(), tb.ty.to.clone());
                Ok(TExpr::new(TExprKind::Compose(Box::new(ta), Box::new(tb)), ty, span))
            }

            // R[S] and `where` are the same semijoin (§3.2).
            ExprKind::Restrict(a, b) | ExprKind::Where(a, b) => self.check_semijoin(a, b, dom, span),

            ExprKind::Fork(a, b) => {
                let ta = self.check_rel(a, dom.clone())?;
                let tb = self.check_rel(b, dom)?;
                self.expect_cokeyed(span, &ta.ty.from, &tb.ty.from, "fork `,`")?;
                let ty = RelTy::new(
                    ta.ty.from.clone(),
                    ValueTy::Product(Box::new(ta.ty.to.clone()), Box::new(tb.ty.to.clone())),
                );
                Ok(TExpr::new(TExprKind::Fork(Box::new(ta), Box::new(tb)), ty, span))
            }

            ExprKind::Union(a, b) => self.check_additive(a, b, dom, span, true),
            ExprKind::Intersect(a, b) => self.check_additive(a, b, dom, span, false),

            ExprKind::Inverse(a) => {
                let ta = self.check_rel(a, None)?;
                let ty = RelTy::new(ta.ty.to.clone(), ta.ty.from.clone());
                Ok(TExpr::new(TExprKind::Inverse(Box::new(ta)), ty, span))
            }
            ExprKind::Distinct(a) => {
                let ta = self.check_rel(a, dom)?;
                let ty = ta.ty.clone();
                Ok(TExpr::new(TExprKind::Distinct(Box::new(ta)), ty, span))
            }
            ExprKind::Proj(side, a) => {
                let ta = self.check_rel(a, dom)?;
                let ValueTy::Product(x, y) = &ta.ty.to else {
                    return self.error(
                        span,
                        format!(
                            "`{}` needs a pair-valued relation (built by fork `,`), \
                             but this one produces `{}`",
                            match side {
                                ProjSide::Fst => "fst",
                                ProjSide::Snd => "snd",
                            },
                            self.env.show(&ta.ty.to)
                        ),
                    );
                };
                let to = match side {
                    ProjSide::Fst => (**x).clone(),
                    ProjSide::Snd => (**y).clone(),
                };
                let ty = RelTy::new(ta.ty.from.clone(), to);
                Ok(TExpr::new(TExprKind::Proj(*side, Box::new(ta)), ty, span))
            }

            ExprKind::By(x, y) => {
                // X by Y == ~Y . X : regroup X (keyed by its own domain) under Y.
                let tx = self.check_rel(x, None)?;
                let ty_e = self.check_rel(y, Some(tx.ty.from.clone()))?;
                self.expect_cokeyed(span, &tx.ty.from, &ty_e.ty.from, "`by`")?;
                let ty = RelTy::new(ty_e.ty.to.clone(), tx.ty.to.clone());
                Ok(TExpr::new(TExprKind::By(Box::new(tx), Box::new(ty_e)), ty, span))
            }

            ExprKind::Except(a, b) | ExprKind::Antijoin(a, b) => {
                let ta = self.check_rel(a, dom)?;
                let tb = self.check_rel(b, Some(ta.ty.to.clone()))?;
                self.expect_join(span, &ta.ty.to, &tb.ty.from, "`except`/`antijoin`")?;
                let ty = ta.ty.clone();
                Ok(TExpr::new(TExprKind::Antijoin(Box::new(ta), Box::new(tb)), ty, span))
            }

            ExprKind::Mul(a, b) => self.check_arith(a, b, dom, span, ArithOp::Mul),
            ExprKind::Add(a, b) => self.check_arith(a, b, dom, span, ArithOp::Add),
            ExprKind::Sub(a, b) => self.check_arith(a, b, dom, span, ArithOp::Sub),
            ExprKind::Div(a, b) => self.check_arith(a, b, dom, span, ArithOp::Div),
            ExprKind::Mod(a, b) => self.check_arith(a, b, dom, span, ArithOp::Mod),
            ExprKind::Concat(a, b) => self.check_arith(a, b, dom, span, ArithOp::Concat),
            ExprKind::Not(_) => self.error(span, "`not` is not supported yet (MVP-PLAN S-40)"),
            ExprKind::Match { .. } => self.error(span, "`match` is not supported yet (MVP-PLAN S-52)"),
            ExprKind::If { .. } => self.error(span, "`if … then … else` is not supported yet (MVP-PLAN S-52)"),

            ExprKind::Compare { op, lhs, rhs } => self.check_compare(*op, lhs, rhs, dom, span),
            ExprKind::In { lhs, rhs } => self.check_in(lhs, rhs, dom, span),

            ExprKind::Call { func, args } => self.check_call(func, args, dom, span),

            ExprKind::New { .. } => {
                self.error(span, "`new` may only appear as the body of a `let`")
            }
        }
    }

    fn check_ident(&mut self, name: &str, span: Span) -> TResult<TExpr> {
        // A member of the recursion group being checked — the knot (§8).
        if self.rec_names.contains(name)
            && let Some(Binding::Rel(rt)) = self.env.binding(name)
        {
            return Ok(TExpr::new(TExprKind::RecVar(name.to_string()), rt.clone(), span));
        }
        // Entity used as its identity relation (§3.3).
        if let Some(sort) = self.env.entity_sort(name) {
            return Ok(TExpr::new(
                TExprKind::Identity(sort),
                RelTy::coreflexive(ValueTy::Id(sort)),
                span,
            ));
        }
        match self.env.binding(name) {
            Some(Binding::Rel(rt)) => {
                Ok(TExpr::new(TExprKind::View(name.to_string()), rt.clone(), span))
            }
            Some(Binding::Value(vt)) => Ok(TExpr::new(
                TExprKind::ValueRef(name.to_string()),
                RelTy::coreflexive(vt.clone()),
                span,
            )),
            None => self.error(span, format!("unknown name `{name}`")),
        }
    }

    fn check_field_path(
        &mut self,
        parts: &[String],
        span: Span,
        dom: Option<ValueTy>,
    ) -> TResult<TExpr> {
        let Some(dom) = dom else {
            return self.error(
                span,
                "cannot resolve a `.field` without a known domain (add a type annotation)",
            );
        };
        let mut sort = self.as_sort(span, &dom, &format!("`.{}`", parts.join(".")))?;
        let mut running = dom.clone();
        let mut hops = Vec::new();
        for (i, field) in parts.iter().enumerate() {
            if i > 0 {
                match running {
                    ValueTy::Id(s) => sort = s,
                    other => {
                        return self.error(
                            span,
                            format!(
                                "cannot follow `.{field}`: `{}` is not an entity",
                                self.env.show(&other)
                            ),
                        );
                    }
                }
            }
            match self.env.field_ty(sort, field) {
                Some(ty) => {
                    running = ty.clone();
                    hops.push(FieldHop {
                        sort,
                        field: field.clone(),
                    });
                }
                None => {
                    return self.error(
                        span,
                        format!(
                            "unknown field `{field}` on entity `{}`",
                            self.entity_name_of(sort)
                        ),
                    );
                }
            }
        }
        Ok(TExpr::new(
            TExprKind::Field(hops),
            RelTy::new(dom, running),
            span,
        ))
    }

    fn check_semijoin(
        &mut self,
        a: &Expr,
        b: &Expr,
        dom: Option<ValueTy>,
        span: Span,
    ) -> TResult<TExpr> {
        let ta = self.check_rel(a, dom)?;
        let tb = self.check_rel(b, Some(ta.ty.to.clone()))?;
        self.expect_join(span, &ta.ty.to, &tb.ty.from, "restriction `[]`/`where`")?;
        let ty = ta.ty.clone();
        // Ground a coreflexive built-in on the value column into a Filter (§9).
        if let TExprKind::Coreflexive(pred) = tb.kind {
            Ok(TExpr::new(TExprKind::Filter(Box::new(ta), pred), ty, span))
        } else {
            Ok(TExpr::new(TExprKind::Semijoin(Box::new(ta), Box::new(tb)), ty, span))
        }
    }

    fn check_additive(
        &mut self,
        a: &Expr,
        b: &Expr,
        dom: Option<ValueTy>,
        span: Span,
        is_union: bool,
    ) -> TResult<TExpr> {
        let ta = self.check_rel(a, dom.clone())?;
        let tb = self.check_rel(b, dom)?;
        let from = self.join(span, &ta.ty.from, &tb.ty.from)?;
        let to = self.join(span, &ta.ty.to, &tb.ty.to)?;
        let ty = RelTy::new(from, to);
        let a = Box::new(ta);
        let b = Box::new(tb);
        let kind = if is_union {
            TExprKind::Union(a, b)
        } else {
            TExprKind::Intersect(a, b)
        };
        Ok(TExpr::new(kind, ty, span))
    }

    fn check_arith(
        &mut self,
        a: &Expr,
        b: &Expr,
        dom: Option<ValueTy>,
        span: Span,
        op: ArithOp,
    ) -> TResult<TExpr> {
        let ta = self.check_rel(a, dom.clone())?;
        let tb = self.check_rel(b, dom)?;
        self.expect_cokeyed(span, &ta.ty.from, &tb.ty.from, op.name())?;
        let to = match op {
            ArithOp::Mul | ArithOp::Add | ArithOp::Sub => {
                if !ta.ty.to.is_numeric() || !tb.ty.to.is_numeric() {
                    return self.error(
                        span,
                        format!(
                            "{} needs numeric operands, got `{}` and `{}`",
                            op.name(),
                            self.env.show(&ta.ty.to),
                            self.env.show(&tb.ty.to)
                        ),
                    );
                }
                if ta.ty.to == ValueTy::Money || tb.ty.to == ValueTy::Money {
                    ValueTy::Money
                } else {
                    ValueTy::Int
                }
            }
            ArithOp::Div | ArithOp::Mod => {
                if ta.ty.to != ValueTy::Int || tb.ty.to != ValueTy::Int {
                    return self.error(
                        span,
                        format!(
                            "{} needs Int operands, got `{}` and `{}`",
                            op.name(),
                            self.env.show(&ta.ty.to),
                            self.env.show(&tb.ty.to)
                        ),
                    );
                }
                ValueTy::Int
            }
            ArithOp::Concat => {
                if ta.ty.to != ValueTy::Text || tb.ty.to != ValueTy::Text {
                    return self.error(
                        span,
                        format!(
                            "`++` needs Text operands, got `{}` and `{}`",
                            self.env.show(&ta.ty.to),
                            self.env.show(&tb.ty.to)
                        ),
                    );
                }
                ValueTy::Text
            }
        };
        let ty = RelTy::new(ta.ty.from.clone(), to);
        let a = Box::new(ta);
        let b = Box::new(tb);
        let kind = match op {
            ArithOp::Mul => TExprKind::Mul(a, b),
            ArithOp::Concat => TExprKind::Concat(a, b),
            ArithOp::Add => TExprKind::Arith(ArithKind::Add, a, b),
            ArithOp::Sub => TExprKind::Arith(ArithKind::Sub, a, b),
            ArithOp::Div => TExprKind::Arith(ArithKind::Div, a, b),
            ArithOp::Mod => TExprKind::Arith(ArithKind::Mod, a, b),
        };
        Ok(TExpr::new(kind, ty, span))
    }

    fn check_compare(
        &mut self,
        op: CmpOp,
        lhs: &Option<Box<Expr>>,
        rhs: &Expr,
        dom: Option<ValueTy>,
        span: Span,
    ) -> TResult<TExpr> {
        match lhs {
            // Prefix filter `> 30`: a groundable coreflexive on the ambient value.
            None => {
                let Some(d) = dom else {
                    return self.error(span, "filter comparison needs a known domain");
                };
                let Some(lit) = lit_of(rhs) else {
                    return self.error(rhs.span, "a filter comparison's right side must be a literal");
                };
                self.expect_comparable(span, &d, &lit_ty(&lit))?;
                Ok(TExpr::new(
                    TExprKind::Coreflexive(Pred::Cmp(op, lit)),
                    RelTy::coreflexive(d),
                    span,
                ))
            }
            // Binary `a OP b`: operands must be co-keyed (§4).
            Some(lhs) => {
                let ta = self.check_rel(lhs, dom.clone())?;
                let tb = self.check_rel(rhs, dom)?;
                self.expect_cokeyed(span, &ta.ty.from, &tb.ty.from, "comparison")?;
                self.expect_comparable(span, &ta.ty.to, &tb.ty.to)?;
                self.warn_composite_cliff(span, &ta.ty.to, "comparison");
                let ty = RelTy::coreflexive(ta.ty.from.clone());
                Ok(TExpr::new(
                    TExprKind::BinCompare(op, Box::new(ta), Box::new(tb)),
                    ty,
                    span,
                ))
            }
        }
    }

    fn check_in(
        &mut self,
        lhs: &Option<Box<Expr>>,
        rhs: &Expr,
        dom: Option<ValueTy>,
        span: Span,
    ) -> TResult<TExpr> {
        let Some(lits) = collect_lits(rhs) else {
            return self.error(rhs.span, "an `in` set must be a set of literals");
        };
        // The set's element type, from the literals themselves — not from
        // checking `rhs` as a relation, which would need a domain to ground
        // scalar literals (`constant`) that an `in` set doesn't have.
        let mut elem_ty = lit_ty(&lits[0]);
        for lit in &lits[1..] {
            elem_ty = self.join(span, &elem_ty, &lit_ty(lit))?;
        }
        match lhs {
            Some(lhs) => {
                let ta = self.check_rel(lhs, dom)?;
                self.expect_subset(span, &elem_ty, &ta.ty.to)?;
                let ty = RelTy::coreflexive(ta.ty.from.clone());
                Ok(TExpr::new(TExprKind::InRel(Box::new(ta), lits), ty, span))
            }
            None => {
                let Some(d) = dom else {
                    return self.error(span, "`in` filter needs a known domain");
                };
                self.expect_subset(span, &elem_ty, &d)?;
                Ok(TExpr::new(
                    TExprKind::Coreflexive(Pred::InSet(lits)),
                    RelTy::coreflexive(d),
                    span,
                ))
            }
        }
    }

    /// The incrementality cliff (nesting-draft §8): consuming a composite
    /// (pair) value *as a value* — aggregating it, comparing it — is legal but
    /// drops that subtree from element-surgical maintenance to
    /// recompute-per-group. Keep composites navigable (project with
    /// `fst`/`snd`, compose onward) to stay on the surgical tier.
    fn warn_composite_cliff(&mut self, span: Span, to: &ValueTy, what: &str) {
        if matches!(to, ValueTy::Product(..)) {
            self.diagnostics.push(Diagnostic::warning(
                span,
                format!(
                    "`{what}` consumes a composite (pair) value opaquely — this is an \
                     incrementality cliff: maintenance degrades from per-element to \
                     recompute-per-group; project the component you need with `fst`/`snd` \
                     to stay surgical"
                ),
            ));
        }
    }

    fn check_call(
        &mut self,
        func: &str,
        args: &[Expr],
        dom: Option<ValueTy>,
        span: Span,
    ) -> TResult<TExpr> {
        let agg = match func {
            "sum" => AggKind::Sum,
            "count" => AggKind::Count,
            "avg" => AggKind::Avg,
            "min" => AggKind::Min,
            "max" => AggKind::Max,
            _ => return self.error(span, format!("unknown function `{func}`")),
        };
        if args.len() != 1 {
            return self.error(
                span,
                format!("`{func}` takes exactly one argument, got {}", args.len()),
            );
        }
        let image = self.check_rel(&args[0], dom)?;
        self.warn_composite_cliff(args[0].span, &image.ty.to, func);
        let to = match agg {
            AggKind::Count => ValueTy::Int,
            AggKind::Avg => ValueTy::Money,
            AggKind::Sum | AggKind::Min | AggKind::Max => {
                if !image.ty.to.is_numeric() {
                    return self.error(
                        args[0].span,
                        format!(
                            "`{func}` needs a numeric image, got `{}`",
                            self.env.show(&image.ty.to)
                        ),
                    );
                }
                image.ty.to.clone()
            }
        };
        let ty = RelTy::new(image.ty.from.clone(), to);
        // `X by unit` regroups everything under the single `Unit` point, so
        // the group key is there whether or not `X` has rows: the aggregate
        // is over a total domain and must emit its identity when empty.
        let total = match &image.kind {
            TExprKind::By(_, g) if matches!(&g.kind, TExprKind::Const { lit: Lit::Unit, .. }) => {
                Total::Unit
            }
            _ => Total::No,
        };
        Ok(TExpr::new(TExprKind::Agg(agg, Box::new(image), total), ty, span))
    }

    // --- type relations ---------------------------------------------------

    fn constant(&mut self, expr: &Expr, dom: Option<ValueTy>, to: ValueTy) -> TResult<TExpr> {
        let Some(d) = dom else {
            return self.error(expr.span, "a constant needs a known domain (add a type annotation)");
        };
        let sort = self.as_sort(expr.span, &d, "a constant")?;
        let lit = lit_of(expr).expect("literal expression");
        Ok(TExpr::new(
            TExprKind::Const { lit, dom: sort },
            RelTy::new(d, to),
            expr.span,
        ))
    }

    fn as_sort(&mut self, span: Span, ty: &ValueTy, what: &str) -> TResult<SortId> {
        match ty {
            ValueTy::Id(s) => Ok(*s),
            other => self.error(
                span,
                format!("{what} needs an entity domain, but the domain is `{}`", self.env.show(other)),
            ),
        }
    }

    fn rel_matches(&self, a: &RelTy, b: &RelTy) -> bool {
        self.assignable(&a.from, &b.from) && self.assignable(&a.to, &b.to)
    }

    fn value_assignable(&self, expected: &ValueTy, got: &ValueTy) -> bool {
        self.assignable(expected, got)
    }

    /// Structural assignability with the small amount of subtyping v1 needs.
    fn assignable(&self, expected: &ValueTy, got: &ValueTy) -> bool {
        if expected == got {
            return true;
        }
        match (expected, got) {
            (ValueTy::Money, ValueTy::Int) => true,
            (ValueTy::Coproduct(_), _) => match (expected.atoms(), got.atoms()) {
                (Some(target), Some(atoms)) => atoms.iter().all(|a| target.contains(a)),
                _ => false,
            },
            _ => false,
        }
    }

    fn expect_join(&mut self, span: Span, left: &ValueTy, right: &ValueTy, what: &str) -> TResult<()> {
        if left == right {
            Ok(())
        } else {
            self.error(
                span,
                format!(
                    "{what} join column mismatch: left column is `{}` but right is `{}`",
                    self.env.show(left),
                    self.env.show(right)
                ),
            )
        }
    }

    fn expect_cokeyed(&mut self, span: Span, a: &ValueTy, b: &ValueTy, what: &str) -> TResult<()> {
        if a == b {
            Ok(())
        } else {
            self.error(
                span,
                format!(
                    "operands not co-keyed in {what}: `{}` vs `{}`",
                    self.env.show(a),
                    self.env.show(b)
                ),
            )
        }
    }

    fn expect_comparable(&mut self, span: Span, a: &ValueTy, b: &ValueTy) -> TResult<()> {
        let ok = a == b
            || (a.is_numeric() && b.is_numeric())
            || self.assignable(a, b)
            || self.assignable(b, a);
        if ok {
            Ok(())
        } else {
            self.error(
                span,
                format!("cannot compare `{}` with `{}`", self.env.show(a), self.env.show(b)),
            )
        }
    }

    /// Every element of `subset` must be a valid member of `universe` (used
    /// for `in`). Atom (co)products check literal membership; scalar types
    /// (`Int`/`Money`/`Text`/`Date`) only need to agree, since any literal of
    /// that type is a valid element (there's no fixed enumeration to check
    /// membership against).
    fn expect_subset(&mut self, span: Span, subset: &ValueTy, universe: &ValueTy) -> TResult<()> {
        let ok = match (subset.atoms(), universe.atoms()) {
            (Some(sub), Some(univ)) => sub.iter().all(|a| univ.contains(a)),
            (None, None) => subset == universe,
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            self.error(
                span,
                format!(
                    "`in` set `{}` is not within the value type `{}`",
                    self.env.show(subset),
                    self.env.show(universe)
                ),
            )
        }
    }

    fn join(&mut self, span: Span, a: &ValueTy, b: &ValueTy) -> TResult<ValueTy> {
        if a == b {
            return Ok(a.clone());
        }
        if a.is_numeric() && b.is_numeric() {
            return Ok(ValueTy::Money);
        }
        if let (Some(av), Some(bv)) = (a.atoms(), b.atoms()) {
            let mut atoms: Vec<String> = av.iter().map(|s| s.to_string()).collect();
            for s in bv {
                if !atoms.iter().any(|x| x == s) {
                    atoms.push(s.to_string());
                }
            }
            return Ok(ValueTy::Coproduct(atoms.into_iter().map(ValueTy::Atom).collect()));
        }
        self.error(
            span,
            format!("cannot unify `{}` with `{}`", self.env.show(a), self.env.show(b)),
        )
    }

    fn entity_name_of(&self, sort: SortId) -> String {
        let name = self.env.sort_name(sort);
        name.strip_suffix("ID").unwrap_or(name).to_string()
    }
}

#[derive(Clone, Copy)]
enum ArithOp {
    Mul,
    Add,
    Sub,
    Div,
    Mod,
    Concat,
}

impl ArithOp {
    fn name(self) -> &'static str {
        match self {
            ArithOp::Mul => "`*`",
            ArithOp::Add => "`+`",
            ArithOp::Sub => "`-`",
            ArithOp::Div => "`/`",
            ArithOp::Mod => "`%`",
            ArithOp::Concat => "`++`",
        }
    }
}

// --- literal helpers ------------------------------------------------------

fn lit_of(expr: &Expr) -> Option<Lit> {
    Some(match &expr.kind {
        ExprKind::Int(n) => Lit::Int(*n),
        ExprKind::Decimal(s) => Lit::Decimal(s.clone()),
        ExprKind::Str(s) => Lit::Str(s.clone()),
        ExprKind::Date { year, month, day } => Lit::Date {
            year: *year,
            month: *month,
            day: *day,
        },
        ExprKind::Atom(a) => Lit::Atom(a.clone()),
        _ => return None,
    })
}

fn lit_ty(lit: &Lit) -> ValueTy {
    match lit {
        Lit::Unit => ValueTy::Unit,
        Lit::Int(_) => ValueTy::Int,
        Lit::Decimal(_) => ValueTy::Money,
        Lit::Str(_) => ValueTy::Text,
        Lit::Date { .. } => ValueTy::Date,
        Lit::Atom(a) => ValueTy::Atom(a.clone()),
    }
}

/// Collect the literals of a set expression like `@west + @east`.
fn collect_lits(expr: &Expr) -> Option<Vec<Lit>> {
    match &expr.kind {
        ExprKind::Union(a, b) => {
            let mut out = collect_lits(a)?;
            out.extend(collect_lits(b)?);
            Some(out)
        }
        _ => Some(vec![lit_of(expr)?]),
    }
}
