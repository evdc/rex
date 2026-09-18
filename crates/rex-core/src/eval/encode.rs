//! Canonical string encoding of [`Value`]s and JSON emission of
//! [`StepResult`]s — the wire protocol between the engine and a JS-side
//! shaper.
//!
//! The encoding is *injective* (distinct values → distinct strings) and
//! resolves interned symbols to their strings, because the shaper keys its
//! maps (nodeMap, attribute mirrors) on these strings and must never depend on
//! the engine's intern-order `Ord`. One escape scheme covers the only
//! ambiguity source: `\`, `,`, `(`, `)` inside text/atom payloads.
//!
//! Grammar:
//! ```text
//! u              Unit
//! i:42           Int
//! m:999          Money (minor units)
//! t:hello        Text (escaped)
//! d:2026-01-15   Date
//! #3:7           Id (sort 3, sequence 7)
//! @west          Atom (escaped)
//! p(<v>,<v>)     Pair (recursive)
//! ```

use super::relation::BinaryRelation;
use super::value::Value;
use crate::dbsp::{ArgValue, Event, StepResult};
use std::fmt::Write;

/// Canonically encode one value.
pub fn encode_value(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v);
    out
}

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Unit => out.push('u'),
        Value::Int(n) => {
            let _ = write!(out, "i:{n}");
        }
        Value::Money(c) => {
            let _ = write!(out, "m:{c}");
        }
        Value::Text(s) => {
            out.push_str("t:");
            write_escaped(out, s.as_str());
        }
        Value::Date { year, month, day } => {
            let _ = write!(out, "d:{year:04}-{month:02}-{day:02}");
        }
        Value::Id(sort, n) => {
            let _ = write!(out, "#{}:{n}", sort.0);
        }
        Value::Atom(a) => {
            out.push('@');
            write_escaped(out, a.as_str());
        }
        Value::Pair(a, b) => {
            out.push_str("p(");
            write_value(out, a);
            out.push(',');
            write_value(out, b);
            out.push(')');
        }
    }
}

/// Escape the structural characters so the encoding stays injective for
/// arbitrary text payloads.
fn write_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        if matches!(ch, '\\' | ',' | '(' | ')') {
            out.push('\\');
        }
        out.push(ch);
    }
}

/// Decode a canonical encoding back into a [`Value`]. The inverse of
/// [`encode_value`]; used by tests and by any host that writes values back
/// (base-table transactions from the bridge).
pub fn decode_value(s: &str) -> Option<Value> {
    let (v, rest) = parse_value(s)?;
    rest.is_empty().then_some(v)
}

fn parse_value(s: &str) -> Option<(Value, &str)> {
    if let Some(rest) = s.strip_prefix("i:") {
        let (n, rest) = take_int(rest)?;
        return Some((Value::Int(n), rest));
    }
    if let Some(rest) = s.strip_prefix("m:") {
        let (n, rest) = take_int(rest)?;
        return Some((Value::Money(n), rest));
    }
    if let Some(rest) = s.strip_prefix("t:") {
        let (text, rest) = take_escaped(rest);
        return Some((Value::text(&text), rest));
    }
    if let Some(rest) = s.strip_prefix("d:") {
        // yyyy-mm-dd with fixed field widths (as emitted above).
        let (ys, rest2) = rest.split_at_checked(4)?;
        let rest2 = rest2.strip_prefix('-')?;
        let (ms, rest2) = rest2.split_at_checked(2)?;
        let rest2 = rest2.strip_prefix('-')?;
        let (ds, rest2) = rest2.split_at_checked(2)?;
        return Some((
            Value::Date {
                year: ys.parse().ok()?,
                month: ms.parse().ok()?,
                day: ds.parse().ok()?,
            },
            rest2,
        ));
    }
    if let Some(rest) = s.strip_prefix('#') {
        let colon = rest.find(':')?;
        let sort: usize = rest[..colon].parse().ok()?;
        let (n, rest) = take_int(&rest[colon + 1..])?;
        return Some((Value::Id(crate::types::ty::SortId(sort), u64::try_from(n).ok()?), rest));
    }
    if let Some(rest) = s.strip_prefix('@') {
        let (name, rest) = take_escaped(rest);
        return Some((Value::atom(&name), rest));
    }
    if let Some(rest) = s.strip_prefix("p(") {
        let (a, rest) = parse_value(rest)?;
        let rest = rest.strip_prefix(',')?;
        let (b, rest) = parse_value(rest)?;
        let rest = rest.strip_prefix(')')?;
        return Some((Value::Pair(Box::new(a), Box::new(b)), rest));
    }
    if let Some(rest) = s.strip_prefix('u') {
        return Some((Value::Unit, rest));
    }
    None
}

