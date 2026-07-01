//! A batch-mode, in-memory evaluator for Rex (experimental).
//!
//! Layers: [`value`] (runtime domain elements), [`relation`] (the
//! `BinaryRelation` storage trait + a BTree impl), [`algebra`] (the relational
//! algebra over the trait), and [`interp`] (the AST-walking interpreter).

pub mod algebra;
pub mod interp;
pub mod relation;
pub mod value;

pub use interp::{run, run_typed_values, EvalResult};
pub use relation::{BTreeRelation, BinaryRelation};
pub use value::Value;
