import type { LoggedEvent, PersistenceAdapter, StoredSnapshot } from "../persist.js";

/** An in-memory adapter: tests, and a `?ephemeral` dev mode. State lives as
 *  long as the object does, which is exactly what a "reload" in a test
 *  reuses. */
export class MemoryAdapter implements PersistenceAdapter {
  snapshot: StoredSnapshot | null = null;
  readonly events = new Map<number, LoggedEvent>();

  async loadSnapshot(): Promise<StoredSnapshot | null> {
    return this.snapshot;
  }
  async saveSnapshot(s: StoredSnapshot): Promise<void> {
    this.snapshot = s;
  }
  async appendEvents(events: readonly LoggedEvent[]): Promise<void> {
    for (const e of events) if (!this.events.has(e.seq)) this.events.set(e.seq, e);
  }
  async eventsSince(seq: number): Promise<LoggedEvent[]> {
    return [...this.events.values()].filter((e) => e.seq >= seq).sort((a, b) => a.seq - b.seq);
  }
}
