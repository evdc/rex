import init, { RexApp } from "./pkg/rex_wasm.js";
import {
  BrowserDriver,
  Shaper,
  decodeText as decText,
  encodeText as encText,
  keyBetween,
  parseStepJson,
  rebalancePlan,
  type ShapeNode,
} from "rex-dom";
import PROGRAM from "./board.rex?raw";

// --- boot -------------------------------------------------------------------

await init();
const app = new RexApp(PROGRAM);

const driver = new BrowserDriver();
const board = document.getElementById("board")! as HTMLElement;

/** Push one engine result (delta JSON) through the shaper. */
function step(deltaJson: string): void {
  shaper.applyStep(parseStepJson(deltaJson));
}

// --- shape tree --------------------------------------------------------------

const cardShape: ShapeNode<HTMLElement> = {
  name: "card",
  membershipView: "card_list",
  orderView: "card_pos",
  template: (d, key) => {
    const el = d.createElement("div");
    el.className = "card";
    el.draggable = true;
    el.dataset.key = key;

    const input = document.createElement("input");
    input.addEventListener("change", () => {
      step(app.update_field(key, "title", encText(input.value)));
    });
    el.appendChild(input);

    const del = document.createElement("button");
    del.textContent = "×";
    del.addEventListener("click", () => step(app.retract(key)));
    el.appendChild(del);

    el.addEventListener("dragstart", (e) => {
      e.dataTransfer!.setData("text/rex-card", key);
      el.classList.add("dragging");
    });
    el.addEventListener("dragend", () => el.classList.remove("dragging"));
    return el;
  },
  attrs: [
    {
      view: "card_title",
      apply: (_d, el, v) => {
        const input = el.querySelector("input")!;
        const text = decText(v);
        // Don't fight the user's cursor: only write when it actually changed.
        if (input.value !== text) input.value = text;
      },
    },
  ],
  children: [],
};

const listShape: ShapeNode<HTMLElement> = {
  name: "list",
  membershipView: "lists",
  orderView: "list_pos",
  template: (d, key) => {
    const el = d.createElement("section");
    el.className = "list";
    el.dataset.key = key;

    const header = document.createElement("header");
    const title = document.createElement("span");
    const add = document.createElement("button");
    add.textContent = "+ card";
    add.addEventListener("click", () => {
      const cards = shaper.orderedChildren("card", key);
      // Order keys live encoded ("t:a0") in the shaper; fractional-index math
      // wants them bare.
      const last = cards.length ? decText(cards[cards.length - 1]![0]) : null;
      const res = JSON.parse(
        app.apply_new(
          "Card",
          ["title", "pos", "list"],
          [encText("New card"), encText(keyBetween(last, null)), key],
        ),
      );
      shaper.applyStep(res.deltas.views);
      shaper.el("card", res.id)?.querySelector("input")?.focus();
    });
    header.append(title, add);
    el.appendChild(header);

    // Drop target: reposition or reparent the dragged card.
    el.addEventListener("dragover", (e) => e.preventDefault());
    el.addEventListener("drop", (e) => {
      e.preventDefault();
      const cardKey = e.dataTransfer!.getData("text/rex-card");
      if (!cardKey) return;
      dropCard(cardKey, key, e.clientY);
    });
    return el;
  },
  attrs: [
    {
      view: "list_title",
      apply: (_d, el, v) => {
        el.querySelector("header > span")!.textContent = decText(v);
      },
    },
  ],
  children: [cardShape],
};

const shaper = new Shaper<HTMLElement>(driver, board, [listShape]);

/** Drop `cardKey` into `listKey` at the vertical position `clientY`:
 *  compute a fractional key between the neighbors, then move + reparent in ONE
 *  atomic update so the shaper sees a single reparent (not a torn
 *  move-in-old-list then reparent). */
function dropCard(cardKey: string, listKey: string, clientY: number): void {
  const siblings = shaper
    .orderedChildren("card", listKey)
    .filter(([, child]) => child !== cardKey);
  // Find the first sibling whose midpoint is below the pointer.
  let index = siblings.length;
  for (let i = 0; i < siblings.length; i++) {
    const el = shaper.el("card", siblings[i]![1])!;
    const rect = el.getBoundingClientRect();
    if (clientY < rect.top + rect.height / 2) {
      index = i;
      break;
    }
  }
  const lo = index > 0 ? decText(siblings[index - 1]![0]) : null;
  const hi = index < siblings.length ? decText(siblings[index]![0]) : null;
  const pos = keyBetween(lo, hi);

  step(app.update_fields(cardKey, ["pos", "list"], [encText(pos), listKey]));

  // Same-gap churn eventually grows keys; re-space when one crosses the limit.
  const plan = rebalancePlan(
    shaper.orderedChildren("card", listKey).map(([k, c]) => [decText(k), c] as const),
  );
  if (plan) {
    for (const [child, fresh] of plan) {
      step(app.update_field(child, "pos", encText(fresh)));
    }
  }
}

// Initial render: the engine has already integrated the program's seed data;
// read each rendered view once and present it as the first "delta" batch.
{
  const views: Record<string, [string, string, number][]> = {};
  for (const v of ["lists", "list_title", "list_pos", "card_list", "card_title", "card_pos"]) {
    views[v] = JSON.parse(app.read_view(v));
  }
  shaper.applyStep(views);
}

// A control to demonstrate list creation too.
{
  const controls = document.getElementById("controls")!;
  const addList = document.createElement("button");
  addList.textContent = "+ list";
  addList.addEventListener("click", () => {
    const lists = shaper.orderedChildren("list", "");
    const last = lists.length ? decText(lists[lists.length - 1]![0]) : null;
    const name = prompt("List name?") ?? "List";
    const res = JSON.parse(
      app.apply_new(
        "List",
        ["title", "pos"],
        [encText(name), encText(keyBetween(last, null))],
      ),
    );
    shaper.applyStep(res.deltas.views);
  });
  controls.appendChild(addList);
}
