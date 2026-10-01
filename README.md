# Rex

A relational language for whole applications: schema, derived state, event
handlers and UI in one `.rex` program, **incrementally maintained end to end**.

The core is a point-free, binary-relational **view language**: every expression
denotes a binary relation `A → B`, relations compose like arrows in a category,
and entities are families of keyed columnar relations. Programs lower to a
**DBSP circuit**, so a change to the data — an insert, a delete, a correction,
all as Z-set deltas — updates every view by doing work proportional to the
change. A `view` is a nested query with element constructors; its deltas drive
the DOM directly, one mutation per logical change, with no virtual DOM.

```rex
entity Todo { text: Text, completed: Bool }
event Toggle(t: Todo)
on Toggle(t) => t.completed := not t.completed

let active : Unit -> Int = count((Todo where not .completed) by unit)

view main = section {
  ul { Todo as t order by id select
    li(class.done=.completed on click => do Toggle(t)) { .text } }
  span { active } " left"
}
```

This repository holds the compiler, a batch reference interpreter, the
incremental engine (Rust, compiled to WASM), a UI compiler (`rex build`), and
two TypeScript packages that run the result in a browser.

| Document | What it is |
|---|---|
| [`SYNTAX.md`](SYNTAX.md) | The surface language, as implemented: schema, state, events, queries, views. **Start here to write a program.** |
| [`SPEC.md`](SPEC.md) | The core model and why: Z-sets, the combinators, aggregation, negation, recursion, the application layer. Working notes, kept current. |
| [`js/rex-dom/README.md`](js/rex-dom/README.md), [`js/rex-runtime/README.md`](js/rex-runtime/README.md) | The delta protocol and shaper; boot, the event log and persistence. |
| [`ROADMAP.md`](ROADMAP.md) | Strategy, architecture decisions with their measurements, milestones. |
| [`MVP-PLAN.md`](MVP-PLAN.md) | The MVP story breakdown, with what each story actually landed as. |
| [`PERF-PLAN.md`](PERF-PLAN.md), [`SYNC.md`](SYNC.md) | Post-MVP: engine performance work (in progress) and the multi-replica design. |
| `nesting-draft.md`, `drafts.md` | Earlier design notes (nesting/shaper/effects; loose ideas), kept for the reasoning. |

> **Status (October 2026): the MVP is functionally complete.** Three apps —
> [Kanban](examples/kanban/src/board.rex), [TodoMVC](examples/todomvc/src/app.rex)
> and the [js-framework-benchmark](examples/js-framework-benchmark/src/app.rex)
> — and a fourth, [chat](examples/chat/src/app.rex), each build from one `.rex`
> file with no hand-written per-app code, run in
> the browser on the generated module, and pass Playwright suites for node
> identity, focus and one DOM operation per logical change. Every change enters
> as a **named event** in an append-only log; a reload restores the app from a
> snapshot plus log replay. `rex-dom` and `rex-runtime` are standalone packages
> with a documented delta contract. See *Status against the MVP* below for
> what is and is not done.

---

## Building & running

Workspace layout:

| Path | What |
|---|---|
| `crates/rex-core` | Library (`rex`): lexer, parser, checker/elaborator, `view` desugar, batch interpreter, DBSP engine, value encoding. |
| `crates/rex-cli` | The `rex` binary: `check`, `run`, `build [--watch]` (UI codegen), and the REPL. |
| `crates/rex-codegen` | Shape IR → generated TypeScript (`main.ts`: shape tree, templates, event wiring). |
| `crates/rex-wasm` | `RexApp`, the wasm-bindgen API: named-event `dispatch`, `rebalance`, `snapshot`, and the log/snapshot calls (`log_since`, `replay`, `base_snapshot`, `restore`). Built into `js/rex-runtime/pkg/`. |
| `js/rex-runtime` | [`rex-runtime`](js/rex-runtime/README.md): the wasm engine (`rex-runtime/wasm`), a typed `Engine` implementing rex-dom's `EnginePort`, and boot + persistence (IndexedDB snapshot + event log, S-80). |
| `js/rex-dom` | [`rex-dom`](js/rex-dom/README.md): the engine-free TS shaper + delta contract: −/+ fusion, phased apply, fractional ordering, drag/drop helpers. |
| `examples/kanban` | End-to-end app (Vite + Playwright). |
| `examples/js-framework-benchmark` | The keyed benchmark on Rex: 10k-row bulk events, `import js` extractors (Vite + Playwright). |
| `examples/todomvc` | TodoMVC, the S-02 program unchanged (Vite + Playwright). |
| `examples/chat` | Chat: a many-to-many through a keyed link entity, a state with no default, and handlers with guards and an `if` (Vite + Playwright). |

