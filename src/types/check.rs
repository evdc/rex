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
}

pub fn check(program: &Program) -> CheckResult {
    let mut cx = Checker {
        env: Env::new(),
        diagnostics: Vec::new(),
    };
    let stmts = cx.run(program);
    let prog = TProgram { stmts };
    // Groundedness (§9.1) runs only on a clean type-check, since the elaborated AST
    // is well-formed only then. A rejection here nulls `elaborated`, so every
    // downstream caller refuses to evaluate the program.
    if cx.diagnostics.is_empty() {
        cx.diagnostics
            .extend(super::ground::check_groundedness(&prog, &cx.env));
    }
    let elaborated = if cx.diagnostics.is_empty() {
        Some(prog)
    } else {
        None
    };
    CheckResult {
        env: cx.env,
        diagnostics: cx.diagnostics,
        elaborated,
    }
}

struct Bail;
type TResult<T> = Result<T, Bail>;

struct Checker {
    env: Env,
    diagnostics: Vec<Diagnostic>,
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
        // Pass 3: check & elaborate `let`s in order.
        let mut stmts = Vec::new();
        for stmt in &program.stmts {
            if let Stmt::Let(l) = stmt
                && let Ok(ts) = self.check_let(l)
            {
                stmts.push(ts);
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

            ExprKind::Ident(name) => self.check_ident(name, span),

            ExprKind::FieldPath(parts) => self.check_field_path(parts, span, dom),

            ExprKind::Atom(a) => Ok(TExpr::new(
                TExprKind::Atom(a.clone()),
                RelTy::coreflexive(ValueTy::Atom(a.clone())),
                span,
            )),

            ExprKind::Int(_) => self.constant(expr, dom, ValueTy::Int),
            ExprKind::Decimal(_) => self.constant(expr, dom, ValueTy::Money),
            ExprKind::Str(_) => self.constant(expr, dom, ValueTy::Text),
            ExprKind::Date { .. } => self.constant(expr, dom, ValueTy::Date),

            ExprKind::Compose(a, b) => {
                let ta = self.check_rel(a, dom)?;
                let tb = self.check_rel(b, Some(ta.ty.to.clone()))?;
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
            ExprKind::Concat(a, b) => self.check_arith(a, b, dom, span, ArithOp::Concat),

            ExprKind::Compare { op, lhs, rhs } => self.check_compare(*op, lhs, rhs, dom, span),
            ExprKind::In { lhs, rhs } => self.check_in(lhs, rhs, dom, span),

            ExprKind::Call { func, args } => self.check_call(func, args, dom, span),

            ExprKind::New { .. } => {
                self.error(span, "`new` may only appear as the body of a `let`")
            }
        }
    }

    fn check_ident(&mut self, name: &str, span: Span) -> TResult<TExpr> {
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
                "cannot resolve a `:field` without a known domain (add a type annotation)",
            );
        };
        let mut sort = self.as_sort(span, &dom, &format!("`:{}`", parts.join(".")))?;
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
            ArithOp::Mul => {
                if !ta.ty.to.is_numeric() || !tb.ty.to.is_numeric() {
                    return self.error(
                        span,
                        format!(
                            "`*` needs numeric operands, got `{}` and `{}`",
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
            ArithOp::Concat => {
                if ta.ty.to != ValueTy::Text || tb.ty.to != ValueTy::Text {
                    return self.error(
                        span,
                        format!(
                            "`||` needs Text operands, got `{}` and `{}`",
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
        let elem_ty = self.check_rel(rhs, None)?.ty.to;
        let Some(lits) = collect_lits(rhs) else {
            return self.error(rhs.span, "an `in` set must be a set of literals");
        };
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
        Ok(TExpr::new(TExprKind::Agg(agg, Box::new(image)), ty, span))
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

    /// Every atom of `subset` must appear in `universe` (used for `in`).
    fn expect_subset(&mut self, span: Span, subset: &ValueTy, universe: &ValueTy) -> TResult<()> {
        match (subset.atoms(), universe.atoms()) {
            (Some(sub), Some(univ)) if sub.iter().all(|a| univ.contains(a)) => Ok(()),
            _ => self.error(
                span,
                format!(
                    "`in` set `{}` is not within the value type `{}`",
                    self.env.show(subset),
                    self.env.show(universe)
                ),
            ),
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
    Concat,
}

impl ArithOp {
    fn name(self) -> &'static str {
        match self {
            ArithOp::Mul => "`*`",
            ArithOp::Concat => "`||`",
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
