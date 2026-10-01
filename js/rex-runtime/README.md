# rex-runtime

Runs a [Rex](../../README.md) app in the browser: it loads the wasm engine,
**persists every change as an event log**, restores the app on reload, and
hands the shaper a typed engine to talk to. It is the browser half of the
engine boundary; [`rex-dom`](../rex-dom/README.md) is the DOM half and has no
dependency on this package (it is a peer, used for types only).

```sh
npm install rex-runtime rex-dom
```

```
generated app ──► Engine ──► wasm RexApp   (rex-runtime/wasm)
  (rex build)       │
                    └──► PersistenceAdapter   (IndexedDB, memory, yours)
```

`rex build` generates the code that uses this; you only touch it directly to
write an adapter, embed an app by hand, or test one.

## Using it

```ts
import init, { RexApp } from "rex-runtime/wasm";
import { boot, IndexedDbAdapter, programKey } from "rex-runtime";
import { BrowserDriver, Shaper } from "rex-dom";
import PROGRAM from "./app.rex?raw";   // the program source; the engine compiles it at boot

await init();                           // fetch + compile the wasm
const engine = await boot({
  RexApp,
  program: PROGRAM,
  adapter: new IndexedDbAdapter(programKey("app", PROGRAM)),
});

const shaper = new Shaper(new BrowserDriver(), document.getElementById("app")!, [/* ShapeNode tree */]);
shaper.applyStep(engine.snapshot());    // initial render

const { ids, deltas } = engine.dispatch("AddTodo", { text: "t:buy milk" });
shaper.applyStep(deltas);
```

`boot` returns an **`Engine`**: rex-dom's `EnginePort` (`snapshot`, `dispatch`,
`rebalance`) with all the JSON hidden behind typed calls, plus
`logSince(seq)`, `baseSnapshot()`, `restore()`, `replay()`, and — on what
`boot` returns — `flush()` and `saveSnapshot()`. `engine.wasm` is the raw
`RexApp`: an escape hatch for measurement and debugging (a write made through it
skips persistence until the next normal write).

Event arguments are canonically-encoded strings (`encodeText("buy milk")` from
`rex-dom`, an id passed back verbatim, …), or rows `[key, value, weight]` for a
relation-typed parameter. An event is **one atomic transaction** against the
state as it was before the event.

### Bundlers

`rex-runtime/wasm` is the wasm-bindgen glue (`pkg/`), which finds its `.wasm`
with `new URL("rex_wasm_bg.wasm", import.meta.url)`. Vite's dev-server
pre-bundling breaks that, so exclude the package:

```ts
// vite.config.ts
export default defineConfig({
  build: { target: "esnext" },          // generated apps use top-level await
  optimizeDeps: { exclude: ["rex-runtime"] },
});
```

If you link the package from outside your project root (`file:../…`) rather than
installing it, Vite also needs `server: { fs: { allow: [...] } }` to serve the
`.wasm`. An installed copy under `node_modules` does not.

## The event log

The engine's only write path is a **named event**, and every accepted event is
appended to an append-only log: `{ seq, name, args, cause?, intent? }`. Two
events are not declared in your program:

- `@genesis` — one per `new` statement in the program source, so the log is
  complete from an empty engine;
- `@rebalance` — a manual-order level re-spacing its keys (one event for the
  whole sweep).

Handlers are deterministic functions of the pre-event state and the event's
arguments, so **replaying the log reproduces the state exactly**. Anything
non-deterministic — a timestamp, a random label, an id minted by the DOM — is
computed by the generated listener and *passed as an argument*, so it is in the
log and replay never re-rolls it.

## Persistence

`boot` makes the log durable. A `PersistenceAdapter` is four calls:

```ts
interface PersistenceAdapter {
  loadSnapshot(): Promise<StoredSnapshot | null>;   // { json, cursor }
  saveSnapshot(snapshot: StoredSnapshot): Promise<void>;
  appendEvents(events: readonly LoggedEvent[]): Promise<void>;
  eventsSince(seq: number): Promise<LoggedEvent[]>; // seq order
}
```

- **First load** (nothing stored): run the program — its `new` statements log
  their `@genesis` events — and persist that log.
- **Reload**: boot with `RexApp.forRestore` (no seed data: it would double up
  with the restored rows), `restore` the latest snapshot if there is one, then
  *silently* `replay` every event after its cursor. A page killed before its
  first snapshot has only the log, and the log alone rebuilds everything.
- **While running**: each `dispatch`/`rebalance` appends the engine's new log
  tail. A snapshot of the engine's base tables is saved every `snapshotEvery`
  events (default 50) and when the page is hidden (`visibilitychange`,
  `pagehide`). **A snapshot is only an optimisation**; losing one loses nothing.

Appends are serialised, never throw into `dispatch`, and are reported through
`onError`. A failed append is retried with the next one (so the stored log never
has a hole), and `await engine.flush()` waits for everything issued so far — use
it before navigating away in a test.

Adapters shipped: `IndexedDbAdapter(name)` (one database per program,
`rex:<name>`) and `MemoryAdapter` (for tests and `?ephemeral`). To write your
own, an event already stored under its `seq` must be left alone, so that a
retried append is harmless.

**`programKey(name, source)`** hashes the program text into the database name,
so an edited program starts clean instead of replaying another version's log.
The consequence: **changing a program discards that browser's saved state.**
Generated apps use `?ephemeral` (or `VITE_REX_PERSIST=0` at build time) to swap
in `MemoryAdapter`.

## Profiling

`profiler(true)` — generated apps enable it with `?profile` — times each
dispatch in three parts and exposes the rows as `window.__rexProfile.timings`:
the wasm step plus its log read (`engine`), `JSON.parse` of the result
(`parse`), and `shaper.applyStep` (`shaper`). It is `null` when disabled, so it
costs nothing per dispatch.

## Testing without wasm

`boot` takes the wasm class as an argument rather than importing it, so the
runtime runs under any object with `RexApp`'s shape (`EngineApp`/`EngineClass`).
`test/boot.test.ts` boots a small fake with a `MemoryAdapter` and covers
restore, replay, snapshot pacing and failing adapters with no wasm and no browser.

```sh
npm test                    # vitest
npm run build               # tsc -> dist/ (needs ../rex-dom built, for types)
../../scripts/build-wasm.sh # builds pkg/, the rex-runtime/wasm export
```
