import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";
import * as runtime from "../src/index.js";

const readme = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../README.md"), "utf8");

test("every export the README names exists, and every export is documented", () => {
  const named = ["boot", "Engine", "IndexedDbAdapter", "MemoryAdapter", "programKey", "profiler"];
  for (const name of named) {
    expect(readme, `README mentions ${name}`).toContain(name);
    expect(typeof (runtime as Record<string, unknown>)[name], `${name} is exported`).toBe("function");
  }
  for (const name of Object.keys(runtime)) expect(readme, `README documents ${name}`).toContain(name);
});

test("the persistence interface in the README lists the adapter's four calls", () => {
  for (const call of ["loadSnapshot", "saveSnapshot", "appendEvents", "eventsSince"]) {
    expect(readme).toContain(call);
  }
});
