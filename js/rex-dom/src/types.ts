/**
 * The engine↔shaper contract.
 *
 * One `step()` on the Rust side produces one `StepDeltas` batch: for each
 * named view, the Z-set delta as `[key, value, weight]` tuples, with keys and
 * values in the engine's canonical string encoding (`rex::eval::encode`).
 * The batch is atomic and consistent across views (the engine computes every
 * view against one frozen snapshot per step), so one batch = one DOM
 * transaction.
 */

/** One Z-set delta row: (left, right, weight). */
export type Tuple = readonly [key: string, value: string, weight: number];

/** One step's per-view deltas, as decoded from the engine's JSON. */
export type StepDeltas = Record<string, readonly Tuple[]>;

/** Parse the engine's `{"views":{...}}` JSON into a StepDeltas. */
export function parseStepJson(json: string): StepDeltas {
  const parsed = JSON.parse(json) as { views: StepDeltas };
  return parsed.views;
}

/**
 * The DOM face the shaper drives. Abstract so tests can substitute a spy that
 * counts mutations — the shaper's correctness claims are mutation-count
 * claims ("a reorder is exactly one insertBefore").
 */
export interface DomDriver<El> {
  createElement(tag: string): El;
  setText(el: El, text: string): void;
  setAttr(el: El, name: string, value: string): void;
  /** Insert (or move — the DOM treats attached-node insertion as a move). */
  insertBefore(parent: El, el: El, ref: El | null): void;
  removeChild(parent: El, el: El): void;
}

/**
 * One level of the shape tree: which relations render as what.
 *
 * Views are classified **by role, not by delta inspection**:
 *  - `membershipView : child -> parent` is structural — a −/+ pair at the
 *    same child key is a *reparent*, a bare + is a mount, a bare − a remove.
 *  - `orderView : child -> fractional key` — a −/+ pair is a *move*.
 *  - each `attrs[i].view : child -> value` — a −/+ pair is an *update*.
 *
 * The root level's `parent` column is ignored: its children mount under the
 * container element the Shaper was constructed with.
 */
export interface ShapeNode<El> {
  /** Unique name within the shape tree. */
  readonly name: string;
  /** View `child -> parent` (for the root level: `child -> anything`). */
  readonly membershipView: string;
  /** Build the skeleton element for a child (attrs are applied separately). */
  readonly template: (driver: DomDriver<El>, key: string) => El;
  /** Attribute views (`child -> value`) and how each lands on the element. */
  readonly attrs: readonly {
    readonly view: string;
    readonly apply: (driver: DomDriver<El>, el: El, value: string) => void;
  }[];
  /** Optional ordering view (`child -> fractional key`). Unordered levels
   *  append. */
  readonly orderView?: string;
  /** Nested shape levels; their membership views' parent column must be this
   *  level's child key. */
  readonly children: readonly ShapeNode<El>[];
}
