//! The interpreter. It type-checks the program to obtain the elaborated
//! [`TProgram`], then evaluates it directly: statements run in order (`new`
//! populates the in-memory DB and binds ids; view `let`s evaluate to
//! relations). Because the typed AST already carries every relation type, all
//! resolved field/sort/identifier references, and grounded filter predicates,
//! evaluation is a straight structural walk — no domain threading, environment
//! lookups, or type recomputation.

use super::algebra::{self, Agg};
use super::relation::{BTreeRelation, BinaryRelation};
use super::value::Value;
use crate::types::check;
use crate::types::env::Env;
use crate::types::ty::{SortId, ValueTy};
use crate::types::typed::*;
use std::collections::{BTreeSet, HashMap};

pub struct EvalResult {
    pub views: HashMap<String, BTreeRelation>,
    pub env: Env,
}

impl EvalResult {
    pub fn view(&self, name: &str) -> Option<&BTreeRelation> {
        self.views.get(name)
    }
}

/// Type-check (producing the elaborated AST) and evaluate `program`. If the
/// program has type errors, no views are produced.
pub fn run(program: &crate::ast::Program) -> EvalResult {
    let checked = check::check(program);
    let mut interp = Interp::new();
    if let Some(typed) = &checked.elaborated {
        interp.run(typed);
    }
    EvalResult {
        views: interp.views,
        env: checked.env,
    }
}

/// Evaluate an already-elaborated program, returning both the view relations and
/// the `new`-bound values (entity ids). The REPL uses the values to display
/// `let x = new E { .. }` bindings alongside view `let`s.
pub fn run_typed_values(typed: &TProgram) -> (HashMap<String, BTreeRelation>, HashMap<String, Value>) {
    let mut interp = Interp::new();
    interp.run(typed);
    (interp.views, interp.values)
}

#[derive(Default)]
struct Interp {
    fields: HashMap<(SortId, String), BTreeRelation>,
    ids: HashMap<SortId, BTreeRelation>,
    next_id: HashMap<SortId, u64>,
    views: HashMap<String, BTreeRelation>,
    values: HashMap<String, Value>,
}

impl Interp {
    fn new() -> Interp {
        Interp::default()
    }

    fn run(&mut self, program: &TProgram) {
        for stmt in &program.stmts {
            match stmt {
                TStmt::New { name, sort, fields } => {
                    let id = self.eval_new(*sort, fields);
                    if let Some(name) = name {
                        self.values.insert(name.clone(), id);
                    }
                }
                TStmt::Let { name, body } => {
                    let rel = self.eval(body);
                    if let Some(name) = name {
                        self.views.insert(name.clone(), rel);
                    }
                }
            }
        }
    }

    fn eval_new(&mut self, sort: SortId, fields: &[(String, TValue)]) -> Value {
        let n = self.next_id.entry(sort).or_insert(0);
        let id = Value::Id(sort, *n);
        *n += 1;
        for (name, tv) in fields {
            let v = self.eval_tvalue(tv);
            self.fields
                .entry((sort, name.clone()))
                .or_default()
                .add(id.clone(), v, 1);
        }
        self.ids.entry(sort).or_default().add(id.clone(), id.clone(), 1);
        id
    }

    fn eval_tvalue(&self, tv: &TValue) -> Value {
        match tv {
            TValue::Lit(lit) => lit_value(lit),
            TValue::Ref(name) => self
                .values
                .get(name)
                .cloned()
                .unwrap_or_else(|| panic!("unbound value `{name}`")),
        }
    }

    fn eval(&self, te: &TExpr) -> BTreeRelation {
        eval_expr_with(self, te)
    }
}

/// Read access to the base tables, views, and value bindings an expression
/// evaluates against. The batch [`Interp`] implements it over its own maps;
/// the incremental Session implements it over the engine's integrals, so a
/// scratch expression (or a test oracle) can be batch-evaluated against live
/// circuit state.
pub trait Store {
    fn field_rel(&self, sort: SortId, field: &str) -> BTreeRelation;
    fn identity_rel(&self, sort: SortId) -> BTreeRelation;
    fn view_rel(&self, name: &str) -> BTreeRelation;
    fn value(&self, name: &str) -> Value;
}

