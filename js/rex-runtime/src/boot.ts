import type { LoggedEvent, PersistenceAdapter } from "./persist.js";

/** The slice of the wasm `RexApp` the runtime drives. */
export interface EngineApp {
  dispatch(name: string, argsJson: string): string;
  rebalance(field: string, rowsJson: string): string;
  snapshot(): string;
  log_since(seq: bigint): string;
  base_snapshot(): string;
  replay(eventsJson: string, silent: boolean): void;
  restore(baseJson: string): void;
}

export interface EngineClass<A extends EngineApp> {
  new (source: string): A;
  forRestore(source: string): A;
}

export interface BootOptions<A extends EngineApp> {
  RexApp: EngineClass<A>;
  program: string;
  adapter: PersistenceAdapter;
  /** Save a snapshot after this many appended events (default 50). */
  snapshotEvery?: number;
  /** Where to hook `visibilitychange`/`pagehide` (default: `document`/`window`
   *  when present). Pass `null` to disable. */
  lifecycle?: { onHide(cb: () => void): void } | null;
  onError?: (err: unknown) => void;
}

/** A booted app: the engine, with every write also persisted. `dispatch` and
 *  `rebalance` are the engine's own (same signature, same return), so
 *  generated code and `rex-dom` helpers use it as they would a bare `RexApp`. */
export type PersistedApp<A extends EngineApp> = A & {
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
 * Afterwards each `dispatch`/`rebalance` appends the engine's new log tail.
 */
export async function boot<A extends EngineApp>(opts: BootOptions<A>): Promise<PersistedApp<A>> {
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
  if (!snap && tail.length === 0) {
    app = new RexApp(program); // logs the genesis events from seq 0
    cursor = 0;
  } else {
    app = RexApp.forRestore(program);
    cursor = snap?.cursor ?? 0;
    if (snap) app.restore(snap.json);
    if (tail.length > 0) {
      app.replay(JSON.stringify(tail), true);
      cursor = Math.max(cursor, tail[tail.length - 1]!.seq + 1);
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

  persistTail(); // the genesis events on a first load

  // Wrap the write methods on the app object itself, so `this` stays the
  // engine and every existing holder of `app` gets persistence.
  const target = app as PersistedApp<A>;
  const dispatch = app.dispatch.bind(app);
  const rebalance = app.rebalance.bind(app);
  target.dispatch = (name, args) => {
    const out = dispatch(name, args);
    persistTail();
    return out;
  };
  target.rebalance = (field, rows) => {
    const out = rebalance(field, rows);
    persistTail();
    return out;
  };
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
