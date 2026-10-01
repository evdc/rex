/** One dispatch's wall-clock split, in milliseconds. */
export interface DispatchTiming {
  name: string;
  /** `wasm.dispatch`: the engine step and its JSON encode, plus (when booted
   *  through `boot`) the persistence hook's log read. */
  engine: number;
  /** `JSON.parse` of the step result. */
  parse: number;
  /** `shaper.applyStep`: mirrors plus DOM construction. */
  shaper: number;
}

/** Opt-in (`?profile`) timing of the dispatch path. Generated code creates one
 *  with `profiler(enabled)` and passes it to `boot`; a disabled profiler is
 *  `null`, so it costs nothing per dispatch.
 *
 *  A row is built in two halves, because the engine half runs inside
 *  `Engine.dispatch` and the shaper half in the caller: `engine` times the
 *  first and `shaper` completes the row. */
export interface Profiler {
  readonly timings: DispatchTiming[];
  /** Time the engine half of one dispatch: `run` returns the step's JSON,
   *  `parse` decodes it. Returns the parsed step. */
  engine<T>(name: string, run: () => string, parse: (raw: string) => T): T;
  /** Time the shaper half and complete the row `engine` began. */
  shaper(apply: () => void): void;
  clear(): void;
}

export function profiler(enabled: boolean): Profiler | null {
  if (!enabled) return null;
  const timings: DispatchTiming[] = [];
  let pending: { name: string; engine: number; parse: number } | null = null;
  const p: Profiler = {
    timings,
    engine<T>(name: string, run: () => string, parse: (raw: string) => T): T {
      const t0 = performance.now();
      const raw = run();
      const t1 = performance.now();
      const step = parse(raw);
      pending = { name, engine: t1 - t0, parse: performance.now() - t1 };
      return step;
    },
    shaper(apply: () => void): void {
      const t0 = performance.now();
      apply();
      const row = { name: "", engine: 0, parse: 0, ...pending, shaper: performance.now() - t0 };
      pending = null;
      timings.push(row);
      console.log(
        `[rex profile] ${row.name}: engine ${row.engine.toFixed(1)} ms, parse ${row.parse.toFixed(1)} ms, shaper ${row.shaper.toFixed(1)} ms`,
      );
    },
    clear() {
      timings.length = 0;
    },
  };
  // For driving from the console or a harness: `__rexProfile.timings`.
  (globalThis as { __rexProfile?: Profiler }).__rexProfile = p;
  return p;
}
