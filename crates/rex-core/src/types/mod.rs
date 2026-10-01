//! Static type/soundness checking (§4, §9).

pub mod check;
pub mod component;
pub mod env;
pub mod ground;
pub mod names;
pub mod shape_ir;
pub mod strat;
pub mod ty;
pub mod typed;
pub mod view;

pub use check::{check, CheckResult};
pub use env::{Binding, Env};
pub use shape_ir::ShapeProgram;
pub use ty::{RelTy, SortId, ValueTy};
pub use typed::{TExpr, TExprKind, TProgram, TStmt};
