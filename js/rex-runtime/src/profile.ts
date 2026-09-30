/** One dispatch's wall-clock split, in milliseconds. */
export interface DispatchTiming {
  name: string;
  /** `app.dispatch`: the wasm engine step, its JSON encode, and (when booted
   *  through `boot`) the persistence wrapper's log read. */
  engine: number;
  /** `JSON.parse` of the step result. */
  parse: number;
  /** `shaper.applyStep`: mirrors plus DOM construction. */
  shaper: number;
}

/** Opt-in (`?profile`) timing of the dispatch path. Generated code calls
 *  `profiler(enabled)` once and uses `prof?.…`, so a disabled profiler costs
 *  nothing per dispatch. */
export interface Profiler {
  readonly timings: DispatchTiming[];
  /** Time one dispatch. `engine` returns the step's JSON, `apply` consumes the
   *  parsed step. Returns the parsed step. */
  dispatch<T>(
    name: string,
    engine: () => string,
    apply: (step: T) => void,
  ): T;
  clear(): void;
}

export function profiler(enabled: boolean): Profiler | null {
  if (!enabled) return null;
  const timings: DispatchTiming[] = [];
  const p: Profiler = {
    timings,
    dispatch<T>(name: string, engine: () => string, apply: (step: T) => void): T {
      const t0 = performance.now();
      const raw = engine();
      const t1 = performance.now();
      const step = JSON.parse(raw) as T;
      const t2 = performance.now();
      apply(step);
      const t3 = performance.now();
      const row = { name, engine: t1 - t0, parse: t2 - t1, shaper: t3 - t2 };
      timings.push(row);
      console.log(
        `[rex profile] ${name}: engine ${row.engine.toFixed(1)} ms, parse ${row.parse.toFixed(1)} ms, shaper ${row.shaper.toFixed(1)} ms`,
      );
      return step;
    },
    clear() {
      timings.length = 0;
    },
  };
  // For driving from the console or a harness: `__rexProfile.timings`.
  (globalThis as { __rexProfile?: Profiler }).__rexProfile = p;
  return p;
}
