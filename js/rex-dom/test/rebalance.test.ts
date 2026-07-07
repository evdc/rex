import { describe, expect, test } from "vitest";
import { keyBetween, rebalancePlan } from "../src/rebalance.js";

describe("rebalance", () => {
  test("quiet below the limit", () => {
    expect(rebalancePlan([["a0", "c1"], ["a1", "c2"]])).toBeNull();
  });

  test("re-spaces once a key grows past the limit", () => {
    // Force the pathology: repeated insertion into the same gap.
    // The library's one pathology: repeated insertion into the same interior
    // gap (always between the fixed floor and the latest key) grows keys
    // steadily — roughly one character every few insertions.
    const lo = "a0";
    let hi = "a1";
    const entries: [string, string][] = [];
    let i = 0;
    do {
      hi = keyBetween(lo, hi);
      entries.push([hi, `c${i++}`]);
    } while (hi.length <= 45 && i < 1000);
    entries.sort(([a], [b]) => (a < b ? -1 : 1));
    expect(entries.some(([k]) => k.length > 40)).toBe(true);

    const plan = rebalancePlan(entries)!;
    expect(plan).not.toBeNull();
    expect(plan.size).toBe(entries.length);
    // The plan preserves order and produces short keys.
    const fresh = entries.map(([, c]) => plan.get(c)!);
    expect([...fresh].sort()).toEqual(fresh);
    expect(fresh.every((k) => k.length <= 4)).toBe(true);
  });
});
