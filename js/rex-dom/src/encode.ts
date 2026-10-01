/**
 * The JS side of Rex's canonical value encoding (mirror of
 * `rex-core/src/eval/encode.rs`). Values cross the engine boundary as these
 * strings, and the shaper keys its maps on them, so apps must produce exactly
 * this format when writing base-table facts. Keeping the scheme here — beside
 * `parseStepJson` — rather than hand-rolled per app means one definition that
 * can't silently drift from the Rust grammar.
 *
 * Only the leaf encodings an app actually constructs are provided (Text and the
 * numeric/atom scalars); ids and composite keys are produced by the engine and
 * flow back opaquely, so an app passes those through verbatim.
 */

/** Escape the structural characters `\ , ( )` — the injective text escape. */
function escape(s: string): string {
  return s.replace(/([\\,()])/g, "\\$1");
}

function unescape(s: string): string {
  return s.replace(/\\(.)/g, "$1");
}

/** Encode a Text value: `t:<escaped>`. */
export function encodeText(s: string): string {
  return "t:" + escape(s);
}

/** Decode a `t:` Text value back to its string (passes non-text through). */
export function decodeText(v: string): string {
  return v.startsWith("t:") ? unescape(v.slice(2)) : v;
}

/** The decimal digits of a whole number, for the wire. A `number` must be
 *  a safe integer after truncation: `(1e21).toString()` is `"1e+21"` and
 *  `NaN` is `"NaN"`, neither of which the engine reads, and past 2^53 a
 *  number is no longer the integer it looks like. Pass a `bigint` for those. */
function digits(n: number | bigint, what: string): string {
  if (typeof n === "bigint") return n.toString();
  const whole = Math.trunc(n);
  if (!Number.isSafeInteger(whole)) {
    throw new RangeError(`${what}: ${n} is not a safe integer (pass a bigint for values past 2^53)`);
  }
  return (whole === 0 ? 0 : whole).toString(); // -0 is 0
}

/** Encode an Int value: `i:<n>`. A fraction truncates toward zero. */
export function encodeInt(n: number | bigint): string {
  return "i:" + digits(n, "encodeInt");
}

/** Encode a Money value in minor units (cents): `m:<cents>`. */
export function encodeMoney(cents: number | bigint): string {
  return "m:" + digits(cents, "encodeMoney");
}

/** Encode a scalar atom: `@<escaped>`. */
export function encodeAtom(name: string): string {
  return "@" + escape(name);
}

/** Encode a JS array as the relation `Int -> T`, keyed by index (0-based):
 *  the wire shape of a relation-typed event arg, `[[key, value, weight], …]`. */
export function encodeRel<T>(values: readonly T[], enc: (v: T) => string): [string, string, number][] {
  return values.map((v, i) => [encodeInt(i), enc(v), 1]);
}

/** A decoded whole number: a `number` when that is exact, a `bigint` past
 *  2^53 (the engine's integers are 64-bit). `String(…)` of either is the
 *  exact digits, which is all a text bind needs. */
function whole(text: string): number | bigint {
  const n = Number(text);
  if (Number.isSafeInteger(n) || !/^-?\d+$/.test(text)) return n;
  return BigInt(text);
}

/** Decode an `i:` Int value (passes non-`i:` through as-is). */
export function decodeInt(v: string): number | bigint {
  return whole(v.startsWith("i:") ? v.slice(2) : v);
}

/** Decode an `m:` Money value back to its minor units (cents). */
export function decodeMoney(v: string): number | bigint {
  return whole(v.startsWith("m:") ? v.slice(2) : v);
}

/** Decode an `@` atom value back to its bare (unescaped) name, for display as text. */
export function decodeAtom(v: string): string {
  return v.startsWith("@") ? unescape(v.slice(1)) : v;
}
