# Rex → MVP: plan, stories, and architecture review

**Status:** proposal, 2026-09-18. Companion to `ROADMAP.md` (strategy) and
`SYNTAX.md` (the current `view` surface). This file is the *work breakdown*:
what "MVP" means, the architectural decisions that gate it, and stories sized
for independent (smaller) agents to implement.

How to read the stories:

- **ID** `S-nn`, **size** S (≤ ½ day) / M (1–2 days) / L (3–5 days), **deps**.
- Each story has *Goal*, *Files*, *Subtasks*, *Acceptance* (what must be
  green), and *Agent notes* (the traps). Subtasks are ordered; an agent should
  land them as separate commits.
- Stories inside an epic are sequential unless marked ∥ (parallelisable).
- "Core" = `crates/rex-core`; "wasm" = `crates/rex-wasm`; "codegen" =
  `crates/rex-codegen`; "dom" = `js/rex-dom`.

---

## 1. What MVP means

**MVP = three apps, one language, no hand-written per-app JS, state survives
a reload.** Concretely:

1. `examples/kanban`, `examples/todomvc`, and `examples/js-framework-benchmark`
   each build from one `.rex` file via `rex build`, run in the browser on the
   generated code, and pass a Playwright suite (node identity, focus, single
   DOM op per logical change).
2. Every change enters through a **named event** and an **append-only log**;
   a page reload restores the app by snapshot + log replay.
3. `rex-dom` and the engine wrapper are **standalone packages** with a frozen
   delta contract, contract-fixture tests, and a README; the examples depend on
   them like any other app would.
4. `rex` CLI has `check`, `build`, `run`; errors point at source spans; CI runs
   cargo tests, vitest, the wasm build, and Playwright.
5. js-framework-benchmark numbers recorded for Rex and elysium26 side by side.

Out of MVP (explicitly): effects membrane (M4), sync/multi-client, `group by`
records beyond what the three apps need, in-engine top-N, fully incremental
recursion, LSP.

The ROADMAP M6 gate is exactly (1)+(2)+(5); MVP adds (3)+(4) because "other
people can build an app" is the bar, not "the author can".

---

## 2. Architecture review (principal-level feedback)

Overall: the hard, novel core is right and well-tested — Z-set engine with
retraction, snapshot-consistent steps, the role-classified shaper with −/+
fusion, binders-as-keys. The design docs are unusually honest. What follows is
where I'd push, ordered by how much it shapes the MVP.

### 2.1 Decide the surface grammar once, on paper, before any parser work

ROADMAP M6 says elysium26 is the "surface donor". Don't port elysium's
indentation-sensitive, `app`-headed, `<jsx>` syntax wholesale — port its
*constructs* (`event`/`on`, `state`, `match`, `where … update {…}`,
`select {…}`, `.field` paths, components with props) into Rex's existing
brace grammar with contextual keywords. Two grammars in one file (`.rex` core
+ an `.ely` skin) is the worst outcome. Rex's `view`/`select`/`on` already
have a coherent brace style; extend it.

**Process recommendation (the single highest-leverage item):** write the three
acceptance programs (`todomvc.rex`, `bench.rex`, revised `board.rex`) *first*,
as fixtures, review them by hand for readability, then implement the grammar
those programs need and nothing else. Language design by acceptance test.
That's story S-02.

### 2.2 The event log is a *semantic* boundary, not a persistence feature

Getting the event model right determines replay, persistence, undo, sync and
M4. The rules I'd fix now:

- An **event** is `(seq, name, args)`; args are engine-encodable values (ids,
  scalars, and — see below — small relations). Handler bodies are
  deterministic functions of `(pre-state, args) → Transaction`. Nothing
  non-deterministic (`now()`, `random()`, `uuid()`, DOM geometry) runs inside a
  handler: it is computed client-side and passed as an arg. Rex's extractor
  vocabulary already embodies this — keep it that way and *say so* in the spec.
- **Every write goes through an event.** Today `maybeRebalance` calls
  `app.update_field` directly (`js/rex-dom/src/interact.ts`), and the REPL
  writes via `apply_typed_stmt`. Under a log, a direct write is a corruption:
  replay diverges. Make rebalance a system event (`@rebalance(level,parent,
  keys)`), and make seed data (`let x = new …` in the program) event 0 of the
  log (the "genesis" transaction), so replay from an empty engine is exact.
- **Id minting must be replay-deterministic.** The per-sort counter in
  `Engine::next_id` is deterministic given event order — good. But don't
  foreclose multi-client: reserve high bits of `Value::Id(sort, u64)` for a
  client/epoch tag, or make the minting function a log-visible parameter.
  Don't implement now; don't make it impossible.
- **Relation-valued event params** (`event Run(rows: Int -> Text)`) solve bulk
  insert (`insert Row from rows { label: … }`) without adding list values to
  the value algebra, and they cross the boundary as the same `[k,v,w]` tuples
  the shaper already speaks. This is the clean answer to js-framework-
  benchmark's `Run(10000)` and to elysium's `Row insert (1 .. n as id …)`.
- **Replay is per-event steps, not one big transaction**, because each
  handler reads the pre-event snapshot. Add a "silent step" (no delta
  serialization) so replay is engine-only cost. Snapshots then are just the
  engine's *input* integrals (base tables) plus the log cursor; views are
  backfilled on load (`Circuit::backfill` already exists).

### 2.3 Handler targets should be maintained views, not scans

elysium's O(N) `where .id = x` is a predicate scan at dispatch time. Rex can do
better and it falls out of the architecture: a `where`-targeted mutation
(`Todo where :completed = @yes delete`) has a *static* target expression, so
the desugarer can emit a hidden `let` for it and dispatch reads a materialized
keyset — O(|targets|) at dispatch, maintained incrementally. Primary-key
targets (binder refs) are already O(1). Only expressions that depend on event
args need evaluation at dispatch time, and those can be evaluated by the batch
interpreter *against the circuit's input integrals* (the interpreter already
runs over `BTreeRelation`s). One rule: "target sets are views; arg-dependent
values are point evaluations". This is a real, explainable advantage over
elysium — write it into SPEC.

### 2.4 Give the view language a `Unit` root level

