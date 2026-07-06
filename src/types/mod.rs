//! Static type/soundness checking (§4, §9).

pub mod check;
pub mod env;
pub mod ground;
pub mod strat;
pub mod ty;
pub mod typed;

pub use check::{check, CheckResult};
pub use env::{Binding, Env};
pub use ty::{RelTy, SortId, ValueTy};
pub use typed::{TExpr, TExprKind, TProgram, TStmt};
