import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import { SpyDriver, type SpyEl } from "../src/driver.js";
import { Shaper } from "../src/shaper.js";
import { parseStepJson } from "../src/types.js";
import { boardShape } from "../examples/board-shape.js";

/**
 * S-31: the README's example is a real test. A reader can write the board's
 * `ShapeNode` tree from the README alone and drive the shaper with the S-03
 * fixtures; this test pins that the code block *is* `examples/board-shape.ts`
 * (so the README cannot drift) and that driving it behaves as documented.
 */

const here = dirname(fileURLToPath(import.meta.url));
const readme = readFileSync(join(here, "../README.md"), "utf8");
const example = readFileSync(join(here, "../examples/board-shape.ts"), "utf8");
const fixtures = join(here, "../../../crates/rex-core/tests/fixtures/steps");
const fixture = (name: string) => parseStepJson(readFileSync(join(fixtures, `${name}.json`), "utf8"));

describe("README example", () => {
  test("the embedded code block is examples/board-shape.ts, verbatim", () => {
    const marker = readme.indexOf("<!-- example: examples/board-shape.ts -->");
    expect(marker).toBeGreaterThan(-1);
    const open = readme.indexOf("```ts\n", marker) + "```ts\n".length;
    const close = readme.indexOf("\n```", open);
    expect(readme.slice(open, close)).toBe(example.trimEnd());
  });

  test("driving it with a fixture does what the README says", () => {
    const driver = new SpyDriver();
    const root = driver.createElement("main");
    const shaper = new Shaper(driver, root, [boardShape<SpyEl>()]);

    shaper.applyStep(fixture("00-mount"));
    expect(root.children.map((list) => list.children.map((card) => card.text))).toEqual([["Design", "Lower"], ["Ship"]]);

    driver.resetCounts();
    shaper.applyStep(fixture("01-rename"));
    expect(driver.counts).toEqual({ createElement: 0, setText: 1, setAttr: 0, insertBefore: 0, removeChild: 0, clear: 0 });
  });

  test("every helper the README names is a real export", async () => {
    const api = (await import("../src/index.js")) as Record<string, unknown>;
    const named = [
      "parseStepJson", "Shaper", "BrowserDriver", "SpyDriver",
      "encodeText", "decodeText", "encodeInt", "decodeInt", "encodeMoney", "decodeMoney",
      "encodeAtom", "decodeAtom", "encodeRel",
      "makeDraggable", "makeDropTarget", "dragValue", "endOf", "dropPos", "maybeRebalance",
    ];
    for (const name of named) {
      expect(readme, `README mentions ${name}`).toContain(name);
      expect(typeof api[name], `${name} is exported`).toBe("function");
    }
    expect(api.REBALANCE_LIMIT).toBe(40);
  });
});
