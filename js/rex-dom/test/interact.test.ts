import { describe, expect, test } from "vitest";
import { SpyDriver, type SpyEl } from "../src/driver.js";
import { decodeText, encodeText } from "../src/encode.js";
import { dropPos, endOf, maybeRebalance } from "../src/interact.js";
import { Shaper } from "../src/shaper.js";
import type { ShapeNode } from "../src/types.js";

/** One parent `p` with cards ordered by `pos`, ascending or `desc`. Each card
 *  gets a fake 10px-tall box stacked in display order, so `dropPos` can
 *  hit-test without a real DOM. */
function setup(orderDesc: boolean, keys: Record<string, string>) {
  const card: ShapeNode<SpyEl> = {
    name: "card",
    membershipView: "card_list",
    template: (d) => d.createElement("div"),
    attrs: [],
    orderView: "card_pos",
    orderDesc,
    children: [],
  };
  const list: ShapeNode<SpyEl> = {
    name: "list",
    membershipView: "list",
    template: (d) => d.createElement("section"),
    attrs: [],
    children: [card],
  };
  const driver = new SpyDriver();
  const shaper = new Shaper(driver, driver.createElement("main"), [list]);
  const ids = Object.keys(keys);
  shaper.applyStep({
    list: [["p", "u", 1]],
    card_list: ids.map((c) => [c, "p", 1] as [string, string, number]),
    card_pos: ids.map((c) => [c, encodeText(keys[c]!), 1] as [string, string, number]),
  });
  shaper.orderedChildren("card", "p").forEach(([, c], i) => {
    (shaper.el("card", c) as unknown as { getBoundingClientRect(): unknown }).getBoundingClientRect =
      () => ({ top: i * 10, height: 10 });
  });
  const shown = () => shaper.orderedChildren("card", "p").map(([, c]) => c);
  const keyOf = (c: string) =>
    decodeText(shaper.orderedChildren("card", "p").find(([, x]) => x === c)![0]);
  return { shaper, shown, keyOf };
}

describe("drag helpers under `order by … desc`", () => {
  const keys = { c1: "a0", c2: "a1", c3: "a2" };

  test("desc displays largest key first", () => {
    expect(setup(true, keys).shown()).toEqual(["c3", "c2", "c1"]);
  });

  test("endOf lands after the last displayed child", () => {
    const asc = setup(false, keys);
    expect(endOf(asc.shaper, "card", "p") > "a2").toBe(true);
    const desc = setup(true, keys);
    expect(endOf(desc.shaper, "card", "p") < "a0").toBe(true);
  });

  test("dropPos between two displayed neighbors", () => {
    // Pointer at y=15: below c3's midpoint (5), above c2's (15.x) -> between c3 and c2.
    const { shaper } = setup(true, keys);
    const k = dropPos(shaper, "card", "p", 12, "none");
    expect(k > "a1" && k < "a2").toBe(true);
    // Top and bottom ends.
    expect(dropPos(shaper, "card", "p", 0, "none") > "a2").toBe(true);
    expect(dropPos(shaper, "card", "p", 100, "none") < "a0").toBe(true);
  });

  test("rebalance keeps the displayed order", () => {
    const long = (n: number) => "a0" + "V".repeat(45) + String(n);
    const desc = setup(true, { c1: long(1), c2: long(2), c3: long(3) });
    expect(desc.shown()).toEqual(["c3", "c2", "c1"]);
    let rows: readonly (readonly [string, string])[] = [];
    maybeRebalance(
      { rebalance: (_f, sent) => ((rows = sent), {}) },
      desc.shaper,
      "card",
      "p",
      "pos",
      encodeText,
    );
    const fresh = new Map(rows.map(([c, k]) => [c, decodeText(k)]));
    expect(fresh.size).toBe(3);
    // Still c3 > c2 > c1 by key, so `desc` still shows c3 first.
    expect(fresh.get("c3")! > fresh.get("c2")!).toBe(true);
    expect(fresh.get("c2")! > fresh.get("c1")!).toBe(true);
  });
});
