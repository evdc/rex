# Rex

A point-free, binary-relational **view language**. Every program denotes a binary
relation `A → B`; relations compose like arrows in a category; entities are
families of keyed columnar relations. The long-term goal (per [`SPEC.md`](SPEC.md))
is to lower programs to **DBSP circuits** so that incremental view maintenance
(inserts, deletes, corrections over Z-sets) is automatic.

This repository holds the compiler, a batch reference interpreter, an
incremental DBSP engine (compiled to WASM), a UI compiler (`rex build`), and a
TypeScript DOM shaper. The design is captured in `SPEC.md` (working notes, not
a spec — read it first), `nesting-draft.md` (nesting/shaper/effects), and
`SYNTAX.md` (the `view` surface). `ROADMAP.md` is the strategy and plan;
`drafts.md` holds looser ideas and project context.

> **Status (Sept 2026):** the core parses, type-checks, elaborates, and
> evaluates end-to-end, both through the batch reference interpreter and
> through a **DBSP circuit backend** with incremental maintenance
> (`crates/rex-core/src/dbsp/`), including recursion (§8). The full UI pipeline
> works for one app: `examples/kanban/src/board.rex` compiles (`rex build`) to
> a running Kanban board — WASM engine → delta JSON → TS shaper → surgical DOM
> mutations, with Playwright tests for node identity and focus. The **view
> language is narrow** (Kanban-shaped); the next phase (ROADMAP M6) grows the
> surface toward elysium26's, with a named-event log as the entry point.

---

## Building & running

Workspace layout:

| Path | What |
|---|---|
| `crates/rex-core` | Library (`rex`): lexer, parser, checker/elaborator, `view` desugar, batch interpreter, DBSP engine, value encoding. |
| `crates/rex-cli` | The `rex` binary: REPL, run-a-file, and `rex build` (UI codegen). |
| `crates/rex-codegen` | Shape IR → generated TypeScript (`main.ts`: shape tree, templates, event wiring). |
| `crates/rex-wasm` | `RexApp`, the wasm-bindgen API (`dispatch`, `snapshot`, `apply_new`, `update_fields`, `retract`, `read_view`). |
| `js/rex-dom` | TS shaper + bridge: −/+ fusion, phased apply, fractional ordering, drag/drop helpers. |
| `examples/kanban` | The one end-to-end app (Vite + Playwright). |

```sh
cargo build
cargo test --workspace         # ~180 tests incl. property tests vs. the batch oracle
cargo run -p rex-cli                         # start the REPL
cargo run -p rex-cli -- path/to/prog.rex     # parse, type-check, and batch-evaluate a file
cargo run -p rex-cli -- build app.rex -o app.ts   # compile `view`s to a TS module
(cd js/rex-dom && npx vitest run)            # shaper tests
```

Running a file prints the canonical s-expression AST, a type-check result, and
each view's materialized contents. `cargo run -p rex-cli --
crates/rex-core/tests/fixtures/spec12.rex` is the best core demo; the Kanban
app is the best whole-system demo — run `scripts/build-wasm.sh` to build
`rex-wasm` and generate the wasm-bindgen glue into `examples/kanban/src/pkg/`
(gitignored, not committed), then `cd examples/kanban && npm run build` and
`npx playwright test`.

The REPL (`crates/rex-cli/src/repl.rs`) accepts `entity`/`let` statements
(committed to the session) and bare expressions (evaluated in a scratch copy
and discarded). `view`/`state`/`rel` are rejected there — use `rex build`.
Commands: `/import <path>`, `/env`, `/reset`, `/help`, `/quit`.

---

## Pipeline & architecture

Source flows through five phases, each in its own module:

```
src → lex → parse → check/elaborate → eval
      │      │            │              │
   Vec<Token>  Program   TProgram   HashMap<name, BTreeRelation>
              (untyped)  (typed)