/// Parse a (possibly signed) integer prefix, returning it and the remainder.
fn take_int(s: &str) -> Option<(i64, &str)> {
    let end = s
        .char_indices()
        .take_while(|&(i, c)| c.is_ascii_digit() || (i == 0 && c == '-'))
        .map(|(i, c)| i + c.len_utf8())
        .last()?;
    Some((s[..end].parse().ok()?, &s[end..]))
}

/// Unescape up to the first unescaped structural character.
fn take_escaped(s: &str) -> (String, &str) {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, ch)) = chars.next() {
        match ch {
            '\\' => {
                if let Some((_, esc)) = chars.next() {
                    out.push(esc);
                }
            }
            ',' | '(' | ')' => return (out, &s[i..]),
            _ => out.push(ch),
        }
    }
    (out, "")
}

/// Emit one step's view deltas as JSON:
/// `{"views":{"<name>":[["<key>","<value>",<weight>],...],...}}`.
/// Views whose delta is empty are omitted (quiescent views are noise to a
/// per-step consumer).
pub fn step_result_to_json(res: &StepResult) -> String {
    let mut names: Vec<&String> =
        res.view_deltas.iter().filter(|(_, d)| !d.is_empty()).map(|(n, _)| n).collect();
    names.sort();

    let mut out = String::from("{\"views\":{");
    for (vi, name) in names.iter().enumerate() {
        if vi > 0 {
            out.push(',');
        }
        json_string(&mut out, name);
        out.push(':');
        append_rows_json(&mut out, &res.view_deltas[*name]);
    }
    out.push_str("}}");
    out
}

/// Canonical JSON encoding of one logged [`Event`] (S-21): `{"seq":0,
/// "name":"MoveCard","args":{"card":"#1:0",...},"cause":null,"intent":null}`.
/// A relation-valued arg (S-42) encodes as the same row-array shape
/// `rows_to_json` uses for a view delta. The wire format a persistence
/// adapter appends and a `log_since`/`replay` call (S-22) crosses the wasm
/// boundary with.
pub fn event_to_json(e: &Event) -> String {
    let mut out = String::new();
    out.push_str("{\"seq\":");
    let _ = write!(out, "{}", e.seq);
    out.push_str(",\"name\":");
    json_string(&mut out, &e.name);
    out.push_str(",\"args\":{");
    for (i, (name, arg)) in e.args.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_string(&mut out, name);
        out.push(':');
        match arg {
            ArgValue::Value(v) => json_string(&mut out, &encode_value(v)),
            ArgValue::Rel(rows) => {
                out.push('[');
                for (ri, (l, r, w)) in rows.iter().enumerate() {
                    if ri > 0 {
                        out.push(',');
                    }
                    out.push('[');
                    json_string(&mut out, &encode_value(l));
                    out.push(',');
                    json_string(&mut out, &encode_value(r));
                    let _ = write!(out, ",{w}]");
                }
                out.push(']');
            }
        }
    }
    out.push_str("},\"cause\":");
    match e.cause {
        Some(c) => {
            let _ = write!(out, "{c}");
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"intent\":");
    match &e.intent {
        Some(i) => json_string(&mut out, i),
        None => out.push_str("null"),
    }
    out.push('}');
    out
}

