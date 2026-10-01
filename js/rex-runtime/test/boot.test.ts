import { describe, expect, test } from "vitest";
import { boot, MemoryAdapter, programKey, type Engine, type EngineApp } from "../src/index.js";

/**
 * A stand-in for the wasm `RexApp` with the same persistence surface: a log
 * with `seq`s, a base snapshot carrying the cursor, `forRestore` (no seed),
 * `restore`, and a silent `replay` that advances the cursor without
 * re-logging. State is a list of strings; `Add` appends one.
 */
class FakeApp implements EngineApp {
  items: string[] = [];
  private log: { seq: number; name: string; args: Record<string, string> }[] = [];
  private next = 0;

  constructor(readonly source: string, seed = true) {
    if (seed) {
      for (const s of source.split(",").filter(Boolean)) this.apply("@genesis", { item: s });
    }
  }
  static forRestore(source: string): FakeApp {
    return new FakeApp(source, false);
  }

  private apply(name: string, args: Record<string, string>) {
    this.items.push(args.item!);
    this.log.push({ seq: this.next++, name, args });
  }
  dispatch(name: string, argsJson: string): string {
    this.apply(name, JSON.parse(argsJson));
    return '{"ids":[],"deltas":{"views":{}}}';
  }
  rebalance(): string {
    return '{"views":{}}';
  }
  snapshot(): string {
    return JSON.stringify({ views: { items: this.items.map((i) => [i, "u", 1]) } });
  }
  log_since(seq: bigint): string {
    return JSON.stringify(this.log.filter((e) => e.seq >= Number(seq)));
  }
  base_snapshot(): string {
    return JSON.stringify({ cursor: this.next, items: this.items });
  }
  /** Like the engine: refuses an event it cannot apply — after applying the
   *  ones before it, so a failed replay leaves this app half-loaded. */
  replay(json: string, _silent: boolean): void {
    for (const e of JSON.parse(json) as { seq: number; name: string; args: Record<string, string> }[]) {
      if (e.name === "@damaged" || e.args?.item === undefined) throw new Error(`cannot replay seq ${e.seq}`);
      this.items.push(e.args.item);
      this.next = Math.max(this.next, e.seq + 1);
    }
  }
  /** Like the engine: refuses a snapshot that is not one of its own. */
  restore(json: string): void {
    const s = JSON.parse(json) as { cursor: number; items: string[] };
    if (!Array.isArray(s.items) || typeof s.cursor !== "number") throw new Error("not a snapshot");
    this.items = [...s.items];
    this.next = s.cursor;
  }
}

const opts = (adapter: MemoryAdapter, extra = {}) => ({
  RexApp: FakeApp,
  program: "seed1,seed2",
  adapter,
  lifecycle: null,
  ...extra,
});

const add = (app: Engine, item: string) => app.dispatch("Add", { item });
/** The fake's state, read through the engine's raw-app escape hatch. */
const itemsOf = (app: Engine) => (app.wasm as FakeApp).items;

