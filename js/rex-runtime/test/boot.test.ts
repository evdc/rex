import { describe, expect, test } from "vitest";
import { boot, MemoryAdapter, programKey, type EngineApp } from "../src/index.js";

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
    return "{}";
  }
  rebalance(): string {
    return "{}";
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
  replay(json: string, _silent: boolean): void {
    for (const e of JSON.parse(json) as { seq: number; args: Record<string, string> }[]) {
      this.items.push(e.args.item!);
      this.next = Math.max(this.next, e.seq + 1);
    }
  }
  restore(json: string): void {
    const s = JSON.parse(json) as { cursor: number; items: string[] };
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

const add = (app: EngineApp, item: string) => app.dispatch("Add", JSON.stringify({ item }));

describe("boot", () => {
  test("a first load runs the program and persists its genesis events", async () => {
    const a = new MemoryAdapter();
    const app = await boot(opts(a));
    await app.flush();
    expect(app.items).toEqual(["seed1", "seed2"]);
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
    expect(second.items).toEqual(["seed1", "seed2", "x", "y"]);
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
    expect(second.items).toEqual(["seed1", "seed2", "x", "late"]);
  });

  test("no snapshot at all: the log alone rebuilds the seed and the edits", async () => {
    const a = new MemoryAdapter();
    const first = await boot(opts(a, { snapshotEvery: 1000 }));
    add(first, "x");
    await first.flush();
    expect(a.snapshot).toBeNull();

    const second = await boot(opts(a));
    expect(second.items).toEqual(["seed1", "seed2", "x"]);
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
    expect(third.items).toEqual(["seed1", "seed2", "x", "y"]);
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
    expect(app.items).toContain("x");
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
    expect(second.items).toEqual(["seed1", "seed2", "lost", "after"]);
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

describe("programKey", () => {
  test("changes with the source, so a new program never replays an old log", () => {
    expect(programKey("todo", "a")).not.toBe(programKey("todo", "b"));
    expect(programKey("todo", "a")).toBe(programKey("todo", "a"));
  });
});
