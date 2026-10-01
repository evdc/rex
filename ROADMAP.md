# Roadmap: from Rex to a whole-app incremental relational system

**Status:** strategy document, July 2026; revised Sept 2026 (engine/elysium
comparison with measurements in §3.2, new M6 plan in §4); **October 2026: the
M6 gate is met** and with it the MVP (MVP-PLAN.md) — §4 says what landed, and
§4.1 is what comes after.
**Scope:** the path from today's four prototypes to one system: a relational language
covering data model → logic → UI, incrementally maintained end to end, with deltas
driving the DOM directly. Companion to the "Incremental Relational → DOM" spec
(the *nesting/shaper/effects spec* below), which specifies the layers between engine
and DOM; this document sequences everything around it.

---

## 1. Thesis & end state

Build an event-driven reactive relational language/framework where the entire
application — schema, derived state, UI structure, external effects — is expressed as
relations and incrementally-maintained views over them. A DBSP-style engine turns base
table changes into Z-set deltas; a shaper/bridge layer turns those deltas into surgical
DOM mutations (no VDOM); an effect membrane (intent/claim/outcome) turns them into
exactly-once interactions with the outside world. One mechanism — justified derivation
and retraction — accounts for reactivity, cancellation, and unmounting alike.

**Horizon, out of scope here:** Firmament — extending the same declarative/relational
model to distributed systems and ontology-driven infrastructure. Nothing below should
foreclose it, but no milestone depends on it.

---

## 2. What exists — asset inventory

Four prototypes, each contributing a different proven piece:

### Rex (this repo) — the core model
- Point-free binary-relational combinator language in Rust: every expression denotes a
  relation `A → B`; entities are families of keyed columnar relations (6NF by
  construction). Small, rigorously specified (SPEC.md), ~60 tests.
- The eval substrate is **already Z-set-based**: `src/eval/relation.rs`'s
  `BTreeRelation` carries i64 weights with retractions; the algebra
  (`src/eval/algebra.rs`) is weight-correct throughout — bilinear compose/fork,
  additive union, min intersect, clamped distinct, negative-weight antijoin.
- The checker (`src/types/check.rs`) emits a fully elaborated, resolved, point-free
  `TProgram` — a clean IR to lower from. `src/operator.rs` already tags each
  combinator with linearity/monotonicity metadata; nothing consumes it yet.
- **Gap = the thesis:** no circuit IR, no delay/integrate/differentiate, no delta
  processing. Eval fully materializes; the REPL re-checks and re-evaluates the whole
  session per line.
- *(Sept 2026: the two bullets above describe the July starting point. Since then
  M1–M3 and the M5 Kanban gate landed — circuit IR + delta engine with recursion,
  nesting, WASM `RexApp`, TS shaper, `rex build` codegen; paths are now under
  `crates/rex-core/src/`. The gap then was **surface-language breadth and the
  event log**, not the engine — see M6.)*
- *(Oct 2026: M6 closed that gap. Rex now has named events and a replayed log,
  `state`, `match`, components, derived binds, and three apps built from single
  `.rex` files. What it still lacks relative to elysium26 is listed under M6.e
  and §4.1.)*

### reactor-ts — the runtime reference
- A working, tested TypeScript DBSP runtime: Z-sets (`src/zset.ts`), delta-driven
  operators (`src/operators/` — indexed joins computing the three cross-terms,
  aggregates via retract+assert, top-k, windows, fixpoint iteration), a `Circuit`
  with a `step()` contract, and a two-queue event loop (`src/reactor.ts`) with a
  cascade guard.
- Its platform (`src/platform/`) already implements the spec's hardest invariant:
  `applyDelta` groups a delta by entity key and collapses retract+assert pairs into a
  single `update` (spec §5.2 −/+ fusion), against a key→DOM registry with
  parent/child nesting and a swappable/spy DOM driver for mutation-count tests.
- `examples/chat-compiled.ts` is hand-written "compiled output" — the concrete shape a
  compiler should emit, including nested rendering and composite string keys.
- No compiler, no language. That is by design; it is the executable semantics oracle.

### elysium26 — the end-to-end proof
- The most complete vertical slice: a real compiler (`.ely` → JS), runtime, DOM
  platform, sync package, and working example apps (TodoMVC, chat, analytics) plus a
  js-framework-benchmark harness. Two earlier generations (2023, 2024) as lessons.
