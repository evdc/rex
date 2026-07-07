import { describe, expect, test } from "vitest";
import { SpyDriver, type SpyEl } from "../src/driver.js";
import { Shaper } from "../src/shaper.js";
import type { ShapeNode, StepDeltas } from "../src/types.js";

/**
 * A Kanban-shaped fixture: lists (ordered by nothing) containing cards
 * (ordered by fractional pos). View roles mirror what the Rex program emits:
 *   list       : ListID -> anything     (root membership)
 *   list_title : ListID -> Text
 *   card_list  : CardID -> ListID       (structural membership)
 *   card_title : CardID -> Text         (attribute)
 *   card_pos   : CardID -> Text         (order)
 */
const cardShape: ShapeNode<SpyEl> = {
  name: "card",
  membershipView: "card_list",
  template: (d) => d.createElement("div"),
  attrs: [{ view: "card_title", apply: (d, el, v) => d.setText(el, v) }],
  orderView: "card_pos",
  children: [],
};

const listShape: ShapeNode<SpyEl> = {
  name: "list",
  membershipView: "list",
  template: (d) => d.createElement("section"),
  attrs: [{ view: "list_title", apply: (d, el, v) => d.setAttr(el, "title", v) }],
  children: [cardShape],
};

function setup() {
  const driver = new SpyDriver();
  const root = driver.createElement("main");
  const shaper = new Shaper(driver, root, [listShape]);
  // Two lists, two cards in todo, one in doing — one atomic initial batch.
  shaper.applyStep({
    list: [
      ["todo", "u", 1],
      ["doing", "u", 1],
    ],
    list_title: [
      ["todo", "Todo", 1],
      ["doing", "Doing", 1],
    ],
    card_list: [
      ["c1", "todo", 1],
      ["c2", "todo", 1],
      ["c3", "doing", 1],
    ],
    card_title: [
      ["c1", "Buy milk", 1],
      ["c2", "Ship it", 1],
      ["c3", "Write docs", 1],
    ],
    card_pos: [
      ["c1", "a0", 1],
      ["c2", "a1", 1],
      ["c3", "a0", 1],
    ],
  });
  driver.resetCounts();
  return { driver, root, shaper };
}

const childKeys = (el: SpyEl) => el.children.map((c) => c.id);

describe("mount", () => {
  test("initial batch assembles wide rows in order", () => {
    const { shaper } = setup();
    const todo = shaper.el("list", "todo")!;
    expect(todo.attrs["title"]).toBe("Todo");
    expect(todo.children.map((c) => c.text)).toEqual(["Buy milk", "Ship it"]);
    expect(shaper.el("list", "doing")!.children.map((c) => c.text)).toEqual(["Write docs"]);
  });

  test("parent and child arriving in one batch mount parent-first", () => {
    const { driver, shaper } = setup();
    shaper.applyStep({
      list: [["done", "u", 1]],
      list_title: [["done", "Done", 1]],
      card_list: [["c4", "done", 1]],
      card_title: [["c4", "Celebrate", 1]],
      card_pos: [["c4", "a0", 1]],
    });
    const done = shaper.el("list", "done")!;
    expect(done.children.map((c) => c.text)).toEqual(["Celebrate"]);
    expect(driver.counts.createElement).toBe(2);
  });

  test("mount into the middle finds its mounted successor", () => {
    const { shaper } = setup();
    shaper.applyStep({
      card_list: [["c9", "todo", 1]],
      card_title: [["c9", "Middle", 1]],
      card_pos: [["c9", "a0V", 1]], // between a0 and a1
    });
    expect(shaper.el("list", "todo")!.children.map((c) => c.text)).toEqual([
      "Buy milk",
      "Middle",
      "Ship it",
    ]);
  });
});

