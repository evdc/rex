//! The wire: the canonical value encoding and the JSON the engine emits
//! around it. Everything that crosses the wasm boundary, and everything
//! persisted, is one of these strings — so they must round-trip any value a
//! program can hold, including text made of the encoding's own punctuation.
//!
//! Also writes `tests/fixtures/encoding.json`, which
//! `js/rex-dom/test/encoding.test.ts` reads: the JS helpers must produce and
//! accept exactly these strings (`UPDATE_FIXTURES=1` regenerates it).

mod common;

use proptest::prelude::*;
use rex::dbsp::{ArgValue, Event};
use rex::eval::{decode_value, encode_value, event_to_json, Value};
use rex::types::ty::SortId;
use std::collections::HashMap;

/// Text chosen to break an encoding: its escape character, its separators,
/// JSON's, lookalikes of the other tags, and code points that need more than
/// one UTF-8 byte or more than one UTF-16 unit.
fn hostile_text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[\\\\,()\"':@#tipmdu0-9 \n\t\r]{0,12}",
        2 => "\\PC{0,16}",
        1 => proptest::char::any().prop_map(|c| c.to_string()),
        1 => prop::sample::select(common::TEXTS).prop_map(str::to_string),
        1 => Just("\u{0}\u{1}\u{1f}\u{7f}\u{2028}\u{2029}\u{feff}\u{10ffff}".to_string()),
    ]
}

fn value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Unit),
        any::<i64>().prop_map(Value::Int),
        any::<i64>().prop_map(Value::Money),
        hostile_text().prop_map(|s| Value::text(&s)),
        hostile_text().prop_map(|s| Value::atom(&s)),
        (0i32..=9999, 0u32..=99, 0u32..=99).prop_map(|(year, month, day)| Value::Date { year, month, day }),
        (0usize..1000, any::<u32>()).prop_map(|(s, n)| Value::Id(SortId(s), n as u64)),
        Just(Value::Id(SortId(0), i64::MAX as u64)),
    ];
    leaf.prop_recursive(5, 24, 2, |inner| (inner.clone(), inner).prop_map(|(a, b)| Value::Pair(Box::new(a), Box::new(b))))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: common::cases(2000), ..ProptestConfig::default() })]

    #[test]
    fn every_value_round_trips(v in value()) {
        let enc = encode_value(&v);
        prop_assert_eq!(decode_value(&enc), Some(v), "through {:?}", enc);
    }

    /// Distinct values never share an encoding — the shaper keys its maps on
    /// these strings, so a collision would merge two rows.
    #[test]
    fn the_encoding_is_injective(a in value(), b in value()) {
        prop_assert_eq!(encode_value(&a) == encode_value(&b), a == b);
    }

    /// A host can send anything. Decoding it never panics, and whatever does
    /// decode is a value that re-encodes to something that decodes the same.
    #[test]
    fn decoding_arbitrary_text_is_total(s in "\\PC{0,40}") {
        if let Some(v) = decode_value(&s) {
            prop_assert_eq!(decode_value(&encode_value(&v)), Some(v));
        }
    }

    /// The same, biased toward almost-valid encodings.
    #[test]
    fn decoding_near_encodings_is_total(s in "(i:|m:|t:|d:|#|@|p\\(|u)?[-0-9a-c:,()\\\\#@pu ]{0,24}") {
        if let Some(v) = decode_value(&s) {
            prop_assert_eq!(decode_value(&encode_value(&v)), Some(v));
        }
    }

    /// Truncating or extending a valid encoding never panics the decoder.
    #[test]
    fn decoding_damaged_encodings_is_total(v in value(), cut in any::<prop::sample::Index>(), tail in "[,()\\\\]{0,3}") {
        let enc = encode_value(&v);
        let mut at = cut.index(enc.len() + 1);
        while !enc.is_char_boundary(at) {
            at -= 1;
        }
        let _ = decode_value(&enc[..at]);
        let _ = decode_value(&format!("{enc}{tail}"));
        let _ = decode_value(&format!("{}{tail}", &enc[..at]));
    }

    /// A logged event is valid JSON whatever its args hold, and says what the
    /// event said: this is the format persisted to IndexedDB and replayed.
    #[test]
    fn a_logged_event_is_json_that_says_what_the_event_said(
        seq in any::<u32>(),
        name in hostile_text(),
        scalars in prop::collection::vec((hostile_text(), value()), 0..4),
        rows in prop::collection::vec((value(), value(), -3i64..4), 0..5),
        cause in prop::option::of(any::<u32>()),
        intent in prop::option::of(hostile_text()),
    ) {
        // Arg names are unique in a real event; keep the last of any repeat.
        let scalars: HashMap<String, Value> = scalars.into_iter().collect();
        let mut args: Vec<(String, ArgValue)> = scalars.iter().map(|(k, v)| (k.clone(), ArgValue::Value(v.clone()))).collect();
        if !scalars.contains_key("rows") {
            args.push(("rows".to_string(), ArgValue::Rel(rows.clone())));
        }
        let event = Event { seq: seq as u64, name: name.clone(), args: args.clone(), cause: cause.map(u64::from), intent: intent.clone() };
        let json: serde_json::Value = serde_json::from_str(&event_to_json(&event)).map_err(|e| TestCaseError::fail(format!("not JSON: {e}")))?;
        prop_assert_eq!(json["seq"].as_u64(), Some(seq as u64));
        prop_assert_eq!(json["name"].as_str(), Some(name.as_str()));
        prop_assert_eq!(json["cause"].as_u64(), cause.map(u64::from));
        prop_assert_eq!(json["intent"].as_str(), intent.as_deref());
        prop_assert_eq!(json["args"].as_object().map(|o| o.len()), Some(args.len()));
        for (k, arg) in &args {
            match arg {
                ArgValue::Value(v) => {
                    let got = json["args"][k].as_str().and_then(decode_value);
                    prop_assert_eq!(got.as_ref(), Some(v), "arg {:?}", k);
                }
                ArgValue::Rel(rows) => {
                    let got: Vec<(Value, Value, i64)> = json["args"][k]
                        .as_array()
                        .expect("a row array")
                        .iter()
                        .map(|r| (decode_value(r[0].as_str().unwrap()).unwrap(), decode_value(r[1].as_str().unwrap()).unwrap(), r[2].as_i64().unwrap()))
                        .collect();
                    prop_assert_eq!(&got, rows);
                }
            }
        }
    }
}

