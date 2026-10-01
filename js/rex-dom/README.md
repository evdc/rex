# rex-dom

The delta shaper and DOM bridge for the [Rex](../../README.md) incremental
engine: it turns **one atomic batch of view deltas into one atomic DOM
transaction**, and does so without ever tearing down a node that merely
changed. It has no dependency on the engine — it consumes the delta protocol
below, from anywhere — and no dependency on the DOM, which it reaches through a
small driver interface.

```
engine ──► StepDeltas ──► Shaper.applyStep ──► DomDriver ──► DOM
 (wasm,      one batch       (mirrors, role      insertBefore,
  worker,                     classification)    setText, …)
  server)
```

Most apps never write against this package by hand: `rex build` generates a
module that builds the `ShapeNode` tree, wires the listeners and calls the
shaper. This document is the contract that module — and any other producer of
deltas or consumer of the DOM — relies on.

```sh
npm install rex-dom
```

Pair it with [`rex-runtime`](../rex-runtime/README.md) to run the engine in the
browser; `rex-runtime` implements the `EnginePort` below.

## The delta protocol

The engine computes every view against one frozen snapshot per step, so a step
is **atomic and consistent across views**. It reports what changed as a
`StepDeltas`:

```ts
type Tuple = readonly [key: string, value: string, weight: number];
type StepDeltas = Record<string, readonly Tuple[]>; // view name -> delta rows
```

Each row is one Z-set entry: `weight` is `+1` (the pair is now present) or `-1`
(it no longer is). A value that *changes* is therefore a pair at the same key —
`[key, old, -1]` and `[key, new, +1]` — and a brand new key is a lone `+1`. The
initial render is the same thing: a batch of `+1` rows covering every view
(`EnginePort.snapshot()`). On the wire it is `{"views": {...}}`; `parseStepJson`
unwraps it.

Keys and values are strings in the engine's canonical encoding, which the
shaper treats as opaque identity (it never decodes a key):

| Encoding | Meaning | JS helper |
|---|---|---|
| `t:hello` | Text (`\ , ( )` backslash-escaped) | `encodeText` / `decodeText` |
| `i:42` | Int | `encodeInt` / `decodeInt` |
| `m:1999` | Money, in minor units | `encodeMoney` / `decodeMoney` |
| `@Active` | Atom (a constructor of a `type`, or a Bool: `@True`) | `encodeAtom` / `decodeAtom` |
| `#3:7` | An entity id: sort 3, row 7 (minted by the engine) | pass through verbatim |
| `u` | Unit | — |

A real step — renaming a card, from
[`crates/rex-core/tests/fixtures/steps/01-rename.json`](../../crates/rex-core/tests/fixtures/steps/01-rename.json):

```json
{"views":{"board#list#card#title":[["#1:0","t:Design",-1],["#1:0","t:Design the schema",1]]}}
```

### Views have roles, and the roles decide the DOM operation

The shaper never inspects values to guess what happened. Each view named in a
`ShapeNode` has a declared **role**, and a −/+ pair at one key means a
different single operation in each:

| Role | Shape field | View maps | A lone `+` | A −/+ pair at one key | A lone `−` |
|---|---|---|---|---|---|
| Membership | `membershipView` | child → parent key | mount | **reparent**: the same element, moved | remove |
| Order | `orderView` | child → order key | — | **move** within its parent | — |
| Attribute | `attrs[i].view` | child → value | apply | **update**: one `apply` on the same element | — |

That is the guarantee the whole design exists for: **a change is one operation
on the same node, never a remove plus a mount.** Focus, selection, scroll
position, an in-flight animation and a half-typed input all live on the node, so
destroying and rebuilding it silently loses them. The conformance fixtures below
pin the counts: a rename is exactly one `setText`, a reorder or a reparent
exactly one `insertBefore`, a delete exactly one `removeChild`.

A **presence** attribute (`presence: true`) is one whose view holds the key
exactly when the thing is *on* — a CSS class, `checked`, `hidden`. Its
retraction is meaningful ("turn it off"), so the shaper calls `apply` with
`undefined` when the row goes away; an ordinary attribute has nothing to apply
without a value.

## What `applyStep` does

One call, one synchronous DOM transaction, in this order:

0. **Integrate and classify.** Every mirrored view's rows are folded into the
   shaper's own copy of that view, and each level's membership delta is sorted
   into mounts, removes and reparents.
1. **Removes**, top-down and subtree-coalesced: one `removeChild` per dead
   subtree root, grouped by parent — and a single `clear` when a batch removes
   every child a parent has.
2. **Mounts**, shallowest level first. A new row's element is built from the
   mirrors (its template plus every attribute that already has a value), so a
   mounted row is complete when it is inserted. Large batches under one parent
   are merged in one pass.
3. **Field updates**: each −/+ on an attribute view at a mounted row is one
   `apply`. Rows mounted *this* batch are skipped; they were built with their
   values.
4. **Moves and reparents**, against the final membership, positioned with the
   order index.

Rows are ordered by `orderView`, compared by the *encoding tag* of the key
(`i:`/`m:`/ids numerically, everything else as strings — so `i:3` sorts before
`i:10`), with the child key as the ascending tiebreak; `orderDesc` reverses the
order keys. An unordered level appends.

## Writing a `ShapeNode` tree

A `ShapeNode<El>` is one level of the tree: which view says which rows exist,
how to build one row's element, and which views feed it. Here is the whole
Kanban board shape — a hand-written version of what `rex build` generates for
`crates/rex-core/tests/fixtures/board.rex` (this file,
[`examples/board-shape.ts`](examples/board-shape.ts), is a real test input; see
*Conformance*):

