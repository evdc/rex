//! Runtime domain elements. A `Value` is one point in the value algebra (§2),
//! concrete enough to store in a Z-set. `Ord + Hash` let it key a `BTreeMap`.
//!
//! `Money` is stored as an integer number of minor units (cents) to stay exact.

use super::intern::{intern, Sym};
use crate::types::ty::SortId;

/// NOTE on `Ord`: the derived order ranks `Text`/`Atom` by *interning order*
/// (see [`crate::eval::intern`]) — a total order fit for Z-set storage, not
/// lexicographic. User-visible comparisons go through [`Value::cmp_semantic`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    Unit,
    Int(i64),
    /// Money in minor units (cents).
    Money(i64),
    Text(Sym),
    Date { year: i32, month: u32, day: u32 },
    /// An entity id: which sort, and a per-sort sequence number.
    Id(SortId, u64),
    Atom(Sym),
    Pair(Box<Value>, Box<Value>),
}

impl Value {
    /// An interned text value.
    pub fn text(s: &str) -> Value {
        Value::Text(intern(s))
    }

    /// An interned atom value.
    pub fn atom(s: &str) -> Value {
        Value::Atom(intern(s))
    }

    /// The total order the *surface language* means: `Text`/`Atom`
    /// lexicographic (resolved through the interner), recursively inside
    /// pairs, everything else as the derived order.
    pub fn cmp_semantic(&self, other: &Value) -> std::cmp::Ordering {
        match (self, other) {
            (Value::Text(a), Value::Text(b)) | (Value::Atom(a), Value::Atom(b)) => {
                a.as_str().cmp(b.as_str())
            }
            (Value::Pair(a1, b1), Value::Pair(a2, b2)) => {
                a1.cmp_semantic(a2).then_with(|| b1.cmp_semantic(b2))
            }
            _ => self.cmp(other),
        }
    }
    /// Parse a decimal literal like `"9.99"` into money cents (`999`). Extra
    /// fractional digits are truncated; missing ones are padded. The sign is
    /// taken from the literal itself so sub-dollar negatives (`"-0.50"`) keep it,
    /// which parsing the integer part alone would lose (`"-0"` parses to `0`).
    pub fn money_from_decimal(text: &str) -> Value {
        let text = text.trim();
        let negative = text.starts_with('-');
        let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
        let whole: i64 = whole.parse().unwrap_or(0);
        let mut cents_str = frac.to_string();
        cents_str.truncate(2);
        while cents_str.len() < 2 {
            cents_str.push('0');
        }
        let cents: i64 = cents_str.parse().unwrap_or(0);
        let magnitude = whole.wrapping_abs().wrapping_mul(100).wrapping_add(cents);
        Value::Money(if negative { magnitude.wrapping_neg() } else { magnitude })
    }

    /// Numeric value on a common scale (cents): `Int` counts are promoted to
    /// whole units so `Money > 30` reads as "$30". Returns `None` if non-numeric.
    pub fn as_cents(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(n.wrapping_mul(100)),
            Value::Money(c) => Some(*c),
            _ => None,
        }
    }

    /// The same common scale, widened so it cannot overflow: what a
    /// *comparison* uses. On the wrapping `i64` scale, `i64::MIN + 2` whole
    /// units is 200 cents and would equal `2`.
    pub fn as_cents_exact(&self) -> Option<i128> {
        match self {
            Value::Int(n) => Some(*n as i128 * 100),
            Value::Money(c) => Some(*c as i128),
            _ => None,
        }
    }

    /// Raw numeric magnitude (Int as-is, Money as cents) for arithmetic that
    /// stays within one type.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            Value::Money(c) => Some(*c),
            _ => None,
        }
    }

}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Unit => write!(f, "()"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Money(c) => {
                let sign = if *c < 0 { "-" } else { "" };
                let c = c.unsigned_abs();
                write!(f, "{sign}${}.{:02}", c / 100, c % 100)
            }
            Value::Text(s) => write!(f, "{:?}", s.as_str()),
            Value::Date { year, month, day } => write!(f, "{year:04}-{month:02}-{day:02}"),
            Value::Id(sort, n) => write!(f, "#{}:{n}", sort.0),
            Value::Atom(a) => write!(f, "@{a}"),
            Value::Pair(a, b) => write!(f, "({a}, {b})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_parse_keeps_sign_for_sub_dollar_negatives() {
        assert_eq!(Value::money_from_decimal("9.99"), Value::Money(999));
        assert_eq!(Value::money_from_decimal("-1.50"), Value::Money(-150));
        // The integer part parses as `0`, so the sign must come from the literal.
        assert_eq!(Value::money_from_decimal("-0.50"), Value::Money(-50));
    }

    #[test]
    fn money_display_shows_sign() {
        assert_eq!(Value::Money(999).to_string(), "$9.99");
        assert_eq!(Value::Money(-50).to_string(), "-$0.50");
        assert_eq!(Value::Money(-150).to_string(), "-$1.50");
    }
}
