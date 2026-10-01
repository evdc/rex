import { describe, expect, test } from "vitest";
import { SpyDriver, type SpyEl } from "../src/driver.js";
import { compareEncoded } from "../src/order.js";
import { Shaper } from "../src/shaper.js";
import type { ShapeNode, StepDeltas, Tuple } from "../src/types.js";

/**
 * Model-based fuzz of the shaper.
 *
 * The model is a relational state: every view the shape tree names, as a
 * `key -> value` map. A step changes the state at random and hands the shaper
 * exactly the delta between the two states (−old/+new rows, shuffled). After
 * every step:
 *
 *  1. the DOM equals a from-scratch render of the new state — structure,
 *     order, text and attributes;
 *  2. every row shown before and after is the *same element* (the shaper's
 *     reason to exist: a change is never a remove plus a mount).
 *
 * A row is shown iff its membership row exists and its parent is shown, all
 * the way up. The states include what an engine really produces and a tidy
 * test would not: children whose parent is not in the parent level (a nested
 * `select` under a filtered or gated level), parents that leave and come
 * back while their children's membership never changes, several levels
 * sharing one element with static markup between them, and batches that do
 * many of these at once.
 */

type View = Map<string, string>;
type State = Record<string, View>;

const VIEWS = [
  "list", "list_pos", "list_title",
  "card", "card_pos", "card_title", "card_done",
  "badge", "note", "note_text",
  "tag", "tag_name",
] as const;

// --- the shape tree ------------------------------------------------------------
// section[ h2, ul[cards…], <badge>, <notes…>, footer ]   card: li[ span, <tags…> ]

function shapes(listDesc: boolean): ShapeNode<SpyEl> {
  const tag: ShapeNode<SpyEl> = {
    name: "tag",
    membershipView: "tag",
    template: (d) => d.createElement("b"),
    attrs: [{ view: "tag_name", apply: (d, el, v) => d.setText(el, v) }],
    children: [],
  };
  const card: ShapeNode<SpyEl> = {
    name: "card",
    membershipView: "card",
    slot: (root) => root.children[1]!,
    slotKey: "1",
    template: (d) => {
      const li = d.createElement("li");
      d.insertBefore(li, d.createElement("span"), null);
      return li;
    },
    attrs: [
      { view: "card_title", apply: (d, el, v) => d.setText(el.children[0]!, v) },
      { view: "card_done", presence: true, apply: (d, el, v) => d.setAttr(el, "done", v === undefined ? "no" : "yes") },
    ],
    orderView: "card_pos",
    children: [tag],
  };
  // A gate: its key is the list's own key, present while the list is flagged.
  const badge: ShapeNode<SpyEl> = {
    name: "badge",
    membershipView: "badge",
    anchor: (root) => root.children[2] ?? null,
    template: (d) => d.createElement("mark"),
    attrs: [],
    children: [],
  };
  const note: ShapeNode<SpyEl> = {
    name: "note",
    membershipView: "note",
    anchor: (root) => root.children[2] ?? null,
    template: (d) => d.createElement("p"),
    attrs: [{ view: "note_text", apply: (d, el, v) => d.setText(el, v) }],
    children: [],
  };
  return {
    name: "list",
    membershipView: "list",
    template: (d) => {
      const section = d.createElement("section");
      for (const t of ["h2", "ul", "footer"]) d.insertBefore(section, d.createElement(t), null);
      return section;
    },
    attrs: [{ view: "list_title", apply: (d, el, v) => d.setText(el.children[0]!, v) }],
    orderView: "list_pos",
    orderDesc: listDesc,
    children: [card, badge, note],
  };
}

// --- the reference renderer ------------------------------------------------------

interface Rendered {
  tag: string;
  text: string;
  attrs: Record<string, string>;
  children: Rendered[];
  /** `level/key` for a row's root element, for the identity check. */
  row?: string;
}

const el = (tag: string, children: Rendered[] = [], text = "", attrs: Record<string, string> = {}): Rendered => ({
  tag, text, attrs, children,
});

/** Children of `parent` at a level, in display order. */
function rows(s: State, member: string, parent: string | null, order?: string, desc = false): string[] {
  const keys = [...s[member]!].filter(([, p]) => parent === null || p === parent).map(([k]) => k);
  const ord = (k: string) => (order ? s[order]!.get(k) ?? "" : "");
  return keys.sort((a, b) => {
    const c = compareEncoded(ord(a), ord(b));
    return (desc ? -c : c) || compareEncoded(a, b);
  });
}

