//! Rex — a point-free binary-relational view language.
//!
//! This crate implements the compiler front end: lexer, parser, and (in
//! progress) the static type/soundness checker. See `SPEC.md` for the design.

pub mod ast;
pub mod dbsp;
pub mod diagnostic;
pub mod eval;
pub mod lexer;
pub mod operator;
pub mod parser;
pub mod pretty;
pub mod repl;
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