```

| Phase | Module(s) | What it does |
|---|---|---|
| **Lex** | `crates/rex-core/src/lexer.rs`, `crates/rex-core/src/token.rs` | ASCII-only, hand-written. Maximal-munch operators; distinguishes decimal `.` from compose `.`; dates, atoms `@foo`, strings with escapes. Recovers and collects diagnostics. |
| **Parse** | `crates/rex-core/src/parser.rs`, `crates/rex-core/src/ast.rs` | Recursive descent for statements, Pratt (binding-power) for expressions. Produces a surface AST that mirrors what was written — **no desugaring here**. Recovers to the next `let`/`entity` on error. |
| **Check / elaborate** | `crates/rex-core/src/types/` | The heart. A single bidirectional, type-directed walk validates the program *and* produces the elaborated `TProgram`: field paths resolved to `(sort, field)` hops, identifiers resolved to their kind (view / entity-identity / value), filter built-ins lowered to `Filter` nodes. Threads an ambient domain through composition to resolve `:field`. |
| **Eval** | `crates/rex-core/src/eval/` | Batch interpreter over the typed AST. `interp.rs` walks `TProgram`; `algebra.rs` is the relational algebra; `relation.rs` is the Z-set store; `value.rs` is the runtime domain element. |
| **Support** | `crates/rex-core/src/diagnostic.rs`, `crates/rex-core/src/span.rs`, `crates/rex-core/src/pretty.rs`, `crates/rex-core/src/operator.rs` | Spans + rendered diagnostics; canonical s-expr printer (used for test assertions and `--ast`-style output); the §7 operator-metadata table. |
| **Incremental engine** | `crates/rex-core/src/dbsp/` | `lower.rs` lowers `TProgram` to a circuit of delta nodes (`node.rs`); `circuit.rs` steps it (incl. fix regions); `engine.rs` is the transactional API (`apply_new`, `update_fields`, `retract_entity`, `dispatch`). |
| **View desugar** | `crates/rex-core/src/types/view.rs`, `shape_ir.rs` | `view … select` → ordinary `let`s (membership / order / per-attribute) + a shape IR + checked handler bodies. |
| **Codegen / edge** | `crates/rex-codegen`, `crates/rex-wasm`, `crates/rex-core/src/eval/encode.rs`, `js/rex-dom` | Shape IR → TS; the WASM API; the canonical value/delta wire encoding; the shaper that turns per-view deltas into DOM mutations. |

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
  substrate incremental maintenance will eventually need.
- **Money is exact**, stored as integer cents.

---

## What exists

- ✅ Full lexer, parser, s-expression pretty-printer.
- ✅ Entity declarations → minted ID-sorts + keyed field columns.
- ✅ `new` creation sugar (atomic per-key population; anonymous `let _`).
- ✅ Bidirectional checker with type-directed `:field` resolution, ambient-domain
  threading, co-keyed/join-column diagnostics, atom/coproduct subtyping.
- ✅ Combinators: compose, semijoin/`where`, inverse, fork, union, intersect,
  distinct, `by`, `except`/`antijoin`.
- ✅ Functional ops `*` and `||`; comparisons (prefix-filter and binary
  column-vs-column); `in`.
- ✅ Aggregations `sum`/`count`/`avg`/`min`/`max` as monoid homomorphisms over
  a key's image Z-set, respecting weights.
- ✅ Batch interpreter; REPL with session state and `/import`.
- ✅ The §12 worked example type-checks and evaluates correctly.
- ✅ **DBSP backend** (`crates/rex-core/src/dbsp/`): lowering of the typed AST to a circuit of
  delta kernels, incremental view maintenance (inserts/retractions), backfill
  of views added over existing data, property-tested against the batch algebra.
- ✅ **Recursion (§8)**: `let recursive path : Node -> Node = edge + edge . path`.
  **Consecutive `let recursive` statements form one fixpoint group** (mutual
  recursion; any other statement ends the group); annotations are mandatory
  (they seed the self-reference's type). Semantics is the joint least fixpoint
  — Kleene iteration from ∅ with a **forced `distinct` at the knot**. The
  batch interpreter runs the Kleene loop directly; the circuit backend wraps a
  nested inner circuit in an Enter/Exit **fix region** (`Circuit::fixes`):
  imports enter as δ₀, the feedback slot is the z⁻¹ edge, inner integrals make
  iteration semi-naive, and Exit diffs the converged fixpoint against the
  previous step's. Cost model: recursion is **non-linear** — each outer step
  re-derives the fixpoint (O(closure), not O(Δ)); fully incremental nested
  deltas are future work.
- ✅ **Stratification check** (`crates/rex-core/src/types/strat.rs`): consumes the `monotone`
  bits of `operator.rs` — no non-monotone operator (`distinct`,
  `except`/`antijoin`, aggregation) may sit above a recursive occurrence.
  (`&` is monotone — min of weights — and is allowed under recursion.)
- ✅ **Nesting (M2)**: `fst`/`snd` projections; composite-key nesting composes
  from `~`/`,`/`.`/`by`; the §8 opaque-value cliff is a checker warning.
- ✅ **UI pipeline (M3/M5)**: `view … select` surface (`SYNTAX.md`), `rel`
  declarations, `Entity as l` binders; `rex build` codegen; atomic handler
  dispatch (one engine step per DOM event); WASM `RexApp`; TS shaper with −/+
  fusion, phased apply, fractional ordering + rebalance, subtree coalescing,
  reparent. The Kanban app runs on it with Playwright gates.

---

## What's left to do

The plan and its rationale live in **`ROADMAP.md` §4 (M6 onward)**. In short:
grow the user-facing surface toward elysium26's (it is the language donor)
while keeping the narrow binary core as the IR, with a **named-event log** as
the entry point for all change. Priority order:

1. **Events + log (M6.a).** Named `event`s and `on E(…)` handlers; an
   append-only, replayable event log. Today handlers are anonymous inline DOM
   handlers that write straight to the engine — nothing is logged or replayable.
2. **Handler expressiveness (M6.b).** `where`-targeted bulk
   update/insert/delete; mutation values as expressions over the pre-event
   snapshot (today: literals and params only — `t:n := t:n * 2` is rejected).
3. **`state` + `match` (M6.c).** `state` parses but is rejected
   (`types/view.rs:58`). Model state as singleton relations so state changes
   are ordinary deltas (elysium's full-recompute-on-state cliff can't arise).
4. **Views over derived relations (M6.d).** Binds are `:field`-only; aggregates
   and joins can't be displayed. Also components with props, `if`/class
   expressions.
5. **Query sugar (M6.e).** `select {…}` records, `group by`, FK paths as sugar
   desugaring to binary relations.
6. **Persistence** via the event log (snapshot + replay), then **effects (M4)**.
7. **Engine/boundary performance** — see *Performance* below and ROADMAP §3.2.

Core-language items, still open:

- **Fully incremental recursion.** The fix region re-derives its fixpoint each
  outer step (semi-naive within the step, O(closure) across steps); the DBSP
  nested-delta construction would make an edge insert cost only the newly
  derivable paths.
- **Analysis consumers.** Set-ness (drop redundant `distinct`), linearity cost
  model surfaced to users, delta fan-out warning (§10) — unimplemented; of the
  metadata table only the `monotone` bits (stratification, §8) and `grounding`
  have readers.
- **Coproduct runtime forms.** Coproducts exist in the *type* system but have
  no runtime injection/case forms — no way to construct or match `(V + Unit)`,
  so the "no NULL, use `V + Unit`" story isn't realizable at runtime yet.
- **Top-N / engine-side ORDER BY** (§11). Ordering is currently the shaper's
  job (`order by` designates an order relation; the engine doesn't sort).
- **Hide fractional order keys.** `pos: Text` and the `endOf`/`dropPos`
  extractors are visible in user code; manual order should become a language
  concept.
- **`from E:` blocks** (§4 secondary sugar) — not parsed.
- **The `+` naming collision** (§11) is unresolved in surface syntax (`+` is
  union; arithmetic add isn't spelled at all — only `*` and `||` exist).

---

## Bugs & correctness issues

- ~~**Negative money parses and prints wrong.**~~ **Fixed.** The sign is now taken
  from the literal itself (`"-0.50" → Money(-50)`) and `Display` prints it
  (`-$0.50`). Covered by tests in `crates/rex-core/src/eval/value.rs`.
- ~~**Coreflexive-producing nodes ignore input weights.**~~ **Fixed.**
  `BinCompare` now combines weights bilinearly (`wa * wb`), and `InRel` /
  standalone `Coreflexive` carry the source row's weight, so they produce faithful
  Z-set coreflexives instead of clamped weight-1 rows.
- ~~**Standalone `Coreflexive` silently yields empty off an entity domain.**~~
  **Closed** by the groundedness pass (`crates/rex-core/src/types/ground.rs`): an infinite
  built-in with no enumerable domain is now a check-time error, so the
  interpreter's `ValueTy::Id`-only materialization is unreachable otherwise.
- **Aggregation of an empty group produces no row.** `sum`/`count` over a key with
  no image emit nothing rather than `0` — there's no outer key to range over, so
  "count of customers with zero orders" is unrepresentable. Standard group-by
  behavior, but worth deciding deliberately given the incremental target.
- **`in` is atoms-only.** `expect_subset` (`crates/rex-core/src/types/check.rs:902`) uses
  `.atoms()`, so `x in (1 + 2 + 3)` over integers is rejected even though
  `collect_lits` happily gathers the int literals. Either generalize or reject
  earlier with a clearer message.
- **`min`/`max` under retraction** take the recompute tier in the engine: the
  `Aggregate` node re-folds the affected group from its integral
  (`dbsp/node.rs`). Correct, but O(group) rather than O(Δ).
- **Non-Text binds render the wire encoding.** Codegen applies every bind via
  `decodeText`, so an `Int` field shows as `i:3` (likewise Money/Date/atoms).
  Decode by the bound value's type.
- **A bare identifier in an element body silently becomes a tag.**
  `span { cnt }` compiles to a `<cnt>` element instead of an error (or a bind).
- **`where :f = @atom` on a view level is rejected** ("not co-keyed"), though
  `SYNTAX.md` says extra `where`s are ordinary restrictions; only
  `where :f in @atom` works. Either accept `=` or fix the docs.

## Performance

Measured Sept 2026 (engine only, no DOM; same Kanban-shaped program and a log
of single events — 10k card inserts, then 1k renames / moves / deletes; µs per
event at N = 10k; Rex figures include delta JSON serialization):

| Engine | insert | rename | move | delete |
|---|---|---|---|---|
| Rex native (Rust) | 7.9 | 4.0 | 12.0 | 9.4 |
| Rex WASM `-O3`, Bun (JSC) | 18 | 7 | 17 | 12 |
| Rex WASM `-O3`, Node (V8) | 62 | 24 | 35 | 32 |
| Rex WASM as shipped (`opt-level="z"`), Node | 87 | 40 | 60 | 42 |
| elysium26 (TS), Node | 58 | 278 | 301 | 257 |

- **The JS↔WASM boundary is not the bottleneck**: a bare call is ~0.4 µs;
  `JSON.parse` of a delta adds ~1–8 µs. The gap is V8 executing this WASM
  (3–8× native; JSC is within ~2×) — likely allocation/`BTreeMap`/`Value`
  cloning; unprofiled.
- **`wasm-release` uses `opt-level = "z"`**, costing ~1.4–1.7× vs `-O3` for
  ~+27 KB gzipped (155 → 182 KB). Switch the shipped profile.
- **No batch entry point across the boundary.** Each `apply_new` /
  `update_fields` is its own step; bulk loads and log replay should cross once
  (`Engine::dispatch` already takes a batch).
- (elysium's O(N) updates come from a predicate scan for `where .id = x`, not
  from TS itself.)
- **The REPL re-parses and re-checks the whole session per line** but applies
  only the new statements to the engine (`Session::commit` in
  `crates/rex-cli/src/repl.rs`); a recursion-group extension rebuilds and
  replays. Fine at prototype scale.
- **The interpreter clones pervasively.** `eval` returns owned `BTreeRelation`s,
  `View`/`Identity`/field lookups `.clone()` whole relations, `Const` clones the
  entire id relation to iterate its domain. `BinaryRelation::iter`/`row` return
  boxed iterators that clone every `Value`. All acceptable for a reference
  interpreter; none of it survives into a columnar/vectorized backend.
- **Field lookups allocate a `String` key per access** (`(sort, name.to_string())`
  in `Interp::field`).

## Code style / cleanup

- ~~Duplicate interpreter entry points `run_typed`/`run_typed_values`.~~
  **Done** — collapsed to the single `run_typed_values`.
- ~~`operator.rs` is dead metadata that can silently drift.~~ **Mostly addressed** —
  a test guards that every combinator has an entry and that names are unique, and
  the `monotone` bits now have a real consumer (`crates/rex-core/src/types/strat.rs`, the §8
  stratification check). The linearity bits still have no reader.
- ~~`Value::is_money` is unused.~~ **Removed.**
- ~~The `spec12.rex` fixture's `result` line deviates from SPEC §12.~~ **Done** — a
  correction note is now in SPEC §12 pointing at the fixture, so the two no longer
  drift silently. The fixture uses `inregion . (custspend where > 30)` because the
  spec's `(custspend where > 30)[inregion]` is (correctly) ill-typed under §3.2.

## Test coverage

Strong where it exists: lexer (18 cases), parser (precedence/associativity of
every operator), checker (positive cases + the important negative diagnostics:
unknown field/type, wrong `new` value type, atom-not-in-coproduct, ID-sort
discipline, the `[]`-on-value-column type error, non-co-keyed comparisons),
elaboration (field-hop resolution, `where`→`Filter` folding, semijoin preserved,
`by`/agg), and algebra primitives + the §12 end-to-end eval.

Gaps:

- **Interpreter coverage is thin beyond §12.** No eval tests drive `antijoin`/
  `except`, `distinct`, `intersect`, `inverse`, `in`-filter, `BinCompare`
  (column-vs-column), fork end-to-end, or `count`/`avg`/`min`/`max` — only `sum`
  is exercised at the interp level.
- **Retraction is covered through the engine, not the interpreter.**
  `tests/dbsp.rs`, `incremental.rs`, `nesting.rs` drive insert/retract
  histories and oracle against batch; the interpreter itself still has no
  program-level negative-weight test.
- **UI layer:** vitest (shaper, spy-driver mutation counts) and Playwright
  (`examples/kanban/e2e`) cover the one app; codegen has no unit tests beyond
  the Kanban output staying in sync.
- ~~No tests for the money bugs (negative parse/display).~~ **Added** in
  `crates/rex-core/src/eval/value.rs`.
- ~~No test asserting the operator table stays in sync.~~ **Added** in
  `crates/rex-core/src/operator.rs`.
- ~~No REPL tests.~~ **Partly added** in `crates/rex-cli/src/repl.rs` (session commit, scratch
  eval, retraction, failed-line rollback, recursive-group extension); `/import`
  is still untested.
- **Recursion is covered end-to-end**: checker (self-reference, groups,
  mandatory annotations, stratification rejections), elaboration
  (`LetRec`/`RecVar`), batch eval (chain/cycle/mutual/empty), the incremental
  engine (backfill, cycle-closing insert, retraction), and a property test
  oracling the fix region against a batch Kleene closure under random
  insert/retract histories (`tests/dbsp.rs`).
