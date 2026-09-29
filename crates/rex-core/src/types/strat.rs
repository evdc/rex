//! Stratification / monotonicity check (§8).
//!
//! `fix` has least-fixed-point semantics, which exist only over the *monotone*
//! fragment of the algebra: within a recursion group's body, no non-monotone
//! operator (`distinct`, `except`/`antijoin`, aggregation) may
//! appear **above a recursive occurrence** — growing the recursive relation
//! could then shrink the result, and Kleene iteration loses its guarantee.
//! Non-monotone operators over non-recursive subexpressions are fine (they are
//! constants of the iteration), as is any use *of* a converged recursive view
//! in a later, non-recursive statement — that is the stratum boundary.
//!
//! Expressed structurally on the combinator tree, consuming the `monotone`
//! bits of the §7 metadata table ([`crate::operator`]) — cleaner than
//! Datalog's predicate-dependency-graph formulation.

use super::typed::{TExpr, TExprKind, TProgram, TStmt};
use crate::diagnostic::Diagnostic;
use crate::operator;

/// Reject any non-monotone operator applied over a recursive occurrence,
/// one diagnostic per offending node.
pub fn check_stratification(prog: &TProgram) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for stmt in &prog.stmts {
        if let TStmt::LetRec { bindings } = stmt {
            for (_, body) in bindings {
                walk(body, &mut out);
            }
        }
    }
    out
}

/// The §7 metadata name of a combinator node, `None` for leaves and for
/// `Filter` (a grounded pointwise filter is monotone by construction).
fn op_name(kind: &TExprKind) -> Option<&'static str> {
    use TExprKind::*;
    Some(match kind {
        Compose(..) => "compose",
        Semijoin(..) => "restrict",
        Inverse(..) => "inverse",
        Fork(..) => "fork",
        Union(..) => "union",
        Intersect(..) => "intersect",
        Distinct(..) => "distinct",
        Proj(..) => "proj",
        By(..) => "by",
        Antijoin(..) => "antijoin",
        Agg(..) => "aggregate",
        BinCompare(..) => "compare",
        InRel(..) => "in",
        Mul(..) => "mul",
        Arith(k, ..) => match k {
            crate::types::typed::ArithKind::Add => "add",
            crate::types::typed::ArithKind::Sub => "sub",
            crate::types::typed::ArithKind::Div => "div",
            crate::types::typed::ArithKind::Mod => "mod",
        },
        Concat(..) => "concat",
        Identity(..) | View(..) | RecVar(..) | ValueRef(..) | Field(..) | Const { .. }
        | Atom(..) | UnitPoint | UnitConst(..) | Filter(..) | Coreflexive(..) => return None,
    })
}

/// Returns whether `e` contains a recursive occurrence, reporting every
/// non-monotone node that sits above one.
fn walk(e: &TExpr, out: &mut Vec<Diagnostic>) -> bool {
    use TExprKind::*;
    let recursive = match &e.kind {
        RecVar(_) => true,
        Identity(_) | View(_) | ValueRef(_) | Field(_) | Const { .. } | Atom(_) | UnitPoint | UnitConst(_)
        | Coreflexive(_) => false,
        Filter(a, _) | Distinct(a) | Inverse(a) | Proj(_, a) | Agg(_, a, _) | InRel(a, _) => {
            walk(a, out)
        }
        Compose(a, b)
        | Semijoin(a, b)
        | Fork(a, b)
        | Union(a, b)
        | Intersect(a, b)
        | By(a, b)
        | Antijoin(a, b)
        | BinCompare(_, a, b)
        | Mul(a, b)
        | Arith(_, a, b)
        | Concat(a, b) => {
            // Both sides always walked so every offending node is reported.
            let ra = walk(a, out);
            let rb = walk(b, out);
            ra || rb
        }
    };
    if recursive && let Some(name) = op_name(&e.kind) {
        // A missing table entry must be loud: silently treating an operator as
        // monotone would break the LFP guarantee. `op_name` and `OPERATORS`
        // share the name as their join key, so drift is a programmer error.
        let meta = operator::lookup(name)
            .unwrap_or_else(|| panic!("no §7 metadata for operator `{name}`"));
        if !meta.monotone {
            out.push(Diagnostic::error(
                e.span,
                format!(
                    "`{name}` is not monotone and cannot be applied over a recursive \
                     occurrence (§8); move it outside the recursive view — applying it \
                     to the converged view in a later `let` is fine"
                ),
            ));
        }
    }
    recursive
}
