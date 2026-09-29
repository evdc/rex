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
                TStmt::LetRec { bindings } => {
                    for (name, rel) in eval_fixpoint(self, bindings) {
                        self.views.insert(name, rel);
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
    /// The current iterate of a recursion-group member. Meaningful only inside
    /// a fixpoint evaluation ([`eval_fixpoint`] overlays it); the checker
    /// guarantees `RecVar` appears nowhere else.
    fn rec_rel(&self, name: &str) -> BTreeRelation {
        unreachable!("RecVar `{name}` outside a recursion group")
    }
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

/// Joint Kleene iteration for one recursion group (§8): every member starts at
/// the empty Z-set; each round re-evaluates all bodies against the previous
/// iterates with a forced `distinct` at the knot; stop when no member changes.
/// The clamp to {0,1} is what guarantees termination on cyclic data.
pub fn eval_fixpoint(
    store: &dyn Store,
    bindings: &[(String, TExpr)],
) -> Vec<(String, BTreeRelation)> {
    let mut iterates: HashMap<String, BTreeRelation> = bindings
        .iter()
        .map(|(n, _)| (n.clone(), BTreeRelation::new()))
        .collect();
    loop {
        let overlay = RecStore { inner: store, iterates: &iterates };
        let mut next = HashMap::new();
        let mut changed = false;
        for (name, body) in bindings {
            let cand = algebra::distinct(&eval_expr_with(&overlay, body));
            changed |= cand != iterates[name];
            next.insert(name.clone(), cand);
        }
        iterates = next;
        if !changed {
            break;
        }
    }
    bindings
        .iter()
        .map(|(n, _)| (n.clone(), iterates.remove(n).expect("iterate exists")))
        .collect()
}

/// Overlay store used during a fixpoint: `RecVar` reads the current iterate;
/// everything else passes through.
struct RecStore<'a> {
    inner: &'a dyn Store,
    iterates: &'a HashMap<String, BTreeRelation>,
}

impl Store for RecStore<'_> {
    fn field_rel(&self, sort: SortId, field: &str) -> BTreeRelation {
        self.inner.field_rel(sort, field)
    }
    fn identity_rel(&self, sort: SortId) -> BTreeRelation {
        self.inner.identity_rel(sort)
    }
    fn view_rel(&self, name: &str) -> BTreeRelation {
        self.inner.view_rel(name)
    }
    fn value(&self, name: &str) -> Value {
        self.inner.value(name)
    }
    fn rec_rel(&self, name: &str) -> BTreeRelation {
        self.iterates
            .get(name)
            .cloned()
            .unwrap_or_else(|| self.inner.rec_rel(name))
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
        TExprKind::RecVar(name) => store.rec_rel(name),
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
        TExprKind::Atom(a) => singleton(Value::atom(a)),
        TExprKind::UnitPoint => singleton(Value::Unit),
        TExprKind::UnitConst(lit) => {
            let mut r = BTreeRelation::new();
            r.add(Value::Unit, lit_value(lit), 1);
            r
        }

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
        TExprKind::Proj(side, a) => algebra::proj(&eval(a), *side),
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
        TExprKind::Arith(kind, a, b) => {
            let money = te.ty.to == ValueTy::Money;
            let kind = *kind;
            algebra::value_join(&eval(a), &eval(b), move |x, y| arith_values(kind, x, y, money))
        }

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
        TExprKind::Agg(kind, arg, total) => {
            let money = arg.ty.to == ValueTy::Money;
            let mut out = algebra::aggregate(&eval(arg), agg_of(*kind), money);
            // A total group key is present even with an empty image, so the
            // aggregate yields its monoid identity rather than no row at all
            // (S-50). `Min`/`Max`/`Avg` have no identity and stay absent —
            // the incremental backend's `Node::Aggregate` matches this arm
            // exactly, and `tests/dbsp.rs` holds the two to each other.
            if *total == Total::Unit
                && out.row_ref(&Value::Unit).next().is_none()
                && let Some(id) = agg_identity(*kind, money)
            {
                out.add(Value::Unit, id, 1);
            }
            out
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

/// The monoid identity a *total* group emits for an empty image (S-50), or
/// `None` for the aggregates that have none.
pub fn agg_identity(kind: AggKind, money: bool) -> Option<Value> {
    match kind {
        AggKind::Count => Some(Value::Int(0)),
        AggKind::Sum => Some(if money { Value::Money(0) } else { Value::Int(0) }),
        AggKind::Avg | AggKind::Min | AggKind::Max => None,
    }
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
        Lit::Unit => Value::Unit,
        Lit::Int(n) => Value::Int(*n),
        Lit::Decimal(s) => Value::money_from_decimal(s),
        Lit::Str(s) => Value::text(s),
        Lit::Date { year, month, day } => Value::Date {
            year: *year,
            month: *month,
            day: *day,
        },
        Lit::Atom(a) => Value::atom(a),
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

/// `a ++ b` text concatenation. Shared with the incremental backend's co-keyed
/// kernel.
/// `a + b`, `a - b`, `a / b`, `a % b`. `money` (set when either operand is
/// `Money`, per the checker's Money-if-either rule) picks the scale: cents
/// when the result is `Money` (so a bare `Int` operand promotes to whole
/// units), raw magnitude when it's `Int` — an unconditional `as_cents()`
/// would silently scale a pure `Int + Int` by 100. Division by zero yields 0
/// (a relation has no place for a runtime error; the checker may later
/// reject non-grounded divisors).
pub fn arith_values(kind: crate::types::typed::ArithKind, a: &Value, b: &Value, money: bool) -> Value {
    use crate::types::typed::ArithKind::*;
    let (x, y) = if money {
        (a.as_cents().unwrap_or(0), b.as_cents().unwrap_or(0))
    } else {
        (a.as_i64().unwrap_or(0), b.as_i64().unwrap_or(0))
    };
    let n = match kind {
        Add => x + y,
        Sub => x - y,
        Div => if y == 0 { 0 } else { x / y },
        Mod => if y == 0 { 0 } else { x % y },
    };
    if money {
        Value::Money(n)
    } else {
        Value::Int(n)
    }
}

pub fn concat_values(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Text(x), Value::Text(y)) => {
            Value::text(&format!("{}{}", x.as_str(), y.as_str()))
        }
        _ => Value::text(&format!("{a}{b}")),
    }
}

/// Compare two values, cents-aware across Int/Money. Shared with the
/// incremental backend's co-keyed kernel.
pub fn compare_values(op: crate::ast::CmpOp, a: &Value, b: &Value) -> bool {
    use crate::ast::CmpOp::*;
    if let (Some(x), Some(y)) = (a.as_cents(), b.as_cents()) {
        return match op {
            Eq => x == y,
            Ne => x != y,
            Lt => x < y,
            Gt => x > y,
            Le => x <= y,
            Ge => x >= y,
        };
    }
    // Non-numeric: the order the language means (Text lexicographic — the
    // derived `Ord` ranks interned symbols by id, which is storage order,
    // not surface semantics).
    let ord = a.cmp_semantic(b);
    match op {
        Eq => ord.is_eq(),
        Ne => ord.is_ne(),
        Lt => ord.is_lt(),
        Gt => ord.is_gt(),
        Le => ord.is_le(),
        Ge => ord.is_ge(),
    }
}
