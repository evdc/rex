import { Engine, type EngineApp } from "./app.js";
import type { LoggedEvent, PersistenceAdapter } from "./persist.js";
import type { Profiler } from "./profile.js";

/** The wasm-bindgen `RexApp` class: a constructor and the restoring boot. */
export interface EngineClass<A extends EngineApp> {
  new (source: string): A;
  forRestore(source: string): A;
}

export interface BootOptions<A extends EngineApp> {
  RexApp: EngineClass<A>;
  program: string;
  adapter: PersistenceAdapter;
  /** `profiler(true)` to time each dispatch's engine / parse / shaper split. */
  profiler?: Profiler | null;
  /** Save a snapshot after this many appended events (default 50). */
  snapshotEvery?: number;
  /** Where to hook `visibilitychange`/`pagehide` (default: `document`/`window`
   *  when present). Pass `null` to disable. */
  lifecycle?: { onHide(cb: () => void): void } | null;
  onError?: (err: unknown) => void;
}

/** A booted engine: the typed `Engine`, with every write also persisted. */
export type PersistedEngine = Engine & {
  /** Resolves when every append issued so far has reached the adapter, first
   *  retrying any events an earlier failed append left unsaved. */
  flush(): Promise<void>;
  /** Save a snapshot now. */
  saveSnapshot(): Promise<void>;
};

/**
 * Boot an app from its adapter (S-80):
 *
 *  - **Nothing stored** — a first load: run the program (its `new`
 *    statements log `@genesis` events) and persist that log.
 *  - **Anything stored** — a reload: boot with `forRestore` (no seed data,
 *    which would double up with the restored rows), `restore` the snapshot
 *    if there is one, then silently `replay` every event after its cursor.
 *    A page killed before its first snapshot has only the log; the log's
 *    genesis events rebuild the seed.
 *
 * If what is stored does not load, `boot` reports it through `onError` and
 * recovers with as much as it can (see the fallbacks in the body) rather than
 * leaving the page dead.
 *
 * Afterwards each `dispatch`/`rebalance` appends the engine's new log tail.
 * Returns the typed `Engine`, which is what generated code hands to the
 * shaper as its `EnginePort`.
 */