```sh
cargo build
cargo test --workspace         # ~420 tests incl. the oracle and fuzz suites (see Test coverage)
cargo run -p rex-cli                         # start the REPL
cargo run -p rex-cli -- check app.rex        # diagnostics only; exit 1 on errors
cargo run -p rex-cli -- run prog.rex         # check, then batch-evaluate and print every view
cargo run -p rex-cli -- build app.rex -o app.ts   # compile `view`s to a TS module
cargo run -p rex-cli -- build app.rex -o app.ts --watch   # ...and rebuild on every save
./scripts/build-wasm.sh                      # wasm engine -> js/rex-runtime/pkg/
./scripts/build-js.sh                        # rex-dom + rex-runtime -> dist/ (the examples import these)
(cd js/rex-dom && npx vitest run)            # shaper tests
(cd js/rex-runtime && npx vitest run)        # boot / persistence tests
```

`rex check` takes several files, prints diagnostics as `file:line:col` with the
offending line underlined, and exits 0 (clean), 1 (errors; or warnings with
`--deny-warnings`) or 2 (unreadable file, bad flag). `rex run` checks, then
prints each view's materialized contents (`--ast` adds the s-expression);
`rex prog.rex` still means `rex run prog.rex`. `cargo run -p rex-cli --
run crates/rex-core/tests/fixtures/spec12.rex` is the best core demo.

### Running the examples

Each example is a Vite app around one `.rex` file. You need Rust with the
`wasm32-unknown-unknown` target, the `wasm-bindgen` CLI at the version pinned
in `Cargo.lock`, and Node 20+.

```sh
# once
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126   # build-wasm.sh says if this is the wrong version
(cd js/rex-dom && npm install) && (cd js/rex-runtime && npm install)

# after changing the engine (crates/) or the packages (js/)
./scripts/build-wasm.sh     # the engine      -> js/rex-runtime/pkg/
./scripts/build-js.sh       # the JS packages -> js/*/dist/

# run one: kanban, todomvc, chat or js-framework-benchmark
cd examples/chat
npm install
npm run dev                 # http://localhost:5173 — the page reloads when src/main.ts changes
```

`src/main.ts` in each example is **generated** and checked in. After editing
the `.rex` file, regenerate it — or leave `--watch` running beside `npm run dev`:

```sh
cargo run -p rex-cli -- build examples/chat/src/app.rex -o examples/chat/src/main.ts --watch
#   kanban's program is examples/kanban/src/board.rex; the others are src/app.rex
```

State persists in the browser's IndexedDB, keyed by the program's text, so a
reload brings it back and an edited program starts clean. Add `?ephemeral` to
the URL to keep nothing, and `?profile` to log each event's engine / parse /
shaper time.

```sh
npm run build && npm run preview    # a production build, served locally
npx playwright install chromium     # once, then:
npx playwright test                 # the example's browser suite (starts its own server)
```

The wasm glue (`js/rex-runtime/pkg/`) and the packages' `dist/` are build
output, not committed; every example imports them as `rex-runtime/wasm`,
`rex-runtime` and `rex-dom`. `npm pack` in `js/rex-dom` and `js/rex-runtime`
produces the two publishable tarballs.

The REPL (`crates/rex-cli/src/repl.rs`) accepts `entity`/`let` statements
(committed to the session) and bare expressions (evaluated in a scratch copy
and discarded). It is for the query core: `view`, `state`, `rel`, `event`, `on`,
`type` and `import` are rejected there — use `rex check` / `rex build`.
Commands: `/import <path>`, `/env`, `/reset`, `/help`, `/quit`.

---

## Pipeline & architecture

Source flows through the front end once; what comes out feeds two evaluators
(which must always agree) and the UI compiler:

```
src → lex → parse → desugar → check/elaborate ─┬─► batch interpreter   (the reference)
                    (views,      │             ├─► DBSP circuit        (the engine)
                     state,   TProgram         └─► ShapeIR + EventIR ─► rex build ─► main.ts
                     events)   (typed)
```

