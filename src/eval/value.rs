//! Runtime domain elements. A `Value` is one point in the value algebra (§2),
//! concrete enough to store in a Z-set. `Ord + Hash` let it key a `BTreeMap`.
//!
//! `Money` is stored as an integer number of minor units (cents) to stay exact.

use crate::types::ty::SortId;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    Unit,
    Int(i64),
    /// Money in minor units (cents).
    Money(i64),
    Text(String),
    Date { year: i32, month: u32, day: u32 },
    /// An entity id: which sort, and a per-sort sequence number.
    Id(SortId, u64),
    Atom(String),
    Pair(Box<Value>, Box<Value>),
}

impl Value {
    /// Parse a decimal literal like `"9.99"` into money cents (`999`). Extra
    /// fractional digits are truncated; missing ones are padded.
    pub fn money_from_decimal(text: &str) -> Value {
        let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
        let whole: i64 = whole.parse().unwrap_or(0);
        let mut cents_str = frac.to_string();
        cents_str.truncate(2);
        while cents_str.len() < 2 {
            cents_str.push('0');
        }
        let cents: i64 = cents_str.parse().unwrap_or(0);
        let sign = if whole < 0 { -1 } else { 1 };
        Value::Money(whole * 100 + sign * cents)
    }

    /// Numeric value on a common scale (cents): `Int` counts are promoted to
    /// whole units so `Money > 30` reads as "$30". Returns `None` if non-numeric.
    pub fn as_cents(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(n * 100),
            Value::Money(c) => Some(*c),
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

    pub fn is_money(&self) -> bool {
        matches!(self, Value::Money(_))
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Unit => write!(f, "()"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Money(c) => write!(f, "${}.{:02}", c / 100, (c % 100).abs()),
            Value::Text(s) => write!(f, "{s:?}"),
            Value::Date { year, month, day } => write!(f, "{year:04}-{month:02}-{day:02}"),
            Value::Id(sort, n) => write!(f, "#{}:{n}", sort.0),
            Value::Atom(a) => write!(f, "@{a}"),
            Value::Pair(a, b) => write!(f, "({a}, {b})"),
        }
    }
}
