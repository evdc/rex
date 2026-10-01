/**
 * Interaction helpers the compiler-generated app code calls (§M5).
 *
 * These are the honest runtime library: drop-geometry hit-testing and
 * fractional-key math are not relational, so they live here as plain
 * functions rather than pretending to be language constructs. The generated
 * event listeners materialize handler arguments through the `Extractor`
 * vocabulary (`drag`, `dropPos`, `endOf`, …) using these, then dispatch a
 * checked relational handler into the engine.
 */

import type { Shaper } from "./shaper.js";
import type { EnginePort } from "./types.js";
import { decodeText } from "./encode.js";
import { keyBetween, rebalancePlan } from "./rebalance.js";

/** The MIME type Rex drag/drop carries a row key under. One universal type
 *  keeps a drag source and a `drag(...)` extractor in sync without threading
 *  the surface mime string through codegen. */
export const DRAG_MIME = "application/rex-key";

/** Make `el` a drag source carrying `key`. */
export function makeDraggable(el: HTMLElement, key: string, mime: string = DRAG_MIME): void {
  el.draggable = true;
  el.addEventListener("dragstart", (e) => {
    e.dataTransfer!.setData(mime, key);
    el.classList.add("dragging");
  });
  el.addEventListener("dragend", () => el.classList.remove("dragging"));
}

/** Make `el` accept drops (the generated `drop` listener does the work). */
export function makeDropTarget(el: HTMLElement): void {
  el.addEventListener("dragover", (e) => e.preventDefault());
}

/** The `drag(...)` extractor: the dragged key, already an encoded id. */
export function dragValue(e: DragEvent, mime: string = DRAG_MIME): string {
  return e.dataTransfer!.getData(mime);
}

/** The `endOf(child)` extractor: a fresh fractional key placing an item after
 *  `parentKey`'s last *displayed* child of the given level (under `desc`, that
 *  is the smallest key, so the new key goes below it). Returns a BARE key; the
 *  caller encodes it. */
export function endOf<El>(
  shaper: Shaper<El>,
  childLevel: string,
  parentKey: string,
): string {
  const children = shaper.orderedChildren(childLevel, parentKey);
  const last = children.length ? decodeText(children[children.length - 1]![0]) : null;
  return shaper.orderDesc(childLevel) ? keyBetween(null, last) : keyBetween(last, null);
}

/** The `dropPos(exclude)` extractor: a fractional key placing an item at the
 *  pointer's vertical position among `parentKey`'s children, excluding the
 *  dragged `excludeKey`. Returns a BARE key; the caller encodes it. */
export function dropPos<El>(
  shaper: Shaper<El>,
  childLevel: string,
  parentKey: string,
  clientY: number,
  excludeKey: string,
): string {
  const siblings = shaper
    .orderedChildren(childLevel, parentKey)
    .filter(([, child]) => child !== excludeKey);
  let index = siblings.length;
  for (let i = 0; i < siblings.length; i++) {
    const el = shaper.el(childLevel, siblings[i]![1]) as unknown as HTMLElement | undefined;
    if (!el) continue;
    const rect = el.getBoundingClientRect();
    if (clientY < rect.top + rect.height / 2) {
      index = i;
      break;
    }
  }
  // `before`/`after` are display neighbors; under `desc` the larger key is
  // the one displayed first.
  const before = index > 0 ? decodeText(siblings[index - 1]![0]) : null;
  const after = index < siblings.length ? decodeText(siblings[index]![0]) : null;
  return shaper.orderDesc(childLevel) ? keyBetween(after, before) : keyBetween(before, after);
}

/** Re-space a level's order keys under `parentKey` when same-gap churn has
 *  grown one past the rebalance limit. A rare amortized maintenance sweep —
 *  deliberately runtime library, not a language construct. Sends ONE
 *  `@rebalance` event covering every re-spaced row (a drag storm that triggers
 *  a sweep is one entry in the log, not N torn field writes) and applies the
 *  batch it returns to `shaper`. */
export function maybeRebalance<El>(
  port: Pick<EnginePort, "rebalance">,
  shaper: Shaper<El>,
  childLevel: string,
  parentKey: string,
  field: string,
  encode: (bare: string) => string,
): void {
  const ordered = shaper
    .orderedChildren(childLevel, parentKey)
    .map(([k, c]) => [decodeText(k), c] as const);
  // The plan hands out ascending keys in list order, so feed it key order.
  if (shaper.orderDesc(childLevel)) ordered.reverse();
  const plan = rebalancePlan(ordered);
  if (!plan) return;
  const rows = Array.from(plan, ([child, fresh]) => [child, encode(fresh)] as const);
  shaper.applyStep(port.rebalance(field, rows));
}
