//! Diagnostics carried through every compiler layer. Errors accumulate rather
//! than panic, so a single pass can report many problems.

use crate::span::Span;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    pub fn error(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            span,
            message: message.into(),
        }
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Warning,
            span,
            message: message.into(),
        }
    }

    /// Render the diagnostic against the source, with a line:col prefix and a
    /// caret-underlined excerpt of the offending line.
    pub fn render(&self, src: &str) -> String {
        let (line, col) = self.span.line_col(src);
        let line_text = src.lines().nth(line - 1).unwrap_or("");
        let caret_pad = " ".repeat(col.saturating_sub(1));
        let caret_len = self.span.len().max(1);
        let carets = "^".repeat(caret_len);
        format!(
            "{sev} at {line}:{col}: {msg}\n  {line_text}\n  {caret_pad}{carets}",
            sev = self.severity,
            msg = self.message,
        )
    }
}
