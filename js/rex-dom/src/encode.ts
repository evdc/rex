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

/** Encode an Int value: `i:<n>`. */
export function encodeInt(n: number): string {
  return "i:" + Math.trunc(n).toString();
}

/** Encode a Money value in minor units (cents): `m:<cents>`. */
export function encodeMoney(cents: number): string {
  return "m:" + Math.trunc(cents).toString();
}

/** Encode a scalar atom: `@<escaped>`. */
export function encodeAtom(name: string): string {
  return "@" + escape(name);
}

/** Decode an `i:` Int value back to a number (passes non-`i:` through as-is). */
export function decodeInt(v: string): number {
  return v.startsWith("i:") ? Number(v.slice(2)) : Number(v);
}

/** Decode an `m:` Money value back to its minor units (cents). */
export function decodeMoney(v: string): number {
  return v.startsWith("m:") ? Number(v.slice(2)) : Number(v);
}

/** Decode an `@` atom value back to its bare (unescaped) name, for display as text. */
export function decodeAtom(v: string): string {
  return v.startsWith("@") ? unescape(v.slice(1)) : v;
}