- Its runtime does IVM where it can but **falls back to full recompute for
  group/sort/take** — precisely the gap the nesting spec's composite-key design
  closes. This is the clearest empirical motivation for M2 below.
- Its reconciler (`packages/platform/src/reconciler.ts`) is keyed list diffing —
  correct, but derivation-based rather than delta-based; the spec's shaper supersedes
  it.
- Contribution going forward: surface-language feel, compiler structure, and honest
  knowledge of where the UX and semantics get hard. Not the codebase to extend.
- *(Sept 2026 re-survey)* Healthy: 278 tests pass; TodoMVC, analytics, project
  tracker, the js-framework-benchmark app, async/HTTP examples, and an event-log
  sync bridge with an IndexedDB adapter. Its language is far more expressive than
  Rex's today: named `event`s + `on` handlers, `where`-targeted bulk
  `update`/`insert`/`delete` with expressions, `state`, `match`, `group by …
  select {…}`, FK paths, components with props and local state. Two structural
  limits Rex's design avoids: any query depending on `state` takes a **full
  recompute** (`emitter-static.ts`, the state-driven branch), and `where .id = x`
  mutations are a **predicate scan** (O(N) per update). It is now the designated
  **surface-language donor** for M6.

### ripple — the type-system story
- Pipeline-first relational language in Rust compiling to SQLite (working REPL,
  ~9.3k LOC). Signature feature: **cardinality-as-a-type** (`One/Opt/Some/Many`
  lattice with key tracking), so fan-out and by-key narrowing are visible in types.
- Designs nested relations as first-class NF² (`group … into g`, `unnest`,
  windows = group + nested sort + unnest) and has the largest IR investment
  (`rir/decorrelate.rs`, ~1k LOC of magic decorrelation).
- SQL is explicitly the validation/bridge backend; its intended DBSP backend was
  never built. Contribution: donate the cardinality/key type discipline and
  decorrelation ideas into Rex's checker when the surface language grows joins and
  subqueries. Its own rewrite onto a Rex core is deferred to M5.

### The nesting/shaper/effects spec — the missing middle
- Part I specifies the **shaper** (delta classifier with −/+ fusion, four-phase batch
  protocol, fractional-index ordering, wide-row assembly at mount, subtree-remove
  coalescing) and **bridge** (atomic synchronous DOM transactions), on top of a flat
  composite-keyed engine contract. Nesting = composite keys, closed under
  project/unnest/aggregate/re-group; opaque collection values are a marked
  incrementality cliff.
- Part II specifies the **effect membrane**: derived `EffectIntent` (retractable
  wish) vs driver-owned `EffectClaim`/`EffectOutcome` (irreversible fact), with
  structured concurrency and cancellation falling out of justification-tree
  retraction — the same mechanism as DOM unmount.
- Both parts include implementation order (§11, §20) and test obligations (§12,
  §20); the milestones below inherit them.

---

## 3. Architecture decisions

### 3.1 Wide vs narrow core: **narrow (6NF/binary). Decided.**

Rex's model *is* the spec's axiom 5: each renderable attribute its own relation,
structural membership its own relation. This makes the structural-vs-attribute
distinction syntactic (spec §5.2/§5.3), keeps IVM fine-grained, and confines wide-row
reassembly to the edges (mount, §6). The cost — reassembly at mount, more relations —
is paid exactly where the spec shows it is affordable.

Rex's existing compromise stands: values in the right column may be tuples/structs,
opaque to the relational machinery. This is the spec's §8 cliff, and the rule is the
same as SQL's for `array_agg`: permitted, but **marked** — the compiler must know (and
eventually warn) that anything consuming an opaque composite drops from
element-surgical to recompute-per-group. Surface sugar makes narrow feel wide;
the core never widens.

### 3.2 Rust+WASM vs full TS: **DECIDED (July 2026) — Rust core → WASM, TS shaper.**

