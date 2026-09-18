//! S-04 item 3: a non-Text bind must decode with its own `Encoding`
//! (`decodeInt`/`decodeMoney`/`decodeAtom`), not always `decodeText` — which
//! used to leave the wire tag (`i:3`, `m:999`, `@west`) in the rendered text.

const SRC: &str = r#"
entity Todo { title: Text, cnt: Int, price: Money, status: {@open | @closed} }
view board =
  Todo select
    li {
      span { .title }
      span { .cnt }
      span { .price }
      span { .status }
    }
"#;

#[test]
fn binds_decode_by_their_field_encoding() {
    let out = rex_codegen::generate(SRC, "./board.rex?raw").expect("clean codegen");
    assert!(out.contains("textContent = String(decodeText(v))"), "{out}");
    assert!(out.contains("textContent = String(decodeInt(v))"), "{out}");
    assert!(out.contains("textContent = String(decodeMoney(v))"), "{out}");
    assert!(out.contains("textContent = String(decodeAtom(v))"), "{out}");
}
