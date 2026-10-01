/**
 * Persistence for a Rex app (MVP-PLAN S-80).
 *
 * The engine's write surface is a named-event log (S-21), so persistence is
 * two things: an append-only **event store**, and periodic **snapshots** of
 * the engine's base tables (S-22's `base_snapshot`) that let a reload skip
 * replaying history from empty. A snapshot is only an optimisation — the log
 * alone reproduces the state — so losing one is harmless and a page killed
 * before its next snapshot loses nothing.
 */

/** One logged event as the engine emits it (`log_since`'s JSON element). */
export interface LoggedEvent {
  readonly seq: number;
  readonly name: string;
  readonly args: Readonly<Record<string, unknown>>;
  readonly cause?: number | null;
  readonly intent?: string | null;
}

/** A saved `base_snapshot`: the engine's JSON, plus the log cursor it covers
 *  (every event with `seq < cursor` is already inside it). */
export interface StoredSnapshot {
  readonly json: string;
  readonly cursor: number;
}

/** Where a Rex app keeps its state between page loads. Shape borrowed from
 *  elysium26's sync adapters. Every method may be asynchronous. */
export interface PersistenceAdapter {
  loadSnapshot(): Promise<StoredSnapshot | null>;
  saveSnapshot(snapshot: StoredSnapshot): Promise<void>;
  /** Append events; an event already stored under its `seq` is left alone, so
   *  a retried append is harmless. */
  appendEvents(events: readonly LoggedEvent[]): Promise<void>;
  /** Every stored event with `seq >= seq`, in `seq` order. */
  eventsSince(seq: number): Promise<LoggedEvent[]>;
  /** Forget everything stored. Optional: `boot` calls it when what is stored
   *  cannot be loaded and it has to start from less (see `boot`). Without it,
   *  a store that cannot be loaded is left as it is, and every later boot
   *  recovers the same way. */
  reset?(): Promise<void>;
}

/** A stable short key for a program's source, so a changed program never
 *  replays another program's log (FNV-1a, 32-bit, hex). */
export function programKey(name: string, source: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < source.length; i++) {
    h ^= source.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return `${name}-${h.toString(16).padStart(8, "0")}`;
}
