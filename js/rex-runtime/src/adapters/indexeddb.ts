import type { LoggedEvent, PersistenceAdapter, StoredSnapshot } from "../persist.js";

const SNAPSHOT = "snapshot";
const EVENTS = "events";

function done<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function finished(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    // Not `onerror`: it also fires for a request error the caller handled
    // with `preventDefault` (a duplicate-event append), which is not a failure.
    tx.onabort = () => reject(tx.error);
  });
}

/** Browser persistence in IndexedDB: one database per program, a `snapshot`
 *  store (a single record) and an `events` store keyed by `seq`. */
export class IndexedDbAdapter implements PersistenceAdapter {
  private db: Promise<IDBDatabase> | null = null;

  constructor(
    private readonly name: string,
    private readonly factory: IDBFactory = indexedDB,
  ) {}

  private open(): Promise<IDBDatabase> {
    this.db ??= new Promise((resolve, reject) => {
      const req = this.factory.open(`rex:${this.name}`, 1);
      req.onupgradeneeded = () => {
        req.result.createObjectStore(SNAPSHOT);
        req.result.createObjectStore(EVENTS, { keyPath: "seq" });
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
    return this.db;
  }

  async loadSnapshot(): Promise<StoredSnapshot | null> {
    const db = await this.open();
    const hit = await done(db.transaction(SNAPSHOT).objectStore(SNAPSHOT).get("current"));
    return (hit as StoredSnapshot | undefined) ?? null;
  }

  async saveSnapshot(s: StoredSnapshot): Promise<void> {
    const db = await this.open();
    const tx = db.transaction(SNAPSHOT, "readwrite");
    tx.objectStore(SNAPSHOT).put(s, "current");
    await finished(tx);
  }

  async appendEvents(events: readonly LoggedEvent[]): Promise<void> {
    if (events.length === 0) return;
    const db = await this.open();
    const tx = db.transaction(EVENTS, "readwrite");
    const store = tx.objectStore(EVENTS);
    // `add`, not `put`: a retried append must not overwrite; a duplicate key
    // raises a ConstraintError we swallow per event.
    for (const e of events) {
      const req = store.add(e);
      req.onerror = (ev) => ev.preventDefault();
    }
    await finished(tx);
  }

  async eventsSince(seq: number): Promise<LoggedEvent[]> {
    const db = await this.open();
    const all = await done(
      db.transaction(EVENTS).objectStore(EVENTS).getAll(IDBKeyRange.lowerBound(seq)),
    );
    return (all as LoggedEvent[]).sort((a, b) => a.seq - b.seq);
  }
}