Today a `view` root ranges over an entity, so static chrome (TodoMVC's header,
footer, the benchmark's button bar) and scalar binds (`count`) have nowhere
to live. Introduce a built-in `Unit` sort with one row and a constant relation
`unit : X -> Unit`. Then:

- a `view` body is an element tree at an implicit `Unit` root level (membership
  view = `unit`, one row, key `""` — the shaper's root already ignores the
  parent column);
- global aggregates are `count(Todo by unit) : Unit -> Int` — no special
  "scalar" concept, the SPEC §5 story holds;
- `state` is a hidden entity with one row referenced via `unit`, so `filter`
  in an expression is `unit . :filter : X -> Atom` — a state change is one
  field delta that flows through joins (no recompute cliff, as ROADMAP wants).

This one addition makes TodoMVC and the benchmark expressible with the
existing shaper model unchanged.

### 2.5 Components = inline expansion; `local` = per-instance relation

`view TodoItem(todo: Todo)` should expand at the call site (macro-style): the
binder is the key, so expansion keeps the "binders are keys" rule and the
shaper needs no new concept. `local` state (elysium's per-component scalars)
is a hidden entity keyed by the instance's binder with a default — i.e. a
relation `Todo -> Text` — which also gives "editing" state in TodoMVC a home
in the data model, not the DOM. Don't add runtime components/props to
`rex-dom`.

### 2.6 Ordering: fix the comparator, then hide the keys

`OrderIndex` compares *encoded strings* (`js/rex-dom/src/order.ts`), so
`order by :points` over `Int` sorts `i:10` before `i:3`. That's a correctness
bug the moment a non-Text order appears (the benchmark's `pos: Int`). Fix:
order keys carry their type through `ShapeNode.orderView` and the index uses a
typed comparator, with child key as the tiebreak (already) and `desc` support.
Manual/drag order (`pos: Text` + `endOf`/`dropPos`) stays for MVP but the
memory note's direction is right: after MVP, `order manual` makes the key a
compiler-owned hidden field and `move c before x` the surface.

### 2.7 Runtime compile is fine for MVP — make it a decision, not an accident

`main.ts` embeds the source and `new RexApp(PROGRAM)` re-parses, re-checks and
re-lowers at boot; handler names (`board#list#card@change3`) are *sequence
numbers* that must agree between the build-time and boot-time checks. That
coupling is brittle. Named events (2.2) remove it — codegen dispatches by
declared event name. Keep runtime compile (it enables a REPL/dev mode and a
single-artifact deploy) but record the decision, and plan a "precompiled
circuit" serialization post-MVP for bundle size. Meanwhile surface warnings at
build time only (already the case).

### 2.8 `rex-dom` extraction: what "extract" should mean

It already lives in `js/rex-dom`; what's missing is that it's *only* usable
from the generated code. Extract means:

- **Two packages, not one:** `rex-dom` (pure: shaper, order, rebalance,
  driver, encode, interact — zero engine dependency) and `rex-runtime` (the
  wasm wrapper: typed `RexApp` API, event log, persistence adapters, boot).
  Codegen emits against both. The wasm-bindgen `pkg/` moves out of
  `examples/kanban/src/` into `rex-runtime`.
- **A frozen, documented delta contract** (`StepDeltas`, `ShapeNode`, the
  encoding) with **contract fixtures**: JSON step batches + expected mutation
  counts, produced by Rust tests and consumed by vitest, so engine and shaper
  are conformance-tested against the same files.
- **An engine port interface** in `rex-dom` (`{ snapshot(): StepDeltas;
  dispatch(...)}`) so the shaper can be driven by a server-pushed delta stream
  later without touching it.

### 2.9 Smaller things worth fixing on the way

- `Bool` as sugar for `{@true+@false}`; `where not P` ≡ `except (where P)`.
- `where :f = @atom` rejected on view levels while `in` works — fix in the
  checker, not the docs.
- Decode binds by type (Int renders `i:3`); bare identifier in an element body
  is a tag — make it an error.
- `-O3` for the shipped wasm profile; a batch/replay entry point across the
  boundary.
- Repo hygiene: `scratch.rs` committed at root; README says `pkg/` is
  committed but `.gitignore` excludes it; `scripts.sh` has a TODO where the
  wasm-bindgen step should be; no CI; ten commits of history with large
  uncommitted doc edits.
- The `+` union/arith collision (SPEC §11) will bite the moment handler
  expressions land (`:n := :n + 1`). Decide now: `+` is union at relation
  type, and arithmetic is spelled `+` only between value-typed co-keyed
  operands, resolved by the checker's relational/functional tag. Document it.

### 2.10 Risks I'd watch

- **Fan-out on state change** is inherent (a filter flip changes O(N) rows'
  membership) — fine, but the shaper must handle a 10k-row membership delta
  in one frame. The benchmark's "clear" and "create 10k" are the tests.
- **Handler expression language** is a second expression language unless it
  *is* Rex expressions evaluated at a key. Keep it to Rex expressions; resist
  a JS-like scalar language.
- **Two evaluators** (batch interpreter, circuit) must stay in agreement as
  handlers start using the interpreter at dispatch — the existing oracle
  property tests are the safety net; extend them to handler evaluation.

---

## 3. Epics and stories

Dependency graph (arrows = "needs"):

```
E0 Foundations ──┬────────────────────────────────────────────┐
E1 Surface spec ─┤                                            │
                 ├─> E2 Events + log ─> E3 Handlers ─> E8 Persistence
                 │        │                  │
                 │        └─> E4 Unit/state/match ─> E5 Derived binds + components
                 ├─> E6 Ordering (∥ with E2–E5)                │
                 ├─> E7 rex-dom / rex-runtime packages (∥)     │
                 └─────────────────────────> E9 Apps + gate <──┘
```

### E0 — Foundations and hygiene

#### S-01 Build script, wasm profile, CI (M, no deps) ∥ — **done 2026-09-18**
*Goal:* one command builds everything; CI proves it.
*Files:* `scripts.sh` → `scripts/build-wasm.sh`, `Cargo.toml` (profile),
`.github/workflows/ci.yml`, `README.md` (Building).
*Subtasks:*
1. Script the wasm build: `cargo build -p rex-wasm --profile wasm-release
   --target wasm32-unknown-unknown` + `wasm-bindgen --target web` into
   `js/rex-runtime/pkg/` (path lands in S-30; use `examples/kanban/src/pkg/`
   until then). Fail loudly if `wasm-bindgen` is missing; pin its version to
   the `wasm-bindgen` crate version.
2. Switch `wasm-release` to `opt-level = 3` (ROADMAP §3.2 measured 1.4–1.7×);
   record gzipped size in the commit message.
3. CI: `cargo test --workspace`, `cargo clippy -D warnings`, `npx vitest run`
   in dom, wasm build, `npm run build` + Playwright in each example.
4. Remove `scratch.rs`; fix README's stale "pkg/ is committed" line.
*Acceptance:* green CI on main; `scripts/build-wasm.sh && (cd examples/kanban
&& npx playwright test)` passes from a clean checkout.
*Landed as:* `scripts/build-wasm.sh` (checks the `wasm-bindgen` CLI version
against the crate version pinned in `Cargo.lock`, fails loudly on mismatch or
if missing); `wasm-release` profile switched to `opt-level = 3` (512,931 bytes
wasm / 169,716 bytes gzipped for Kanban, vs. 633,146 / 155,356 at `opt-level =
"z"` — smaller binary *and* the ROADMAP §3.2 throughput win, gzip is slightly
larger since `-O3` code is less compressible); `.github/workflows/ci.yml` runs
`cargo test --workspace`, `cargo clippy --workspace --all-targets -D
warnings` (fixed the 5 pre-existing lints this surfaced —
`drop_non_drop`, `while_let_loop`, `cloned_ref_to_slice_refs`,
`if_same_then_else`, `question_mark` — no behavior changes), `rex-dom`
vitest, the wasm build, and `npm run build` + Playwright for every
`examples/*` directory with a `package.json` (so S-90/S-91's new examples
need no CI changes); `scratch.rs` and the old `scripts.sh` removed; README's
stale "`pkg/` is committed" line fixed (it's gitignored, not committed).

#### S-02 Acceptance programs first (M, no deps) ∥ — *design story* — **revised 2026-09-18 after owner review** — rules: `.` is compose-join and `.f` replaces `:f`; `tag(props) { children }` elements; brace bodies with `=> stmt` shorthand; named union types; prefix verbs `new/update/delete/set/do`; DOM actions are statements. Programs: `examples/todomvc/src/app.rex`, `examples/js-framework-benchmark/src/app.rex`, `examples/chat/src/app.rex` (added: many-to-many via link entity, defaultless `state` as an empty singleton), `examples/kanban/src/board.rex`, `SYNTAX.md` §9 open questions, `crates/rex-core/tests/surface_v1.rs`)
*Goal:* the three MVP apps written in the *target* surface before it exists.
*Files:* `examples/todomvc/src/app.rex`, `examples/js-framework-benchmark/src/app.rex`,
`examples/kanban/src/board.rex` (revised), `SYNTAX.md` (rewritten as the v1
surface reference).
*Subtasks:*
1. Translate `../elysium26/examples/todomvc-views.ely`, `chat.ely` and
   `../elysium26/bench/js-framework-benchmark/main.ely` into Rex brace syntax
   using: `event`/`on`, `do E(args)` in DOM handlers, `state`, `match`,
   `where … update/delete`, `insert … from`, `Unit`-root views, components
   with props, `local`. Follow §2 above; keep every construct desugarable to
   the binary core and write the desugaring next to each construct in
   `SYNTAX.md` (the existing "What desugars to what" table, extended).
2. Revise `board.rex` so its handlers dispatch named events.
3. Add the programs as parser fixtures now (they will fail to parse; mark
   `#[ignore]` with the story that un-ignores each).
4. Owner review of the three programs *before* E2 starts.
*Acceptance:* the owner signs off on the three programs' readability; each
construct in them has a one-line desugaring in `SYNTAX.md`.
*Agent notes:* this is language design; produce a short list of open
questions (e.g. `Bool` spelling, `desc`, how `local` is initialised) rather
than silently choosing.

#### S-03 Codegen snapshot tests + contract fixtures (M, no deps) ∥ — **done 2026-09-18**
*Goal:* codegen and the shaper get tests independent of the Kanban app.
*Files:* `crates/rex-codegen/tests/snapshots/*.ts`, `crates/rex-core/tests/fixtures/steps/*.json`,
`js/rex-dom/test/contract.test.ts`.
*Subtasks:*
1. Snapshot test: `generate_with(board.rex)` equals a checked-in `.ts`
   (use `insta` or a plain string compare with an `UPDATE_SNAPSHOTS` env).
2. Rust side: a test drives `Engine::dispatch` through a scripted Kanban
   history and writes each `StepResult` (via `step_result_to_json`) plus the
   `snapshot()` to fixtures, with a sidecar of expected shaper ops
   (`{mount:1, update:0, move:1, remove:0}` per step).
3. TS side: vitest loads the fixtures, applies them through `Shaper` with
   `SpyDriver`, asserts the counts.
*Acceptance:* fixtures regenerate deterministically; a deliberate engine
change to emit remove+mount instead of −/+ fails the TS test.
*Landed as:* a new shared fixture program, `crates/rex-core/tests/fixtures/board.rex`
(a self-contained copy of the Kanban shape — two entities, nested `select`,
order, attrs, create/update/move/delete handlers — so these tests don't
depend on `examples/kanban`). (1) `crates/rex-codegen/tests/snapshot.rs`
compares `rex_codegen::generate(board.rex)` against the checked-in
`tests/snapshots/board.ts`, `UPDATE_SNAPSHOTS=1` regenerates it. (2)
`crates/rex-core/tests/contract_fixtures.rs` scripts a history (rename,
reorder, reparent, delete, insert) through `Engine::dispatch`/`apply_typed_stmt`
and checks each step's `step_result_to_json` output (plus the initial
`{"views":{...}}` snapshot, reproducing `RexApp::snapshot()`'s shape without a
wasm dependency) against `tests/fixtures/steps/*.json`, `UPDATE_FIXTURES=1`
regenerates them; a second test (`scripted_history_is_deterministic`) reruns
the script and asserts byte-identical output, directly covering the
"fixtures regenerate deterministically" acceptance line. No hand-derived
sidecar op-count file — the TS side (3) hand-writes a `ShapeNode` tree
mirroring `board.rex`'s generated view names and asserts the same exact
`SpyDriver` counts `js/rex-dom/test/shaper.test.ts` already established as
the shaper's per-operation contract (rename → 1 `setText`; reorder/reparent →
1 `insertBefore`, 0 `createElement`/`removeChild`; delete → 1 `removeChild`).
Verified the acceptance line directly: hand-corrupting `03-reparent.json` to
a different key (simulating a remove+mount instead of a −/+ move) fails
`contract.test.ts`'s reparent case. Added `@types/node` (+ `"node"` in
`tsconfig.json`'s `types`) since the contract test reads fixture files via
Node's `fs`/`path`/`url` across the package boundary into `crates/rex-core`.

#### S-04 Small checker/codegen bugs from README (S, no deps) ∥ — **done 2026-09-18**
*Files:* `crates/rex-core/src/types/check.rs`, `types/view.rs`, `rex-codegen/src/lib.rs`.
*Subtasks:* (1) `where :f = @atom` on a view level accepted (co-keyed via the
coreflexive rule, SPEC §3.1); (2) bare identifier in an element body is an
error "unknown element `cnt`; did you mean `{ :cnt }`?"; (3) binds decode by
`Encoding` (`decodeInt`/`decodeMoney`/atom → text) — add `decodeInt`,
`decodeMoney`, `decodeAtom` to `js/rex-dom/src/encode.ts`; (4) `in` over
integer literal sets (`expect_subset`).
*Acceptance:* a checker test per item; Kanban snapshot unchanged.
*Landed as:* (1) an atom literal grounds to `Const{lit, dom}` when checked
against an `Id(sort)` ambient domain (`check_rel`'s `ExprKind::Atom` arm),
matching how `Int`/`Text`/etc. already ground via `constant()`; outside an
entity domain (e.g. a `{@a}`-typed recursive `let`, or an `in` set) it stays
its prior standalone coreflexive, so `check_rec_group`'s `{@a}` case is
unaffected. (2) `Desugar` now collects each entity's field name → declared
`Type` up front (`entity_fields`, also reused by (3)); `LevelWalk::element`
flags a childless, attr/handler-less `Content::Element` whose tag matches a
field of the enclosing entity. (3) `AttrBinding` gained an `encoding: Encoding`
field, resolved by walking `entity_fields` through dotted paths
(`LevelWalk::field_encoding`, falling back to `Text` for a path it can't
resolve — the checker rejects those independently); codegen's `decode_fn`
picks `decodeInt`/`decodeMoney`/`decodeAtom`/`decodeText` accordingly (`Id`
shares `decodeText`'s passthrough-on-non-`t:` behavior). (4) `check_in` now
derives the `in` set's element type by folding `lit_ty`/`join` over the
collected literals instead of re-`check_rel`-ing the set expression with no
domain (which is why numeric `in` previously errored "needs a known domain"
rather than hitting `expect_subset` at all); `expect_subset` itself now
accepts equal non-atom scalar types, not just atom (co)products. Tests:
`crates/rex-core/tests/check.rs` (5 new cases) and
`crates/rex-codegen/tests/decode.rs` (new file). Kanban's generated
`main.ts` only gained a `String(...)` wrapper around its (unchanged)
`decodeText` calls — Playwright suite still green.

### E1 — Surface: grammar and AST

#### S-10 AST + parser for events, `do`, `state`, `match`, mutations (L, deps S-02) — **done 2026-09-18** (all four acceptance programs parse; `tests/parser.rs` v1 section; `+ - / %` and `++` also evaluate; deviations: `&` binds tighter than `|`; `by` etc. are reserved words, so chat's `by` fields became `sender`/`user`; the Kanban fixtures/example keep inline-mutation handlers in the new spelling until S-20)
*Goal:* the three fixture programs parse to an AST; nothing is checked yet.
*Files:* `crates/rex-core/src/ast.rs`, `parser.rs`, `token.rs`/`lexer.rs`
(contextual keywords only — keep `on`, `event`, `state`, `match`, `update`,
`delete`, `insert`, `from`, `do`, `local`, `desc` as identifiers in the core),
`pretty.rs`, `tests/parser.rs`.
*Subtasks:*
1. `Stmt::Event { name, params: Vec<(name, Type)> }` where a param type may
   be a relation type `A -> B`.
2. `Stmt::On { event, params, body: Vec<Mutation> }`; extend `Mutation` with
   `Update { target: Expr, sets }`, `Delete { target: Expr }`,
   `InsertFrom { entity, source: Expr, fields }`, `SetState { name, value }`,
   `Do { event, args }` (handler-to-handler dispatch, elysium `do`).
3. Mutation values become `Expr` (not `Lit | Arg`).
4. `Stmt::State { name, ty, default }` already parses; add `local` inside
   `view` params; `view Name(params) = …` component form; `<Name arg=… />`
   equivalent as `Name(todo: t)` element form (pick in S-02).
5. Expression forms: `match e { pat => expr, … }`, `if c then a else b`,
   `not`, `desc` on `order by`.
5a. Operator respelling (§5 decision 4): `|` is union, `&` intersect,
   `except` difference; `+ - * /` arithmetic only; `++` text concat replaces
   `||`; coproduct types `{@a | @b}`. Update `operator.rs`, `pretty.rs`,
   every fixture, SPEC §3.1/§11, SYNTAX.md. `R - S` at relation type is a
   type error suggesting `except`.
5b. Component call form `Name(args) { children }` with one `children` slot
   in the component body (§5 decision 1).
6. DOM handler bodies become `do E(args)` (one or more); keep inline
   mutations parsing for one release with a deprecation diagnostic.
7. Pretty-printer round-trip for all of the above; un-ignore S-02 fixtures.
*Acceptance:* `tests/parser.rs` round-trips each construct; the three fixture
programs parse with no diagnostics.
*Agent notes:* recursive-descent + Pratt already; add nodes, don't restructure.
No desugaring in the parser (README rule).

### E2 — Named events and the log

#### S-20 Check `event`/`on`; EventIR replaces HandlerDef (L, deps S-10) — **done 2026-09-18** (`shape_ir::EventDef`/`Dispatch`/`ArgRef`/`UiAction`; `rex::events::dispatch_event` is the shared engine write path the wasm bridge and `tests/dispatch.rs` use; `focus`/`clear` are explicit `UiAction`s, the hard-coded focus-on-insert is gone; templates take `ancestors`; transaction writes to one cell compose (`Engine::net_rows`); non-path binds get their encoding from the elaborated view, so `examples/kanban/src/board.rex` is now the v1 program with the per-list count and `board.v1.rex` is deleted. Deviations: inline mutations in a DOM handler are a hard error rather than a deprecation warning; an `event` without an `on` is allowed (empty body); relation-typed params parse/check but dispatch rejects them until S-42)
*Goal:* a declared event has a checked handler; DOM listeners dispatch by
event name; sequence-numbered handler names disappear.
*Files:* `types/view.rs`, `types/check.rs`, `types/shape_ir.rs`, `rex-codegen/src/lib.rs`.
*Subtasks:*
1. `EventDef { name, params: Vec<(String, ParamTy)> }` with `ParamTy = Scalar(Encoding) | Id(sort) | Rel(A,B)`.
2. Check `on E(p…)`: params bind; body mutations checked as today
   (literals/args only — expressions arrive in S-40) but targets may be any
   param of id type.
3. `EventBinding.handler` becomes the event name; args are the `do` call's
   arg expressions restricted (for now) to binders, params, literals,
   extractors. Allow **enclosing binders** in `do` args: codegen passes
   ancestor keys to templates (`template(driver, key, ancestors)`), the
   shaper already knows `parentOf`. Update `ShapeNode.template` signature.
4. Reject an inline mutation list in a DOM handler unless it is exactly `do`s
   (deprecation path from S-10.6).
4a. Synchronous `do E(args)` inside an `on` body: the child handler's
   mutations join the same transaction against the same pre-event snapshot;
   the static `do` graph must be acyclic (compile error otherwise). Only the
   outer event is logged (§5 decision 2).
5. Codegen: `dispatch("TodoToggled", {t: key})`; drop the `@click4` naming.
*Acceptance:* Kanban's snapshot test updated; Playwright unchanged;
`tests/dispatch.rs` rewritten around named events.

#### S-21 Engine event log (M, deps S-20) — **done 2026-09-18** (`crates/rex-core/src/dbsp/log.rs`, `engine.rs`, `crates/rex-core/tests/log.rs`; deviations below)
*Goal:* every state change is an appended `Event`; replay reproduces state.
*Files:* `crates/rex-core/src/dbsp/log.rs` (new), `engine.rs`, `tests/log.rs` (new).
*Subtasks:*
1. `Event { seq: u64, name: String, args: Vec<(String, ArgValue)>, cause: Option<u64>, intent: Option<String> }`
   — `cause`/`intent` are reserved now for M4's async responses (§6
   decision 2) so the log format never migrates; unused in MVP — with
   `ArgValue = Value | Rel(Vec<(Value,Value,i64)>)`; canonical JSON encoding in
   `eval/encode.rs`.
2. `Engine::apply_event(&EventDef, &Event) -> StepResult` — the only public
   write path besides `apply_typed_stmt` for program setup. `dispatch(ops)`
   becomes private.
3. Genesis: `apply_typed_stmt` for `new` statements appends a synthetic
   `@genesis` event so the log is complete from empty.
4. `Engine::replay(events, silent: bool)`; `Circuit::step_silent` skips
   output delta collection.
5. Property test: random event histories; `replay(log)` from empty ==
   live engine's input integrals and every view.
*Acceptance:* tests green; replay of a 10k-event log is within 2× of live
apply time (record the number).
*Landed as:* `dbsp/log.rs` defines `Event`/`ArgValue` plus the reserved
`GENESIS`/`GENESIS_SORT` constants a genesis event tags its target sort
with; `eval::encode::event_to_json` is the canonical (one-way for now —
decode is S-22's `log_since`/`replay` wasm API's job) JSON encoding.
`Engine::dispatch` is now `pub(crate)`; the public write surface is
`Engine::apply_event(name, ops, args) -> (Vec<Value>, StepResult)`, which
runs the transaction and appends the log entry, plus `apply_typed_stmt`
(unchanged signature), whose `TStmt::New` arm now also logs a `@genesis`
event carrying the target sort (`GENESIS_SORT`) and every field. **Deviation
from subtask 2's literal signature:** `apply_event` takes already-derived
`ops`/`args` rather than `(&EventDef, &Event)` — deriving ops from an
`EventDef` (binding params, expanding nested `do`s) is `crate::events`'
existing machinery (`dispatch_event`, unchanged besides now calling
`apply_event`), and keeping that in `events.rs` rather than pulling
`types::shape_ir::EventDef` into the `dbsp` layer preserves the
engine/surface layering already in place. For the same reason,
**`Engine::replay` is `crate::events::replay(engine, env, events, log,
silent)`**, not a bare `Engine` method: replaying a non-genesis event needs
the same `EventDef` registry `dispatch_event` does, to re-derive its ops
from its logged args (`check_param` is skipped — the log was produced by an
already-checked dispatch, so replay is pure engine cost per §2.2/§2.10); a
`@genesis` event is special-cased (no `EventDef` — it never went through a
declared `on`) and reconstructed straight from its tagged sort via a new
`Engine::apply_new_silent`. `Circuit::step`/`backfill` share one `run(floor,
deltas, collect: bool)`; `step_silent`/`Engine::dispatch_silent` pass
`collect: false`. `crates/rex-core/tests/contract_fixtures.rs`'s direct
`Engine::dispatch` calls moved to `apply_event` (now the only way an
external crate can drive an arbitrary `DispatchOp` batch). Property test
(`replay_matches_live_for_any_history`, 256 proptest cases of 0–16 random
`AddList`/`Rename`/`MoveCard` ops) passes; perf recorded via an `#[ignore]`d
test (`replay_of_10k_events_is_within_2x_of_live_apply`, run with
`--release`): live apply of 10k events ~28ms, silent replay ~15ms — **0.53×**,
comfortably under the 2× budget (silent replay skips the per-step delta
clone `step` does, so it should generally beat live apply, not just meet
it).

#### S-22 wasm API around events (M, deps S-21)
*Files:* `crates/rex-wasm/src/lib.rs`, `js/rex-runtime/src/app.ts` (new; see S-30).
*Subtasks:*
1. `RexApp.dispatch(event, args_json)` takes the event name; relation args
   as `[[k,v,w]…]`.
2. `RexApp.log_since(seq) -> json`, `RexApp.replay(events_json, silent)`,
   `RexApp.base_snapshot() -> json` (input integrals + cursor),
   `RexApp.restore(base_json)` then backfill views.
3. Remove `apply_new`/`update_field(s)`/`retract` from the public wasm API
   (they bypass the log); keep `read_view`, `snapshot`.
4. Rebalance becomes a system event `@rebalance` declared implicitly for any
   manual-order level; `maybeRebalance` dispatches it.
*Acceptance:* Kanban runs; drag-storm Playwright test (100 drags into the
same gap) triggers a rebalance that appears in the log.

### E3 — Handler expressiveness

#### S-40 Mutation values as expressions over the pre-event snapshot (L, deps S-21) — **done 2026-09-18** (`types/shape_ir.rs`'s `ValExpr`, `types/view.rs`'s `val`/`val_expr`/`val_arith`/`resolve_val_ty`, `events.rs`'s `eval_val_expr`; `tests/dispatch.rs`'s S-40 section; also fixed a latent `arith_values` bug — see deviations)
*Goal:* `t.completed := not t.completed`, `t.n := t.n + 1`.
*Deviations from the literal subtasks:* view desugaring (which checks `on`
bodies) runs *before* the checker mints entity sorts, so it has no `Env`/
`SortId`s to check mutation values as real `TExpr` via `check_rel` without a
much larger reordering of `check()`. And even with that reordering,
`check_rel`'s ambient-domain model is built to type whole relations
(literals ground to `dom -> lit` over every id of an entity, per §4) — wrong
shape for a value that must point-evaluate to exactly one row per dispatch.
So mutation values got their own small sublanguage instead (§2.10's "Rex
expressions evaluated at a key" taken literally): `types/shape_ir.rs`'s
`ValExpr` (`Lit`/`Param`/`Field`/`Not`/`Arith`/`Compare`), checked in
`types/view.rs` against the same `entity_fields: HashMap<Entity, HashMap<Field,
Type>>` map view desugaring already collects (a parallel `ValTy`, not the
checker's `ValueTy` — no `SortId`s needed), and evaluated in `events.rs`'s
`eval_val_expr` by reading `Engine`'s live `Circuit` input integrals directly
(`InputKey::Field` + one indexed `.row(id)` lookup) rather than materializing
a `BTreeRelation` through `eval::interp::eval_expr_with`/a `Store` impl over
`&Circuit` — a real point read, not "evaluate the whole field relation and
take one row". `not` bakes its two-atom pair into the IR at check time
(`Not(Box<ValExpr>, [String; 2])`) since the evaluator has no type
environment to consult. `if … then … else` stays unimplemented (`check.rs`
already attributed it to S-52, not S-40, before this story started) and
`set filter = f` stays unimplemented (`state`, S-51). Added `Compare` (not
in the subtask list) so "conditional" has a real acceptance case: `t.big :=
t.n > limit` decodes to the atom `@true`/`@false`.
*Bug fixed on the way:* `eval::interp::arith_values` unconditionally read
operands through `as_cents()` (`Int` ×100), so a pure `Int + Int` (the
`money` flag false) silently returned a result scaled by 100 — never
exercised by an existing test since no prior test dispatched arithmetic on
two `Int`s. Now branches on `money` to pick `as_cents()` vs `as_i64()`.
*Acceptance:* `tests/dispatch.rs`: `toggle_flips_a_two_atom_field`,
`increment_adds_to_the_fields_current_value`,
`conditional_set_from_a_comparison`,
`increment_matches_an_independent_batch_oracle` (computes the expected value
via a *fresh* `eval::interp::run` of the source plus the shared
`arith_values` kernel, independent of the live engine, then checks the
engine's dispatch produced exactly that).

#### S-41 `where`-targeted bulk update/delete as hidden views (M, deps S-40) — **done 2026-09-21** (`types/shape_ir.rs`'s `Target`, `types/view.rs`'s `Desugar::mutation_target`/`expr_refs_scope`/`rewrite_row_refs`, `events.rs`'s `resolve_target`; `tests/dispatch.rs`'s S-41 section)
*Goal:* `update Todo where P { … }`, `delete Todo where P`, `delete Todo`.
*Deviations from the literal subtasks:* the surface is `update`/`delete
Entity where P` (the existing `HStmt::Update`/`HStmt::Delete` target
position), not a new `Todo where P update {…}` postfix form — no parser
work was needed, `target: Expr` already accepts `ExprKind::Where`.
Arg-freeness is checked syntactically (`expr_refs_scope`: does `P` mention
any event param?) rather than by running the S-40 interpreter and observing
whether it needed an arg — cheaper, and conservative in the safe direction
(a shape the walk doesn't recognize counts as arg-dependent, so the worst
case is an avoidable scan, never a wrongly-cached view). The arg-dependent
scan predicate is checked as an ordinary `ValExpr` (not a fresh mechanism):
a `.field` path is rewritten (`rewrite_row_refs`) to reference a synthetic
scope param (`shape_ir::ROW_SELF`), so `val_expr`'s existing `Field` case
handles it unchanged, and `events.rs::resolve_target`'s scan binds
`ROW_SELF` to each candidate row in turn using the same `eval_val_expr` S-40
already has. No checker *note* diagnostic was added for the O(N) scan cost
(SPEC §10 spirit) — nothing currently renders a note-severity diagnostic
anywhere in the checker, so this would have been the first, out of scope
for a story about dispatch semantics. The hidden view name is
`on#{event}#{seq}` with `seq` a single counter across the whole program
(not per-event as the literal template suggests) — simpler, still unique,
still legible. Hidden views are collected during Pass 0 (`on` handlers are
checked before entities are re-emitted) but spliced into the program after
every other statement, so a hidden view — which reads an entity — never
lands before that entity regardless of source order.
*Acceptance:* `tests/dispatch.rs`: `delete_with_no_predicate_retracts_every_row`
(subtask 3), `where_delete_arg_free_uses_a_materialized_hidden_view` (asserts
the hidden view exists and already holds exactly the matching keys *before*
any dispatch — the proof that dispatch reads a materialized keyset rather
than scanning), `where_update_arg_dependent_scans_by_predicate` (subtask 2).

#### S-42 `insert … from` with relation-valued params (M, deps S-40) — **done 2026-09-21** (`types/shape_ir.rs`'s `FromField`/`MutationIR::InsertFrom`, `types/view.rs`'s `Desugar::new_from`/`named_val_ty`, `events.rs`'s `RelArgs`/`InsertFrom` expansion/`check_param` split, `rex-wasm/src/lib.rs`'s `dispatch`; `tests/dispatch.rs`'s S-42 section)
*Goal:* mint one entity per row of a relation-typed event param, in one transaction.
*Deviations from the literal subtasks:* the surface is the existing `new
Entity from source as (k, v) { … }` (parsed since S-10 as `HStmt::New`'s
`FromClause`), not a new `insert … from` keyword — `insert` doesn't exist as
surface syntax anywhere in the implemented grammar, and `new … from` is
exactly this shape already. `rows : Int -> Text`'s two sides don't need to
be entities: `Desugar::named_val_ty` resolves either side (an entity or a
scalar type name) to a mutation-value `ValTy` by reusing `resolve_val_ty`
against a synthesized `Type`, so `pos: k` / `label: v` type-check against
the target entity's declared fields exactly like any other mutation value.
`dispatch_event`'s public API changed from `&HashMap<String, Value>` to
`&HashMap<String, ArgValue>` (`ArgValue` already existed, added for S-21's
log with S-42 in mind) so a relation-typed arg can cross the boundary
alongside scalars; `check_param` split into a `ParamTy`/`ArgValue` shape
match plus the original scalar checks, unchanged. `ops_for_event` (the
replay path) gained real `ArgValue::Rel` handling as a side effect — it
previously errored "not supported yet" on any relation-valued logged arg,
which was a latent replay gap for `@rebalance` events sharing the same
`ArgValue::Rel` shape. Subtask 3 (`Extractor::Js`, a client-side
`makeRows`/benchmark harness) is out of scope for this pass: it's DOM/JS
work with no benchmark harness present in this repo, not a `rex-core`
dispatch concern; the wasm bridge's `dispatch` was extended to decode a
JSON array arg into `ArgValue::Rel` (previously it errored on any array),
so the engine side is fully wired for whatever produces the rows.
*Acceptance:* `tests/dispatch.rs`: `new_from_mints_one_row_per_source_row_in_one_transaction`,
`new_from_mints_in_key_order` (subtask 2).

### E4 — `Unit`, `state`, `match`

#### S-50 `Unit` sort, `unit` constant, and `type` declarations (M, deps S-10) ∥ with E2 — **done 2026-09-25** (`types/typed.rs`'s `Lit::Unit`/`Total`/`UNIT`, `types/check.rs`'s pass 0 + `atom_rel`, `types/env.rs`'s `declare_type`/`ctor_owner`, `dbsp/node.rs`'s `Node::Aggregate { total, seeded }`, `dbsp/circuit.rs`'s third wake exception, `eval/interp.rs`'s `agg_identity`; `crates/rex-core/tests/unit_sort.rs`, 16 tests)
*Scope note:* `type` declarations were folded into this story rather than
left loose — `types/view.rs` already rejected them with *this story's* name
on the message, and `Unit`-rooted TodoMVC needs `Filter`/`Bool` to exist.
*Landed as:* `ValueTy::Unit`/`Value::Unit` already existed in the value
algebra (encoded `u`), and `ground.rs` already treated `Unit` as groundable,
so the story was smaller than written on the `Unit` side and larger on the
`type` side.
- `unit : X -> Unit` checks as a grounded constant relation (`Lit::Unit`),
  the same machinery as a literal constant, and lowers to
  `MapConst(identity(dom), Unit)` — no new circuit node. It is a built-in
  *name*, not a reserved word, so a program binding `unit` shadows it.
- `count(X by unit)` tags its aggregate `Total::Unit`, since `X by unit`
  regroups everything under the one `Unit` point and the group key is
  therefore present whether or not `X` has rows. **This decides the README's
  open "empty group produces no row" question in the `by unit` direction:** a
  total group emits its monoid *identity*. `Count`/`Sum` have one;
  `Min`/`Max` do not and `Avg` is not a monoid, so those still emit nothing.
  Both evaluators implement it (`tests/dbsp.rs` holds them to each other),
  the key survives its image emptying so the identity comes *back*, and
  `Circuit::run`'s dirty-cone skip gained a third fire-from-no-children
  exception beside `ConstSingleton`/`FixOutput` — with no rows there is no
  delta to wake the node with.
- `type Filter = All | Active | Completed` resolves to `{@All | @Active |
  @Completed}`: **a constructor names its atom verbatim** (§5 decision 3,
  amended). A bare constructor is an atom literal in expression position,
  mutation values, and `do` arguments; a binding or param of the same name
  shadows it; a constructor may belong to only one type. `Bool` is
  predeclared, which also retires `resolve_val_ty`'s opaque-`Bool`
  placeholder, so `not t.completed` now knows which two atoms to flip.
- `Scope` gained the param's resolved mutation-value type: `EventDef` keeps
  only the wire `ParamTy`, and `Scalar(Encoding::Atom)` cannot say *which*
  atoms a declared type admits — without it `on SetFilter(f) => t.shown := f`
  failed against a `Filter` field.
*Known gaps left for their own stories (surfaced by re-running the ignored
acceptance programs):* `update Todo { … }` with no `where` reports "unknown
parameter `Todo` in mutation" — a bulk-update-without-predicate gap in S-41,
which only did the `delete` case; and relational `not` in a view expression
(`Todo where not .completed`) is still S-52's `except (where P)`.

*Original story text:*
*Files:* `types/ty.rs`, `types/env.rs`, `check.rs`, `eval/interp.rs`,
`dbsp/lower.rs`, `SPEC.md` §3.
*Subtasks:*
1. Built-in sort `Unit` with one value; `unit : X -> Unit` constant relation
   grounded by the ambient domain (same machinery as `Const`).
2. `count(Todo by unit)`, `sum(... by unit)` type-check to `Unit -> M`.
3. SPEC §3/§5 paragraph; interp + circuit tests; empty-group aggregate over
   `unit` yields the monoid identity (this *decides* the README "empty group
   produces no row" question: for `by unit`, emit `0`).
*Acceptance:* `let n : Unit -> Int = count(Todo by unit)` evaluates
incrementally under insert/retract to 0 when empty.

#### S-51 `state` as a singleton relation (M, deps S-50, S-40) — **done 2026-09-25** (`types/shape_ir.rs`'s `STATE_ENTITY`/`STATE_ROW`/`ValExpr::State`, `types/view.rs`'s state collection + `HStmt::Set`, `types/check.rs`'s `state_rel`, `events.rs`'s `ValExpr::State` arm; `crates/rex-core/tests/state.rs`, 10 tests)
*Landed as:* every `state s : T [= d]` is a field of one hidden `State#`
entity, whose single row is emitted as an ordinary top-level `new` — so S-21's
genesis logging already covers it and replay from an empty engine restores the
state with no new mechanism (`the_state_row_is_logged_as_genesis_so_replay_reproduces_it`).
A defaultless `state` simply contributes no field initializer, so it is 0 rows
until `set` (chat's `current : User`); reading it before then is a dispatch
error rather than a silent null.

**A bare state name elaborates to `unit . ~(State# . unit) . .s`.** §2.4 said
"referenced via `unit`" without saying how, and the answer turned out to need
no new machinery at all: `unit : X -> Unit` grounded in the ambient domain
(S-50), the *inverse* of `State# . unit` to reach the one row through `Unit`,
then the field. Every hop is an operator the circuit already maintains, which
is exactly what makes §2.4's promise true — a `set` is one field delta flowing
through two joins. The acceptance test asserts the observable form of that: a
view that does not read the state does not move at all when the state changes,
however many rows are downstream. When the ambient domain is already `Unit`
(an S-53 root level) the first hop is skipped.

`set s = v` is `MutationIR::Set` with `Target::All { entity: "State#" }` — the
singleton needs no `Target` variant of its own, since "every row" is that row.
Reading a state *inside* a mutation value (`set nextId = nextId + n`) is
`ValExpr::State`, a point read of the one row against the pre-event snapshot,
so the benchmark's increment is well defined rather than a loop.

*Also in this story (folded in at the owner's request):*
- **The S-41 bulk-update gap.** `update Todo { … }` with no `where` reported
  "unknown parameter `Todo` in mutation": S-41 special-cased a bare entity
  target inside the `delete` arm only. That case moved into
  `Desugar::mutation_target`, so both verbs share one target rule and cannot
  drift apart again. Covered by `update_with_no_predicate_sets_every_row`,
  which also asserts it is one step (4 retractions + 4 assertions in one
  delta batch), and it is what TodoMVC's `ToggleAll` needs.
- **A `Bool` coherence bug S-50's verbatim spelling created.** Comparisons
  still produced lowercase `@true`/`@false` while `Bool` had become
  `{@True | @False}`, so a comparison could not be stored in a `Bool` field.
  `TRUE`/`FALSE` are now single constants in `types/typed.rs`, shared by the
  checker, the desugarer and the dispatcher. A constructor in *value*
  position (a top-level `new`'s field initializer) also needed the S-50
  treatment in `check_value`, which the type story missed.
- `check.rs`'s relational-`not` diagnostic said "MVP-PLAN S-40", a story that
  has landed; it is S-52's `except (where P)` and now says so.

*Still open for their own stories:* TodoMVC is down from 8 check errors to 4,
all of them S-52 (`match`, relational `not`), S-53 (`Unit` root) and S-61
(components). Chat additionally needs `let x = new …` inside a handler.

*Original story text:*
*Files:* `types/view.rs` (the `state` rejection at line ~58), `check.rs`, `engine.rs`.
*Subtasks:*
1. Desugar `state filter : {@all+@active+@completed} = @all` to a hidden
   entity `State#` with one genesis row and field `filter`; a bare `filter`
   in expression position elaborates to `unit . :filter`.
2. `set filter = e` in handlers = `Set` on the singleton.
3. Checker: a state name shadows nothing; union-literal types are just atom
   coproducts.
*Acceptance:* a view filtered by state changes membership with one field
delta; test asserts no view is backfilled/recomputed (count circuit node
evaluations).

#### S-52 `match` / `if` in filters and attributes (M, deps S-51) — **done 2026-09-26** (`types/check.rs`'s `expand_match`/`gated_union`/`as_predicate`/`coerce_bool_filter`, `types/view.rs`'s `gate_view`, `rex-codegen`'s `BindKind::Class` arm, `js/rex-dom`'s `AttrBinding` union; `crates/rex-core/tests/match_if.rs` (9), `crates/rex-codegen/tests/class_gate.rs` (3), 3 new vitest cases)
*Landed as:* `match`, `if … then … else` and relational `not` **expand to
core forms in the checker and are then checked normally**, so none of them
adds a node to the typed IR, the batch evaluator or the circuit — and the
oracle property tests cover them for free. The expansions are in SYNTAX.md's
table; each is also asserted against a fresh batch evaluation.

The one subtlety: an arm's gate must be *coreflexive on the ambient domain*,
so every gate is built as `id where c` rather than `c` itself. Composing a
bare `Todo -> Bool` would thread `Bool` through as the next ambient domain
and the arm body would then fail to ground. Wrapping in `where` also routes
the condition through filter position, which is where a `Bool` picks up its
implicit `= True` — so `if .completed then … else …` and `match` arms get
that coercion by construction rather than by a second rule.

*Deviation from the literal subtasks:* subtask 1 wrote the surface as
`Todo where match filter { … }`, but the acceptance program (`app.rex`, the
S-02 authority) has `match` at *expression* level as a `let` body, with whole
relations as arms. That is what was implemented. `_` is the complement of
every named gate, and must come last.

`not P` is `id except P`, not `except (where P)` applied to a specific base —
the complement within the ambient domain. This matters at 6NF: a row with no
value for the field at all *is* "not completed", which `= False` would miss
and the complement gets right. Covered by
`relational_not_is_the_complement_within_the_domain`.

Subtask 2 (class binds by presence) went end to end, since TodoMVC's
`class.selected=(filter = All)` is coreflexive-valued and could never have
matched a decoded atom:
- `Desugar::gate_view` emits `Binder where e` for a `class.x=` bind, so the
  attribute view is a coreflexive gate holding the key exactly when the class
  is on.
- Codegen emits `classList.toggle(x, v !== undefined)` plus `presence: true`
  on the binding.
- `js/rex-dom`'s attribute contract became a discriminated union: a value
  attribute's `apply` still takes `string`, a presence attribute's takes
  `string | undefined`, and the shaper applies the latter **on absence too**.
  Without that the class could be turned on but never off — the shaper's
  `if (v !== undefined)` guard silently skipped the retraction.

*Also fixed:* codegen's `checked` prop compared against `encodeAtom("true")`,
the last lowercase-`Bool` site left over from S-50's verbatim spelling.

*Result:* TodoMVC is down to **2** check errors, S-53 and S-61. Per the
owner's call, the element-level `if (total > 0) { … }` that wraps static
chrome stays in S-53 — it is a `Unit`-level conditional membership view, not
a bind.

*Original story text:*
*Files:* `types/view.rs`, `check.rs`, `shape_ir.rs` (`BindKind::Class` with
an expression), codegen.
*Subtasks:*
1. `Todo where match filter { @all => true, @active => not :completed, … }`
   desugars to a union of `Todo[filter = @k] & (branch)` per arm (SPEC §4
   coreflexives); `_` arm = complement of the other keys.
2. `class.danger = (id = selected)` and `class.done = :completed` bind
   through a coreflexive-valued attribute view; codegen toggles by presence
   (a row present ⇒ class on) — no boolean decoding at all.
3. `if c then "a" else "b"` in a text/attr bind = two coreflexive-gated
   constant relations unioned.
*Acceptance:* TodoMVC filter buttons work with node identity preserved
(Playwright: toggling filter keeps the surviving `<li>` nodes).

#### S-53 `Unit` root levels and scalar binds in views (M, deps S-50, S-20) — **done 2026-09-29** (`types/view.rs`'s `Desugar::view`/`walk_level`/`LevelWalk::gate`/`base`, `types/check.rs`'s `UNIT_ROOT`/`UnitConst`, `types/typed.rs`'s `UnitPoint`/`UnitConst`; `crates/rex-core/tests/unit_root.rs` (7), `crates/rex-codegen/tests/unit_root.rs` (1))
*Landed as:* a bare-element `view` is one `ShapeLevel` with `entity: "Unit"`,
membership `let main#unit = unit#root`. `unit#root` is a desugarer-only name
(the `#` keeps user code from writing it) for the coreflexive point
`{unit ↦ unit}`: it needs no ambient domain, which is what lets a hidden
`let` for a `Unit`-level bind go **un-annotated** like every other view `let`.
Two checker additions made everything else in the level ground with no new
circuit node beyond one `ConstSingleton`: `TExprKind::UnitPoint`, and
`UnitConst(lit)` — what a literal (`0` in `total > 0`, `All` in
`filter = All`) grounds to when the ambient domain is `Unit` instead of an
entity, lowered as `MapConst(ConstSingleton(unit), lit)`. Binds, class gates
and state reads at a Unit level all reuse `LevelWalk::base()` (the point
instead of the entity), so `active`, `filter = All` and `count(… by unit)`
work unchanged; a bare state skips its first hop, as S-51 anticipated.

`if (c) { … }` is **one child level per element in the body**, over the same
point/row, membership `Base where c`. A level holds one root element, so the
alternative — a single wrapper — would have changed the DOM. Because the
gate is coreflexive the child key equals its parent's, the shaper mounts and
removes it as `c` flips, and it works at *any* level (an entity row too), not
just the root. A `select` directly under a `Unit` level takes membership
`E . unit` and needs no `where`. Two `select`s of one entity in a level now
get distinct names (`…#todo`, `…#todo2`); `collect_child_levels` and the
walk number them in the same document order.

*Sibling order (fixed later):* levels mounting into one element used to append
in mount order, so a gate that flipped after a later sibling was already there
landed out of source order. `ShapeLevel.anchor` (the first static child that
follows the level) and `slotKey` (which levels share an element) now reach the
shaper, which puts a level's last row before the first mounted row of a later
same-slot level, else before the anchor. Tests: `rex-dom/test/shaper.test.ts`
("sibling order under one element"), `rex-codegen/tests/handlers.rs`.

*Acceptance:* `unit_root.rs` checks the root is one level with the one-point
membership; the gate asserts exactly one assertion at the root key when the
count goes 0→1 and *no* delta on the gate for 1→2; the count bind is a
single −1/+2 pair at one key (the shaper fuses it to one `setText`, S-03's
contract); a class gate over `filter` retracts on `Set`. A shaper-level
spy-driver test was not added: it would only re-prove S-03's fusion contract
on a new fixture. **TodoMVC now checks with only S-61 errors left** (component
decl + `select TodoItem(t)`); `todomvc_checks`'s ignore reason says so. Chat's
remaining errors are a binder used as an expression (`u = current`) and
`let x = new …` in a handler; the benchmark's are `import js` and untyped
extractor params.

*Original story text:*
*Files:* `types/view.rs`, `shape_ir.rs`, codegen, `js/rex-dom` (none expected).
*Subtasks:*
1. A `view` whose body starts with an element (not a `select`) gets an
   implicit root level over `unit`; membership view = `unit`, key `""`.
2. Text/attr binds at that level may be any `Unit -> V` relation
   (`span { count(visible by unit) }`).
3. Static children and multiple nested `select`s under one root.
*Acceptance:* TodoMVC's header/footer render; the count updates with one
text mutation per change (spy-driver test via S-03 fixtures).

### E5 — Derived binds and components

#### S-60 Bind any relation co-keyed with the level (M, deps S-53) — **done 2026-09-29** (`types/view.rs`'s `LevelWalk::subst`/`emit_bind`/`Desugar::view_stack`, `types/check.rs`'s `bind_ctx`, `types/component.rs`'s `Renamer`; `crates/rex-core/tests/binds.rs`, 7 tests)
*Landed as:* most of the story was already true — a non-path bind has been
`Level . expr` since S-20 and the checker types it — so the work was the
three things that were not:
- **A binder is a value in a bind.** `(u = current)`, `l.user.name`, and an
  *enclosing* level's `m.text` all failed with "unknown name". `LevelWalk::subst`
  rewrites each explicit `as` binder in a bind/gate expression before it is
  emitted: the level's own binder is `id`, and an ancestor's is the chain of
  membership views up to that level (child→parent, then parent→grandparent…),
  read off `Desugar::view_stack`, so `m.text` from two levels down is one more
  join, incrementally maintained like any other. Only an *explicit* alias
  substitutes — a defaulted binder (`Card select …`) is also the relation's
  name (`count(Card by .list)`), so it never does. This reuses S-61's
  `Renamer`, generalised from name→name to name→`Expr`.
- **Diagnostics.** A bind view is now emitted at the bind's own source span
  (it was `Span::point(0)`, so every bind error pointed at byte 0), and a
  failed bind is prefixed `not co-keyed with `c : Card`: …` (the root reads
  `the `Unit` root`). Done in the checker off a `Desugared::bind_ctx` side
  table rather than a new `LetDecl` field, which the parser and tests all
  construct. "Unknown name" errors are left alone — that is not a co-keying
  problem.
- **Inline aggregates** (`span { (count(Card by .list)) }`) already type and get
  their encoding from the elaborated view (`type_binds`); now pinned by a test.
  Note the parentheses: a bare `count(…)` in an element body parses as an
  element.
Kanban's per-list count was already a bind on a named `let`
(`{ cards }`), so its output is unchanged; I did not rewrite it inline.

*Result:* **`chat` now fails only on `let x = new …` inside a handler** (no
story owns it yet). TodoMVC is unchanged by this story — see S-62.

*Original story text:*
*Files:* `types/view.rs`, `check.rs`.
*Subtasks:* a bind `{ e }` where `e : Binder -> V` is any expression (join,
FK path `:author.name`, aggregate `count(Comment by :post)`); the desugarer
emits the attribute `let` and the checker verifies co-keying; `:field`
remains the common case. Diagnostics name the expected domain.
*Acceptance:* Kanban shows per-list card counts; a bind on a wrong domain is
a "not co-keyed with `l : List`" error.

#### S-61 Components with props by inline expansion (M, deps S-60) — **done 2026-09-29** (`types/component.rs`'s `Expander`/`Renamer`, `types/view.rs`'s `expand_components`/`source_entity`; `crates/rex-core/tests/components.rs` (7), `crates/rex-codegen/tests/components.rs` (1))
*Landed as:* a **pre-pass**, not an expansion inside the level walk.
`Desugar::view` expands every component call in a root view's body first
(`component::Expander`), so level lowering, `collect_child_levels`' naming and
`focus(c)` resolution only ever see plain elements and `select`s and cannot
disagree about what exists. The expander tracks the `(binder, entity)` scope
itself, since a call may sit under any number of nested `select`s and each
argument is checked against the type of the binder it names.

A call renames the template's params to the caller's binders (`Renamer`, an
exhaustive walk over `Expr`/`HStmt`/handlers/content — a bare identifier is
renamed, but never the field on the right of a `.`, so `x.text` survives a
param called `text`). Checked: unknown component, arity, an argument that is
not a binder in scope, wrong entity (`ListID` spelling accepted), recursion
(a stack of components being expanded; the call's own `{ … }` block is
expanded in the caller's scope *before* the callee is pushed, so
`Card { Card { … } }` is nesting, not recursion), and a block needs exactly one
`children` slot (two is an error even with no block). A component is any
`view` with an element body; only ones with params are template-only — a
zero-param view still lowers as a root too.

*Also needed:* `visible as t select TodoItem(t)` ranges over a derived keyset
`let`, not an entity, so `select` now resolves its source to an entity
(`Desugar::source_entity`: an entity, or a top-level `let x : Entity`). The
`ShapeLevel`/binder/handler typing use the entity; the membership, order and
attribute views are still built on the source as written, so the keyset
restriction applies (`LevelWalk::source` vs `entity`).

*Deviation:* level names do **not** gain a `#TodoItem` segment
(`app#todo#TodoItem` in the story text). A select-body component adds no level
of its own, and the call site already names the level, so there is nothing to
disambiguate — and it is what makes the generated TS *byte-identical* to the
hand-inlined view, which is the stronger form of the acceptance line
(`generated_code_is_identical_to_the_inlined_version`, and
`a_component_expands_to_the_same_shapes_as_the_inlined_view` on the ShapeIR).
Nested components and a component-in-a-component are covered.

*Result:* **TodoMVC now fails to check only on S-62** (`local editing`,
`set editing`, and the `editing` name they define). Chat additionally shows a
binder used as an expression (`u = current`, `l`), which is S-60 territory.

*Original story text:*
*Files:* `ast.rs` (done), `types/view.rs`.
*Subtasks:* `view TodoItem(t: Todo) = li { … }` is a template; `TodoItem(c)`
inside a `select` expands with positional binder substitution; a
`Name(args) { … }` block is substituted at the component's single
`children` slot (error if the component has none, or has two); level names include the
call site (`app#todo#TodoItem`); recursion between components is an error.
*Acceptance:* TodoMVC's `TodoItem`/`Footer` are components; generated TS is
identical in shape to the inlined version (snapshot).

#### S-62 `local` per-instance state (S, deps S-61, S-51) — **done 2026-09-29** (`types/view.rs`'s `Desugar::local_decls`, `types/component.rs`'s `local_view`/`local_event` and `Renamer::sets`; `crates/rex-core/tests/locals.rs`, 6 tests)
*Landed as:* a `local` is a hidden **field** on the entity of the component's
first param (`local#TodoItem#editing : Todo -> Bool`), not a hidden entity: it
needs no row to mint, and it dies with the row for free (deleting a Todo
retracts its fields — `a_local_dies_with_its_row`). Absence is the default, so
nothing is written until the first `set`. A read view is emitted right after the
entity, `(K . .f) | ((K except .f) . default)` — the `except` defaulting the
story text asked for — and a bare `editing` in the component body expands to
`arg . local#TodoItem#editing`, which S-60's binder substitution then turns
into `id` (or an ancestor chain), so the local is read at the *argument's* row
wherever the call sits. A `set editing = v` in a DOM handler becomes
`do local#TodoItem#editing#set(arg, v)`: a real, declared event with a checked
`Set` body, so it is logged and replays like any other
(`a_local_is_logged_and_replays`), and it is one step touching only the
local's own views (`setting_a_local_is_one_step_on_one_field`).

The type is the annotation, or is inferred from the default literal
(`False` → `Bool`, `0` → `Int`, `"x"` → `Text`); anything else asks for an
annotation. A `local` needs a component whose first param is an entity — a
root view or a non-entity param is an error saying what to key it by. The
event's value param is named `#value` so it cannot collide with the
component's own param name.

*Not done here:* the acceptance line's Playwright check (double-click to edit
with focus preserved) has no app to run against yet — `examples/todomvc` has
only `app.rex` until **S-90** builds it. What is proven at this level: the
class gate for `editing` flips per row on one field delta, and
`focus(.edit)` targets an `input` that is always in the DOM (the class only
toggles visibility), so nothing is mounted or removed when editing starts,
which is what preserves focus. S-90 should carry the browser assertion.

**TodoMVC (`examples/todomvc/src/app.rex`) now checks clean and `rex build`
generates code for it**; `todomvc_checks` is un-ignored.

*Original story text:*
*Subtasks:* `local editing : Bool = @false` in a component desugars to a
hidden entity keyed by the component's binder (`Todo -> Bool`) with the
default applied via `except`-based defaulting (`present + (Todo except present) . @false`);
`set editing = …` in a `do` body targets the instance key.
*Acceptance:* TodoMVC edit-in-place (double-click) with focus preserved.

### E6 — Ordering (∥ with E2–E5)

#### S-70 Typed order keys, `desc`, tiebreak (M, deps S-04) — **done 2026-09-29** (`js/rex-dom/src/order.ts`'s `compareEncoded`/`OrderIndex(desc)`, `types.ts`'s `orderDesc`, `shape_ir.rs`'s `order_desc`, `types/view.rs`, codegen; `js/rex-dom/test/order.test.ts` (10), `rex-codegen/tests/unit_root.rs`)
*Was it a real dependency of S-90? Yes.* TodoMVC is `order by id`, and ids
encode as `#3:9` / `#3:10`: compared as strings, the tenth todo sorts before
the ninth. (The same bug bites any `Int` order key — the benchmark's `pos`.)
*Landed as:* the comparator is picked by the **value's own encoding tag**
rather than a new `orderKind` field threaded through the IR — the wire format
is already self-describing (`i:`/`m:` numeric via `BigInt`, `#sort:seq`
numeric by sort then sequence, everything else string order), so codegen needs
to say nothing about types and a mixed/unknown key still sorts totally. The
child key is the ascending tiebreak (numeric for ids too), also under `desc`.
`order by … desc` is the only new IR: `ShapeLevel.order_desc` →
`orderDesc: true` on the emitted `ShapeNode`, and the checker's "not
supported yet" error is gone. Acceptance: `3 < 10`, ids `9 < 10`, and the
benchmark's `SwapRows` is exactly two `insertBefore`, zero `createElement`/
`removeChild` (spy driver).
*Not done:* `orderedChildren` still returns encoded keys (its only consumer,
the rebalance/drop helpers, wants them encoded).

*Files:* `js/rex-dom/src/order.ts`, `types.ts` (`ShapeNode.orderKind`),
`shape_ir.rs`, codegen.
*Subtasks:* `OrderIndex` takes a comparator built from the order view's
`Encoding` (Text/Int/Money/Date) and direction; child key tiebreak stays;
`order by :pos desc`; `orderedChildren` returns decoded keys.
*Acceptance:* vitest: Int keys 3 < 10; benchmark's `SwapRows` is exactly two
`insertBefore` calls (spy driver).

#### S-71 Rebalance as an event + drag helpers cleanup (S, deps S-22, S-70) — **done** (the event in S-22; `Rebalancer`/`FieldWriter` removed in S-30; `prompt(...)` is gone)
*Subtasks:* `maybeRebalance` dispatches `@rebalance`; `interact.ts` loses
its `FieldWriter` dependency on the wasm app; `prompt(...)` extractor
removed.
*Acceptance:* no direct writes remain in `js/rex-dom` (grep `update_field`
returns nothing).

#### S-72 (post-MVP) Hidden manual order: `order manual`, `move x before y`
Design only for MVP: a one-page note under `docs/ordering.md` consistent with
the memory note "fractional keys are an impl detail".

### E7 — Package extraction

#### S-30 Split `rex-dom` / `rex-runtime`; move `pkg/` (M, deps S-01) ∥ — **done 2026-09-30** (`js/rex-dom/src/types.ts`'s `EnginePort`, `js/rex-runtime/src/app.ts`'s `Engine`, both `package.json` `exports` maps, `scripts/build-wasm.sh`/`build-js.sh`, `.github/workflows/ci.yml`)
*Files:* `js/rex-dom/*`, `js/rex-runtime/*` (new), `examples/*/package.json`,
`scripts/build-wasm.sh`, codegen preamble.
*Subtasks:*
1. `js/rex-runtime`: `pkg/` (wasm-bindgen output, built by S-01's script),
   `src/app.ts` — a typed wrapper over `RexApp` (`dispatch(event, args)`,
   `snapshot()`, `log`, `replay`, `restore`) implementing `rex-dom`'s
   `EnginePort` interface; `src/boot.ts` (init wasm, mount, dispatch loop).
2. `js/rex-dom`: remove every import of the wasm app; `interact.ts` takes an
   `EnginePort`/dispatch callback; export `EnginePort`.
3. Both packages: `exports` map, `tsc` build to `dist/`, `npm pack` works;
   examples depend on `file:` links to the packages (workspaces at
   `js/package.json` optional).
4. Codegen preamble imports from `rex-runtime`/`rex-dom` only.
*Acceptance:* `npm pack` produces two tarballs; Kanban runs against them;
vitest for `rex-dom` runs with no wasm on the path.
*Landed as:* **`EnginePort`** (rex-dom) is `{ snapshot(): StepDeltas;
dispatch(event, args): { ids, deltas }; rebalance(field, rows): StepDeltas }`
over typed values, so no JSON string crosses it. rex-runtime's **`Engine`**
implements it over the wasm `RexApp` and owns all the JSON; `boot` now returns
that `Engine` (plus `flush`/`saveSnapshot`) instead of monkey-patching the raw
wasm object, with persistence as an `afterWrite` hook. `maybeRebalance(port,
shaper, …)` takes the port and applies the batch itself, so `Rebalancer` and the
`apply` callback are gone and nothing in rex-dom knows about wasm (S-71's
"no direct writes" line is met too). The wasm glue builds into
`js/rex-runtime/pkg/` and is the **`rex-runtime/wasm`** export; nothing is
copied into examples any more. Generated code imports only `rex-runtime/wasm`,
`rex-runtime` and `rex-dom`, and exposes the typed engine as `window.__rexApp`
(`logSince`, `flush`, `.wasm` for raw access) — the e2e specs use `flush()`
instead of sleeping for IndexedDB. The profiler is now two halves
(`engine(…)` inside `Engine.dispatch`, `shaper(…)` from the caller) because the
parse moved inside the wrapper; `__rexProfile.timings` rows are unchanged.
*Packaging:* both packages have an `exports` map to `dist/` (`tsc -p
tsconfig.build.json`, `prepack` builds; rex-runtime's also refuses to pack
without `pkg/`). rex-dom is a *peer* of rex-runtime (types only, no runtime
import). `npm pack` gives two tarballs (17.9 kB; 285 kB with the wasm). Verified
by installing both tarballs into a scratch copy of Kanban outside the repo
(real `node_modules` copies, no links): `tsc`, `vite build` and all 12
Playwright tests pass on the dev server. Needed on the Vite side:
`optimizeDeps.exclude: ["rex-runtime"]` (pre-bundling breaks the glue's
`new URL(…, import.meta.url)`) and, for the in-repo `file:` links only,
`server.fs.allow: ["../.."]`. CI gained a `packages` job (packs both, checks the
tarball contents) and builds `dist/` before the examples.
*Not done:* no workspaces file (the `file:` links work and the plan called it
optional); not published to npm; the examples must run `scripts/build-js.sh`
after changing either package, because they consume `dist/`, not `src/`.

#### S-31 Contract doc + README for both packages (S, deps S-30, S-03) — **done 2026-09-30** (`js/rex-dom/README.md`, `js/rex-runtime/README.md`, `js/rex-dom/examples/board-shape.ts`, `test/readme.test.ts` in both)
*Subtasks:* `js/rex-dom/README.md`: the delta protocol (`Tuple`, `StepDeltas`,
roles), the four phases, the −/+ guarantee, `ShapeNode` reference, how to
write a `DomDriver`; `js/rex-runtime/README.md`: boot, events, log,
persistence adapters. Link the S-03 fixtures as the conformance suite.
*Acceptance:* a reviewer can write a `ShapeNode` by hand from the README and
drive the shaper with a fixture (add exactly that as an example test).
*Landed as:* rex-dom's README covers the delta protocol (`Tuple`/`StepDeltas`,
the encoding table, a real fixture row), the **role table** (what a lone +, a
−/+ pair and a lone − mean for membership/order/attribute views, and why a
change is never remove+mount), presence attributes, the phases of `applyStep`,
ordering, a full `ShapeNode` reference (including `slot`/`slotKey`/`anchor`),
how to write a `DomDriver`, the `EnginePort`, and the S-03 fixtures as the
conformance suite with the regenerate command. Its centrepiece example is
`examples/board-shape.ts`: the Kanban board's `ShapeNode` tree, hand-written and
generic in the element type. **`test/readme.test.ts` embeds it**: it asserts the
README's code block *is* that file byte for byte (so it cannot drift), drives the
shaper with the real engine fixtures (`00-mount`, `01-rename`) and checks the
documented DOM and mutation counts; `contract.test.ts` now imports the same
file. It also checks every helper the README names is a real export. Reading the
README at the "can I write a shape tree from this" bar is the acceptance, and the
example is the proof it can be done from the document alone. rex-runtime's README
covers boot, the typed `Engine`, bundler setup, the event log (`@genesis`,
`@rebalance`, determinism), the persistence flow and adapter contract,
`programKey`'s "editing a program discards saved state" consequence, profiling,
and testing without wasm; its test checks the exports named in the README exist
and that every export is documented. The wasm-dependent snippets are not
executed by a test.

#### S-32 Boundary batch entry + V8 profile (M, deps S-22) — *perf, can slip*
*Subtasks:* `replay` crosses once for N events (done in S-22); measure with
`crates/rex-core/benches/dbsp_vs_batch.rs`'s dataset in Node and Bun;
profile allocation hot spots (`Value` clones, `BTreeMap`) and record findings
in ROADMAP §3.2. No optimisation beyond low-hanging fruit.
*Acceptance:* numbers in ROADMAP; replay of 10k events under 200 ms in Node.

### E8 — Persistence

#### S-80 Persistence adapter + reload restore (M, deps S-22, S-30) — **done 2026-09-29** (`js/rex-runtime/`: `src/{boot,persist,index}.ts`, `src/adapters/{memory,indexeddb}.ts`; codegen preamble; `js/rex-runtime/test/*` (12), `examples/kanban/e2e/persist.spec.ts` (3))
*Was it a real dependency of S-90? Yes* — "reload restores" is in S-90's
acceptance, and until now nothing consumed S-22's `log_since`/`base_snapshot`/
`restore`/`forRestore`: generated code always did `new RexApp(PROGRAM)`.
*Its own deps:* S-22's Rust/wasm side was already complete; **S-30 was not
done and is only partly needed.** What S-80 needs from it is a home for
`persist.ts` and `boot`, so this created a *minimal* `js/rex-runtime`. Not
done (still S-30): moving `pkg/` out of `examples/kanban/src`, the `EnginePort`
interface, removing `rex-dom`'s `Rebalancer` shim, `npm pack`, READMEs.
`boot` takes the wasm class as an argument (`{ RexApp, program, adapter }`)
instead of importing the glue, so `rex-runtime` has no wasm dependency and S-30
can move `pkg/` without touching it.
*Landed as:* `PersistenceAdapter { loadSnapshot, saveSnapshot, appendEvents,
eventsSince }`, `MemoryAdapter`, `IndexedDbAdapter` (one DB per program; an
`events` store keyed by `seq`, a one-record `snapshot` store; a duplicate
append is ignored, so a retry is harmless). `boot`:
- **nothing stored** → `new RexApp(program)`, persist its `@genesis` events;
- **anything stored** → `RexApp.forRestore` (no seed — it would double the
  restored rows), `restore(snapshot)` if there is one, `replay(tail, silent)`;
  with *no* snapshot the log alone rebuilds seed + edits, so a page killed
  before the first snapshot loses nothing;
- afterwards each `dispatch`/`rebalance` (the engine's own methods, wrapped in
  place) appends `log_since(cursor)`; a snapshot every 50 events, on
  `visibilitychange`→hidden and on `pagehide`. Appends are serialised through
  one promise chain and never throw into `dispatch`; `flush()` awaits them.
Two decisions worth knowing: the DB name is `programKey(name, source)` — a hash
of the program text — so an edited program starts clean instead of replaying
another version's log (**consequence: changing `app.rex` discards that
browser's saved state**); and `?ephemeral` swaps in `MemoryAdapter` for dev.
`replay` advances the engine's cursor without re-logging, so seqs continue
after a restore with no collisions (tested). `log_since` takes a `u64`, which
wasm-bindgen exposes as `bigint`.
*Acceptance:* the Kanban Playwright suite is unchanged and green with
persistence on (12/12, real wasm); new e2e: an edit survives a reload with the
seed not doubled, an edit that exists only in the log (no snapshot) is
replayed, and `?ephemeral` does not persist. **TodoMVC and the benchmark have
no browser app yet**, so "all three apps" waits on S-90/S-91, which now only
need the codegen'd preamble.

*Files:* `js/rex-runtime/src/persist.ts`, `src/adapters/{memory,indexeddb}.ts`, codegen boot.
*Subtasks:*
1. `PersistenceAdapter { loadSnapshot, saveSnapshot, appendEvents, eventsSince }`
   (shape borrowed from `../elysium26/packages/sync/src/adapters.ts`).
2. Boot: `restore(snapshot)` → `replay(eventsSince(cursor), silent)` →
   `snapshot()` → shaper mount. Append each dispatched event; snapshot every
   N events and on `visibilitychange`.
3. Genesis handling: on first load with no snapshot, the program's `new`
   statements run and are logged; on later loads they must not re-run
   (restore replaces them).
*Acceptance:* Playwright in all three apps: make changes, reload, state is
back, node identity irrelevant; an event appended after the last snapshot is
replayed (test kills the page before the snapshot interval).

### E9 — Apps, gate, docs

#### S-90 TodoMVC on Rex (M, deps S-52, S-53, S-61, S-62, S-70, S-80) — **done 2026-09-29** (`examples/todomvc/`, 8 Playwright specs)
*Files:* `examples/todomvc/*` (Vite + Playwright, same layout as kanban).
*Acceptance:* the S-02 program runs unchanged; Playwright: add/toggle/edit/
delete/filter/clear-completed, focus survives edit, reload restores.

*Landed as:* `app.rex` is the S-02 program with one edit: Escape is
`{ set editing = False; revert }` (see 4). Running it in a real browser found
five gaps that no unit test had reached, all fixed generally:
1. **Child levels mounted into the parent row's root**, not the element they
   were written in (`ul.todo-list` inside `section.main`). `ShapeLevel.slot`
   is the child-index path in the parent template; codegen emits
   `slot: root => …` and `Shaper.parentEl` applies it. Kanban never hit it
   because its selects sit directly under the row element.
2. **Key selectors were ignored**: `keydown.enter` fired on every key. Codegen
   now filters on `KeyboardEvent.key` (`enter`, `escape`, `tab`, `space`,
   arrows, single characters).
3. **`autofocus` became a class.** It is now the attribute plus a `focus()`
   after insertion.
4. **`checked=(active = 0)`**: a comparison is a coreflexive, not a `Bool`
   value, so the bind never turned on. Non-field binds on boolean DOM
   properties (`checked disabled hidden selected readOnly required`) are now
   gates applied by presence (`BindKind::Flag`), like class binds.
5. **Escape re-committed the edit**: hiding the input fires `blur`, whose
   handler saved the abandoned text. New handler action `revert` restores the
   input from `defaultValue`, which value binds now keep in sync.
The wasm glue is now copied into every example by `scripts/build-wasm.sh`
(CI does the same), since each Vite app imports its own `src/pkg`.
*Not covered:* S-53's sibling-order limitation (a gate flipping after a later
sibling mounted appends out of order) does not bite here: `main` and `footer`
gate on the same condition and mount in one batch.

#### S-91 js-framework-benchmark on Rex (M, deps S-41, S-42, S-70, S-80) — **done 2026-09-29, except the frame-budget number** (`examples/js-framework-benchmark/`, 11 Playwright specs; `crates/rex-core/tests/{bulk_values,bench_create}.rs`)
*Files:* `examples/js-framework-benchmark/*` following
`../elysium26/bench/js-framework-benchmark/` (same `index.html` ids so the
upstream harness works).
*Acceptance:* all seven operations; `create 10k` under 1 frame budget in the
engine (record µs); numbers for Rex and elysium in `ROADMAP.md` §3.2.

*Landed as:* all seven operations (plus select/remove) pass in a real browser
against the real wasm, including reload-restores with the same random labels
(they are logged with the event). **`create 10k` is 207 ms native, ~283 ms in
wasm, not 16 ms**; numbers for both engines are in `ROADMAP.md` §3.2, with the
caveat that they are not like-for-like. Not met, and not closable inside this
story: the profile is flat BTree/`Value` work, so it needs the post-MVP
precompiled circuit, not a hot-spot fix.

`app.rex` compiled with four gaps, all closed in the language rather than
worked around:
1. **`new … from rows as (i, v) { num: nextId + i }`**: the key/value binders
   are per-row params in a compound field value (`MutationIR::InsertFrom` now
   carries the binder names; `expand` binds them per minted row).
2. **Bulk update values are per-row**: `update Row where P { label: .label ++ "!" }`
   evaluates `.label` on the row being updated (`ROW_SELF`, as S-41's scan
   predicate already did). Before, the value was computed once for all rows.
3. **`++`** in mutation values (`ValExpr::Concat`, `Text` only).
4. **`import js` extractors**: `import js "./utils.js" as utils` is recorded in
   `ShapeProgram.imports` and emitted as `import * as utils`; a handler param
   `rows = utils.randomLabels(1000)` takes its type from the event it is passed
   to (`do Run(1000, rows)` fixes `Int -> Text`), arguments must be literals, and
   the JS result is encoded at the boundary (`encodeRel` turns an array into the
   relation `Int -> T` keyed by 0-based index), so the log holds plain
   canonical values and replay never re-randomises.
Also fixed: `Engine::push_retract`/`push_set` re-scanned the transaction's
deltas per row (O(N²) for a bulk delete: `Clear` on 10k rows 2.3 s → 0.19 s);
`Transaction` now remembers which ids it has written and only composes with
pending writes when one applies.
*Not covered:* the harness's `#main` container is `#app` here; the upstream
Bootstrap stylesheet is replaced by a small inline one (the glyphicon is a CSS
`::before`), so the visual is approximate, the ids are the harness's.

#### S-92 Kanban re-port to named events (S, deps S-22, S-71)
*Acceptance:* existing Playwright suite unchanged and green; per-list card
count bind (S-60) shown.

#### S-93 `rex` CLI: `check`, `build`, `run`, `--watch` (M, deps S-20) ∥ — **done 2026-09-30** (`crates/rex-cli/src/main.rs`, `tests/cli.rs` (17), `Diagnostic::render_file`, `crates/rex-core/tests/diagnostic.rs` (5))
*Files:* `crates/rex-cli/src/main.rs` (use `clap`).
*Subtasks:* `rex check app.rex` (diagnostics only, exit code), `rex build`
(as today, `--watch` via `notify`), `rex run` (batch eval, current
behaviour), REPL keeps `rex` with no args. Diagnostics render spans for view
bodies (verify handler/element spans are real, not `Span::default()`).
*Acceptance:* each subcommand has an integration test in `crates/rex-cli/tests/`.
*Landed as:* `clap` subcommands `check <files…> [--deny-warnings]`, `run <file>
[--ast]`, `build <file> [-o] [--import] [--debug] [--watch]`, and `repl` (also
plain `rex`); a bare `rex file.rex` is rewritten to `run`. Exit codes: 0 ok, 1
program errors (or warnings under `--deny-warnings`), 2 usage/I-O. Diagnostics
print as `file:line:col` with a gutter and underline (`render_file`; the old
`render` is unchanged for the wasm bridge). `--watch` uses `notify` on the
file's *directory* (editors save via rename), settles 80 ms, and keeps watching
through a failed build. `run` no longer prints the s-expression unless `--ast`.
*Span audit:* of 18 deliberately broken view programs, two reported at 1:1
(`Span::point(0)`): a text bind `{ .nope }` and views built on an unknown
`select` source. `emit_let`/`attr_view`/`LevelWalk::base` now take real spans;
`tests/cli.rs` pins the bind, `do` handler, order key and `if` positions.
*Known:* an unknown `select` source still reports once at the select and once
more from the row's own bind (a cascade, now at real positions); there is no
`--format json`, and `rex check` does not read stdin.

#### S-94 Docs pass (S, deps everything above)
*Subtasks:* `README.md` status + "What's left" rewritten; `SYNTAX.md` is the
v1 surface reference (from S-02, updated); SPEC gets the `Unit`, event-log,
and `+` decisions; ROADMAP marks M6 gate met and lists post-MVP (M4,
ordering hide, precompiled circuit, sync).

---

## 4. Sequencing and parallel tracks

Critical path: **S-02 → S-10 → S-20 → S-21 → S-40 → S-41/S-42 → S-80 → S-91**.

Parallel tracks that can start on day one with separate agents:

| Track | Stories | Touches |
|---|---|---|
| A. Foundations | S-01, S-03, S-04 | scripts, CI, tests, small checker fixes |
| B. Language design | S-02 then S-10 | ast/parser/pretty, SYNTAX.md |
| C. Packages | S-30, S-31 | `js/*`, codegen preamble |
| D. Ordering | S-70 | `js/rex-dom/src/order.ts`, shape_ir orderKind |
| E. Core `Unit` | S-50 | ty/env/check/interp/lower, SPEC |

After S-20 lands, tracks converge: E2/E3 (engine + wasm) and E4/E5 (checker +
desugar) can run in parallel since they touch different modules, meeting at
S-53/S-80. Merge conflicts concentrate in `types/view.rs` and
`shape_ir.rs`; assign one agent to own those files at a time.

Rough effort: ~25 stories, ~35–45 agent-days at the sizes above; the
critical path is ~15 days if tracks are truly parallel.

---

## 5. Owner decisions (resolved 2026-09-18)

1. **Component syntax: call-like with a children block.** `Name(args) { … }`
   mirrors `tag attrs { … }`; a capitalised name with parentheses is
   unambiguous. Args are positional against the declared params
   (`TodoItem(t)`). The component body may contain one `children` slot where
   the caller's block is substituted; since components expand inline, a
   nested `select` inside the block is keyed by the enclosing binder as if
   written in place. No slot ⇒ a block is an error.
2. **`do` allowed, two kinds with different logging.**
   - *Synchronous* `do E(args)` in a handler body runs the child handler in
     the **same transaction** against the same pre-event snapshot. Only the
     outer event is logged; replay re-derives children. The static `do`
     graph must be acyclic (compile-time check; elysium does this at runtime
     with an iteration limit).
   - *Async* (elysium's `do X then { on Resp… }`, spec §8): the response is
     **never** in the same transaction — the transaction commits before the
     request can start, and handlers stay pure. Rex's version is the
     nesting-draft §6–7 membrane with the closure replaced by data: the
     outer transaction inserts an `EffectIntent` row keyed by
     `(outer seq, call site)`; the DOM-layer driver performs the request; the
     response arrives as a **separately logged event**
     `HttpResponse(idem, status, body)`; `then` arms are ordinary handlers
     joining on `idem`, and the "closed-over" bindings are fields of the
     intent row. On replay the driver is muted and responses come from the
     log. `@latest` = slot-keyed intent + stale anti-join; cancellation =
     intent retraction. Elysium's target-less `then` ("next tick") is not
     needed: handlers already read a consistent snapshot. **MVP implements
     synchronous `do` only** and reserves `cause`/`intent` log fields (S-21).
3. **`Bool`** is sugar for `{@True | @False}` — i.e. the predeclared
   `type Bool = True | False`; `not P` on a filter is `except (where P)`;
   `checked` extractors and `class.x=` binds use it. *(Spelling settled in
   S-50: a constructor names its atom **verbatim**, so `All` is `@All`. This
   line originally read `{@true | @false}`, written before S-02 introduced
   constructor syntax; SYNTAX.md §9 already had the verbatim form. Verbatim
   keeps the mapping reversible and stops a constructor and a hand-written
   atom literal from silently colliding.)*
4. **Operators:** `|` union, `&` intersect, `except` difference (raw
   subtraction stays core-only per SPEC §6, so `-` has no relational
   meaning at the surface and `R - S` is a type error suggesting `except`);
   `+ - * /` arithmetic only; `++` text concat replaces `||` (too close to
   `|`); coproduct types `{@a | @b}`. Rationale beyond freeing `+`: union of
   coreflexives is disjunction and `&` is conjunction, so filters read as
   logic (`where (:region = @west | :region = @east) & :active`), and the
   type syntax matches elysium's `'a' | 'b'`. Differs from DBSP's `+`
   notation; acceptable.
5. **Runtime compile stays** (§2.7); precompiled circuit is post-MVP.
6. **JS escape hatch** is a DOM-layer extractor only (`Extractor::Js`); the
   engine stays pure and replay-deterministic because the produced values
   are event args.