| Phase | Module(s) | What it does |
|---|---|---|
| **Lex** | `crates/rex-core/src/lexer.rs`, `crates/rex-core/src/token.rs` | Hand-written; ASCII syntax, any UTF-8 in strings and comments. Maximal-munch operators; distinguishes decimal `.` from compose `.`; dates, atoms `@foo`, strings with escapes. Recovers and collects diagnostics. |
| **Parse** | `crates/rex-core/src/parser.rs`, `crates/rex-core/src/ast.rs` | Recursive descent for statements, Pratt (binding-power) for expressions. Produces a surface AST that mirrors what was written — **no desugaring here**. Recovers to the next statement on error; nesting is capped at 128 levels so no later pass can run out of stack. |
| **Desugar** | `crates/rex-core/src/types/view.rs`, `component.rs`, `names.rs`, `shape_ir.rs` | `view`, `state`, `local`, components, `match`/`if` and `event`/`on` become ordinary entities and `let`s (membership / order / per-attribute views, hidden keyset views for `where` targets), plus the **ShapeIR** (what to render) and **EventIR** (checked handler bodies). The core language never sees them. |
| **Check / elaborate** | `crates/rex-core/src/types/check.rs`, `ground.rs`, `strat.rs` | The heart. A single bidirectional, type-directed walk validates the program *and* produces the elaborated `TProgram`: field paths resolved to `(sort, field)` hops, identifiers resolved to their kind (view / entity-identity / value), filter built-ins lowered to `Filter` nodes. Threads an ambient domain through composition to resolve `.field`. |
| **Eval** | `crates/rex-core/src/eval/` | Batch interpreter over the typed AST — the definition of the semantics, and the oracle the engine is tested against. `interp.rs` walks `TProgram`; `algebra.rs` is the relational algebra; `relation.rs` is the Z-set store; `value.rs` is the runtime domain element. |
| **Support** | `crates/rex-core/src/diagnostic.rs`, `crates/rex-core/src/span.rs`, `crates/rex-core/src/pretty.rs`, `crates/rex-core/src/operator.rs` | Spans + rendered diagnostics; canonical s-expr printer (test assertions, `rex run --ast`); the §7 operator-metadata table. |
| **Incremental engine** | `crates/rex-core/src/dbsp/`, `events.rs` | `lower.rs` lowers `TProgram` to a circuit of delta nodes (`node.rs`), with algebraic rewrites and node sharing; `circuit.rs` steps it (incl. fix regions); `integral.rs` holds state (dense columns for entity-keyed relations); `engine.rs` is the transactional write path and the event log (`log.rs`); `events.rs` dispatches a named event as one transaction, and replays and restores. |
| **Codegen / edge** | `crates/rex-codegen`, `crates/rex-wasm`, `crates/rex-core/src/eval/encode.rs`, `js/rex-dom`, `js/rex-runtime` | ShapeIR → TS; the WASM API; the canonical value/delta wire encoding; the shaper that turns per-view deltas into DOM mutations; boot, the persisted log, and recovery. |

Key design choices realized in code:

- **Entities are identity relations** (§3.3). `entity Customer {…}` mints a fresh
  `SortId`, per-field keyed relations `(sort, field) → BTreeRelation`, and treats
  the bare name `Customer` as the diagonal `CustID → CustID`. `id` is the generic
  version, resolved against the ambient domain.
- **`.` vs `[]`** (§3.2). Both are the same join; `Compose` keeps outer columns,
  `Semijoin` keeps the left's columns. The historically-confusable mistake
  (`[]` against a value column) is a genuine **type error** — see the test
  `semijoin_on_value_column_is_a_type_error`.
- **One desugar rule surfaces in the typed AST**, not the parser: `by` stays as
  the idiom `~Y . X`; filter comparisons fold into `Filter`; `except`/`antijoin`
  both become `Antijoin` (evaluated as `A − A[B]`, the weight-safe form, §6).
- **Z-sets everywhere** (§2). `BTreeRelation` maps `left → right → i64 weight`,
  prunes zero-weight entries, and supports negative weights (retractions) — the
  batch evaluator and the circuit share it, and its kernels (`eval/algebra.rs`).
