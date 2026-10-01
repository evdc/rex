import { describe, expect, test } from "vitest";
import { SpyDriver, type SpyEl } from "../src/driver.js";
import { compareEncoded, OrderIndex } from "../src/order.js";
import { Shaper } from "../src/shaper.js";
import type { ShapeNode } from "../src/types.js";

describe("compareEncoded (S-70)", () => {
  test("Int keys order numerically, not lexically", () => {
    expect(compareEncoded("i:3", "i:10")).toBeLessThan(0);
    expect(compareEncoded("i:10", "i:3")).toBeGreaterThan(0);
    expect(compareEncoded("i:-5", "i:2")).toBeLessThan(0);
    expect(compareEncoded("i:7", "i:7")).toBe(0);
  });

  test("Money keys order numerically", () => {
    expect(compareEncoded("m:999", "m:1000")).toBeLessThan(0);
  });

  test("entity ids order by sequence, then sort", () => {
    expect(compareEncoded("#3:9", "#3:10")).toBeLessThan(0);
    expect(compareEncoded("#3:10", "#4:1")).toBeLessThan(0);
  });

  test("text and dates keep their string order", () => {
    expect(compareEncoded("t:a0", "t:a1")).toBeLessThan(0);
    expect(compareEncoded("d:2026-01-15", "d:2026-02-01")).toBeLessThan(0);
  });
});

describe("OrderIndex", () => {
  const keys = (ix: OrderIndex) => ix.childrenOf("p").map(([, c]) => c);

  test("ascending by typed key, child key breaks ties", () => {
    const ix = new OrderIndex();
    ix.insert("p", "i:10", "#1:1");
    ix.insert("p", "i:3", "#1:2");
    ix.insert("p", "i:3", "#1:10");
    ix.insert("p", "i:3", "#1:9");
    expect(keys(ix)).toEqual(["#1:2", "#1:9", "#1:10", "#1:1"]);
  });

  test("desc reverses the key but not the tiebreak", () => {
    const ix = new OrderIndex(true);
    ix.insert("p", "i:10", "#1:1");
    ix.insert("p", "i:3", "#1:2");
    ix.insert("p", "i:3", "#1:3");
    expect(keys(ix)).toEqual(["#1:1", "#1:2", "#1:3"]);
  });

  test("successor and remove agree with the typed order", () => {
    const ix = new OrderIndex();
    ix.insert("p", "i:3", "a");
    ix.insert("p", "i:10", "b");
    expect(ix.successor("p", "i:3", "a")).toBe("b");
    ix.remove("p", "i:3", "a");
    expect(keys(ix)).toEqual(["b"]);
  });
});

// The benchmark's rows: keyed by id, ordered by an Int `pos`.
const rowShape = (desc = false): ShapeNode<SpyEl> => ({
  name: "row",
  membershipView: "row",
  template: (d, key) => {
    const el = d.createElement("tr");
    d.setAttr(el, "key", key);
    return el;
  },
  attrs: [],
  orderView: "row_pos",
  orderDesc: desc,
  children: [],
});

function rows(n: number, desc = false) {
  const driver = new SpyDriver();
  const root = driver.createElement("tbody");
  const shaper = new Shaper(driver, root, [rowShape(desc)]);
  shaper.applyStep({
    row: Array.from({ length: n }, (_, i) => [`#1:${i + 1}`, "u", 1] as const),
    row_pos: Array.from({ length: n }, (_, i) => [`#1:${i + 1}`, `i:${i + 1}`, 1] as const),
  });
  driver.resetCounts();
  return { driver, root, shaper };
}

describe("shaper with typed order", () => {
  test("12 rows mount in numeric order (10 after 9)", () => {
    const { root } = rows(12);
    expect(root.children.map((c) => c.attrs.key)).toEqual(
      Array.from({ length: 12 }, (_, i) => `#1:${i + 1}`),
    );
  });

  test("desc mounts in reverse", () => {
    const { root } = rows(3, true);
    expect(root.children.map((c) => c.attrs.key)).toEqual(["#1:3", "#1:2", "#1:1"]);
  });

  test("the benchmark's SwapRows is exactly two insertBefore calls", () => {
    const { driver, root, shaper } = rows(10);
    // Swap rows 2 and 9: two −/+ pairs on the order view.
    shaper.applyStep({
      row_pos: [
        ["#1:2", "i:2", -1],
        ["#1:2", "i:9", 1],
        ["#1:9", "i:9", -1],
        ["#1:9", "i:2", 1],
      ],
    });
    expect(driver.counts.insertBefore).toBe(2);
    expect(driver.counts.createElement).toBe(0);
    expect(driver.counts.removeChild).toBe(0);
    const ids = root.children.map((c) => c.attrs.key);
    expect(ids[1]).toBe("#1:9");
    expect(ids[8]).toBe("#1:2");
  });
});

