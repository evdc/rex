//! Rex — a point-free binary-relational view language.
//!
//! This crate is the compiler and both evaluators: the front end (lexer,
//! parser, the `view`/`state`/event desugaring, the checker and elaborator),
//! the batch interpreter that defines the semantics (`eval`), and the
//! incremental DBSP engine that implements them (`dbsp`, `events`). See
//! `SPEC.md` for the design and `SYNTAX.md` for the surface language.

pub mod ast;
pub mod dbsp;
pub mod diagnostic;
pub mod eval;
pub mod events;
pub mod lexer;
pub mod operator;
pub mod parser;
pub mod pretty;
pub mod span;
pub mod token;
pub mod types;

pub use diagnostic::Diagnostic;
pub use lexer::{lex, LexResult};
pub use parser::{parse, ParseResult};
pub use pretty::program_to_sexpr;
pub use types::{check, CheckResult};

/// Lex, parse, and type-check a source string, returning all diagnostics from
/// every phase. Type checking runs only if parsing produced no errors.
pub fn check_source(src: &str) -> Vec<Diagnostic> {
    let parsed = parse(src);
    if !parsed.diagnostics.is_empty() {
        return parsed.diagnostics;
    }
    check(&parsed.program).diagnostics
}
