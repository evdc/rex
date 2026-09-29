import "fake-indexeddb/auto";
import { expect, test } from "vitest";
import { IndexedDbAdapter } from "../src/index.js";

const ev = (seq: number) => ({ seq, name: "E", args: { a: "t:x" } });

test("events append idempotently and read back in order from a cursor", async () => {
  const a = new IndexedDbAdapter("t1");
  await a.appendEvents([ev(2), ev(0), ev(1)]);
  await a.appendEvents([ev(1), ev(3)]); // 1 again: ignored, not an error
  expect((await a.eventsSince(0)).map((e) => e.seq)).toEqual([0, 1, 2, 3]);
  expect((await a.eventsSince(2)).map((e) => e.seq)).toEqual([2, 3]);
});

test("a snapshot round-trips and a new adapter on the same db sees it", async () => {
  const a = new IndexedDbAdapter("t2");
  expect(await a.loadSnapshot()).toBeNull();
  await a.saveSnapshot({ json: '{"cursor":4}', cursor: 4 });
  const b = new IndexedDbAdapter("t2");
  expect(await b.loadSnapshot()).toEqual({ json: '{"cursor":4}', cursor: 4 });
});

test("databases are per name", async () => {
  await new IndexedDbAdapter("t3").appendEvents([ev(0)]);
  expect(await new IndexedDbAdapter("t4").eventsSince(0)).toEqual([]);
});