describe("the #1 rule: same-key −/+ fuses to ONE op", () => {
  test("retitle is one setText on the same element", () => {
    const { driver, shaper } = setup();
    const before = shaper.el("card", "c1")!;
    shaper.applyStep({
      card_title: [
        ["c1", "Buy milk", -1],
        ["c1", "Buy oat milk", 1],
      ],
    });
    const after = shaper.el("card", "c1")!;
    expect(after).toBe(before); // node identity preserved
    expect(after.text).toBe("Buy oat milk");
    expect(driver.counts).toEqual({
      createElement: 0,
      setText: 1,
      setAttr: 0,
      insertBefore: 0,
      removeChild: 0,
    });
  });

  test("reorder is exactly one insertBefore", () => {
    const { driver, shaper } = setup();
    shaper.applyStep({
      card_pos: [
        ["c2", "a1", -1],
        ["c2", "a", 1], // "a" < "a0": c2 moves first
      ],
    });
    expect(shaper.el("list", "todo")!.children.map((c) => c.text)).toEqual([
      "Ship it",
      "Buy milk",
    ]);
    expect(driver.counts.insertBefore).toBe(1);
    expect(driver.counts.createElement).toBe(0);
    expect(driver.counts.removeChild).toBe(0);
  });

  test("kanban drag: reparent reuses the DOM node, one insertBefore", () => {
    const { driver, shaper } = setup();
    const el = shaper.el("card", "c1")!;
    shaper.applyStep({
      card_list: [
        ["c1", "todo", -1],
        ["c1", "doing", 1],
      ],
    });
    expect(shaper.el("card", "c1")).toBe(el); // same node, no flicker
    expect(el.parent).toBe(shaper.el("list", "doing"));
    expect(shaper.el("list", "doing")!.children.map((c) => c.text)).toEqual([
      "Buy milk",
      "Write docs",
    ]);
    expect(driver.counts.insertBefore).toBe(1);
    expect(driver.counts.createElement).toBe(0);
    expect(driver.counts.removeChild).toBe(0);
  });

  test("reparent + retitle in one batch: one move, one setText, same node", () => {
    const { driver, shaper } = setup();
    const el = shaper.el("card", "c1")!;
    shaper.applyStep({
      card_list: [
        ["c1", "todo", -1],
        ["c1", "doing", 1],
      ],
      card_title: [
        ["c1", "Buy milk", -1],
        ["c1", "Bought milk", 1],
      ],
    });
    expect(shaper.el("card", "c1")).toBe(el);
    expect(el.text).toBe("Bought milk");
    expect(driver.counts.insertBefore).toBe(1);
    expect(driver.counts.setText).toBe(1);
    expect(driver.counts.createElement).toBe(0);
  });
});

describe("removal", () => {
  test("dead subtree is ONE removeChild", () => {
    const { driver, shaper } = setup();
    // The engine retracts the list and both its cards' memberships in one
    // atomic batch; the shaper must coalesce to a single DOM detach.
    shaper.applyStep({
      list: [["todo", "u", -1]],
      list_title: [["todo", "Todo", -1]],
      card_list: [
        ["c1", "todo", -1],
        ["c2", "todo", -1],
      ],
      card_title: [
        ["c1", "Buy milk", -1],
        ["c2", "Ship it", -1],
      ],
      card_pos: [
        ["c1", "a0", -1],
        ["c2", "a1", -1],
      ],
    });
    expect(driver.counts.removeChild).toBe(1);
    expect(shaper.el("list", "todo")).toBeUndefined();
    expect(shaper.el("card", "c1")).toBeUndefined();
  });

  test("single card removal detaches only that card", () => {
    const { driver, shaper } = setup();
    shaper.applyStep({
      card_list: [["c2", "todo", -1]],
      card_title: [["c2", "Ship it", -1]],
      card_pos: [["c2", "a1", -1]],
    });
    expect(driver.counts.removeChild).toBe(1);
    expect(shaper.el("list", "todo")!.children.map((c) => c.text)).toEqual(["Buy milk"]);
  });
});

describe("batch atomicity", () => {
  test("mixed batch applies as one consistent transaction", () => {
    const { shaper } = setup();
    // Simultaneously: c2 deleted, c3 dragged into todo, c1 retitled, and a
    // new card mounted — one step, one transaction, final state consistent.
    shaper.applyStep({
      card_list: [
        ["c2", "todo", -1],
        ["c3", "doing", -1],
        ["c3", "todo", 1],
        ["c4", "todo", 1],
      ],
      card_title: [
        ["c2", "Ship it", -1],
        ["c1", "Buy milk", -1],
        ["c1", "Buy oat milk", 1],
        ["c4", "New", 1],
      ],
      card_pos: [
        ["c2", "a1", -1],
        ["c3", "a0", -1],
        ["c3", "a2", 1],
        ["c4", "a3", 1],
      ],
    });
    expect(shaper.el("list", "todo")!.children.map((c) => c.text)).toEqual([
      "Buy oat milk",
      "Write docs",
      "New",
    ]);
    expect(shaper.el("list", "doing")!.children).toEqual([]);
  });
});
