import { describe, expect, it } from "vitest";
import { profiler } from "../src/profile.js";

describe("profiler", () => {
  it("is null when disabled", () => {
    expect(profiler(false)).toBeNull();
  });

  it("splits a dispatch into engine, parse and shaper", () => {
    const p = profiler(true)!;
    let applied = false;
    const step = p.engine("Run", () => '{"ids":["#0:1"]}', (raw) => JSON.parse(raw) as { ids: string[] });
    expect(step.ids).toEqual(["#0:1"]);
    expect(p.timings).toHaveLength(0); // the row is not complete until the shaper half
    p.shaper(() => {
      applied = true;
    });
    expect(applied).toBe(true);
    expect(p.timings).toHaveLength(1);
    const t = p.timings[0]!;
    expect(t.name).toBe("Run");
    for (const ms of [t.engine, t.parse, t.shaper]) expect(ms).toBeGreaterThanOrEqual(0);
    p.clear();
    expect(p.timings).toHaveLength(0);
  });
});
