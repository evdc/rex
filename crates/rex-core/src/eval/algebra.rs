//! The relational algebra (§3), written against the `BinaryRelation` trait and
//! producing `BTreeRelation`. Batch/eager: every operation fully materializes.
//!
//! Weights follow Z-set semantics: composition/fork/value-join are *bilinear*
//! (weights multiply then accumulate); union adds; intersect takes the
//! elementwise min; distinct clamps to {0,1}. Semijoin/filter are sub-relations
//! of their left input and keep its weights.

use super::relation::{BTreeRelation, BinaryRelation};
use super::value::Value;
use crate::ast::ProjSide;

/// `R . S` (§3.1): join R's right column against S's left; keep outer columns
/// `(A, C)`, projecting away the join column.
pub fn compose(r: &dyn BinaryRelation, s: &dyn BinaryRelation) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for (a, b, w1) in r.iter() {
        for (c, w2) in s.row(&b) {
            out.add(a.clone(), c, w1 * w2);
        }
    }
    out
}

/// `~R` (§3.1): converse.
pub fn inverse(r: &dyn BinaryRelation) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for (a, b, w) in r.iter() {
        out.add(b, a, w);
    }
    out
}

/// `R[S]` (§3.2): the same join as compose, but keep R's columns `(A, B)`. R's
/// right must be a *key* of S; matching entries keep R's weight (so `A[B] ⊆ A`,
/// §6).
pub fn semijoin(r: &dyn BinaryRelation, s: &dyn BinaryRelation) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for (a, b, w) in r.iter() {
        if s.has_left(&b) {
            out.add(a, b, w);
        }
    }
    out
}

/// `R + S`: Z-set addition (weights add).
pub fn union(r: &dyn BinaryRelation, s: &dyn BinaryRelation) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for (a, b, w) in r.iter().chain(s.iter()) {
        out.add(a, b, w);
    }
    out
}

/// `R & S`: elementwise `min` of weights (§3.1), over the union of supports —
/// symmetric, as `min` demands. A pair present in only one side has weight 0
/// on the other, so it contributes `min(w, 0)`: nothing when `w` is positive,
/// `w` itself when `w` is negative (a retraction intersects as a retraction).
pub fn intersect(r: &dyn BinaryRelation, s: &dyn BinaryRelation) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for (a, b, w) in r.iter() {
        let m = w.min(s.weight(&a, &b));
        if m != 0 {
            out.add(a, b, m);
        }
    }
    // Pairs present only in s: min(0, w) is nonzero only for negative w.
    for (a, b, w) in s.iter() {
        if r.weight(&a, &b) == 0 && w < 0 {
            out.add(a, b, w);
        }
    }
    out
}

/// `distinct R` (§3.1): clamp weights to {0, 1}.
pub fn distinct(r: &dyn BinaryRelation) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for (a, b, w) in r.iter() {
        if w > 0 {
            out.add(a, b, 1);
        }
    }
    out
}

/// `R , S` (§3.1): tupling. Co-keyed on the left; the right column becomes a
/// pair. Bilinear in weights.
pub fn fork(r: &dyn BinaryRelation, s: &dyn BinaryRelation) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for a in r.domain() {
        for (b, w1) in r.row(&a) {
            for (c, w2) in s.row(&a) {
                out.add(a.clone(), Value::Pair(Box::new(b.clone()), Box::new(c)), w1 * w2);
            }
        }
    }
    out
}

/// `fst R` / `snd R`: project one component of a pair-valued right column.
/// Linear (a pointwise map); weights accumulate when projections collide.
/// The checker guarantees the right column is pair-typed.
pub fn proj(r: &dyn BinaryRelation, side: ProjSide) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for (a, b, w) in r.iter() {
        out.add(a, proj_value(&b, side), w);
    }
    out
}

/// Select one component of a pair value.
pub fn proj_value(v: &Value, side: ProjSide) -> Value {
    let Value::Pair(x, y) = v else {
        panic!("type checker guarantees a pair-valued right column")
    };
    match side {
        ProjSide::Fst => (**x).clone(),
        ProjSide::Snd => (**y).clone(),
    }
}

/// Keep entries whose *right* column satisfies `pred` — the grounded form of a
/// coreflexive built-in used in filter position (`where > 30`).
pub fn filter_right(r: &dyn BinaryRelation, pred: impl Fn(&Value) -> bool) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for (a, b, w) in r.iter() {
        if pred(&b) {
            out.add(a, b, w);
        }
    }
    out
}

/// Co-keyed value-join for a binary functional built-in (`*`, `||`): for each
/// shared key, combine R's and S's right values with `f`. Bilinear in weights.
pub fn value_join(
    r: &dyn BinaryRelation,
    s: &dyn BinaryRelation,
    f: impl Fn(&Value, &Value) -> Value,
) -> BTreeRelation {
    let mut out = BTreeRelation::new();
    for a in r.domain() {
        for (b, w1) in r.row(&a) {
            for (c, w2) in s.row(&a) {
                out.add(a.clone(), f(&b, &c), w1 * w2);
            }
        }
    }
    out
}

/// The aggregations of §5: a homomorphism from each key's image Z-set into a
/// commutative monoid. `sum`/`count`/`avg` respect weights (multiplicity);
/// `min`/`max` are computed over the present values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agg {
    Sum,
    Count,
    Avg,
    Min,
    Max,
}

/// The mean of a group as `Money`: `sum / count` in cents, truncating toward
/// zero. An `Int` image is in whole units, so it is scaled to cents first
/// (`avg` of 1 and 2 is `$1.50`, not `$0.01`). `count` is nonzero. Shared with
/// the incremental backend's aggregate node.
pub fn avg_value(sum: i64, count: i64, money: bool) -> Value {
    let cents = if money { sum } else { sum.wrapping_mul(100) };
    Value::Money(cents.wrapping_div(count))
}

/// Aggregate `image : K -> V` into `K -> M`. `money` says the image is
/// `Money` (so numeric results are too, else `Int`); `avg` is always `Money`.
pub fn aggregate(image: &dyn BinaryRelation, agg: Agg, money: bool) -> BTreeRelation {
    use std::collections::BTreeMap;
    // Per-key accumulator: (sum, count, min, max).
    let mut acc: BTreeMap<Value, (i64, i64, Option<i64>, Option<i64>)> = BTreeMap::new();
    for (k, v, w) in image.iter() {
        let n = v.as_i64().unwrap_or(0);
        let entry = acc.entry(k).or_insert((0, 0, None, None));
        entry.0 = entry.0.wrapping_add(n.wrapping_mul(w));
        entry.1 = entry.1.wrapping_add(w);
        // min/max ignore weight sign; they consider present values.
        if w > 0 {
            entry.2 = Some(entry.2.map_or(n, |m| m.min(n)));
            entry.3 = Some(entry.3.map_or(n, |m| m.max(n)));
        }
    }

    let mk = |n: i64, as_money: bool| {
        if as_money {
            Value::Money(n)
        } else {
            Value::Int(n)
        }
    };

    let mut out = BTreeRelation::new();
    for (k, (sum, count, min, max)) in acc {
        let result = match agg {
            Agg::Sum => mk(sum, money),
            Agg::Count => Value::Int(count),
            Agg::Avg => {
                if count == 0 {
                    continue;
                }
                avg_value(sum, count, money)
            }
            Agg::Min => match min {
                Some(m) => mk(m, money),
                None => continue,
            },
            Agg::Max => match max {
                Some(m) => mk(m, money),
                None => continue,
            },
        };
        out.add(k, result, 1);
    }
    out
}