- **Money is exact**, stored as integer cents; arithmetic wraps (SPEC §2.1).
- **Sugar desugars; the core never widens.** Views, state, components, `match`
  and events all become entities, `let`s and data before the checker runs, so
  none of them adds a node to the typed IR, either evaluator, or the circuit.
- **Events are the only write path, and reads see the pre-event state.** One
  event is one logged, atomic transaction; handlers are deterministic, so the
  log replays exactly (SPEC §14).
- **Binders are keys, never relations**, so a generated listener closes over
  exactly the key the shaper hands its template.

---

## What exists

**The core language**
- Lexer, parser, s-expression printer; a bidirectional checker with
  type-directed `.field` resolution, ambient-domain threading, co-keyed /
  join-column diagnostics and atom/coproduct subtyping; groundedness and
  stratification passes.
- Combinators: compose, semijoin/`where`, inverse, fork with `fst`/`snd`,
  union `|`, intersect `&`, `except`/`antijoin`, `distinct`, `by`.
- Arithmetic `+ - * / %` and `++` on co-keyed columns; comparisons (filter
  form and column-vs-column); `in`; `match`, `if … then … else`, relational
  `not`.
- Aggregations `sum`/`count`/`avg`/`min`/`max` as monoid homomorphisms over a
  key's image, respecting weights; `count`/`sum` grouped `by unit` yield their
  identity when empty.
- **Recursion** (SPEC §8): `let recursive path : Node -> Node = edge | edge .
  path`; consecutive recursive `let`s form one fixpoint group; a stratification
  check keeps non-monotone operators out from under it.

**The engine**
- Lowering to a circuit of delta kernels with rewrites, node sharing and
  demand-driven state; inserts, retractions, backfill of views added over
  existing data; nested fix regions for recursion.
- Named-event dispatch as one atomic transaction against the pre-event
  snapshot; an append-only event log; silent replay; base snapshots and
  validated restore.

**The application layer** (SYNTAX.md)
- `type` unions and `Bool`; `state` as a singleton relation; `event`/`on`
  handlers with `new`, `new … from` a relation-valued param, `update`,
  `delete`, `:=`, `set`, `do`; `where`-targeted bulk mutations as hidden
  maintained views.
- `view`s: `Unit`-rooted static chrome, nested `select` levels over entities
  or derived keysets, `order by … [desc]`, `if` gates, binds of any co-keyed
  expression, presence binds for classes and boolean props, components with a
  `children` slot, per-instance `local` state, DOM handlers with extractors
  (incl. `import js`).

**The edge**
- `rex` CLI: `check`, `run`, `build [--watch]`, REPL.
- `rex-wasm` (`RexApp`), `rex-dom` (shaper: −/+ fusion, phased apply, typed
  ordering, fractional keys + rebalance, subtree coalescing, reparent), and
  `rex-runtime` (typed engine, boot, IndexedDB persistence, recovery).
- Four example apps with Playwright suites.

---

## Status against the MVP

MVP-PLAN §1 defines the MVP as five things. Where each stands:

| | Criterion | Status |
|---|---|---|
| 1 | Three apps, each from one `.rex` file via `rex build`, passing Playwright | **Met.** The benchmark's random labels come from a 24-line `utils.ts` through `import js`, the one sanctioned escape hatch. |
| 2 | Every change is a named event in an append-only log; reload restores | **Met.** Snapshot + log replay, validated on load, with recovery when storage does not load. |
| 3 | `rex-dom` and the engine wrapper as standalone packages with a documented contract | **Met.** `npm pack` works and the tarballs run Kanban; not published to npm. |
| 4 | `rex check`/`build`/`run`; errors at source spans; CI | **Met locally.** The CI workflow covers all of it, but the commits since `bb7a655` have not been pushed, so it has not run on them. |
| 5 | Benchmark numbers for Rex and elysium26 side by side | **Met** (ROADMAP §3.2, PERF-PLAN). The stretch target "create 10,000 rows inside one frame" is not: see *Performance*. |

Not part of the MVP, and not done: the effects membrane (M4), sync and
multi-client, the query sugar of ROADMAP M6.e, hidden manual order, fully
incremental recursion, an LSP.

## What's left

**Small, known, and worth doing next**
- Handler values lack `*`, `if`/`match` *expressions* and aggregates, and
  cannot write through a path; component arguments must be row binders
  (SYNTAX.md lists these where they come up).