<!-- example: examples/board-shape.ts -->
```ts
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
```

Driving it with the real engine's output:

```ts
import { readFileSync } from "node:fs";
import { Shaper, SpyDriver, parseStepJson } from "rex-dom";
import { boardShape } from "./examples/board-shape";

const fixture = (name: string) =>
  parseStepJson(readFileSync(`crates/rex-core/tests/fixtures/steps/${name}.json`, "utf8"));

const driver = new SpyDriver();
const root = driver.createElement("main");
const shaper = new Shaper(driver, root, [boardShape()]);

shaper.applyStep(fixture("00-mount"));   // two lists, three cards, in order
driver.resetCounts();
shaper.applyStep(fixture("01-rename"));  // the card's title changes…
driver.counts;                           // …{ setText: 1, everything else: 0 }
```

### `ShapeNode` reference

| Field | Meaning |
|---|---|
| `name` | Unique within the tree. `shaper.el(name, key)` finds a mounted row. |
| `membershipView` | `child -> parent`. For a root level the value column is ignored (rows mount into the container). A level nested under `p` must hold `p`'s row keys in its value column. |
| `template(driver, key, ancestors)` | Build the row's element (attributes are applied separately). `ancestors` are the enclosing rows' keys, nearest first — what a listener needs to pass an enclosing row to an event. Called once per mounted row. |
| `attrs` | `{ view, apply, presence? }[]` — each view feeds one `apply(driver, el, value)`. |
| `orderView?` | `child -> order key`. Absent: rows append. |
| `orderDesc?` | Reverse the order keys. |
| `slot?(root)` | The element *inside* the parent row that this level mounts into (default: the parent row's element). |
| `slotKey?` | Names the slot element so sibling levels sharing it stay in source order (absent counts as `""`). |
| `anchor?(root)` | The parent template's static node that follows this level; the level's rows go before it. Evaluated right after the parent row is built, before anything has mounted into it. |
| `children` | Nested levels. |

`slot`, `slotKey` and `anchor` exist for a parent template like
`ul { <rows> } footer`, where several levels (a `select`, an `if`) mount into
one element alongside static markup: whichever mounts first, the DOM comes out
in source order.

## The driver

The shaper reaches the DOM only through `DomDriver<El>`, which is why its
claims are testable as *mutation counts*:

```ts
interface DomDriver<El> {
  createElement(tag: string): El;
  clone(el: El): El;                                // deep copy, no listeners
  setText(el: El, text: string): void;
  setAttr(el: El, name: string, value: string): void;
  insertBefore(parent: El, el: El, ref: El | null): void; // also a move, as in the DOM
  removeChild(parent: El, el: El): void;
  childCount(parent: El): number;
  clear(parent: El): void;                          // remove all children at once
}
```

`BrowserDriver` is the thin DOM veneer. `SpyDriver` is a DOM-free tree that
counts every call (`driver.counts`) and models a real child list, so ordering is
assertable too. To target something else (a canvas, a terminal, a virtual DOM),
implement the interface; `insertBefore` of an attached element **must** behave
as a move, because that is how reparent and reorder keep node identity.

## The engine port

```ts
interface EnginePort {
  snapshot(): StepDeltas;
  dispatch(event: string, args: Readonly<Record<string, EventArg>>): DispatchResult; // { ids, deltas }
  rebalance(field: string, rows: readonly (readonly [child, key])[]): StepDeltas;
}
```

This is the shaper host's entire view of the engine: get a snapshot, send named
events, apply what comes back with `shaper.applyStep`. An `EventArg` is a
canonically-encoded scalar, or — for a relation-typed parameter — rows of
`[key, value, weight]` (`encodeRel` builds them from a JS array). Nothing says
where the engine runs, so a worker, or a server pushing batches over a socket,
can implement it without touching the shaper. [`rex-runtime`](../rex-runtime/README.md)
implements it over the wasm engine.

## Interaction helpers

Drop geometry and fractional-key arithmetic are not relational, so they live
here as plain functions the generated listeners call:
`makeDraggable`, `makeDropTarget`, `dragValue` (the `drag(...)` extractor),
`endOf` and `dropPos` (fresh fractional order keys — via the published
[`fractional-indexing`](https://www.npmjs.com/package/fractional-indexing)
library, not hand-rolled), and `maybeRebalance`, which re-spaces a parent's keys
when repeated insertion into one gap has grown one past `REBALANCE_LIMIT` —
sent to the engine as **one** logged `@rebalance` event, not N field writes.

## Conformance

The delta shape is tested from both ends against the same files.
[`crates/rex-core/tests/contract_fixtures.rs`](../../crates/rex-core/tests/contract_fixtures.rs)
drives a scripted Kanban history through the real engine and writes each step's
delta JSON to
[`crates/rex-core/tests/fixtures/steps/`](../../crates/rex-core/tests/fixtures/steps)
(`UPDATE_FIXTURES=1 cargo test -p rex --test contract_fixtures` regenerates
them). [`test/contract.test.ts`](test/contract.test.ts) applies those same files
through the real shaper with a `SpyDriver` and asserts the mutation counts, and
[`test/readme.test.ts`](test/readme.test.ts) checks that the example above is
the file the tests run. A change to the engine's delta shape, or to the shaper's
role classification, fails a test on the other side.

```sh
npm test            # vitest, no wasm and no browser needed
npm run build       # tsc -> dist/
```
