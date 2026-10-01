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

/** One argument of an event: a canonically-encoded scalar (`"t:hello"`,
 *  `"#3:7"`, `"i:4"`), or — for a relation-typed parameter — its rows as
 *  `[key, value, weight]` (see `encodeRel`). */
export type EventArg = string | readonly (readonly [key: string, value: string, weight: number])[];

/** What one dispatched event did: the ids its `new` mutations minted (in
 *  order, canonically encoded) and the resulting delta batch. */
export interface DispatchResult {
  readonly ids: readonly string[];
  readonly deltas: StepDeltas;
}

/**
 * The engine, as the shaper's host sees it. This is the whole boundary: the
 * generated app asks the port for an initial snapshot, dispatches named events
 * to it, and hands every returned batch to `Shaper.applyStep`. Nothing here
 * says *where* the engine runs — `rex-runtime` implements it over the wasm
 * engine, but a worker, or a server pushing batches over a socket, can too.
 */
export interface EnginePort {
  /** The full contents of every view: the initial render, as one batch. */
  snapshot(): StepDeltas;
  /** Run a declared event as one atomic transaction. */
  dispatch(event: string, args: Readonly<Record<string, EventArg>>): DispatchResult;
  /** Re-space one manual-order level's keys (`[child, newKey]` rows) as one
   *  logged `@rebalance` transaction. */
  rebalance(field: string, rows: readonly (readonly [child: string, key: string])[]): StepDeltas;
}

/**
 * The DOM face the shaper drives. Abstract so tests can substitute a spy that
 * counts mutations — the shaper's correctness claims are mutation-count
 * claims ("a reorder is exactly one insertBefore").
 */
export interface DomDriver<El> {
  createElement(tag: string): El;
  /** A deep copy of `el` (no listeners): how a template stamps a row from
   *  its prototype skeleton. */
  clone(el: El): El;
  setText(el: El, text: string): void;
  setAttr(el: El, name: string, value: string): void;
  /** Insert (or move — the DOM treats attached-node insertion as a move). */
  insertBefore(parent: El, el: El, ref: El | null): void;
  removeChild(parent: El, el: El): void;
  /** How many child nodes `parent` has (the bulk-clear test). */
  childCount(parent: El): number;
  /** Remove every child of `parent` at once — the shaper's bulk path when a
   *  batch removes all of a parent's children (`tbody.textContent = ""`). */
  clear(parent: El): void;
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
export type AttrBinding<El> =
  | {
      readonly view: string;
      readonly presence?: false;
      readonly apply: (driver: DomDriver<El>, el: El, value: string) => void;
    }
  | {
      readonly view: string;
      readonly presence: true;
      readonly apply: (driver: DomDriver<El>, el: El, value: string | undefined) => void;
    };

export interface ShapeNode<El> {
  /** Unique name within the shape tree. */
  readonly name: string;
  /** View `child -> parent` (for the root level: `child -> anything`). */
  readonly membershipView: string;
  /** Build the skeleton element for a child (attrs are applied separately).
   *  `ancestors` are the enclosing rows' keys, nearest first (`[0]` is the
   *  parent row), so a listener can pass an enclosing binder to a dispatch. */
  readonly template: (driver: DomDriver<El>, key: string, ancestors: readonly string[]) => El;
  /** Attribute views and how each lands on the element.
   *
   *  A **value** attribute (`child -> value`) is applied only when the view
   *  has a value for the key. A **presence** attribute (`child -> child`, a
   *  coreflexive gate holding the key exactly when a class is on) is applied
   *  on absence as well, since the row going away *is* "turn the class off";
   *  its `value` is therefore `string | undefined`. */
  readonly attrs: readonly AttrBinding<El>[];
  /** Optional ordering view (`child -> fractional key`). Unordered levels
   *  append. */
  readonly orderView?: string;
  /** `order by … desc`: reverse the order view's key order (child key still
   *  breaks ties ascending). */
  readonly orderDesc?: boolean;
  /** The element inside a parent row's element that this level mounts into
   *  (default: the parent row's element itself). */
  readonly slot?: (root: El) => El;
  /** Identifies the slot element among a parent's child levels: levels with
   *  equal keys (absent counts as `""`) mount into one element, so the shaper
   *  keeps them in source order. */
  readonly slotKey?: string;
  /** The static node of the parent row's template that follows this level in
   *  the source, found right after the row is built (before any level has
   *  mounted into it). Rows of this level go before it; absent means the level
   *  is last in its slot element. */
  readonly anchor?: (root: El) => El | null;
  /** Nested shape levels; their membership views' parent column must be this
   *  level's child key. */
  readonly children: readonly ShapeNode<El>[];
}
