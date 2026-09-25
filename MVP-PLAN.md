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

#### S-50 `Unit` sort and `unit` constant (M, deps S-10) ∥ with E2
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

#### S-51 `state` as a singleton relation (M, deps S-50, S-40)
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

#### S-52 `match` / `if` in filters and attributes (M, deps S-51)
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

#### S-53 `Unit` root levels and scalar binds in views (M, deps S-50, S-20)
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

#### S-60 Bind any relation co-keyed with the level (M, deps S-53)
*Files:* `types/view.rs`, `check.rs`.
*Subtasks:* a bind `{ e }` where `e : Binder -> V` is any expression (join,
FK path `:author.name`, aggregate `count(Comment by :post)`); the desugarer
emits the attribute `let` and the checker verifies co-keying; `:field`
remains the common case. Diagnostics name the expected domain.
*Acceptance:* Kanban shows per-list card counts; a bind on a wrong domain is
a "not co-keyed with `l : List`" error.

#### S-61 Components with props by inline expansion (M, deps S-60)
*Files:* `ast.rs` (done), `types/view.rs`.
*Subtasks:* `view TodoItem(t: Todo) = li { … }` is a template; `TodoItem(c)`
inside a `select` expands with positional binder substitution; a
`Name(args) { … }` block is substituted at the component's single
`children` slot (error if the component has none, or has two); level names include the
call site (`app#todo#TodoItem`); recursion between components is an error.
*Acceptance:* TodoMVC's `TodoItem`/`Footer` are components; generated TS is
identical in shape to the inlined version (snapshot).

#### S-62 `local` per-instance state (S, deps S-61, S-51)
*Subtasks:* `local editing : Bool = @false` in a component desugars to a
hidden entity keyed by the component's binder (`Todo -> Bool`) with the
default applied via `except`-based defaulting (`present + (Todo except present) . @false`);
`set editing = …` in a `do` body targets the instance key.
*Acceptance:* TodoMVC edit-in-place (double-click) with focus preserved.

### E6 — Ordering (∥ with E2–E5)

#### S-70 Typed order keys, `desc`, tiebreak (M, deps S-04)
*Files:* `js/rex-dom/src/order.ts`, `types.ts` (`ShapeNode.orderKind`),
`shape_ir.rs`, codegen.
*Subtasks:* `OrderIndex` takes a comparator built from the order view's
`Encoding` (Text/Int/Money/Date) and direction; child key tiebreak stays;
`order by :pos desc`; `orderedChildren` returns decoded keys.
*Acceptance:* vitest: Int keys 3 < 10; benchmark's `SwapRows` is exactly two
`insertBefore` calls (spy driver).

#### S-71 Rebalance as an event + drag helpers cleanup (S, deps S-22, S-70)
*Subtasks:* `maybeRebalance` dispatches `@rebalance`; `interact.ts` loses
its `FieldWriter` dependency on the wasm app; `prompt(...)` extractor
removed.
*Acceptance:* no direct writes remain in `js/rex-dom` (grep `update_field`
returns nothing).

#### S-72 (post-MVP) Hidden manual order: `order manual`, `move x before y`
Design only for MVP: a one-page note under `docs/ordering.md` consistent with
the memory note "fractional keys are an impl detail".

### E7 — Package extraction

#### S-30 Split `rex-dom` / `rex-runtime`; move `pkg/` (M, deps S-01) ∥
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

#### S-31 Contract doc + README for both packages (S, deps S-30, S-03)
*Subtasks:* `js/rex-dom/README.md`: the delta protocol (`Tuple`, `StepDeltas`,
roles), the four phases, the −/+ guarantee, `ShapeNode` reference, how to
write a `DomDriver`; `js/rex-runtime/README.md`: boot, events, log,
persistence adapters. Link the S-03 fixtures as the conformance suite.
*Acceptance:* a reviewer can write a `ShapeNode` by hand from the README and
drive the shaper with a fixture (add exactly that as an example test).

#### S-32 Boundary batch entry + V8 profile (M, deps S-22) — *perf, can slip*
*Subtasks:* `replay` crosses once for N events (done in S-22); measure with
`crates/rex-core/benches/dbsp_vs_batch.rs`'s dataset in Node and Bun;
profile allocation hot spots (`Value` clones, `BTreeMap`) and record findings
in ROADMAP §3.2. No optimisation beyond low-hanging fruit.
*Acceptance:* numbers in ROADMAP; replay of 10k events under 200 ms in Node.

### E8 — Persistence

#### S-80 Persistence adapter + reload restore (M, deps S-22, S-30)
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

#### S-90 TodoMVC on Rex (M, deps S-52, S-53, S-61, S-62, S-70, S-80)
*Files:* `examples/todomvc/*` (Vite + Playwright, same layout as kanban).
*Acceptance:* the S-02 program runs unchanged; Playwright: add/toggle/edit/
delete/filter/clear-completed, focus survives edit, reload restores.

#### S-91 js-framework-benchmark on Rex (M, deps S-41, S-42, S-70, S-80)
*Files:* `examples/js-framework-benchmark/*` following
`../elysium26/bench/js-framework-benchmark/` (same `index.html` ids so the
upstream harness works).
*Acceptance:* all seven operations; `create 10k` under 1 frame budget in the
engine (record µs); numbers for Rex and elysium in `ROADMAP.md` §3.2.

#### S-92 Kanban re-port to named events (S, deps S-22, S-71)
*Acceptance:* existing Playwright suite unchanged and green; per-list card
count bind (S-60) shown.

#### S-93 `rex` CLI: `check`, `build`, `run`, `--watch` (M, deps S-20) ∥
*Files:* `crates/rex-cli/src/main.rs` (use `clap`).
*Subtasks:* `rex check app.rex` (diagnostics only, exit code), `rex build`
(as today, `--watch` via `notify`), `rex run` (batch eval, current
behaviour), REPL keeps `rex` with no args. Diagnostics render spans for view
bodies (verify handler/element spans are real, not `Span::default()`).
*Acceptance:* each subcommand has an integration test in `crates/rex-cli/tests/`.

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
3. **`Bool`** is sugar for `{@true | @false}`; `not P` on a filter is
   `except (where P)`; `checked` extractors and `class.x=` binds use it.
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
