//! Source spans: byte ranges into the original source, with line/column resolution
//! for diagnostics.

use std::fmt;

/// A half-open byte range `[start, end)` into the source string.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        debug_assert!(start <= end, "span start must not exceed end");
        Span { start, end }
    }

    /// The empty span at a single offset (used for EOF and synthetic nodes).
    pub fn point(at: usize) -> Span {
        Span { start: at, end: at }
    }

    /// The smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn len(self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The source text this span covers.
    pub fn slice(self, src: &str) -> &str {
        &src[self.start..self.end]
    }

    /// Resolve the 1-based line and column of this span's start.
    pub fn line_col(self, src: &str) -> (usize, usize) {
        let mut line = 1;
        let mut col = 1;
        for (i, ch) in src.char_indices() {
            if i >= self.start {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}
