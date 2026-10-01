import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
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

/**
 * The wire encoding, held to the engine's. `crates/rex-core/tests/encode_props.rs`
 * writes `fixtures/encoding.json` from the Rust encoder; the helpers here must
 * produce exactly those strings and read them back. A disagreement is silent
 * otherwise: the engine would store a different text than the user typed, or
 * the shaper would key a row under a string the engine never sends.
 */
const fixture = JSON.parse(
  readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../../crates/rex-core/tests/fixtures/encoding.json"), "utf8"),
) as Record<"text" | "atom" | "int" | "money", [string, string][]>;

describe("the JS encoders agree with the engine", () => {
  test.each(fixture.text)("text %j", (plain, encoded) => {
    expect(encodeText(plain)).toBe(encoded);
    expect(decodeText(encoded)).toBe(plain);
  });

  test.each(fixture.atom)("atom %j", (plain, encoded) => {
    expect(encodeAtom(plain)).toBe(encoded);
    expect(decodeAtom(encoded)).toBe(plain);
  });

  test.each(fixture.int)("int %s", (digits, encoded) => {
    // Every i64 goes in and comes out exactly, including those past 2^53.
    expect(encodeInt(BigInt(digits))).toBe(encoded);
    expect(String(decodeInt(encoded))).toBe(digits);
    if (Number.isSafeInteger(Number(digits))) {
      expect(encodeInt(Number(digits))).toBe(encoded);
      expect(decodeInt(encoded)).toBe(Number(digits));
    }
  });

  test.each(fixture.money)("money %s", (digits, encoded) => {
    expect(encodeMoney(BigInt(digits))).toBe(encoded);
    expect(String(decodeMoney(encoded))).toBe(digits);
  });
});

/** A small deterministic generator, so a failure names its seed. */
function rng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

describe("round trips", () => {
  const alphabet = ["\\", ",", "(", ")", '"', "t:", "@", "#", " ", "a", "é", "😀", " ", "\n", "\0", ":", "i:3"];

  test("random text made of the encoding's own punctuation", () => {
    for (let seed = 1; seed <= 300; seed++) {
      const next = rng(seed);
      const len = Math.floor(next() * 12);
      const s = Array.from({ length: len }, () => alphabet[Math.floor(next() * alphabet.length)]).join("");
      expect(decodeText(encodeText(s)), `seed ${seed}`).toBe(s);
      expect(decodeAtom(encodeAtom(s)), `seed ${seed}`).toBe(s);
      // An encoded text never contains a bare structural character, so it can
      // sit inside a pair without ending it.
      expect(encodeText(s).slice(2).replace(/\\./gs, ""), `seed ${seed}`).not.toMatch(/[,()\\]/);
    }
  });

  test("distinct texts encode distinctly", () => {
    const seen = new Map<string, string>();
    for (const a of alphabet) {
      for (const b of alphabet) {
        const s = a + b;
        const enc = encodeText(s);
        expect(seen.get(enc) ?? s).toBe(s);
        seen.set(enc, s);
      }
    }
  });
});

describe("integers a JS number cannot hold", () => {
  test("a number that is not a whole, finite, safe integer is refused, not mangled", () => {
    // `(1e21).toString()` is "1e+21", which the engine cannot read.
    for (const bad of [NaN, Infinity, -Infinity, 1e21, 2 ** 53, -(2 ** 53) - 2]) {
      expect(() => encodeInt(bad), String(bad)).toThrow(RangeError);
      expect(() => encodeMoney(bad), String(bad)).toThrow(RangeError);
    }
  });

  test("a fractional number truncates toward zero", () => {
    expect(encodeInt(3.9)).toBe("i:3");
    expect(encodeInt(-3.9)).toBe("i:-3");
    expect(encodeInt(-0)).toBe("i:0");
  });

  test("a value past 2^53 decodes to a bigint, exactly", () => {
    expect(decodeInt("i:9007199254740993")).toBe(9007199254740993n);
    expect(decodeInt("i:42")).toBe(42);
    expect(decodeMoney("m:-9223372036854775808")).toBe(-9223372036854775808n);
  });

  test("encodeRel keys rows by index", () => {
    expect(encodeRel(["a", "b,c"], encodeText)).toEqual([
      ["i:0", "t:a", 1],
      ["i:1", "t:b\\,c", 1],
    ]);
    expect(encodeRel([], encodeText)).toEqual([]);
  });
});
