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

/// Finiteness for the (later) groundedness analysis (§9.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grounding {
    Finite,
    /// Built-ins like `<`, `+arith` are infinite and must be grounded by
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
    meta("intersect", Relational, false, false, Finite),
    meta("distinct", Relational, false, false, Finite),
    meta("except", Relational, false, false, Finite),
    meta("antijoin", Relational, false, false, Finite),
    meta("by", Relational, true, true, Finite),
    meta("aggregate", Relational, false, false, Finite),
    // Built-in comparison/arithmetic relations are infinite (need grounding).
    meta("compare", Relational, true, false, Infinite),
    meta("mul", Functional, true, true, Infinite),
    meta("concat", Functional, true, true, Infinite),
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
