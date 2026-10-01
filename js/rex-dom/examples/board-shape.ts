import { decodeText, type ShapeNode } from "rex-dom";

// A shape tree describes WHAT to render for each view. It is generic in the
// element type, because a template only ever calls the driver: `HTMLElement`
// with `BrowserDriver` in a browser, `SpyEl` with `SpyDriver` in a test.
//
// This is `crates/rex-core/tests/fixtures/board.rex` written out by hand: lists
// ordered by `.pos`, each holding cards ordered by `.pos`.

export function boardShape<El>(): ShapeNode<El> {
  // Level 2: a card. Its membership view maps card -> the list it belongs to,
  // so a −/+ pair on it (a card moving lists) is one reparent.
  const card: ShapeNode<El> = {
    name: "board#list#card",
    membershipView: "board#list#card",
    template: (d) => d.createElement("div"),
    // Attribute views map card -> value; a −/+ pair is one `setText`.
    attrs: [{ view: "board#list#card#title", apply: (d, el, v) => d.setText(el, decodeText(v)) }],
    // The order view maps card -> sort key; a −/+ pair is one move.
    orderView: "board#list#card#order",
    children: [],
  };

  // Level 1: a list. A root level's membership view only says which rows
  // exist (its value column is ignored); rows mount into the container.
  return {
    name: "board#list",
    membershipView: "board#list",
    template: (d) => d.createElement("section"),
    attrs: [{ view: "board#list#title", apply: (d, el, v) => d.setText(el, decodeText(v)) }],
    orderView: "board#list#order",
    // Child levels mount into the parent row's element, keyed by the parent.
    children: [card],
  };
}