Measured, not assumed: the engine ships as `crates/rex-wasm` (414 KB raw / 121 KB gzipped
before wasm-opt); the boundary is one JSON delta batch per step over the canonical value
encoding (`rex-core/src/eval/encode.rs`), measured at ~18–42 µs per event round-trip with
~176 B payloads (node microbench). The shaper is TypeScript (`js/rex-dom`) — the
classifier is the highest-risk piece and debugs JS-side, the fractional-indexing library
is JS, and the delta batch crosses the boundary anyway. The original decision criteria
below are answered; kept for the record.

**Re-examined Sept 2026 against elysium26 — decision stands, rationale revised.**
Measured on one Kanban-shaped program and an identical log of single events (10k
inserts, then 1k renames / moves / deletes; engine only, no DOM; µs/event at N=10k):

| Engine | insert | rename | move | delete |
|---|---|---|---|---|
| Rex native (Rust) | 7.9 | 4.0 | 12.0 | 9.4 |
| Rex WASM `-O3`, Bun (JSC) | 18 | 7 | 17 | 12 |
| Rex WASM `-O3`, Node (V8) | 62 | 24 | 35 | 32 |
| Rex WASM as shipped (`opt-level="z"`), Node | 87 | 40 | 60 | 42 |
| elysium26 (TS), Node | 58 | 278 | 301 | 257 |

Findings:
- **The boundary is cheap** (~0.4 µs per bare call; delta `JSON.parse` ~1–8 µs).
  What costs is V8 *executing* this WASM: 3–8× native, where JSC is within ~2×.
  Likely the allocation-heavy `BTreeMap`/`Value`-cloning style under the WASM
  allocator — unprofiled.
- **In Chrome, WASM is not currently a speed win** over a well-built TS engine
  (estimated 10–40 µs/event for these ops; elysium's O(N) updates are its
  predicate-scan, not TS). At app scale both are far inside a frame budget; engine
  speed matters only for bulk loads, **event-log replay**, and server fan-out.
- **The shipped `wasm-release` profile (`opt-level = "z"`) costs ~1.4–1.7×** for
  ~27 KB gzipped (155 → 182 KB at `-O3`). The earlier 121 KB figure is stale.

