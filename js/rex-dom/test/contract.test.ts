import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import { SpyDriver, type SpyEl } from "../src/driver.js";
import { Shaper } from "../src/shaper.js";
import { parseStepJson } from "../src/types.js";
import { boardShape } from "../examples/board-shape.js";

/**
 * S-03: the engine/shaper contract. `crates/rex-core/tests/contract_fixtures.rs`
 * drives the shared `crates/rex-core/tests/fixtures/board.rex` program
 * through a scripted history via the real `Engine` and writes each step's
 * delta JSON here. This test applies those same files through the real
 * `Shaper` with a `SpyDriver` and asserts DOM mutation counts — so a
 * regression in either the engine's delta shape or the shaper's role
 * classification (mount vs. update vs. move vs. remove) fails a test on
 * *this* side, independent of `examples/kanban`.
 *
 * The shape tree (`examples/board-shape.ts`, which the README embeds)
 * mirrors `board.rex`'s generated view names exactly (see
 * `crates/rex-codegen/tests/snapshots/board.ts`), but is hand-written rather
 * than the generated module: this test's job is the *shaper's* contract, not
 * codegen's (that's `rex-codegen`'s snapshot test).
 */

const fixturesDir = join(dirname(fileURLToPath(import.meta.url)), "../../../crates/rex-core/tests/fixtures/steps");

function loadStep(name: string) {
  return parseStepJson(readFileSync(join(fixturesDir, `${name}.json`), "utf8"));
}

const listShape = boardShape<SpyEl>();

function setup() {
  const driver = new SpyDriver();
  const root = driver.createElement("main");
  const shaper = new Shaper(driver, root, [listShape]);
  shaper.applyStep(loadStep("00-mount"));
  driver.resetCounts();
  return { driver, shaper };
}

describe("contract fixtures (real engine deltas through the real shaper)", () => {
  test("00-mount: two lists, three cards, in order", () => {
    const { shaper } = setup();
    expect(shaper.el("board#list", "#0:0")!.children.map((c) => c.text)).toEqual(["Design", "Lower"]);
    expect(shaper.el("board#list", "#0:1")!.children.map((c) => c.text)).toEqual(["Ship"]);
  });

  test("01-rename: exactly one setText, same node", () => {
    const { driver, shaper } = setup();
    const before = shaper.el("board#list#card", "#1:0");
    shaper.applyStep(loadStep("01-rename"));
    expect(shaper.el("board#list#card", "#1:0")).toBe(before);
    expect(before!.text).toBe("Design the schema");
    expect(driver.counts).toEqual({ createElement: 0, setText: 1, setAttr: 0, insertBefore: 0, removeChild: 0, clear: 0 });
  });

  test("02-reorder: exactly one insertBefore, no create/remove", () => {
    const { driver, shaper } = setup();
    shaper.applyStep(loadStep("02-reorder"));
    expect(shaper.el("board#list", "#0:0")!.children.map((c) => c.text)).toEqual(["Lower", "Design"]);
    expect(driver.counts.insertBefore).toBe(1);
    expect(driver.counts.createElement).toBe(0);
    expect(driver.counts.removeChild).toBe(0);
  });

  test("03-reparent: exactly one insertBefore, node reused, no create/remove", () => {
    const { driver, shaper } = setup();
    const el = shaper.el("board#list#card", "#1:0")!;
    shaper.applyStep(loadStep("03-reparent"));
    expect(shaper.el("board#list#card", "#1:0")).toBe(el);
    expect(el.parent).toBe(shaper.el("board#list", "#0:1"));
    expect(driver.counts.insertBefore).toBe(1);
    expect(driver.counts.createElement).toBe(0);
    expect(driver.counts.removeChild).toBe(0);
  });

  test("04-delete: exactly one removeChild, no create/insert", () => {
    const { driver, shaper } = setup();
    shaper.applyStep(loadStep("04-delete"));
    expect(shaper.el("board#list#card", "#1:1")).toBeUndefined();
    expect(driver.counts).toEqual({ createElement: 0, setText: 0, setAttr: 0, insertBefore: 0, removeChild: 1, clear: 0 });
  });

  test("05-insert: a fresh mount, no removals", () => {
    const { driver, shaper } = setup();
    shaper.applyStep(loadStep("05-insert"));
    const card = shaper.el("board#list#card", "#1:3");
    expect(card).toBeDefined();
    expect(card!.text).toBe("Write tests");
    expect(card!.parent).toBe(shaper.el("board#list", "#0:1"));
    expect(driver.counts.removeChild).toBe(0);
    expect(driver.counts.createElement).toBeGreaterThan(0);
  });
});