impl Store for Interp {
    fn field_rel(&self, sort: SortId, field: &str) -> BTreeRelation {
        self.fields
            .get(&(sort, field.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    fn identity_rel(&self, sort: SortId) -> BTreeRelation {
        self.ids.get(&sort).cloned().unwrap_or_default()
    }

    fn view_rel(&self, name: &str) -> BTreeRelation {
        self.views.get(name).cloned().unwrap_or_default()
    }

    fn value(&self, name: &str) -> Value {
        self.values
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("unbound value `{name}`"))
    }
}

/// Batch-evaluate one elaborated expression against any [`Store`]. This is the
/// structural walk the interpreter always did, factored out of `Interp` so it
/// can also read through a live incremental engine.
pub fn eval_expr_with(store: &dyn Store, te: &TExpr) -> BTreeRelation {
    let eval = |e: &TExpr| eval_expr_with(store, e);
    match &te.kind {
        TExprKind::Identity(sort) => store.identity_rel(*sort),
        TExprKind::View(name) => store.view_rel(name),
        TExprKind::ValueRef(name) => singleton(store.value(name)),
        TExprKind::Field(hops) => eval_field(store, hops),
        TExprKind::Const { lit, dom } => {
            let val = lit_value(lit);
            let mut r = BTreeRelation::new();
            for id in store.identity_rel(*dom).domain() {
                r.add(id, val.clone(), 1);
            }
            r
        }
        TExprKind::Atom(a) => singleton(Value::Atom(a.clone())),

        TExprKind::Compose(a, b) => algebra::compose(&eval(a), &eval(b)),
        TExprKind::Semijoin(a, b) => algebra::semijoin(&eval(a), &eval(b)),
        TExprKind::Filter(a, pred) => {
            let p = predicate(pred);
            algebra::filter_right(&eval(a), |v| p(v))
        }
        TExprKind::Fork(a, b) => algebra::fork(&eval(a), &eval(b)),
        TExprKind::Union(a, b) => algebra::union(&eval(a), &eval(b)),
        TExprKind::Intersect(a, b) => algebra::intersect(&eval(a), &eval(b)),
        TExprKind::Inverse(a) => algebra::inverse(&eval(a)),
        TExprKind::Distinct(a) => algebra::distinct(&eval(a)),
        TExprKind::By(x, y) => {
            // X by Y == ~Y . X
            algebra::compose(&algebra::inverse(&eval(y)), &eval(x))
        }
        TExprKind::Antijoin(a, b) => {
            let ra = eval(a);
            let sj = algebra::semijoin(&ra, &eval(b));
            let mut out = ra.clone();
            for (l, r, w) in sj.iter() {
                out.add(l, r, -w);
            }
            out
        }
        TExprKind::Mul(a, b) => {
            let money = te.ty.to == ValueTy::Money;
            algebra::value_join(&eval(a), &eval(b), move |x, y| mul_values(x, y, money))
        }
        TExprKind::Concat(a, b) => algebra::value_join(&eval(a), &eval(b), concat_values),

        TExprKind::Coreflexive(pred) => {
            // Standalone coreflexive built-in: materializable only over an
            // enumerable (entity) domain. The groundedness pass (§9.1,
            // `types::ground`) statically rejects any other domain, so the
            // non-`Id` fallthrough below is unreachable on a checked program;
            // the empty fallback remains only as a release-build safety net.
            debug_assert!(
                matches!(te.ty.from, ValueTy::Id(_)),
                "groundedness pass guarantees an enumerable domain"
            );
            let mut r = BTreeRelation::new();
            if let ValueTy::Id(sort) = te.ty.from {
                let p = predicate(pred);
                for (l, right, w) in store.identity_rel(sort).iter() {
                    if p(&right) {
                        r.add(l, right, w);
                    }
                }
            }
            r
        }
        TExprKind::BinCompare(op, a, b) => {
            let ra = eval(a);
            let rb = eval(b);
            let mut out = BTreeRelation::new();
            // A coreflexive on the shared key: for each key, `a OP b` over its
            // co-keyed values. Weights combine bilinearly (as in `a . OP . ~b`).
            for k in ra.domain() {
                for (va, wa) in ra.row(&k) {
                    for (vb, wb) in rb.row(&k) {
                        if compare_values(*op, &va, &vb) {
                            out.add(k.clone(), k.clone(), wa * wb);
                        }
                    }
                }
            }
            out
        }
        TExprKind::InRel(a, lits) => {
            let set: BTreeSet<Value> = lits.iter().map(lit_value).collect();
            let ra = eval(a);
            let mut out = BTreeRelation::new();
            for (k, v, w) in ra.iter() {
                if set.contains(&v) {
                    out.add(k.clone(), k, w);
                }
            }
            out
        }
        TExprKind::Agg(kind, arg) => {
            let money = arg.ty.to == ValueTy::Money;
            algebra::aggregate(&eval(arg), agg_of(*kind), money)
        }
    }
}

fn eval_field(store: &dyn Store, hops: &[FieldHop]) -> BTreeRelation {
    let mut rel = store.field_rel(hops[0].sort, &hops[0].field);
    for hop in &hops[1..] {
        rel = algebra::compose(&rel, &store.field_rel(hop.sort, &hop.field));
    }
    rel
}

// --- free helpers ---------------------------------------------------------

fn singleton(v: Value) -> BTreeRelation {
    let mut r = BTreeRelation::new();
    r.add(v.clone(), v, 1);
    r
}

fn agg_of(kind: AggKind) -> Agg {
    match kind {
        AggKind::Sum => Agg::Sum,
        AggKind::Count => Agg::Count,
        AggKind::Avg => Agg::Avg,
        AggKind::Min => Agg::Min,
        AggKind::Max => Agg::Max,
    }
}

/// Convert a types-level literal into a runtime [`Value`]. Shared with the
/// incremental backend's lowering and filter kernels.
pub fn lit_value(lit: &Lit) -> Value {
    match lit {
        Lit::Int(n) => Value::Int(*n),
        Lit::Decimal(s) => Value::money_from_decimal(s),
        Lit::Str(s) => Value::Text(s.clone()),
        Lit::Date { year, month, day } => Value::Date {
            year: *year,
            month: *month,
            day: *day,
        },
        Lit::Atom(a) => Value::Atom(a.clone()),
    }
}

/// Build the grounded-value predicate a [`Pred`] denotes. Shared with the
/// incremental backend's filter kernel.
pub fn predicate(pred: &Pred) -> Box<dyn Fn(&Value) -> bool> {
    match pred {
        Pred::Cmp(op, lit) => {
            let op = *op;
            let target = lit_value(lit);
            Box::new(move |v| compare_values(op, v, &target))
        }
        Pred::InSet(lits) => {
            let set: BTreeSet<Value> = lits.iter().map(lit_value).collect();
            Box::new(move |v| set.contains(v))
        }
    }
}

/// `a * b` on the shared numeric scale, tagged `Money` or `Int`. Shared with
/// the incremental backend's co-keyed kernel.
pub fn mul_values(a: &Value, b: &Value, money: bool) -> Value {
    let n = a.as_i64().unwrap_or(0) * b.as_i64().unwrap_or(0);
    if money {
        Value::Money(n)
    } else {
        Value::Int(n)
    }
}

/// `a || b` text concatenation. Shared with the incremental backend's co-keyed
/// kernel.
pub fn concat_values(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Text(x), Value::Text(y)) => Value::Text(format!("{x}{y}")),
        _ => Value::Text(format!("{a}{b}")),
    }
}

/// Compare two values, cents-aware across Int/Money. Shared with the
/// incremental backend's co-keyed kernel.
pub fn compare_values(op: crate::ast::CmpOp, a: &Value, b: &Value) -> bool {
    use crate::ast::CmpOp::*;
    if let (Some(x), Some(y)) = (a.as_cents(), b.as_cents()) {
        return match op {
            Eq => x == y,
            Lt => x < y,
            Gt => x > y,
            Le => x <= y,
            Ge => x >= y,
        };
    }
    match op {
        Eq => a == b,
        Lt => a < b,
        Gt => a > b,
        Le => a <= b,
        Ge => a >= b,
    }
}