function render(s: State, listDesc: boolean): Rendered[] {
  return rows(s, "list", null, "list_pos", listDesc).map((l) => {
    const cards = rows(s, "card", l, "card_pos").map((c) => {
      const tags = rows(s, "tag", c).map((t) => ({ ...el("b", [], s.tag_name!.get(t) ?? ""), row: `tag/${t}` }));
      return {
        ...el("li", [el("span", [], s.card_title!.get(c) ?? ""), ...tags], "", { done: s.card_done!.has(c) ? "yes" : "no" }),
        row: `card/${c}`,
      };
    });
    const badge = rows(s, "badge", l).map((b) => ({ ...el("mark"), row: `badge/${b}` }));
    const notes = rows(s, "note", l).map((n) => ({ ...el("p", [], s.note_text!.get(n) ?? ""), row: `note/${n}` }));
    return {
      ...el("section", [el("h2", [], s.list_title!.get(l) ?? ""), el("ul", cards), ...badge, ...notes, el("footer")]),
      row: `list/${l}`,
    };
  });
}

/** The DOM as the reference would print it, plus each shown row's element. */
function observe(shaper: Shaper<SpyEl>, root: SpyEl, expected: Rendered[]) {
  const strip = (e: SpyEl): Rendered => ({ tag: e.tag, text: e.text, attrs: { ...e.attrs }, children: e.children.map(strip) });
  const want = (r: Rendered): Rendered => ({ tag: r.tag, text: r.text, attrs: r.attrs, children: r.children.map(want) });
  const ids = new Map<string, number>();
  const walk = (r: Rendered) => {
    if (r.row) {
      const [level, key] = [r.row.slice(0, r.row.indexOf("/")), r.row.slice(r.row.indexOf("/") + 1)];
      const node = shaper.el(level, key);
      if (node) ids.set(r.row, node.id);
    }
    r.children.forEach(walk);
  };
  expected.forEach(walk);
  return { dom: root.children.map(strip), want: expected.map(want), ids };
}

/** `section[h2"t:x" ul[li{done=yes}[span"t:y"]] footer]` */
function show(r: Rendered): string {
  const attrs = Object.entries(r.attrs).map(([k, v]) => `${k}=${v}`).join(",");
  const kids = r.children.map(show).join(" ");
  return r.tag + (attrs ? `{${attrs}}` : "") + (r.text ? JSON.stringify(r.text) : "") + (kids ? `[${kids}]` : "");
}

// --- random states ----------------------------------------------------------------

function rng(seed: number) {
  let s = seed >>> 0;
  const next = () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  return { next, int: (n: number) => Math.floor(next() * n), pick: <T>(xs: readonly T[]) => xs[Math.floor(next() * xs.length)]! };
}

const clone = (s: State): State => Object.fromEntries(Object.entries(s).map(([k, v]) => [k, new Map(v)]));
const empty = (): State => Object.fromEntries(VIEWS.map((v) => [v, new Map()]));

/** The delta from `a` to `b`: −old/+new per changed key, rows shuffled. */
function delta(a: State, b: State, r: ReturnType<typeof rng>): StepDeltas {
  const out: Record<string, Tuple[]> = {};
  for (const view of VIEWS) {
    const rowsOut: Tuple[] = [];
    for (const k of new Set([...a[view]!.keys(), ...b[view]!.keys()])) {
      const [x, y] = [a[view]!.get(k), b[view]!.get(k)];
      if (x === y) continue;
      if (x !== undefined) rowsOut.push([k, x, -1]);
      if (y !== undefined) rowsOut.push([k, y, 1]);
    }
    for (let i = rowsOut.length - 1; i > 0; i--) {
      const j = r.int(i + 1);
      [rowsOut[i], rowsOut[j]] = [rowsOut[j]!, rowsOut[i]!];
    }
    // An untouched view is usually omitted, sometimes sent empty.
    if (rowsOut.length > 0 || r.int(8) === 0) out[view] = rowsOut;
  }
  return out;
}

const POS = ["t:a0", "t:a1", "t:a2", "t:a0V", "t:Zz", "t:", "t:a1"];
const TEXT = ["t:x", "t:y", "t:", "t:a\\,b"];

