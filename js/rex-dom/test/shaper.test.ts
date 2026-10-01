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
      clear: 0,
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

/**
 * S-52: a class bind is a *gate*, not a value. Its view is coreflexive —
 * it holds the child's key exactly when the class should be on — so the
 * driver toggles by presence and never decodes a boolean. The case that
 * makes this different from a value attribute is the retraction: the row
 * going away has to turn the class *off*, and a value attribute would
 * simply not be applied at all.
 */
describe("presence attributes", () => {
  const gatedShape: ShapeNode<SpyEl> = {
    name: "todo",
    membershipView: "todo",
    template: (d) => d.createElement("li"),
    attrs: [
      { view: "todo_title", apply: (d, el, v) => d.setText(el, v) },
      {
        view: "todo_done",
        presence: true,
        apply: (d, el, v) => d.setAttr(el, "done", v === undefined ? "off" : "on"),
      },
    ],
    children: [],
  };

  function setupGated() {
    const driver = new SpyDriver();
    const root = driver.createElement("ul");
    const shaper = new Shaper(driver, root, [gatedShape]);
    shaper.applyStep({
      todo: [
        ["t1", "u", 1],
        ["t2", "u", 1],
      ],
      todo_title: [
        ["t1", "Write", 1],
        ["t2", "Ship", 1],
      ],
      // Only t2 is done, so only t2's gate has a row.
      todo_done: [["t2", "t2", 1]],
    });
    return { driver, shaper };
  }

  test("applies the gate on mount, on and off", () => {
    const { shaper } = setupGated();
    expect(shaper.el("todo", "t1")!.attrs.done).toBe("off");
    expect(shaper.el("todo", "t2")!.attrs.done).toBe("on");
  });

  test("turns the class off when the gate row is retracted", () => {
    const { driver, shaper } = setupGated();
    driver.resetCounts();
    shaper.applyStep({ todo_done: [["t2", "t2", -1]] });
    expect(shaper.el("todo", "t2")!.attrs.done).toBe("off");
    // One setAttr on the same element — no remount, no other element touched.
    expect(driver.counts).toMatchObject({ setAttr: 1, createElement: 0, removeChild: 0 });
  });

  test("turns the class on when a gate row appears", () => {
    const { driver, shaper } = setupGated();
    driver.resetCounts();
    shaper.applyStep({ todo_done: [["t1", "t1", 1]] });
    expect(shaper.el("todo", "t1")!.attrs.done).toBe("on");
    expect(driver.counts).toMatchObject({ setAttr: 1, createElement: 0, removeChild: 0 });
  });
});

describe("slot (S-90)", () => {
  test("a child level mounts into the element its slot names, not the row root", () => {
    const item: ShapeNode<SpyEl> = {
      name: "item",
      membershipView: "item",
      template: (d) => d.createElement("li"),
      attrs: [],
      children: [],
      slot: (root) => root.children[1]!,
    };
    const app: ShapeNode<SpyEl> = {
      name: "app",
      membershipView: "app",
      template: (d) => {
        const root = d.createElement("section");
        d.insertBefore(root, d.createElement("h1"), null);
        d.insertBefore(root, d.createElement("ul"), null);
        return root;
      },
      attrs: [],
      children: [{ ...item, membershipView: "item" }],
    };
    const driver = new SpyDriver();
    const root = driver.createElement("main");
    const shaper = new Shaper(driver, root, [app]);
    shaper.applyStep({ app: [["u", "u", 1]], item: [["i1", "u", 1]] });
    const section = shaper.el("app", "u")!;
    expect(section.children.map((c) => c.tag)).toEqual(["h1", "ul"]);
    expect(section.children[1]!.children.map((c) => c.tag)).toEqual(["li"]);
    // Removal finds the same slot.
    shaper.applyStep({ item: [["i1", "u", -1]] });
    expect(section.children[1]!.children).toEqual([]);
  });
});