export async function boot<A extends EngineApp>(opts: BootOptions<A>): Promise<PersistedEngine> {
  const { RexApp, program, adapter } = opts;
  const every = opts.snapshotEvery ?? 50;
  const onError = opts.onError ?? ((e) => console.error("[rex] persistence failed", e));

  const snap = await adapter.loadSnapshot();
  const tail = await adapter.eventsSince(snap?.cursor ?? 0);

  let app: A;
  /** The next `seq` not yet confirmed in the adapter. Only a *successful*
   *  append moves it, so a failed batch is re-read and retried by the next
   *  append rather than leaving a hole in the stored log. */
  let cursor: number;
  /** A snapshot to write back once the store has been reset (recovery). */
  let resave: { json: string; cursor: number } | null = null;
  /** Take a new snapshot right after boot, over one that would not load. */
  let refresh = false;
  if (!snap && tail.length === 0) {
    app = new RexApp(program); // logs the genesis events from seq 0
    cursor = 0;
  } else {
    // What is stored may not load: a snapshot or log that was damaged, cut
    // short, or written by something else is refused by the engine (it
    // validates both). Each attempt gets a fresh engine — a failed replay
    // leaves one half-applied — and each falls back to less:
    //   1. the snapshot plus the events after it   (the normal reload)
    //   2. the whole log, from empty               (the snapshot was bad)
    //   3. the snapshot alone                      (an event after it was bad)
    //   4. a first load                            (nothing stored loads)
    // 3 and 4 lose work, so they are reported, and the store is reset to
    // match what the app now holds — otherwise the next reload fails again.
    const load = (s: typeof snap, events: readonly LoggedEvent[]): A => {
      const fresh = RexApp.forRestore(program);
      if (s) fresh.restore(s.json);
      if (events.length > 0) fresh.replay(JSON.stringify(events), true);
      return fresh;
    };
    const after = (events: readonly LoggedEvent[], from: number) =>
      events.length > 0 ? Math.max(from, events[events.length - 1]!.seq + 1) : from;
    let loaded: { app: A; cursor: number } | null = null;
    try {
      loaded = { app: load(snap, tail), cursor: after(tail, snap?.cursor ?? 0) };
    } catch (first) {
      onError(first);
      let recovered = false;
      if (snap) {
        try {
          const all = await adapter.eventsSince(0);
          // Only a log that reaches back to the start can stand alone.
          if (all.length > 0 && all[0]!.seq === 0) {
            loaded = { app: load(null, all), cursor: after(all, 0) };
            recovered = true;
            refresh = true; // replace the snapshot that would not load
          }
        } catch (second) {
          onError(second);
        }
        if (!loaded) {
          try {
            loaded = { app: load(snap, []), cursor: snap.cursor };
            resave = { json: snap.json, cursor: snap.cursor };
          } catch (third) {
            onError(third);
          }
        }
      }
      if (!recovered) {
        try {
          await adapter.reset?.();
        } catch (e) {
          onError(e);
        }
      }
    }
    if (loaded) {
      app = loaded.app;
      cursor = loaded.cursor;
    } else {
      app = new RexApp(program);
      cursor = 0;
    }
  }

  let chain: Promise<void> = Promise.resolve();
  let sinceSnapshot = 0;
  const enqueue = (work: () => Promise<void>): Promise<void> => {
    chain = chain.then(work).catch(onError);
    return chain;
  };

  /** The next `seq` already handed to an append (for snapshot pacing only). */
  let queued = cursor;

  /** Events read from the log but not yet confirmed by the adapter, oldest
   *  first. Each event is read (serialized and parsed) once, by
   *  `persistTail`; a failed append leaves its events here for the next. */
  let unconfirmed: LoggedEvent[] = [];

  /** Append everything from `cursor` on, as it stands when the append *runs*:
   *  this call's events plus any a failed earlier append left behind. */
  const appendUnconfirmed = (): Promise<void> =>
    enqueue(async () => {
      const events = unconfirmed.filter((e) => e.seq >= cursor);
      if (events.length === 0) return;
      await adapter.appendEvents(events);
      cursor = Math.max(cursor, events[events.length - 1]!.seq + 1);
      unconfirmed = unconfirmed.filter((e) => e.seq >= cursor);
    });

  const persistTail = () => {
    const fresh = readLog(app, queued);
    if (fresh.length === 0) return;
    for (const e of fresh) unconfirmed.push(e);
    queued = fresh[fresh.length - 1]!.seq + 1;
    sinceSnapshot += fresh.length;
    void appendUnconfirmed();
    if (sinceSnapshot >= every) void saveSnapshot();
  };

  const saveSnapshot = (): Promise<void> => {
    sinceSnapshot = 0;
    // Serialize now, synchronously: the snapshot must match `cursor` exactly,
    // and an await in between would let another dispatch move the engine.
    const json = app.base_snapshot();
    const at = (JSON.parse(json) as { cursor: number }).cursor;
    return enqueue(() => adapter.saveSnapshot({ json, cursor: at }));
  };

  if (resave) {
    const kept = resave;
    void enqueue(() => adapter.saveSnapshot(kept));
  }
  persistTail(); // the genesis events on a first load
  if (refresh) void saveSnapshot();

  const engine = new Engine(app, { afterWrite: persistTail, profiler: opts.profiler });
  const target = engine as PersistedEngine;
  // A flush also retries anything a failed append left unconfirmed.
  target.flush = () => appendUnconfirmed();
  target.saveSnapshot = saveSnapshot;

  const hide = opts.lifecycle === undefined ? browserLifecycle() : opts.lifecycle;
  hide?.onHide(() => void saveSnapshot());
  return target;
}

function readLog(app: EngineApp, from: number): LoggedEvent[] {
  return JSON.parse(app.log_since(BigInt(from))) as LoggedEvent[];
}

function browserLifecycle(): { onHide(cb: () => void): void } | null {
  if (typeof document === "undefined" || typeof window === "undefined") return null;
  return {
    onHide(cb) {
      document.addEventListener("visibilitychange", () => {
        if (document.visibilityState === "hidden") cb();
      });
      window.addEventListener("pagehide", cb);
    },
  };
}
