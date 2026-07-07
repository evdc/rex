//! A batch-mode, in-memory evaluator for Rex (experimental).
//!
//! Layers: [`value`] (runtime domain elements), [`relation`] (the
//! `BinaryRelation` storage trait + a BTree impl), [`algebra`] (the relational
//! algebra over the trait), and [`interp`] (the AST-walking interpreter).

pub mod algebra;
pub mod encode;
pub mod intern;
pub mod interp;
pub mod relation;
pub mod value;

pub use encode::{decode_value, encode_value, json_quote, rows_to_json, step_result_to_json};
pub use intern::{intern, Sym};
pub use interp::{eval_expr_with, run, run_typed_values, EvalResult, Store};
pub use relation::{BTreeRelation, BinaryRelation};
pub use value::Value;
