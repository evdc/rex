import { describe, expect, test } from "vitest";
import { decodeText, encodeAtom, encodeInt, encodeMoney, encodeText } from "../src/encode.js";

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
});
