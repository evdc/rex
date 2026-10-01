import { describe, expect, test } from "vitest";
import { compareEncoded, OrderIndex } from "../src/order.js";
import { keyBetween, rebalancePlan, REBALANCE_LIMIT } from "../src/rebalance.js";

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

/** Order keys of every kind one view might hold, and some it should not:
 *  integers past 2^53 and past 15 digits, negatives, leading zeros, ids,
 *  text, the empty key, and malformed numbers. */
const KEYS = [
  "i:0", "i:3", "i:10", "i:-1", "i:-10", "i:007", "i:9007199254740993", "i:9007199254740992", "i:999999999999999",
  "i:1000000000000000", "i:-9223372036854775808", "i:9223372036854775807", "m:250", "m:-5", "m:1000",
  "#3:9", "#3:10", "#2:11", "#10:0", "t:a", "t:b", "t:", "t:a0", "t:Zz", "", "i:", "i:abc", "@True", "u",
];

describe("the comparator is a total order", () => {
  test("antisymmetric and transitive over every key kind", () => {
    for (const a of KEYS) {
      for (const b of KEYS) {
        expect(Math.sign(compareEncoded(a, b)) + Math.sign(compareEncoded(b, a)), `${a} vs ${b}`).toBe(0);
        for (const c of KEYS) {
          if (compareEncoded(a, b) <= 0 && compareEncoded(b, c) <= 0) {
            expect(compareEncoded(a, c), `${a} <= ${b} <= ${c}`).toBeLessThanOrEqual(0);
          }
        }
      }
    }
  });

  test("integers compare by value, exactly, past 2^53", () => {
    expect(compareEncoded("i:3", "i:10")).toBeLessThan(0);
    expect(compareEncoded("i:-10", "i:-1")).toBeLessThan(0);
    expect(compareEncoded("i:9007199254740992", "i:9007199254740993")).toBeLessThan(0);
    expect(compareEncoded("i:999999999999999", "i:1000000000000000")).toBeLessThan(0);
    expect(compareEncoded("#3:9", "#3:10")).toBeLessThan(0);
    expect(compareEncoded("#2:11", "#3:9")).toBeLessThan(0);
  });
});

describe("OrderIndex vs. a sorted array", () => {
  for (const desc of [false, true]) {
    test(`random inserts, removes and bulk changes (${desc ? "desc" : "asc"})`, () => {
      for (let seed = 1; seed <= 60; seed++) {
        const r = rng(seed);
        const index = new OrderIndex(desc);
        // parent -> child -> orderKey
        const model = new Map<string, Map<string, string>>();
        const sorted = (parent: string) =>
          [...(model.get(parent) ?? [])]
            .sort(([ca, ka], [cb, kb]) => {
              const c = compareEncoded(ka, kb);
              return (desc ? -c : c) || compareEncoded(ca, cb);
            })
            .map(([c]) => c);
        let nextChild = 0;
        for (let step = 0; step < 60; step++) {
          const parent = r.pick(["p", "q"]);
          const kids = model.get(parent) ?? new Map<string, string>();
          model.set(parent, kids);
          switch (r.int(6)) {
            case 0: case 1: {
              const [child, key] = [`#1:${nextChild++}`, r.pick(KEYS)];
              kids.set(child, key);
              index.insert(parent, key, child);
              break;
            }
            case 2: {
              const batch = Array.from({ length: r.int(25) }, () => [r.pick(KEYS), `#1:${nextChild++}`] as const);
              for (const [key, child] of batch) kids.set(child, key);
              index.insertMany(parent, batch);
              break;
            }
            case 3: {
              if (!kids.size) break;
              const child = r.pick([...kids.keys()]);
              index.remove(parent, kids.get(child)!, child);
              kids.delete(child);
              break;
            }
            case 4: {
              const gone = new Set([...kids.keys()].filter(() => r.int(3) === 0));
              // A child that was never there is ignored.
              if (r.int(4) === 0) gone.add("#1:99999");
              index.removeMany(parent, gone);
              for (const c of gone) kids.delete(c);
              break;
            }
            // A move: remove under the old key, insert under a new one.
            default: {
              if (!kids.size) break;
              const child = r.pick([...kids.keys()]);
              const key = r.pick(KEYS);
              index.remove(parent, kids.get(child)!, child);
              index.insert(parent, key, child);
              kids.set(child, key);
            }
          }
          for (const p of ["p", "q"]) {
            const want = sorted(p);
            expect(index.childrenOf(p).map((e) => e[1]), `seed ${seed}, step ${step}, parent ${p}`).toEqual(want);
            // The successor of each entry is the next one; of the last, null.
            const kidsOf = model.get(p);
            want.forEach((c, i) => {
              expect(index.successor(p, kidsOf!.get(c)!, c), `seed ${seed}: successor of ${c}`).toBe(want[i + 1] ?? null);
            });
          }
        }
      }
    });
  }
});

describe("fractional order keys", () => {
  test("random drops keep keys distinct and in order, and a rebalance preserves it", () => {
    for (let seed = 1; seed <= 20; seed++) {
      const r = rng(seed);
      // The list in display order: [key, child].
      let list: [string, string][] = [];
      for (let i = 0; i < 150; i++) {
        // Mostly hammer one gap (the pathology that grows keys), sometimes anywhere.
        const at = r.int(4) === 0 ? r.int(list.length + 1) : Math.min(list.length, 1);
        const key = keyBetween(list[at - 1]?.[0] ?? null, list[at]?.[0] ?? null);
        list.splice(at, 0, [key, `c${i}`]);
        for (let j = 1; j < list.length; j++) {
          expect(list[j - 1]![0] < list[j]![0], `seed ${seed}, insert ${i}: ${list[j - 1]![0]} !< ${list[j]![0]}`).toBe(true);
        }
        const plan = rebalancePlan(list);
        if (plan) {
          expect(list.some(([k]) => k.length > REBALANCE_LIMIT)).toBe(true);
          const before = list.map(([, c]) => c);
          list = list.map(([, c]) => [plan.get(c)!, c]);
          expect([...list].sort((a, b) => (a[0] < b[0] ? -1 : 1)).map(([, c]) => c), `seed ${seed}: rebalance kept the order`).toEqual(before);
          expect(list.every(([k]) => k.length <= REBALANCE_LIMIT)).toBe(true);
        } else {
          expect(list.every(([k]) => k.length <= REBALANCE_LIMIT)).toBe(true);
        }
      }
    }
  });
});
