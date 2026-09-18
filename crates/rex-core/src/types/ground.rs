//! Groundedness analysis (§9.1).
//!
//! Built-in relations (`<`, `>`, `=`, `in`, `*`, `||`) are **infinite** — they can
//! only be evaluated when *applied to a finite relation* that grounds their domain.
//! This pass walks the elaborated [`TProgram`] and rejects any program that would
//! require enumerating an infinite value domain, before the evaluator ever runs.
//!
//! Elaboration has already folded the grounded cases (`R[> 30]`, `R where > 30`)
//! into [`Filter`](TExprKind::Filter)/[`Semijoin`](TExprKind::Semijoin) over a finite
//! left relation, so a *surviving standalone* [`Coreflexive`](TExprKind::Coreflexive)
//! whose left-domain isn't enumerable is exactly the ungrounded signal.
//!
//! The check is sound but incomplete (§9.1): it may reject some terminating programs
//! — the accepted borrow-checker-style tax.

use super::env::Env;
use super::ty::ValueTy;
use super::typed::{TExpr, TExprKind, TProgram, TStmt};
use crate::diagnostic::Diagnostic;

/// Domains the interpreter can enumerate (`Interp::identity`). Finite coproducts
/// are finite in principle but not yet enumerable at runtime, so they are a
/// documented future extension rather than grounded today.
fn enumerable(ty: &ValueTy) -> bool {
    matches!(ty, ValueTy::Id(_) | ValueTy::Unit)
}

/// Reject any ungrounded relation in `prog`, returning one diagnostic per offending
/// standalone built-in.
pub fn check_groundedness(prog: &TProgram, env: &Env) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for stmt in &prog.stmts {
        match stmt {
            TStmt::Let { body, .. } => {
                walk(body, env, &mut out);
            }
            TStmt::LetRec { bindings } => {
                for (_, body) in bindings {
                    walk(body, env, &mut out);
                }
            }
            // `TStmt::New` field values are literals/refs — always grounded.
            TStmt::New { .. } => {}
        }
    }
    out
}

/// Returns whether `e` denotes a grounded (materializable) relation, pushing a
/// diagnostic at each offending leaf. Both children of binary nodes are always
/// visited (no short-circuit) so every ungrounded leaf is reported; the diagnostic
/// is emitted only at the offending `Coreflexive`, so enclosing nodes never
/// re-report it.
fn walk(e: &TExpr, env: &Env, out: &mut Vec<Diagnostic>) -> bool {
    use TExprKind::*;
    match &e.kind {
        // Finite leaves. A `RecVar` is grounded: the fixpoint iterates from the
        // empty Z-set, so every iterate is finite when the rest of the body is.
        Identity(_) | View(_) | RecVar(_) | ValueRef(_) | Field(_) | Const { .. } | Atom(_) => true,

        // The one leaf that can be ungrounded: an infinite built-in standing on its
        // own, grounded only when its domain is enumerable.
        Coreflexive(_) => {
            if enumerable(&e.ty.from) {
                true
            } else {
                out.push(Diagnostic::error(
                    e.span,
                    format!(
                        "ungrounded relation: this filter has no finite domain (`{}`); \
                         apply it to a finite relation — inside a filter `R[...]`, a \
                         `where`, or a composition `R . ...` — to ground it",
                        env.show(&e.ty.from)
                    ),
                ));
                false
            }
        }

        // Unary: grounded iff the operand is.
        Filter(a, _) | Distinct(a) | Inverse(a) | Proj(_, a) | Agg(_, a) | InRel(a, _) => {
            walk(a, env, out)
        }

        // Binary: grounded iff both operands are.
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
            let ga = walk(a, env, out);
            let gb = walk(b, env, out);
            ga && gb
        }
    }
}