describe("sibling order under one element", () => {
  // section { <gate1> <gate2> footer } — levels flip on after the footer exists.
  const gate = (name: string, tag: string): ShapeNode<SpyEl> => ({
    name,
    membershipView: name,
    template: (d) => d.createElement(tag),
    attrs: [],
    children: [],
    anchor: (root) => root.children[0] ?? null,
  });
  const app: ShapeNode<SpyEl> = {
    name: "app",
    membershipView: "app",
    template: (d) => {
      const root = d.createElement("section");
      d.insertBefore(root, d.createElement("footer"), null);
      return root;
    },
    attrs: [],
    children: [gate("g1", "p"), gate("g2", "aside")],
  };
  const tags = (el: SpyEl) => el.children.map((c) => c.tag);

  test("a level that mounts late goes before the static sibling that follows it", () => {
    const driver = new SpyDriver();
    const shaper = new Shaper(driver, driver.createElement("main"), [app]);
    shaper.applyStep({ app: [["u", "u", 1]] });
    shaper.applyStep({ g1: [["u", "u", 1]] });
    expect(tags(shaper.el("app", "u")!)).toEqual(["p", "footer"]);
  });

  test("levels in one slot keep source order whichever mounts first", () => {
    const driver = new SpyDriver();
    const shaper = new Shaper(driver, driver.createElement("main"), [app]);
    shaper.applyStep({ app: [["u", "u", 1]] });
    shaper.applyStep({ g2: [["u", "u", 1]] });
    shaper.applyStep({ g1: [["u", "u", 1]] });
    expect(tags(shaper.el("app", "u")!)).toEqual(["p", "aside", "footer"]);
    // Flipping one off and on again lands back in the same place.
    shaper.applyStep({ g1: [["u", "u", -1]] });
    shaper.applyStep({ g1: [["u", "u", 1]] });
    expect(tags(shaper.el("app", "u")!)).toEqual(["p", "aside", "footer"]);
  });

  test("levels that mount in one batch are in source order", () => {
    const driver = new SpyDriver();
    const shaper = new Shaper(driver, driver.createElement("main"), [app]);
    shaper.applyStep({ app: [["u", "u", 1]], g2: [["u", "u", 1]], g1: [["u", "u", 1]] });
    expect(tags(shaper.el("app", "u")!)).toEqual(["p", "aside", "footer"]);
  });
});

describe("rows whose parent is not shown", () => {
  const item: ShapeNode<SpyEl> = {
    name: "item",
    membershipView: "item",
    template: (d) => d.createElement("li"),
    attrs: [{ view: "item_text", apply: (d, el, v) => d.setText(el, v) }],
    children: [],
  };
  const group: ShapeNode<SpyEl> = {
    name: "group",
    membershipView: "group",
    template: (d) => d.createElement("ul"),
    attrs: [],
    children: [item],
  };
  const fresh = () => {
    const driver = new SpyDriver();
    const root = driver.createElement("main");
    return { driver, root, shaper: new Shaper(driver, root, [group]) };
  };
  const texts = (root: SpyEl) => root.children.map((g) => g.children.map((i) => i.text));

  test("a child of a parent that is not in the parent level is not mounted, and is not an error", () => {
    // A nested `select` under a filtered level: the child's membership names
    // a parent the filter left out.
    const { root, shaper } = fresh();
    shaper.applyStep({ group: [["g1", "u", 1]], item: [["a", "g1", 1], ["b", "g2", 1]], item_text: [["a", "A", 1], ["b", "B", 1]] });
    expect(texts(root)).toEqual([["A"]]);
    expect(shaper.el("item", "b")).toBeUndefined();
  });

  test("it mounts when the parent arrives, with the values it already had", () => {
    const { root, shaper } = fresh();
    shaper.applyStep({ item: [["b", "g2", 1]], item_text: [["b", "B", 1]] });
    expect(texts(root)).toEqual([]);
    shaper.applyStep({ group: [["g2", "u", 1]] });
    expect(texts(root)).toEqual([["B"]]);
  });

  test("a parent that leaves and returns gets its children back", () => {
    // An `if (open) { ul { Item as i select … } }`: toggling `open` changes
    // only the gate's membership, never the items'.
    const { root, shaper } = fresh();
    shaper.applyStep({ group: [["g1", "u", 1]], item: [["a", "g1", 1], ["b", "g1", 1]], item_text: [["a", "A", 1], ["b", "B", 1]] });
    shaper.applyStep({ group: [["g1", "u", -1]] });
    expect(texts(root)).toEqual([]);
    // An item changes while its parent is away.
    shaper.applyStep({ item_text: [["a", "A", -1], ["a", "A2", 1]] });
    shaper.applyStep({ group: [["g1", "u", 1]] });
    expect(texts(root)).toEqual([["A2", "B"]]);
  });

  test("a removed child of a row that is reparented out of a removed parent is really removed", () => {
    // g1 goes, its item moves to g2 (same element), and that item's own
    // child goes too: it must not survive inside the reused element.
    const leaf: ShapeNode<SpyEl> = { name: "leaf", membershipView: "leaf", template: (d) => d.createElement("b"), attrs: [], children: [] };
    const driver = new SpyDriver();
    const root = driver.createElement("main");
    const shaper = new Shaper(driver, root, [{ ...group, children: [{ ...item, children: [leaf] }] }]);
    shaper.applyStep({ group: [["g1", "u", 1], ["g2", "u", 1]], item: [["a", "g1", 1]], leaf: [["x", "a", 1]] });
    const a = shaper.el("item", "a")!;
    expect(a.children.map((c) => c.tag)).toEqual(["b"]);
    shaper.applyStep({ group: [["g1", "u", -1]], item: [["a", "g1", -1], ["a", "g2", 1]], leaf: [["x", "a", -1]] });
    expect(shaper.el("item", "a")).toBe(a);
    expect(a.parent).toBe(shaper.el("group", "g2"));
    expect(a.children).toEqual([]);
  });
});