/// A live engine's step deltas and base snapshot are valid JSON, with hostile
/// text in the data and in between.
#[test]
fn step_and_snapshot_json_parse_with_hostile_text_in_them() {
    let src = "entity T { s: Text, n: Int }\nevent Add(s: Text, n: Int)\non Add(s, n) => new T { s: s, n: n }\nlet all : T -> Text = .s\n";
    let mut app = common::App::build(src);
    for (i, s) in common::TEXTS.iter().enumerate() {
        let (_, step) = app.dispatch("Add", &[("s", Value::text(s)), ("n", Value::Int(common::INTS[i % common::INTS.len()]))]).unwrap();
        let json: serde_json::Value = serde_json::from_str(&rex::eval::step_result_to_json(&step)).expect("step JSON");
        let row = &json["views"]["all"][0];
        assert_eq!(decode_value(row[1].as_str().unwrap()), Some(Value::text(s)), "{s:?}");
    }
    let snap: serde_json::Value = serde_json::from_str(&rex::eval::base_snapshot_to_json(&app.engine.base_snapshot())).expect("snapshot JSON");
    assert_eq!(snap["cursor"].as_u64(), Some(common::TEXTS.len() as u64));
    let rows: usize = snap["inputs"].as_array().unwrap().iter().map(|i| i["rows"].as_array().unwrap().len()).sum();
    assert_eq!(rows, 3 * common::TEXTS.len(), "an identity row and two fields per row");
    for e in app.engine.log() {
        serde_json::from_str::<serde_json::Value>(&event_to_json(e)).expect("event JSON");
    }
}

/// The cross-language fixture: values and the exact strings they encode to.
#[test]
fn the_encoding_fixture_is_current() {
    let mut texts: Vec<String> = common::TEXTS.iter().map(|s| s.to_string()).collect();
    texts.extend(["a\\,b(c)d", "\\\\", "((", "))", ",,", "naïve café", "日本語", "𝒳 😀 🏳️‍🌈", "tab\there", "line\nbreak", "\u{2028}\u{2029}", "\u{feff}bom", " lead and trail "].map(String::from));
    let ints = [0i64, 1, -1, 42, 1000, i64::MAX, i64::MIN, 9_007_199_254_740_991, 9_007_199_254_740_993, -9_007_199_254_740_993];
    let quote = |s: &str| serde_json::to_string(s).unwrap();
    let mut out = String::from("{\n  \"text\": [\n");
    out += &texts.iter().map(|s| format!("    [{}, {}]", quote(s), quote(&encode_value(&Value::text(s))))).collect::<Vec<_>>().join(",\n");
    out += "\n  ],\n  \"atom\": [\n";
    out += &["True", "False", "All", "a b", "x,y", "(", "\\"].iter().map(|s| format!("    [{}, {}]", quote(s), quote(&encode_value(&Value::atom(s))))).collect::<Vec<_>>().join(",\n");
    // Integers as decimal strings: JSON numbers cannot carry an i64.
    out += "\n  ],\n  \"int\": [\n";
    out += &ints.iter().map(|n| format!("    [\"{n}\", {}]", quote(&encode_value(&Value::Int(*n))))).collect::<Vec<_>>().join(",\n");
    out += "\n  ],\n  \"money\": [\n";
    out += &ints.iter().map(|n| format!("    [\"{n}\", {}]", quote(&encode_value(&Value::Money(*n))))).collect::<Vec<_>>().join(",\n");
    out += "\n  ]\n}\n";

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/encoding.json");
    if std::env::var_os("UPDATE_FIXTURES").is_some() {
        std::fs::write(&path, &out).unwrap();
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(on_disk, out, "tests/fixtures/encoding.json is stale; rerun with UPDATE_FIXTURES=1");
}