describe("bulk paths (P-6)", () => {
  const keys = (root: SpyEl) => root.children.map((c) => c.attrs.key);

  test("insertMany and removeMany keep the typed order", () => {
    const ix = new OrderIndex();
    ix.insertMany("p", [["i:5", "e"], ["i:1", "a"], ["i:3", "c"]]);
    ix.insertMany("p", [["i:4", "d"], ["i:2", "b"], ["i:6", "f"]]); // interleaved merge
    ix.insertMany("p", [["i:7", "g"], ["i:8", "h"]]); // append
    expect(ix.childrenOf("p").map((e) => e[1])).toEqual(["a", "b", "c", "d", "e", "f", "g", "h"]);
    ix.removeMany("p", new Set(["a", "d", "h"]));
    expect(ix.childrenOf("p").map((e) => e[1])).toEqual(["b", "c", "e", "f", "g"]);
    ix.removeMany("p", new Set(["b", "c", "e", "f", "g"]));
    expect(ix.childrenOf("p")).toEqual([]);
  });

  test("a bulk mount interleaves with mounted rows, in order", () => {
    // 10 odd-positioned rows mounted, then 40 more in shuffled delta order
    // (some between, some past the end): one batch, bulk path.
    const { driver, root, shaper } = rows(0);
    shaper.applyStep({
      row: Array.from({ length: 10 }, (_, i) => [`#1:${2 * i + 1}`, "u", 1] as const),
      row_pos: Array.from({ length: 10 }, (_, i) => [`#1:${2 * i + 1}`, `i:${2 * i + 1}`, 1] as const),
    });
    const fresh = [...Array.from({ length: 10 }, (_, i) => 2 * i + 2), ...Array.from({ length: 30 }, (_, i) => 21 + i)];
    fresh.sort((a, b) => ((a * 7919) % 97) - ((b * 7919) % 97));
    driver.resetCounts();
    shaper.applyStep({
      row: fresh.map((n) => [`#1:${n}`, "u", 1] as const),
      row_pos: fresh.map((n) => [`#1:${n}`, `i:${n}`, 1] as const),
    });
    expect(keys(root)).toEqual(Array.from({ length: 50 }, (_, i) => `#1:${i + 1}`));
    expect(driver.counts.insertBefore).toBe(40);
  });

  test("removing every child of a parent is one clear", () => {
    const { driver, root, shaper } = rows(30);
    shaper.applyStep({
      row: Array.from({ length: 30 }, (_, i) => [`#1:${i + 1}`, "u", -1] as const),
      row_pos: Array.from({ length: 30 }, (_, i) => [`#1:${i + 1}`, `i:${i + 1}`, -1] as const),
    });
    expect(driver.counts).toMatchObject({ clear: 1, removeChild: 0 });
    expect(root.children).toEqual([]);
    expect(shaper.orderedChildren("row", "")).toEqual([]);
    // ...and the level still works afterwards.
    shaper.applyStep({ row: [["#1:99", "u", 1]], row_pos: [["#1:99", "i:1", 1]] });
    expect(keys(root)).toEqual(["#1:99"]);
  });

  test("removing most children stays per-child removes", () => {
    const { driver, root, shaper } = rows(30);
    shaper.applyStep({
      row: Array.from({ length: 29 }, (_, i) => [`#1:${i + 1}`, "u", -1] as const),
      row_pos: Array.from({ length: 29 }, (_, i) => [`#1:${i + 1}`, `i:${i + 1}`, -1] as const),
    });
    expect(driver.counts).toMatchObject({ clear: 0, removeChild: 29 });
    expect(keys(root)).toEqual(["#1:30"]);
  });

  test("a move whose +new row arrives before its −old row", () => {
    // The mirror holds two values for a moment, then collapses to one.
    const { driver, root, shaper } = rows(3);
    shaper.applyStep({ row_pos: [["#1:1", "i:9", 1], ["#1:1", "i:1", -1]] });
    expect(keys(root)).toEqual(["#1:2", "#1:3", "#1:1"]);
    expect(driver.counts.insertBefore).toBe(1);
    shaper.applyStep({ row_pos: [["#1:1", "i:9", -1], ["#1:1", "i:0", 1]] });
    expect(keys(root)).toEqual(["#1:1", "#1:2", "#1:3"]);
  });
});