/// One relation's rows as a JSON array `[["<key>","<value>",<weight>],...]` —
/// the shared row-encoding used by both the per-step delta stream and the
/// `read_view` escape hatch, so the two can't drift.
pub fn rows_to_json(rel: &dyn BinaryRelation) -> String {
    let mut out = String::new();
    append_rows_json(&mut out, rel);
    out
}

fn append_rows_json(out: &mut String, rel: &dyn BinaryRelation) {
    out.push('[');
    for (i, (l, r, w)) in rel.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        json_string(out, &encode_value(&l));
        out.push(',');
        json_string(out, &encode_value(&r));
        let _ = write!(out, ",{w}]");
    }
    out.push(']');
}

/// A canonically-encoded value as a JSON string literal (quotes + escapes) —
/// for hosts embedding a single encoded value in JSON they assemble by hand.
pub fn json_quote(s: &str) -> String {
    let mut out = String::new();
    json_string(&mut out, s);
    out
}

/// Append a JSON string literal (quotes, escapes) to `out`.
fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ty::SortId;

    fn roundtrip(v: Value) {
        let enc = encode_value(&v);
        assert_eq!(decode_value(&enc), Some(v), "through {enc:?}");
    }

    #[test]
    fn scalar_roundtrips() {
        roundtrip(Value::Unit);
        roundtrip(Value::Int(-42));
        roundtrip(Value::Money(999));
        roundtrip(Value::text("hello"));
        roundtrip(Value::Date { year: 2026, month: 1, day: 15 });
        roundtrip(Value::Id(SortId(3), 7));
        roundtrip(Value::atom("west"));
    }

    #[test]
    fn hostile_text_roundtrips() {
        roundtrip(Value::text("a,b(c)d\\e"));
        roundtrip(Value::text("p(i:1,i:2)")); // text that mimics the grammar
        roundtrip(Value::text(""));
    }

    #[test]
    fn nested_pairs_roundtrip() {
        let v = Value::Pair(
            Box::new(Value::Pair(
                Box::new(Value::Id(SortId(0), 1)),
                Box::new(Value::text("x,y")),
            )),
            Box::new(Value::Int(5)),
        );
        assert_eq!(encode_value(&v), "p(p(#0:1,t:x\\,y),i:5)");
        roundtrip(v);
    }

    #[test]
    fn injectivity_on_lookalikes() {
        // A text value spelling out a pair encoding must not collide with the
        // pair itself.
        let fake = Value::text("p(i:1,i:2)");
        let real = Value::Pair(Box::new(Value::Int(1)), Box::new(Value::Int(2)));
        assert_ne!(encode_value(&fake), encode_value(&real));
    }

    #[test]
    fn event_json_shape() {
        use crate::dbsp::Event;

        let e = Event {
            seq: 3,
            name: "MoveCard".to_string(),
            args: vec![
                ("card".to_string(), ArgValue::Value(Value::Id(SortId(1), 0))),
                ("pos".to_string(), ArgValue::Value(Value::text("a5"))),
            ],
            cause: None,
            intent: None,
        };
        assert_eq!(
            event_to_json(&e),
            r##"{"seq":3,"name":"MoveCard","args":{"card":"#1:0","pos":"t:a5"},"cause":null,"intent":null}"##
        );
    }

    #[test]
    fn step_result_json_shape() {
        use crate::dbsp::StepResult;
        use crate::eval::relation::{BTreeRelation, BinaryRelation};

        let mut res = StepResult::default();
        let mut d = BTreeRelation::new();
        d.add(Value::Id(SortId(1), 0), Value::text("Todo"), 1);
        res.view_deltas.insert("card_title".into(), d);
        res.view_deltas.insert("quiet".into(), BTreeRelation::new());

        assert_eq!(
            step_result_to_json(&res),
            r##"{"views":{"card_title":[["#1:0","t:Todo",1]]}}"##
        );
    }
}
