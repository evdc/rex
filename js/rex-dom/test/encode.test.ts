import { describe, expect, test } from "vitest";
import {
  decodeAtom,
  decodeInt,
  decodeMoney,
  decodeText,
  encodeAtom,
  encodeInt,
  encodeMoney,
  encodeRel,
  encodeText,
} from "../src/encode.js";

describe("canonical encoding (mirror of rex-core encode.rs)", () => {
  test("text round-trips through encode/decode", () => {
    for (const s of ["hello", "", "a,b(c)d\\e", "p(i:1,i:2)"]) {
      expect(decodeText(encodeText(s))).toBe(s);
    }
  });

  test("text escapes exactly the structural chars", () => {
    expect(encodeText("a,b")).toBe("t:a\\,b");
    expect(encodeText("(x)")).toBe("t:\\(x\\)");
    expect(encodeText("back\\slash")).toBe("t:back\\\\slash");
  });

  test("decodeText passes non-text through unchanged", () => {
    expect(decodeText("#1:0")).toBe("#1:0");
    expect(decodeText("i:5")).toBe("i:5");
  });

  test("scalar encoders match the grammar", () => {
    expect(encodeInt(42)).toBe("i:42");
    expect(encodeInt(-3)).toBe("i:-3");
    expect(encodeMoney(999)).toBe("m:999");
    expect(encodeAtom("west")).toBe("@west");
  });

  test("scalar decoders round-trip their encoders", () => {
    expect(decodeInt(encodeInt(42))).toBe(42);
    expect(decodeInt(encodeInt(-3))).toBe(-3);
    expect(decodeMoney(encodeMoney(999))).toBe(999);
    expect(decodeAtom(encodeAtom("west"))).toBe("west");
    expect(decodeAtom(encodeAtom("a,b(c)"))).toBe("a,b(c)");
  });
});

describe("encodeRel (S-91)", () => {
  test("an array becomes the relation Int -> T keyed by 0-based index", () => {
    expect(encodeRel(["a", "b,c"], encodeText)).toEqual([
      ["i:0", "t:a", 1],
      ["i:1", "t:b\\,c", 1],
    ]);
    expect(encodeRel([], encodeText)).toEqual([]);
  });
});