- Guards and `reject "reason"` reject an event with a reason, but the reason
  only reaches the console: there is no way yet to show it in the page.
- Entities can declare a `key (…)`, enforced by rejecting the event that
  would break it. Not yet: more than one key, changing a key field, an indexed
  lookup by key from a handler (a row test with params scans), and `rel`
  sugar for a many-to-many (SYNTAX §9).
- A `Money` text bind renders minor units (`250` for 2.50).
- `docs/ordering.md` (MVP-PLAN S-72) — the design note for hiding fractional
  order keys behind `order manual` — is not written.

**Persistence** (the weakest part of what is built)
- **Two tabs on one store lose writes**: both number events from the same
  cursor. SYNC.md is the real answer; nothing guards against it today.
- **Editing a program discards its saved state**: the store is keyed by a hash
  of the source, there is no migration, and old databases are never deleted.
- **The log is never compacted**: events a snapshot already covers stay in
  IndexedDB and in the engine's in-memory log.

**Larger, each with its own document**
- **Performance** — PERF-PLAN.md: P-5 (a smaller `Value`), the rest of P-6
  (event delegation, change records for functional views), and P-7 (startup,
  up to compiling the circuit ahead of time).
- **Sync** — SYNC.md: replicas that accept writes offline and converge.
- **Effects** (M4) — ROADMAP §4: intent/claim/outcome on top of the event log.
- **Query sugar** (M6.e) — `select {…}` records, `group by`, `from E:` blocks.

**Core-language items, still open**
- **Fully incremental recursion.** A fix region re-derives its fixpoint each
  outer step (semi-naive within the step, O(closure) across steps).
- **Analysis consumers.** Of the operator metadata table, `monotone` and
  `grounding` have readers; set-ness and the linearity cost model do not, and
  the delta fan-out warning (SPEC §10) is unimplemented.
- **Coproduct runtime forms.** Coproducts of atoms exist (`type`, `Bool`);
  general `(V + W)` has no injection or case form, so "no NULL, use
  `V + Unit`" is not realizable — absence of a row is what stands in for it.
- **Empty groups.** `count`/`sum` give `0` for an empty group when the key is
  `Unit`, an entity or an enum ("customers with zero orders" is
  `Customer where orders = 0`). `min`/`max`/`avg`, and any aggregate keyed by
  a scalar, still have no row there and no way to give a default (SYNTAX §9).
- **`min`/`max` under retraction** re-fold the affected group: correct, but
  O(group) rather than O(Δ).
- **Top-N / engine-side ordering.** `order by` names an order relation; the
  shaper sorts.

---

## Performance

The work and its measurements live in **PERF-PLAN.md** (and the engine
comparison behind the Rust+WASM decision in ROADMAP §3.2). Where it stands
after P-0…P-4 and the first cut of P-6 (official js-framework-benchmark
harness, Chrome, medians, ms, total / script):

| Benchmark | Rex | vanillajs | elysium26 |
|---|---|---|---|
| create 1,000 | 45.0 / 13.5 | 85.1 / 6.4 | 107.6 / 19.1 |
| replace 1,000 | 53.7 / 21.3 | 99.9 / 18.0 | 124.7 / 31.5 |
| update every 10th | 26.1 / 4.8 | 20.2 / 1.1 | 36.7 / 6.0 |
| create 10,000 | 483.6 / 126.7 | 353.4 / 26.3 | 508.5 / 152.8 |
| append 1,000 | 54.9 / 14.9 | 41.1 / 2.9 | 53.5 / 16.1 |
| clear 1,000 (4× throttle) | 41.3 / 37.5 | 19.3 / 15.4 | 34.5 / 30.8 |

- A create-10,000 click is about 120 ms of script: ~40 ms in `dispatch`
  (the engine step itself is ~44 ms measured alone in the browser, down from
  ~283 ms before the plan) and ~70 ms in the shaper and DOM construction. The
  one-frame target for the engine is still missed by about 3×.
- **Startup is the weak number**: first paint ~1.6 s. The browser downloads an
  858 KB wasm (281 KB gzipped) that contains the whole compiler, and parses,
  checks and lowers the program at boot. P-7 is about this.
