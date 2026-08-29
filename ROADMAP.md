# Roadmap: from Rex to a whole-app incremental relational system

**Status:** strategy document, July 2026.
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
| **Rex** | The system. Core model, compiler, and (pending M0) engine. |
| **reactor-ts** | Runtime semantics reference and test oracle; its platform is evaluated against, and likely ported to satisfy, the spec (M3). |
| **elysium26** | Design reference for surface language, compiler structure, examples, and benchmarks. Frozen. |
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

### M1 — Rex incremental core
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

### M4 — Effects membrane (spec Part II)
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
**Gate:** Kanban ✓. **Remaining:** TodoMVC (needs `state` singletons + a `match`
membership construct for filter-gated visibility + bulk where-target dispatch);
js-framework-benchmark numbers via elysium26's harness.

---

## 5. Parked

- **Delta fan-out amplification** (Rex SPEC §10) — cost-model open problem; revisit
  with M1 profiling data.
- **Recursion / fixpoints** in the language (Rex SPEC §8, PARKED) — reactor-ts's
  `iterate.ts` shows the runtime shape; no milestone needs it before M5.
- **Sync / multi-client** — elysium26's `sync/` package proves the event-sourcing
  shape; out of scope until the single-client story is done.
- **Firmament** — the distributed/ontology extension. The effect membrane (M4) and
  the derivation/justification model are its foundations; nothing more yet.