describe("boot", () => {
  test("a first load runs the program and persists its genesis events", async () => {
    const a = new MemoryAdapter();
    const app = await boot(opts(a));
    await app.flush();
    expect(itemsOf(app)).toEqual(["seed1", "seed2"]);
    expect([...a.events.values()].map((e) => e.name)).toEqual(["@genesis", "@genesis"]);
  });

  test("a reload restores state without re-running the seed", async () => {
    const a = new MemoryAdapter();
    const first = await boot(opts(a, { snapshotEvery: 2 }));
    add(first, "x");
    add(first, "y"); // 4 events total: crosses the snapshot threshold
    await first.flush();
    expect(a.snapshot).not.toBeNull();

    const second = await boot(opts(a));
    expect(itemsOf(second)).toEqual(["seed1", "seed2", "x", "y"]);
  });

  test("an event after the last snapshot is replayed", async () => {
    const a = new MemoryAdapter();
    const first = await boot(opts(a, { snapshotEvery: 3 }));
    add(first, "x"); // genesis 0,1 + x = 3 events -> snapshot at cursor 3
    await first.flush();
    expect(a.snapshot?.cursor).toBe(3);
    add(first, "late"); // appended, never snapshotted: the page "dies" here
    await first.flush();

    const second = await boot(opts(a));
    expect(itemsOf(second)).toEqual(["seed1", "seed2", "x", "late"]);
  });

  test("no snapshot at all: the log alone rebuilds the seed and the edits", async () => {
    const a = new MemoryAdapter();
    const first = await boot(opts(a, { snapshotEvery: 1000 }));
    add(first, "x");
    await first.flush();
    expect(a.snapshot).toBeNull();

    const second = await boot(opts(a));
    expect(itemsOf(second)).toEqual(["seed1", "seed2", "x"]);
  });

  test("seqs continue after a restore instead of colliding", async () => {
    const a = new MemoryAdapter();
    const first = await boot(opts(a, { snapshotEvery: 2 }));
    add(first, "x");
    await first.flush();
    const second = await boot(opts(a));
    add(second, "y");
    await second.flush();
    const seqs = [...a.events.keys()].sort((p, q) => p - q);
    expect(seqs).toEqual([0, 1, 2, 3]);
    const third = await boot(opts(a));
    expect(itemsOf(third)).toEqual(["seed1", "seed2", "x", "y"]);
  });

  test("saveSnapshot() snapshots on demand (the page-hide path)", async () => {
    const a = new MemoryAdapter();
    const app = await boot(opts(a, { snapshotEvery: 1000 }));
    add(app, "x");
    await app.saveSnapshot();
    await app.flush();
    expect(a.snapshot?.cursor).toBe(3);
  });

  test("a hide hook triggers a snapshot", async () => {
    const a = new MemoryAdapter();
    let hide: () => void = () => {};
    const app = await boot(opts(a, { lifecycle: { onHide: (cb: () => void) => (hide = cb) } }));
    add(app, "x");
    hide();
    await app.flush();
    expect(a.snapshot?.cursor).toBe(3);
  });

  test("a failing adapter reports instead of throwing into dispatch", async () => {
    const a = new MemoryAdapter();
    a.appendEvents = async () => {
      throw new Error("disk full");
    };
    const errors: unknown[] = [];
    const app = await boot(opts(a, { onError: (e: unknown) => errors.push(e) }));
    expect(() => add(app, "x")).not.toThrow();
    await app.flush();
    expect(errors.length).toBeGreaterThan(0);
    expect(itemsOf(app)).toContain("x");
  });

  test("a failed append is retried, not skipped, so the log has no hole", async () => {
    const a = new MemoryAdapter();
    const append = a.appendEvents.bind(a);
    let failNext = false;
    a.appendEvents = async (events) => {
      if (failNext) {
        failNext = false;
        throw new Error("quota");
      }
      return append(events);
    };
    const errors: unknown[] = [];
    const first = await boot(opts(a, { snapshotEvery: 1000, onError: (e: unknown) => errors.push(e) }));
    await first.flush();
    failNext = true;
    add(first, "lost"); // this append fails
    await first.flush().catch(() => {});
    add(first, "after"); // this one succeeds, and must carry "lost" with it
    await first.flush();
    expect(errors.length).toBe(1);
    expect([...a.events.keys()].sort((p, q) => p - q)).toEqual([0, 1, 2, 3]);

    const second = await boot(opts(a));
    expect(itemsOf(second)).toEqual(["seed1", "seed2", "lost", "after"]);
  });

  test("flush() alone retries a failed append", async () => {
    const a = new MemoryAdapter();
    const append = a.appendEvents.bind(a);
    let fail = true;
    a.appendEvents = async (events) => {
      if (fail) throw new Error("transient");
      return append(events);
    };
    const app = await boot(opts(a, { onError: () => {} }));
    add(app, "x");
    await app.flush();
    expect(a.events.size).toBe(0);
    fail = false;
    await app.flush();
    expect(a.events.size).toBe(3);
  });
});

