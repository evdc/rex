// DOM-layer helper for the benchmark (`import js "./utils.js" as utils`).
// Labels are made client-side because they are random: the array crosses into
// the engine as a relation-valued event arg, so it is logged with the event
// and a replay never re-randomises.

const adjectives = [
  "pretty", "large", "big", "small", "tall", "short", "long", "handsome", "plain",
  "quaint", "clean", "elegant", "easy", "angry", "crazy", "helpful", "mushy", "odd",
  "unsightly", "adorable", "important", "inexpensive", "cheap", "expensive", "fancy",
];
const colours = [
  "red", "yellow", "blue", "green", "pink", "brown", "purple", "brown",
  "white", "black", "orange",
];
const nouns = [
  "table", "chair", "house", "bbq", "desk", "car", "pony", "cookie",
  "sandwich", "burger", "pizza", "mouse", "keyboard",
];

const pick = <T>(xs: readonly T[]): T => xs[Math.floor(Math.random() * xs.length)]!;

export function randomLabels(n: number): string[] {
  return Array.from({ length: n }, () => `${pick(adjectives)} ${pick(colours)} ${pick(nouns)}`);
}
