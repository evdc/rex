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

    /// Render in the `file:line:col` style editors and terminals link on:
    ///
    /// ```text
    /// error: unknown field `nope` on entity `Todo`
    ///  --> app.rex:6:40
    ///   |
    /// 6 | view main = ul { Todo as t select li { .nope } }
    ///   |                                         ^^^^^
    /// ```
    ///
    /// Tabs in the excerpt are kept in the caret padding so the marker lines
    /// up, and a span that runs past the end of its first line is underlined
    /// only as far as that line.
    pub fn render_file(&self, src: &str, path: &str) -> String {
        let (line, col) = self.span.line_col(src);
        let text = src.lines().nth(line - 1).unwrap_or("");
        let pad: String = text.chars().take(col.saturating_sub(1)).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
        let covered = src.get(self.span.start..self.span.end.min(src.len())).unwrap_or("");
        let carets = covered.split('\n').next().unwrap_or("").trim_end_matches('\r').chars().count().max(1);
        let margin = " ".repeat(line.to_string().len());
        format!(
            "{sev}: {msg}\n{margin}--> {path}:{line}:{col}\n{margin} |\n{line} | {text}\n{margin} | {pad}{carets}",
            sev = self.severity,
            msg = self.message,
            carets = "^".repeat(carets),
        )
    }
}