describe("boot when what is stored does not load", () => {
  /** A store holding seed1, seed2, x, y, with a snapshot covering the first three. */
  async function stored() {
    const a = new MemoryAdapter();
    const app = await boot(opts(a, { snapshotEvery: 3 }));
    add(app, "x"); // third event: the snapshot is taken at cursor 3
    await app.flush();
    add(app, "y");
    await app.flush();
    expect(a.snapshot?.cursor).toBe(3);
    expect(a.events.size).toBe(4);
    return a;
  }
  const errors = () => {
    const seen: unknown[] = [];
    return { seen, onError: (e: unknown) => seen.push(e) };
  };

  test("a damaged snapshot: the whole log rebuilds everything, and the snapshot is replaced", async () => {
    const a = await stored();
    a.snapshot = { json: "{garbage", cursor: 3 };
    const e = errors();
    const app = await boot(opts(a, e));
    expect(itemsOf(app)).toEqual(["seed1", "seed2", "x", "y"]);
    expect(e.seen).toHaveLength(1);
    await app.flush();
    // The next reload is a normal one again.
    expect(a.snapshot?.cursor).toBe(4);
    const again = errors();
    expect(itemsOf(await boot(opts(a, again)))).toEqual(["seed1", "seed2", "x", "y"]);
    expect(again.seen).toHaveLength(0);
  });

  test("a damaged event after the snapshot: the snapshot alone, and the store is reset to it", async () => {
    const a = await stored();
    a.events.set(3, { seq: 3, name: "@damaged", args: {} });
    const e = errors();
    const app = await boot(opts(a, e));
    // Not half of the tail on top of a half-loaded engine: exactly the snapshot.
    expect(itemsOf(app)).toEqual(["seed1", "seed2", "x"]);
    expect(e.seen.length).toBeGreaterThan(0);
    add(app, "z");
    await app.flush();
    expect([...a.events.keys()]).toEqual([3]);
    expect(a.snapshot?.cursor).toBe(3);
    const again = errors();
    expect(itemsOf(await boot(opts(a, again)))).toEqual(["seed1", "seed2", "x", "z"]);
    expect(again.seen).toHaveLength(0);
  });

  test("a damaged log and no snapshot: a first load, and the store is reset", async () => {
    const a = new MemoryAdapter();
    const first = await boot(opts(a, { snapshotEvery: 1000 }));
    add(first, "x");
    await first.flush();
    a.events.set(1, { seq: 1, name: "@damaged", args: {} });
    const e = errors();
    const app = await boot(opts(a, e));
    expect(itemsOf(app)).toEqual(["seed1", "seed2"]);
    expect(e.seen).toHaveLength(1);
    await app.flush();
    expect([...a.events.values()].map((ev) => ev.name)).toEqual(["@genesis", "@genesis"]);
    expect(itemsOf(await boot(opts(a)))).toEqual(["seed1", "seed2"]);
  });

  test("a damaged snapshot and a damaged log: a first load", async () => {
    const a = await stored();
    a.snapshot = { json: "null", cursor: 3 };
    a.events.set(0, { seq: 0, name: "@damaged", args: {} });
    const e = errors();
    const app = await boot(opts(a, e));
    expect(itemsOf(app)).toEqual(["seed1", "seed2"]);
    expect(e.seen.length).toBeGreaterThanOrEqual(2);
    add(app, "new");
    await app.flush();
    expect(itemsOf(await boot(opts(a)))).toEqual(["seed1", "seed2", "new"]);
  });

  test("a log that does not start at 0 cannot stand in for a bad snapshot", async () => {
    const a = await stored();
    a.snapshot = { json: "{garbage", cursor: 3 };
    a.events.delete(0);
    const app = await boot(opts(a, errors()));
    expect(itemsOf(app)).toEqual(["seed1", "seed2"]);
  });

  test("an adapter that cannot reset still boots, every time", async () => {
    const a = await stored();
    a.events.set(3, { seq: 3, name: "@damaged", args: {} });
    (a as { reset?: unknown }).reset = undefined;
    for (let i = 0; i < 2; i++) {
      const e = errors();
      const app = await boot(opts(a, e));
      expect(itemsOf(app)).toEqual(["seed1", "seed2", "x"]);
      expect(e.seen.length).toBeGreaterThan(0);
      await app.flush();
    }
  });

  test("an adapter whose reset fails still boots", async () => {
    const a = await stored();
    a.events.set(3, { seq: 3, name: "@damaged", args: {} });
    a.reset = async () => {
      throw new Error("blocked");
    };
    const e = errors();
    expect(itemsOf(await boot(opts(a, e)))).toEqual(["seed1", "seed2", "x"]);
    expect(e.seen.map(String).some((m) => m.includes("blocked"))).toBe(true);
  });
});

describe("programKey", () => {
  test("changes with the source, so a new program never replays an old log", () => {
    expect(programKey("todo", "a")).not.toBe(programKey("todo", "b"));
    expect(programKey("todo", "a")).toBe(programKey("todo", "a"));
  });
});

describe("Engine", () => {
  test("parses a dispatch into ids and per-view deltas, and a snapshot into deltas", async () => {
    const engine = await boot(opts(new MemoryAdapter()));
    const res = engine.dispatch("Add", { item: "x" });
    expect(res).toEqual({ ids: [], deltas: {} });
    expect(engine.snapshot()).toEqual({ items: [["seed1", "u", 1], ["seed2", "u", 1], ["x", "u", 1]] });
  });

  test("logSince reads the typed log", async () => {
    const engine = await boot(opts(new MemoryAdapter()));
    engine.dispatch("Add", { item: "x" });
    expect(engine.logSince(2).map((e) => [e.seq, e.name])).toEqual([[2, "Add"]]);
  });

  test("a profiler sees the engine half in dispatch and the shaper half from the caller", async () => {
    const { profiler } = await import("../src/profile.js");
    const prof = profiler(true)!;
    const engine = await boot(opts(new MemoryAdapter(), { profiler: prof }));
    const res = engine.dispatch("Add", { item: "x" });
    prof.shaper(() => void res);
    expect(prof.timings.map((t) => t.name)).toEqual(["Add"]);
  });
});
