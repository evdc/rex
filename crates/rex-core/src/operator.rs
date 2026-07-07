//! §7 operator metadata table. Each combinator carries four bits that encode
//! much of the semantics. In v1 this is *forward-compatibility data*: it is
//! recorded here but the consumers (linearity cost model, `fix` stratification)
//! arrive in later milestones.

/// Relational (coreflexive-valued) vs functional (value-valued). Drives the
/// desugar form (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Relational,
    Functional,
}

/// Finiteness for the groundedness analysis (§9.1); consumed by
/// [`crate::types::ground`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grounding {
    Finite,
    /// Built-ins like `<`, `+arith`, `in` are infinite and must be grounded by
    /// application to enough finite arguments.
    Infinite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpMeta {
    pub name: &'static str,
    pub kind: Kind,
    /// Legality below `fix` (§8) — dormant until recursion.
    pub monotone: bool,
    /// Drives the incremental cost model (§9.3, §10).
    pub linear: bool,
    pub grounding: Grounding,
}

use Grounding::*;
use Kind::*;

/// The metadata table (§7). Examples from the SPEC:
/// `.`, `[]`, `~`, `+union` — monotone, linear; `distinct`, `except`,
/// aggregation — non-monotone and non-linear.
pub const OPERATORS: &[OpMeta] = &[
    meta("compose", Relational, true, true, Finite),
    meta("restrict", Relational, true, true, Finite),
    meta("inverse", Relational, true, true, Finite),
    meta("fork", Relational, true, true, Finite),
    meta("union", Relational, true, true, Finite),
    // Elementwise `min` of weights is monotone in both arguments (§3.1 calls
    // intersect non-*linear* only — it shares distinct's cost tier, not its
    // non-monotonicity).
    meta("intersect", Relational, true, false, Finite),
    meta("distinct", Relational, false, false, Finite),
    // fst/snd: pointwise map over the right column — monotone, linear.
    meta("proj", Relational, true, true, Finite),
    meta("except", Relational, false, false, Finite),
    meta("antijoin", Relational, false, false, Finite),
    meta("by", Relational, true, true, Finite),
    meta("aggregate", Relational, false, false, Finite),
    // Built-in comparison/arithmetic relations are infinite (need grounding).
    meta("compare", Relational, true, false, Infinite),
    meta("in", Relational, true, false, Infinite),
    meta("mul", Functional, true, true, Infinite),
    meta("concat", Functional, true, true, Infinite),
    // The fixpoint itself is monotone (it *requires* a monotone body, §8) but
    // firmly in the non-linear/expensive tier — never "free" like a filter.
    meta("fix", Relational, true, false, Finite),
];

const fn meta(
    name: &'static str,
    kind: Kind,
    monotone: bool,
    linear: bool,
    grounding: Grounding,
) -> OpMeta {
    OpMeta {
        name,
        kind,
        monotone,
        linear,
        grounding,
    }
}

pub fn lookup(name: &str) -> Option<&'static OpMeta> {
    OPERATORS.iter().find(|m| m.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every combinator the checker/evaluator can produce must have metadata, so
    /// the table can't silently drift from the real operator set. When a new
    /// combinator is added, add it here and to `OPERATORS` together.
    #[test]
    fn every_combinator_has_metadata() {
        let expected = [
            "compose", "restrict", "inverse", "fork", "union", "intersect",
            "distinct", "proj", "except", "antijoin", "by", "aggregate", "compare",
            "in", "mul", "concat", "fix",
        ];
        for name in expected {
            assert!(lookup(name).is_some(), "missing metadata for `{name}`");
        }
    }

    #[test]
    fn operator_names_are_unique() {
        let mut seen = HashSet::new();
        for m in OPERATORS {
            assert!(seen.insert(m.name), "duplicate metadata for `{}`", m.name);
        }
    }
}