- The JS↔WASM boundary is not the bottleneck (a bare call is ~0.4 µs).
- `REX_PROFILE=1 npx playwright test e2e/profile.spec.ts` in
  `examples/js-framework-benchmark` prints the engine / parse / shaper split
  of one click; `?profile` on any generated app logs it per dispatch.
- The batch interpreter clones pervasively and the REPL re-checks the whole
  session per line. Both are fine for what they are for.

## Test coverage

Unit tests cover each layer — lexer, parser (precedence and associativity of
every operator), checker (positive cases and the diagnostics that matter),
elaboration, the algebra kernels, each DBSP operator against its batch kernel
under random insert/retract histories, codegen snapshots, the shaper's
mutation counts — and Playwright covers the four apps in a real browser.

On top of those sits a set of **oracle and fuzz suites**, which is where to
look (and add) when changing the language or the engine:

| Suite | What it holds the system to |
|---|---|
| `rex-core/tests/histories.rs` | Random event histories with hostile arguments over the example apps and a graph program: after every event the base invariant holds, every view equals batch evaluation, each step's deltas are exactly the change, the log replays, and a mid-history snapshot plus the tail restores. |
| `rex-core/tests/models.rs` | Hand-written reference models (TodoMVC, the benchmark, Kanban, chat, a stockroom built around arg-dependent `where`): what a handler *means*, independent of the engine. |
| `rex-core/tests/gen_queries.rs` | Randomly **generated queries** from a typed grammar (~90% check); every accepted one must be maintained exactly under random histories. |
| `rex-core/tests/fuzz_frontend.rs` | Mutants of real programs, token soup, arbitrary text, every construct nested far past the limit, very wide programs: no panic, no stack overflow (on a 1 MB stack in release), every diagnostic renders — and a mutant that still checks is driven through the engine oracle. |
| `rex-core/tests/encode_props.rs` | The wire encoding round-trips any value and is injective; emitted JSON parses and says what it should. Writes `fixtures/encoding.json`, which `rex-dom` is tested against. |
| `rex-core/tests/guards.rs` | Handler guards and `if`: rejection writes and logs nothing, a callee's guard rejects its caller, conditions read the pre-event state; a bank with preconditions checked against a model. |
| `rex-core/tests/keys.rs` | Entity keys: a duplicate rejects the event, a transaction is checked against its own rows, damaged snapshots and logs are refused; keyed rows checked against a set. |
| `rex-core/tests/totals.rs` | Total aggregates: `0` for an empty group over entity, enum and `Unit` keys, deltas when a key and its group arrive or leave together, a model. |
| `rex-core/tests/adversarial.rs` | One named test per corner case the suites above found (see SPEC §2.1, SYNTAX §8a). |
| `rex-wasm/tests/boundary.rs` | The wasm boundary, natively: sessions round-trip through their own JSON; hostile `dispatch`/`rebalance` calls fail cleanly; a damaged snapshot or log is refused. |
| `rex-codegen/tests/hostile.rs` | Hostile text and names produce well-formed modules whose literals say the same thing; mutants never panic codegen. |
| `rex-dom/test/shaper.fuzz.test.ts` | The shaper against a from-scratch render of random relational states: same DOM, same elements for surviving rows. |
| `rex-dom/test/order.fuzz.test.ts`, `encoding.test.ts` | The order index against a sorted array; the comparator is a total order; the JS encoders agree with the engine's. |
| `rex-runtime/test/boot.test.ts` | Boot recovers from storage that does not load. |
| `examples/todomvc/e2e/fuzz.spec.ts` | A seeded random walk through the real app in a browser, with reloads, against a model. |

They run in the ordinary `cargo test` / `vitest` / Playwright runs at modest
sizes. For a soak: `REX_FUZZ_CASES=20000 cargo test --release -p rex` (the
release build also runs the front end on the 1 MB stack a browser gives it),
`REX_FUZZ_CASES=6000 npx vitest run` in `js/rex-dom`, and
`REX_FUZZ_STEPS=800 npx playwright test e2e/fuzz.spec.ts` in `examples/todomvc`.
A failing shaper seed replays with `REX_FUZZ_SEED=<n>`.

Known gaps: the batch interpreter has few direct program-level tests of its own
(it is the oracle, checked only against the algebra kernels); the Kanban and
benchmark apps have no browser-level random walk; multi-tab use of one
IndexedDB store is untested (and unsupported).