**js-framework-benchmark operations (S-91, Sept 2026)**, engine only (no DOM), µs per
event, the real `examples/js-framework-benchmark/src/app.rex` with every view live
(`cargo test --release -p rex --test bench_create -- --ignored --nocapture`; the wasm
figure is the `engine cost` spec in that example's Playwright suite):

| Operation | Rex native | elysium26, Bun | elysium26, Node |
|---|---|---|---|
| create 1,000 | 26,100 | 15,200 | 69,600 |
| create 10,000 | **206,900** (wasm in Chrome: ~283,000) | 70,400 | 356,100 |
| replace 10,000 | 382,700 | 84,100 | 195,600 |
| append 1,000 | 14,500 | 7,100 | 18,500 |
| update every 10th | 4,200 | 5,700 | 10,900 |
| swap rows | 30 | 3,000 | 6,800 |
| clear (10,000) | 190,700 | 20,900 | 27,500 |

- **The "create 10k under one frame (16.6 ms)" target is not met**: ~13× over natively.
  It is ~20 µs per row for ~4.5 delta rows per row through ~30 nodes of `BTreeMap` inserts
  and `Value` compares (the profile is flat: `Value::cmp`, `BTreeRelation::add`, malloc).
  Closing it is the precompiled-circuit / bulk-load work the plan lists as post-MVP.
- **Not like-for-like**: elysium's numbers are its handler alone — `main.js` driven
  through `rt.dispatch` with no platform mounted, so no row views are being maintained;
  Rex's include maintaining every bind, membership and order view and building the delta
  batch. The row's `swap` and `update` are where Rex's incremental design shows (30 µs
  vs a predicate scan).
- Found and fixed on the way: `Engine::push_retract` re-scanned the whole transaction per
  retracted row, so `Clear` on 10k rows took 2.3 s; it now takes 0.19 s.

**Superseded by PERF-PLAN.md (30 Sept 2026).** The table above is the starting point
that plan was written from. After its phases P-0…P-4 and the first cut of P-6, the same
`Run(10000)` dispatch costs about 44 ms in the browser's wasm (from ~283 ms), and in
the official harness Rex's script time is below elysium26's on every CPU benchmark
except clear (create 10,000: 127 ms script vs 153; vanillajs 26). The finding that
changed the plan: against elysium the gap had been the **JS shaper** — order-index
parsing, per-row removal — not the engine or the boundary. Current figures, and what is
left (startup above all: ~1.6 s to first paint, an 858 KB wasm that carries the whole
compiler), are in PERF-PLAN.md.

**Why keep Rust+WASM, then:** not browser speed, but (1) **one engine everywhere** —
the event log wants a server (replay, sync, multi-client fan-out), where native Rex is
5–10× faster than either engine in V8; (2) the correctness investment (batch-oracle
property tests, typed core) is in Rust; (3) the compiler is Rust regardless. All-TS
would be simpler and is the right call **only** if the engine is committed to be
browser-only forever. Follow-ups (M6.f): ship `-O3`; add a batch/transaction entry
point across the boundary (`Engine::dispatch` already batches); profile under V8.

Recommended end state: the compiler (already Rust) and the engine run as a WASM core;
a thin JS/TS bridge owns the DOM. The delta protocol favors this split — deltas are
small, and crossing the WASM↔JS boundary once per `step()` with a compact op batch is
cheap; what must live JS-side regardless is shaper state that touches DOM references
(`nodeMap`) — though the classifier and ordering logic could sit on either side.

Decision criteria the M0 spike must answer before this is final:
- WASM↔JS boundary cost for realistic delta traffic (measure, don't assume).
- Bundle size of the Rust engine compiled to WASM vs a TS engine.
- Debuggability: stepping through circuit execution matters during M1–M3; a TS
  engine is materially easier to instrument.

Interim position regardless of outcome: **reactor-ts remains the executable semantics
oracle.** Its operator behaviors (join cross-terms, retract+assert aggregation, −/+
fusion) are the reference tests any Rust engine must reproduce. If the spike goes
badly for Rust/WASM, the fallback is promoting reactor-ts to the production runtime
with Rex compiling to JS against its platform API — the `chat-compiled.ts` shape.

### 3.3 Engine: bundled minimal vs Feldera `dbsp` crate: **DECIDED — bundled, by construction.**

The M1 work grew the bundled engine (`src/dbsp/`) to the point where the M0 spike's
question answered itself: it lowers the dynamic `TProgram` graph directly, exposes
relation internals to the shaper (every `BTreeRelation` *is* an IndexedZSet), builds to
wasm at ~121 KB gzipped, and its correctness surface is held by the batch-oracle property
tests. The Feldera comparison is moot unless the bundled engine hits a wall. Original
criteria kept below for the record.

Time-boxed comparison, one small circuit implemented both ways (two joins, one
aggregate, one nesting level — a mini-Kanban core):

- **(a) Bundled:** grow a minimal circuit/operator runtime from Rex's
  `BTreeRelation` + `algebra.rs`, mirroring reactor-ts's operator set.
- **(b) Feldera:** lower the same circuit onto the `dbsp` crate.

Exit criteria:
1. Can the dynamic `TProgram` graph be lowered without fighting Feldera's
   generic/type-level circuit API? (This is the expected failure mode for (b).)
2. WASM build size and build ergonomics for each.
3. Freedom to expose composite-key/IndexedZSet internals to the shaper — the nesting
   design (M2) needs per-level indexed access, not just opaque output streams.
4. Honest estimate of the correctness surface we take on by hand-rolling (a):
   join/aggregate delta rules, distinct, iteration.

The decision is binding before M2 begins. Weight in case of a tie: bundled — the
nesting and shaper work is the novel part of this project, and owning the engine
internals de-risks it.

### 3.4 Project roles going forward

| project | role |
|---|---|
| **Rex** | The system. Core model, compiler, engine, and the two JS packages that run it. |
| **reactor-ts** | Runtime semantics reference and test oracle; its platform is evaluated against, and likely ported to satisfy, the spec (M3). |
| **elysium26** | Design reference for surface language, compiler structure, examples, and benchmarks. Frozen as a codebase; **its surface language and event model are the M6 donor**, and its example apps (TodoMVC, js-framework-benchmark) are M6 acceptance targets. |
| **ripple** | Type-system and IR donor (cardinality lattice, key tracking, decorrelation). Rewrite-onto-Rex question re-opened at M5, not before. |

---

## 4. Milestones

Ordering follows the spec's own implementation orders (§11 for Part I, §20 for
Part II), adapted to start from Rex's actual state. Each milestone names its gate:
what must be demonstrably true before the next starts.

### M0 — Engine spike — **CLOSED** (satisfied by the built engine + §3.2/§3.3 decisions)
The §3.3 comparison. Deliverable: a short written decision with the four exit
criteria answered, plus the surviving mini-circuit as the seed of M1.
**Gate:** engine choice committed. ✓

### M1 — Rex incremental core — **effectively met** (engine, backfill, retraction property tests, recursion; the linearity-metadata consumer is still open)
- Circuit IR lowered from `TProgram`; delay (`z⁻¹`), integrate, differentiate;
  delta-driven operator implementations reusing the existing Z-set algebra.
- The REPL/Session stops re-evaluating the world: `let`/`entity` extend the circuit,
  data statements become base-table deltas, each line is a `step()`. (README already
  flags the Session as this seam.)
- `operator.rs` linearity/monotonicity metadata gets its consumer: linear operators
  take the O(Δ) path; non-linear ones (min/max under retraction, non-abelian
  aggregates) take the recompute-per-group tier, explicitly (SPEC §10).
- **Test debt paid here:** program-level negative-weight/retraction tests through the
  interpreter — currently absent, and load-bearing for everything after. Port
  reactor-ts operator behaviors as cross-checks.
**Gate:** spec12.rex runs delta-at-a-time with results identical to batch eval,
including under retraction.

### M2 — Nested structures (spec Part I, §§2–3, 8) — **GATE MET (July 2026)**
Delivered: `fst`/`snd` projections end-to-end (the one missing eliminator — composite-key
nesting otherwise composes from existing `~`/`,`/`.`/`by`); `tests/nesting.rs` proves
correct per-view deltas under insert/retract/regroup; the §8 cliff is a checker warning;
`encode.rs` is the canonical value/delta wire format. Notes below kept for the record.
- Composite keys as first-class: nesting = flat relations keyed by the composite of
  enclosing grouping keys; each nesting level its own IndexedZSet (axiom 3).
- `group` / `unnest` / `aggregate` / re-group closed and incrementally maintained
  (axiom 2); the shape-tree output contract (§3): one delta per shape node per
  `step()`, batch-complete and consistent.
- Rex language gaps that block this, noted in exploration: projection eliminators for
  forked pairs (`fst`/`snd`) and coproduct constructors/eliminators.
- Cliff enforcement in the checker: consuming a nested collection as an opaque value
  is legal but marked (§8).
**Gate:** a nested two-level query (lists → ordered cards) emits correct per-node
deltas under insert/retract/regroup.

### M3 — Shaper + bridge prototype (spec Part I, §§4–7) — **GATE MET (July 2026)**
Delivered: workspace split (`crates/rex-core` / `rex-cli` / `rex-wasm`, `js/rex-dom`);
`RexApp` wasm API; TS shaper with view-role classifier, −/+ fusion, phased apply,
fractional ordering + rebalance, mirror-based mount, subtree coalescing, reparent —
spy-driver mutation-count tests in vitest plus Playwright gates on the live Kanban app
(`examples/kanban`): retitle preserves node identity *and focus*, drag reuses the DOM
node, delete detaches once. Notes below kept for the record.
Build in the spec's §11 order:
1. Same-entity −/+ fusion classifier (§5.2) — highest-risk, silent failure mode;
   stress-test node-identity preservation first.
2. Batch → DOM transaction protocol and the four-phase order (§7).
3. Ordering: published fractional-indexing library (do not hand-roll), per-parent
   sorted child index with `successor`, rebalance hook (§4).
4. Wide-row assembly at mount + attribute subsumption (§6); subtree-remove
   coalescing (§7.1).
5. Reparent (§7.4).
- Reuse reactor-ts platform pieces directly or as ports: `applyDelta`'s fusion logic,
  the key→DOM registry, and especially the spy driver for mutation-count assertions.
- Language target for the prototype: whichever the M0/§3.2 decisions chose
  (WASM+bridge or compile-to-JS); the shaper design is identical either way.
- **Validation app: Kanban** (spec §10) — drag across lists must reuse the DOM node;
  the §12 test obligations are the acceptance suite.
**Gate:** spec §12 obligations green: field edit preserves node identity, reorder is
one move, subtree delete is one `removeChild`, coupled batches apply atomically.

### M4 — Effects membrane (spec Part II) — **not started** (its prerequisite, M6.a, is done)
The named-event log (M6.a) is its foundation: intents/outcomes are logged events.
In the spec's §20 order: intent/claim/outcome relations + driver skeleton (GET only)
→ `Async<T>` and single `await` desugar with Pending/Loaded/Failed rendering →
structural cancellation (unmount aborts in-flight GETs) → latest-wins slots + stale
anti-join → the `once` commit-log path with the server idempotency contract →
combinators → pagination loop last. The §19 checkout trace and §20 obligations are
the acceptance suite (exactly-once under re-derivation, retraction, and crash
windows).
**Gate:** the §19 trace reproduced in a test; no surface async form suspends a
`step()`.

### M5 — Surface language convergence — **KANBAN GATE MET (July 2026)**
Delivered: the whole Kanban app — data model, 6NF views, UI, and event handling — is
one relational `.rex` program (`examples/kanban/src/board.rex`); the compiler
(`crates/rex-codegen`, driven by `rex build`) generates all of `main.ts` (shape tree,
templates, event wiring, boot). The **existing M3 Playwright suite passes unchanged**
against the generated app — node identity + focus on retitle, drag reuses the DOM node,
single-`removeChild` delete, atomic drag. Nothing is hand-written per app. Build order
and notes below kept for the record.

- **Surface (relational select-style, nesting-draft §9):** a `view` is a nested
  relational expression with element constructors — `Entity [where …] [order by :f]
  select <element>`. UI structure *is* a query: nesting is a nested `select`,
  membership is the `where :field == Parent` conjunct, ordering is `order by`. Inline
  `on <domEvent>(params) => <mutations>` handlers. New AST/parser (`view`/`state`,
  contextual keywords so the core keeps `order`/`select`/`on`/`delete` as identifiers).
- **Desugaring (`crates/rex-core/src/types/view.rs`, narrow core untouched):** each
  level auto-derives ordinary `let`s — membership (`board#list#card = Card . :list`),
  order, and per-attribute (`board#list#card#title = Card . :title`) views — proven
  *delta-equivalent* to the old hand-written 6NF `board.rex` (`tests/view.rs`). The one
  load-bearing rule: binders are second-class (keys, never relations), so every
  generated listener closes over exactly the key the shaper hands its template.
- **Engine-side dispatch (`dbsp/dispatch.rs`, `RexApp.dispatch`/`snapshot`):** an
  `on … =>` handler is a checked list of relational mutations run as ONE atomic
  transaction (`tests/dispatch.rs`); a Kanban drag setting `list` + `pos` is a single
  step, so the shaper still sees one reparent, not a torn move.
- **Runtime (`js/rex-dom`):** the honest non-relational library — fractional-key drop
  math, drag/drop wiring, rebalance sweep — as plain helpers (`interact.ts`), not
  smuggled language features. Encoding already shared (`encode.ts`).
- **Deferred:** ripple's cardinality/key typing (not needed for the two MVP apps);
  the ripple-rewrite question.
**Gate:** Kanban ✓. **Remaining** (TodoMVC, js-framework-benchmark) is folded into
M6, which generalizes it.

### M6 — Surface convergence with elysium26 + event log — **GATE MET (Sept–Oct 2026)**
Delivered (story by story in MVP-PLAN.md): TodoMVC and js-framework-benchmark ported
from elysium26 and running on generated code beside Kanban, each from one `.rex` file;
named events as the only write path, an append-only log, and reload by snapshot +
replay with validated restore; benchmark numbers recorded for both systems (§3.2,
PERF-PLAN.md). Against the build order below: **M6.a–d landed**; **M6.e did not** (no
`select {…}` records, `group by` or `from E:` — the three apps did not need them; FK
paths and `order by` over intrinsic fields are in); **M6.f** became PERF-PLAN.md, whose
first phases landed; **hiding fractional order keys** is still a design note to write
(MVP-PLAN S-72). The surface took elysium26's *constructs* in Rex's own brace grammar
rather than its syntax (MVP-PLAN §2.1); the reference is SYNTAX.md. Plan as written,
kept for the record:

**Decision: extend Rex, don't rewrite.** The novel, hard parts — Z-set DBSP with
correct retraction, recursion, composite-key nesting, the delta shaper with
node-identity guarantees — are built and tested here; a rewrite re-earns them. What's
missing is surface, and Rex's architecture already takes it the right way: sugar
desugars into the narrow core (as `types/view.rs` does). elysium26's
pipeline/SQL-flavored language becomes the **user-facing front end**; Rex's
point-free combinators become the IR users rarely write directly.

Build order:
- **M6.a — Named events + event log.** `event E(params)` / `on E(…)` handlers; DOM
  handlers dispatch named events rather than inline mutation lists. Every change enters
  through an append-only, replayable log (the "incrementally maintained against a log
  of incoming events" goal). Log replay = snapshot restore; this also seeds
  persistence and is M4's substrate.
- **M6.b — Handler bodies.** `T where … update {f: expr}` / `insert` / `delete`:
  bulk targets and expression values evaluated against the pre-event snapshot, still
  one atomic step. Primary-key targets must be O(1) lookups, not scans.
- **M6.c — `state` + `match`.** State as singleton relations, so a state change is an
  ordinary delta — no recompute-on-state cliff. `match`/conditional membership for
  filter-gated visibility.
- **M6.d — Views over derived relations.** Bind any relation keyed by the level's
  binder (aggregates, joins, FK paths), not just `:field`; components with props;
  `if`/class expressions; type-directed decoding of binds (fixes Int rendering as
  `i:3`).
- **M6.e — Query sugar.** `select {…}` records, `group by`, FK paths, `order by` over
  intrinsic fields — each desugaring to binary relations (records = forks of
  per-field relations; wide rows only at the edge).
- **M6.f — Engine/boundary performance** (see §3.2): `-O3` wasm profile, a batch
  entry point for bulk load and log replay, V8 allocation profiling.
- **Hide fractional order keys** (manual order as a language concept) alongside
  M6.d/e.

**Gate:** elysium26's TodoMVC and js-framework-benchmark apps ported to Rex, running
on the generated code with no hand-written per-app JS; js-framework-benchmark
numbers recorded for both, and a page reload restoring state via log replay.

### 4.1 After the MVP

In rough order of how much they block real use:

1. **Persistence that survives real use.** Two tabs on one store lose writes; editing
   a program discards its saved state (the store is keyed by a hash of the source, with
   no migration); the log is never compacted. The first is the smallest instance of
   sync — SYNC.md (post-MVP design, decisions made) treats tabs, devices and a server
   as replicas of one log.
2. **Startup and bundle size** — PERF-PLAN P-7: the browser downloads the whole
   compiler and compiles the program at boot. The end of that road is a circuit
   compiled ahead of time.
3. **The rest of the handler language** — `*`/`if`/`match` in values, scalar
   component arguments. (`let x = new …` landed 2026-10-01, and with it the `chat`
   example.)
4. **M4, the effects membrane** — its substrate (logged events with reserved
   `cause`/`intent` fields, MVP-PLAN §5 decision 2) is in place; nothing else is.
5. **Manual order as a language concept** — `order manual` and `move x before y`,
   with the fractional keys compiler-owned (MVP-PLAN S-72).
6. **M6.e query sugar**, and ripple's cardinality typing with it.
7. **Engine**: fully incremental recursion; in-engine top-N; the linearity cost model
   surfaced to users.

---

## 5. Parked

- **Delta fan-out amplification** (Rex SPEC §10) — cost-model open problem; revisit
  with M1 profiling data.
- ~~**Recursion / fixpoints**~~ — **done** (fix regions, stratification); fully
  incremental nested deltas remain future work.
- **Sync / multi-client** — elysium26's `sync/` package proves the event-sourcing
  shape; out of scope until the single-client story is done. M6.a's event log is
  designed to be its substrate, and native Rex on a server is the reason §3.2 keeps
  Rust. *(Oct 2026: the single-client story is done; the design is SYNC.md.)*
- **Firmament** — the distributed/ontology extension. The effect membrane (M4) and
  the derivation/justification model are its foundations; nothing more yet.