function mutate(s: State, r: ReturnType<typeof rng>, ids: { n: number }): void {
  const fresh = (sort: number) => `#${sort}:${ids.n++}`;
  const some = (view: string) => (s[view]!.size ? r.pick([...s[view]!.keys()]) : undefined);
  // A list that exists, or — sometimes — one that does not.
  const listRef = () => (r.int(6) === 0 || !s.list!.size ? `#0:${900 + r.int(3)}` : r.pick([...s.list!.keys()]));
  const cardRef = () => (r.int(8) === 0 || !s.card!.size ? `#1:${900 + r.int(3)}` : r.pick([...s.card!.keys()]));
  const addCard = (list: string) => {
    const c = fresh(1);
    s.card!.set(c, list);
    s.card_pos!.set(c, r.pick(POS));
    s.card_title!.set(c, r.pick(TEXT));
    if (r.int(2)) s.card_done!.set(c, c);
  };
  switch (r.int(20)) {
    case 0: case 1: {
      const l = fresh(0);
      s.list!.set(l, l);
      s.list_pos!.set(l, `i:${r.int(12) - 2}`);
      s.list_title!.set(l, r.pick(TEXT));
      break;
    }
    // A list leaves the level. Its cards, notes and badge keep their
    // membership rows: nothing cascades.
    case 2: { const l = some("list"); if (l) for (const v of ["list", "list_pos", "list_title"]) s[v]!.delete(l); break; }
    // …and one that left earlier comes back, same key.
    case 3: {
      const gone = [...new Set([...s.card!.values(), ...s.note!.values(), ...s.badge!.keys()])].filter((l) => !s.list!.has(l));
      if (gone.length) {
        const l = r.pick(gone);
        s.list!.set(l, l);
        s.list_pos!.set(l, `i:${r.int(12) - 2}`);
        s.list_title!.set(l, r.pick(TEXT));
      }
      break;
    }
    case 4: { const l = some("list"); if (l) s.list_pos!.set(l, `i:${r.int(12) - 2}`); break; }
    case 5: { const l = some("list"); if (l) s.list_title!.set(l, r.pick(TEXT)); break; }
    case 6: case 7: case 8: addCard(listRef()); break;
    case 9: { const c = some("card"); if (c) for (const v of ["card", "card_pos", "card_title", "card_done"]) s[v]!.delete(c); break; }
    case 10: { const c = some("card"); if (c) s.card!.set(c, listRef()); break; }
    case 11: { const c = some("card"); if (c) s.card_pos!.set(c, r.pick(POS)); break; }
    case 12: { const c = some("card"); if (c) s.card_title!.set(c, r.pick(TEXT)); break; }
    case 13: { const c = some("card"); if (c) (s.card_done!.has(c) ? s.card_done!.delete(c) : s.card_done!.set(c, c)); break; }
    // The gate: flips per list, keyed by the list's own key.
    case 14: { const l = listRef(); s.badge!.has(l) ? s.badge!.delete(l) : s.badge!.set(l, l); break; }
    case 15: {
      if (r.int(3) === 0 && s.note!.size) { const n = some("note")!; s.note!.delete(n); s.note_text!.delete(n); break; }
      const n = fresh(2);
      s.note!.set(n, listRef());
      s.note_text!.set(n, r.pick(TEXT));
      break;
    }
    case 16: {
      if (r.int(3) === 0 && s.tag!.size) { const t = some("tag")!; s.tag!.delete(t); s.tag_name!.delete(t); break; }
      const t = fresh(3);
      s.tag!.set(t, cardRef());
      s.tag_name!.set(t, r.pick(TEXT));
      break;
    }
    case 17: { const t = some("tag"); if (t) s.tag!.set(t, cardRef()); break; }
    // Bulk: enough cards under one list to take the shaper's merge path.
    case 18: { const l = listRef(); for (let i = 0; i < 18 + r.int(8); i++) addCard(l); break; }
    // Bulk: every card of one list goes at once (the `clear` path).
    default: {
      const l = some("list");
      if (l) for (const [c, p] of [...s.card!]) if (p === l) for (const v of ["card", "card_pos", "card_title", "card_done"]) s[v]!.delete(c);
    }
  }
}

function run(seed: number, steps: number) {
  const r = rng(seed);
  const listDesc = r.int(3) === 0;
  const driver = new SpyDriver();
  const root = driver.createElement("main");
  const shaper = new Shaper(driver, root, [shapes(listDesc)]);
  let state = empty();
  let before = new Map<string, number>();
  const ids = { n: 0 };
  for (let step = 0; step < steps; step++) {
    const next = clone(state);
    for (let i = 1 + r.int(r.int(4) === 0 ? 8 : 3); i > 0; i--) mutate(next, r, ids);
    shaper.applyStep(delta(state, next, r));
    const seen = observe(shaper, root, render(next, listDesc));
    // Compared as text, so a failure prints two short lines, not two trees.
    expect(seen.dom.map(show).join("\n"), `seed ${seed}, step ${step}: DOM`).toBe(seen.want.map(show).join("\n"));
    for (const [row, id] of seen.ids) {
      if (before.has(row)) expect(id, `seed ${seed}, step ${step}: ${row} was rebuilt`).toBe(before.get(row));
    }
    before = seen.ids;
    state = next;
  }
}

describe("shaper vs. a from-scratch render", () => {
  // `REX_FUZZ_SEED=2037 npx vitest run test/shaper.fuzz.test.ts` replays one.
  const only = process.env.REX_FUZZ_SEED;
  test.runIf(only)("one seed", () => run(Number(only), 400));

  const seeds = Number(process.env.REX_FUZZ_CASES ?? 300);
  test(`${seeds} random histories`, () => {
    for (let seed = 1; seed <= seeds; seed++) run(seed, 40);
  });

  test("long histories", () => {
    for (let seed = 1000; seed < 1004; seed++) run(seed, 150);
  });
});
