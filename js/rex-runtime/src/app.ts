import type { DispatchResult, EnginePort, EventArg, StepDeltas } from "rex-dom";
import type { LoggedEvent } from "./persist.js";
import type { Profiler } from "./profile.js";

/** The slice of the wasm `RexApp` the runtime drives. Everything crosses the
 *  boundary as JSON strings, once per call. */
export interface EngineApp {
  dispatch(name: string, argsJson: string): string;
  rebalance(field: string, rowsJson: string): string;
  snapshot(): string;
  log_since(seq: bigint): string;
  base_snapshot(): string;
  replay(eventsJson: string, silent: boolean): void;
  restore(baseJson: string): void;
}

export interface EngineHooks {
  /** Called after every write the engine accepts (`dispatch`, `rebalance`),
   *  before its result is parsed — how `boot` appends the new log tail. */
  afterWrite?: () => void;
  /** Splits each dispatch into engine / parse / shaper time (`?profile`). */
  profiler?: Profiler | null;
}

/**
 * A typed face over the wasm engine: the `EnginePort` the generated app and
 * `rex-dom` talk to, plus the log and snapshot accessors persistence needs.
 * It owns all the JSON — callers never see a string.
 */
export class Engine implements EnginePort {
  /** The raw wasm app. An escape hatch for measurement and debugging: a write
   *  made here skips `afterWrite` (persistence picks it up with the next
   *  write, since it reads the engine's log, not its own). */
  readonly wasm: EngineApp;
  private readonly hooks: EngineHooks;

  constructor(wasm: EngineApp, hooks: EngineHooks = {}) {
    this.wasm = wasm;
    this.hooks = hooks;
  }

  snapshot(): StepDeltas {
    return (JSON.parse(this.wasm.snapshot()) as { views: StepDeltas }).views;
  }

  dispatch(event: string, args: Readonly<Record<string, EventArg>>): DispatchResult {
    const run = () => {
      const raw = this.wasm.dispatch(event, JSON.stringify(args));
      this.hooks.afterWrite?.();
      return raw;
    };
    const parse = (raw: string): DispatchResult => {
      const r = JSON.parse(raw) as { ids: string[]; deltas: { views: StepDeltas } } | { rejected: string };
      if ("rejected" in r) return { ids: [], deltas: {}, rejected: r.rejected };
      return { ids: r.ids, deltas: r.deltas.views };
    };
    const p = this.hooks.profiler;
    return p ? p.engine(event, run, parse) : parse(run());
  }

  rebalance(field: string, rows: readonly (readonly [string, string])[]): StepDeltas {
    const raw = this.wasm.rebalance(field, JSON.stringify(rows));
    this.hooks.afterWrite?.();
    return (JSON.parse(raw) as { views: StepDeltas }).views;
  }

  /** Every logged event with `seq >= seq`, in order. */
  logSince(seq: number): LoggedEvent[] {
    return JSON.parse(this.wasm.log_since(BigInt(seq))) as LoggedEvent[];
  }

  /** The engine's input tables plus id-minting and log-cursor state, as the
   *  JSON `restore` takes back. */
  baseSnapshot(): string {
    return this.wasm.base_snapshot();
  }

  /** Load a `baseSnapshot` into an engine booted with `forRestore`. */
  restore(baseJson: string): void {
    this.wasm.restore(baseJson);
  }

  /** Re-apply logged events. `silent` skips per-step deltas — the engine-only
   *  path for a page reload. Replayed events are not appended to the log. */
  replay(events: readonly LoggedEvent[], silent = true): void {
    this.wasm.replay(JSON.stringify(events), silent);
  }
}
