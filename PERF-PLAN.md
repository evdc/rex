# Rex engine and data-model performance plan

Status: proposal, 2026-09-30. Written after S-91 put Rex through the official
js-framework-benchmark harness (`ROADMAP.md` §3.2). Guiding rule: **go faster by
doing less.** Prefer changes that delete nodes, state and generality from the hot
path over changes that make the same work cheaper.

---

## 1. Where the time goes (measured)

Official harness, Chrome, medians, ms. Script time is the part Rex controls:

| Benchmark | Rex total / script | vanillajs total / script | elysium total / script |
|---|---|---|---|
| create 1,000 | 112.5 / 30.5 | 85.1 / 6.4 | 107.6 / 19.1 |
| create 10,000 | 803.9 / **422.1** | 353.4 / 26.3 | 508.5 / 152.8 |
| clear 1,000 (4× throttle) | 103.4 / **100.4** | 19.3 / 15.4 | 34.5 / 30.8 |
| update every 10th | 28.4 / 6.4 | 20.2 / 1.1 | 36.7 / 6.0 |
| first paint | 1,580 | 58 | 142 |

Paint is roughly equal across all three. **Rex's gap is script**, and for create 10k
it splits roughly as: ~283 ms in the wasm engine (measured with `performance.now()`
around one `dispatch`), with the remaining ~140 ms spread over JSON encode/parse, the
shaper's mirrors and DOM construction. That remaining split is **not measured yet**
(step 0 below).

Native engine, create 10k (instrumented `Circuit::run`, since removed):

- Building the transaction: ~4 ms. The step: ~210 ms, of which compute is ~110 ms,
  commit (folding deltas into integrals) ~57 ms, and seeding, result cloning and
  dropping about 35 ms.
- The circuit has 31 nodes: 8 inputs and 22 computed. **Every computed node is fed by
  the whole 10k-row batch**, each costing about 10 ms (~450 ns per input row).
- Of the ~110 ms of compute, about 43 ms is rendering views (membership, order,
  `num`, `label`), 12 ms the `danger` class gate, and **54 ms four hidden keyset
  views** (`on#Update`, two `on#SwapRows`, `on#Select`). Those four select 0–1,100
  rows but read all 10k.
- Relation microbenchmark, 11k rows, warm. `BTreeRelation::add` costs 150–300 ns per
  row. A flat `BTreeMap<(V,V), w>` costs about 130 ns, and a `HashMap<V, (V, w)>`
  60–70 ns. A probe costs about 140 ns, against about 30 ns for a hash map.

## 2. Core structures today

| Structure | Definition | Where |
|---|---|---|
| `Value` | 24-byte enum: `Unit`, `Int(i64)`, `Money(i64)`, `Text(Sym)`, `Date{…}`, `Id(SortId, u64)`, `Atom(Sym)`, `Pair(Box<Value>, Box<Value>)`. Derived `Ord` (texts by intern order). Not `Copy`. | `eval/value.rs` |
| `Sym` | `u32` into a global interner behind a **`Mutex`**; `as_str()` takes the lock. | `eval/intern.rs` |
| `SortId` | `usize`. Entity ids are `Value::Id(sort, seq)`, and `seq` is **per-sort, dense, sequential from 0, never reused** (`Engine::next_id`). Ids are persisted in the log as `#sort:seq`. | `types/ty.rs`, `dbsp/engine.rs` |
| `InputKey` | `Identity(SortId)` (the diagonal `E : E -> E`) or `Field(SortId, Sym)` (one attribute, `E -> T`). One base relation per attribute (6NF). | `dbsp/node.rs` |
| `BTreeRelation` | `BTreeMap<Value, BTreeMap<Value, i64>>`, left → right → weight. The *only* relation type: base integrals, node integrals, node deltas, `linv` indexes, and step results. | `eval/relation.rs` |
| `Transaction` | `Vec<(InputKey, Value, Value, i64)>` plus a touched-id set (S-91). | `dbsp/circuit.rs` |
| `Node` | `Input`, `ConstSingleton`, `Inverse`, `Union`, `Filter`, `InRel`, `MapConst`, `Compose{linv}`, `CoKeyed{f}`, `Semijoin{linv}`, `Antijoin{linv}`, `Intersect`, `Proj`, `Distinct`, `Aggregate`, fix-region nodes. | `dbsp/node.rs` |
| `Circuit` | Node arena in topological order. Per step: `deltas: Vec<BTreeRelation>` (one per node, allocated fresh), then **every node's delta is committed into its own integral**. | `dbsp/circuit.rs` |
| `StepResult` | `HashMap<view name, BTreeRelation>` (cloned), serialized to JSON with every value as a canonical string (`#3:7`, `t:…`). | `eval/encode.rs` |
| Shaper mirror | `Map<view, Map<key, Map<value, weight>>>` of strings, for every view the shape tree names. `resolveOne` returns the first positive value. | `js/rex-dom/src/shaper.ts` |

### Does Rex have a keyed Z-set?

**It has an indexed Z-set, not a keyed (functional) one.** `BTreeRelation` is DBSP's
`IndexedZSet`: a Z-set of pairs, grouped by left key. Nothing records that a relation
has **at most one value per key**:

- The **type system** doesn't say it. `Row -> Text` is a binary relation type.
- The **engine** enforces it for base fields only operationally: `set` pushes −old/+new.
- The **shaper** relies on it in two places. Fusing a −/+ pair at one key into a single
  update assumes the key identifies one DOM node, and `resolveOne` assumes one live
  value. It still stores a general multimap, pays for that generality, and never
  checks the assumption.

So keys are used correctly for identity: one DOM node per key, and −/+ fusion by key.
Neither side of the boundary *exploits* functionality in how it represents the data.

## 3. The plan

Ordered by payoff per unit of risk. Estimates are estimates. Each phase should land
with a measurement (`crates/rex-core/tests/bench_create.rs` natively, plus the harness
run) and keep the batch-oracle property tests (`tests/dbsp.rs`) green.

### Step 0: measure the browser split (small)

Add an opt-in timing hook (a `?profile` flag in the generated `dispatch`) that records
engine time, `JSON.parse` time and `shaper.applyStep` time separately. Use it to size
phases 5–6 before starting them. The only measurement so far is the engine alone
(~283 ms).

### Phase 1: lower less circuit (algebraic rewrites in `dbsp/lower.rs`)

**The biggest win for the least code, and it is purely deletion.** The benchmark's 22
computed nodes are mostly identity plumbing and constant columns:

| Today | Rewrite | Why it's sound |
|---|---|---|
| `Compose(Identity(E), Field(E,f))`: the `num`, `label` and order views, 4 nodes | `Field(E,f)`; the view **aliases the input node** | Field rows exist only for live ids. `push_new` writes identity and fields together, `push_set` refuses dead ids, and `push_retract` removes both. Make that an asserted engine invariant. |
| `Compose(Identity(E), MapConst(Identity(E), c))`: membership under `unit`, 2 nodes | `MapConst(Identity(E), c)` | Same argument. |
| `CoKeyed(Field(E,f), MapConst(Identity(E), c), =)`: `.selected`, `.pos = 2`, 2 nodes each | `InRel(Field(E,f), {c})`, a coreflexive in one node, which already exists | Comparison with a constant is pointwise on the field value. |
| `.num % 10 = 1`: MapConst, CoKeyed, MapConst, CoKeyed (4 nodes) | One **`FilterMap(Field, compiled scalar expr)`** node | Any expression over *one* row's fields and constants is pointwise. Compile it once to a closure (or to the S-40 `ValExpr` evaluator). |
| `Semijoin(Identity(E), X)` where X is coreflexive on E | `X` | Restricting to live ids is a no-op for the same reason as the first row. |

Expected shape: 8 inputs plus about 6 computed nodes (one `MapConst` membership, one
`InRel` gate, and four filters for the hidden keyset views), with 4 views aliasing
inputs outright. By node count, create-10k compute should fall about 3–4×. Commit
falls with it, because aliased views have no integral of their own.

This also resolves the "hidden keyset views are expensive" finding **without
changing their semantics**. After fusion each one is a single filter over one field:
about 10k closure calls per bulk create, which is noise, and `SwapRows` stays O(1).

**Landed as P-1 (2026-09-30).** Native `bench_create`, before → after: Run(1k)
11.3 → 7.4 ms, Run(10k) 205 → 59 ms, replace-10k 374 → 117 ms, Clear 188 → 60 ms,
Add(1k) 16.2 → 4.4 ms. Delta row counts are unchanged. The benchmark lowers to 8 inputs + 7
computed nodes (`ConstSingleton`, the membership `MapConst`, and five `FilterMap`s:
the `danger` gate and the four keyset views). `num`, `label` and `order` alias their
inputs. Notes on what was built:
- Lowering returns a pending `Low` (`Node` / `Const` / per-row `Map`) so a parent can
  fuse a child without leaving the unfused node computing in the arena.
- The rewrites check node properties, not syntax: `anchor` (which sort's live ids bound
  the left keys) and `functional` (≤1 weight-1 row per key). Fix-region bodies read
  `FixInput` imports, so they are never rewritten.
- `Identity(E)[X] → X` requires X to be a weight-1 coreflexive, so it also depends on
  base fields being functional. The engine asserts both halves in debug builds
  (`Engine::debug_assert_base_invariant`); `tests/lower_rewrites.rs` checks them from
  outside and checks every view and every step delta against batch evaluation.
- `Circuit::backfill` now takes the names of the views being added, since an aliased
  view's node can sit below the backfill floor.
- The duplicate `.selected = True` filter (`gate1` and `on#Select#4`) is not deduplicated;
  CSE is left for later.

Trade-offs:
- Rewrites must be proven per rule. The first and last rows depend on the no-orphan
  invariant, so it becomes a debug assertion in `Engine::build_tx`, plus a property test.
- A view aliasing an input means `Circuit::view(name)` returns the input's integral.
  Output bookkeeping then maps a name to any node, not only the node that computed it.
- The rewrites belong in lowering, not in the checker, so the typed program stays
  what the user wrote and diagnostics are unaffected.

### Phase 2: keep only the state someone reads (`dbsp/circuit.rs`)

Today **every** node commits its delta into an integral. An integral is needed only if:
- a bilinear or semijoin parent reads it (`Compose` R-side, `CoKeyed` both sides,
  `Semijoin`/`Antijoin` R-side, `Intersect`, `Distinct`, `Aggregate` input);
- it is an output view that is read back (`snapshot()` at boot, `Target::View` keyset
  reads at dispatch, `maybeRebalance`); or
- it is a base input (always).

Mark `needs_integral` per node at lowering, by demand analysis, and skip the commit
for the rest. Linear chains (`Filter`, `MapConst`, `InRel`, `Proj`, `Union`) feeding
only other linear nodes keep no state at all. Commit was about a third of step time;
after phase 1 most of what remains is inputs, which must commit anyway, so this
matters most for larger programs.

**Landed as P-2 (2026-09-30)**, with every output forced to keep its integral.
`Node::integral_reads` declares which children each kernel reads whole. `Circuit`
keeps a `keep` flag per node, set by that demand, by `set_output`, by being an input,
and by being a fix-region member. `commit` skips nodes without the flag. In debug
builds `Ctx::integral` asserts the flag, so an undeclared read fails loudly. Only
a node that has never stepped can start keeping an integral. Lowering guarantees
that, because a new node can name only new nodes, inputs and views.
Measured:
- js-framework-benchmark: no change (Run(10k) about 60 ms). After P-1 every
  computed node there is a view.
- TodoMVC: 48 of 114 nodes now keep no state. The new
  `bench_create::todomvc_engine_cost`, keep-all vs P-2 (noisy): AddTodo ×2000
  about 65 → 58 ms, ToggleAll(False) on 2k about 47 → 39 ms, ClearCompleted on 2k
  about 63 → 51 ms.
- Profiling Run(10k) after P-2 (temporary probes, native release, 59.8 ms total):

  | Stage | ms | What it is |
  |---|---|---|
  | expand | 5.6 | handler → ops: per row, 4 `String` field-name clones, a `HashMap` insert of the row binders, a re-read of `nextId`'s state row, `intern(field)` under the global mutex |
  | build_tx | 3.8 | ops → `Transaction` |
  | seed | 12.3 | `Transaction` → one `BTreeRelation` per input |
  | compute | 8.5 | the nodes |
  | commit | 18.0 | mostly the five 10k-row inputs |
  | collect | 4.1 | clone of every view's delta into `StepResult` |
  | drop deltas | 6.4 | freeing the delta BTrees |

  The same 50k base rows are built, cloned and freed about three times, each at the
  price of an inner `BTreeMap` allocation per row (`BTreeRelation` is a map of maps,
  so a one-entry row still allocates a leaf). Seed, collect and drop (~23 ms) are
  P-3's target; commit is P-4's. Two cheap fixes outside the plan: resolve field
  names to `Sym`s and row-invariant values once per mutation rather than per row
  (~5 ms of expand + build_tx), and move rather than clone deltas into
  `StepResult`. Run(10k) replacing 10k live rows is 117 ms: the retracts add
  17 ms of `build_tx` and double seed and commit.
- TodoMVC still lowers to 114 nodes. P-1's rewrites fire little there; see P-2b.

### Phase 2b: lower TodoMVC-shaped programs (`dbsp/lower.rs`, `dbsp/circuit.rs`)

A dump of TodoMVC's circuit after P-2 shows six kinds of waste. None of them is
TodoMVC-specific: each comes from a desugaring every program uses (`match`,
`by`, `where`, `if`, repeated `.field` reads across views).

1. **No sharing.** Every occurrence of a subterm is lowered afresh: `.completed = True`
   six times, the `filter` read through unit (3 nodes) six times, `MapConst(Todo, Unit)`
   seven times, the unit singleton sixteen times, `Todo where not .completed`, `total > 0`
   and `visible.text` twice each. Hash-consing in `Circuit::add_node` (reuse a
   structurally equal node) removes about 48 of 114 nodes and roughly halves the
   2000-row nodes ToggleAll and ClearCompleted drive. Constraint from P-2: a node that
   has stepped without keeping its integral cannot start keeping one, so a stepped
   node is shared only when it already keeps one or the new use needs none.
2. **`X . Identity(E)` survives.** P-1 removes the left identity only. The right one
   is `X` whenever X's values are live `E` ids. It comes from every `match` branch
   that returns the entity, from `count(E by k)` (`Inverse(k) . E`), and from
   `order by id`.
3. **`Identity(E)[Antijoin(Identity(E), Y)]` survives.** `coreflexive_on` recognises
   `FilterMap`/`InRel` only; `Semijoin`/`Antijoin` whose left side is itself a weight-1
   coreflexive on `E` are coreflexives too. Every `where not …` hits this.
4. **Unit-total counts keep their input.** `Aggregate` reads its input integral only
   to decide key presence, which for `Total::Unit` is always true. Dropping that read
   frees a 2000-row integral per `count(… by unit)`.
5. **Scalar tests run after the broadcast.** `match filter` compares `filter` with each
   constant after composing it onto every todo, so each branch keeps a 2000-row
   `FilterMap`. Value maps commute with composition (`Map(A . B, f) = A . Map(B, f)`
   when `f` ignores the key), which moves the test onto the one `filter` row.
6. **Unit-level constants don't fuse.** In `total > 0` the `0` is a
   `MapConst(ConstSingleton)` node, not `Low::Const`, so P-1's fusion misses it.
   Anchoring `Total::Unit` aggregates and the unit singleton on Unit fixes it. These
   are one-row nodes: a node-count win, not a time win.

Beyond these, `match` on a unit-level scalar wants a gate node: today each branch
retracts and re-asserts the whole entity on a switch and the union cancels most of it.

Soundness is checked the same way as P-1, by driving each example program through
random event histories and comparing every view and delta against batch evaluation
of its unrewritten body.

**Items 1–3 landed as P-2b (2026-09-30).**
- *Sharing.* `Circuit::add_node` returns an existing node with the same
  `Node::share_key` (its structure minus private state). Views are added one at a
  time, each backfilled before the next, so a later view often wants a node that has
  already stepped without keeping an integral. Such a node is still shared, and the
  new view's backfill resets and recomputes it from its children's history. That
  works for every kernel but a fix region's, because each kernel's private state
  (`linv`, aggregate accumulators, `fired`) is a function of that history. If a
  view names such a node outright, it starts keeping an integral, which the backfill
  rebuilds (`revive`). Sharing only unstepped or kept nodes was the first cut; it
  left 7 duplicates in TodoMVC and would leave more in any program with many views.
- *Right identity.* `X . Identity(E)` is `X` when `values_anchor` proves X's values
  are live `E` ids: identities, coreflexive maps and compares, `InRel`, `Inverse`
  of an anchored node, and subsets or composes of those. A field of ids never
  qualifies, since nothing cascades a retract to the rows pointing at it. Applied
  to `By` as well as `Compose`.
- *Coreflexives.* `Semijoin`/`Antijoin` whose left side is a weight-1 coreflexive
  on `E`, and `Compose` of two, count as coreflexives for `Identity(E)[X] = X`.
- *Tests.* `tests/lower_rewrites.rs` now drives any program from its declared
  events, making up arguments by parameter type. It runs the benchmark, TodoMVC and
  kanban, each with extra `let`s: ones that reach the new rules, dangling-id
  shapes that must not be rewritten (`(.todo) . Todo`, `(.list) . List`,
  `Tag[(.todo) except …]`), and `new` rows before them so every backfill runs over
  data. `tests/dbsp.rs` covers replay of stateless and stateful shared nodes
  directly. Four deliberately unsound mutations were each caught: a right identity
  that ignores anchoring, replay without reset, no replay, and an unconditional
  antijoin coreflexive.

Measured (native release, median of 5, P-2 → P-2b): TodoMVC lowers to 57 nodes
(from 114), with no two equal. AddTodo ×2000 54.5 → 35.7 ms; ToggleAll on 2k
21–36 → 11–16 ms; SetFilter(Active) 21.2 → 14.3 ms; ClearCompleted on 2k 45.4 →
22.0 ms. js-framework-benchmark: 15 → 14 nodes, timings unchanged within noise
(Run(10k) 65.7 → 60.5 ms). Items 4–6 remain.

Trade-off: `snapshot()` on an output with no integral would need to recompute it
(batch-evaluate from inputs). That only happens at boot, where batch evaluation is
exactly right; simpler still is to force `needs_integral` for every output.

### Phase 3: deltas as flat, consolidated batches

Deltas are transient: built once, read by a few parents, then dropped. A BTree per
delta per node pays per-row allocation and rebalancing for an order nobody needs
until output. Replace the delta type with **`Batch = Vec<(Value, Value, i64)>`,
sorted and consolidated once** (sort by key and value, merge weights, drop zeros).
This is what DBSP itself uses (sorted runs).

- Kernels that iterate a delta (all of them) get a straight slice walk.
- Kernels that group by key (`CoKeyed`, `Aggregate`) get runs of equal keys for free.
- Most kernels already emit in input key order, so the consolidation sort is nearly
  linear.
- Integrals stay indexed (next phase), because they are probed.

Trade-off: two relation types instead of one. The `BinaryRelation` trait already
exists; kernels read deltas through a slice API and integrals through a probe API.
Keep `BTreeRelation` as the reference implementation used by the batch oracle in
tests.

**Landed as P-3 (2026-09-30).** `dbsp/batch.rs`: `Batch` is a `Vec<(V, V, w)>` sorted
by `(left, right)` and consolidated once by `BatchBuilder::finish` (an `is_sorted`
check first, so a kernel that emits in order pays a linear pass). Every kernel pushes
into a builder; grouping kernels (`Aggregate`) walk runs of equal keys; probes are
binary searches. Notes:
- `StepResult::view_deltas` is now `HashMap<String, Batch>`, and each view's delta
  is **moved** out of the step, not cloned; only a node several views alias is
  cloned. `Batch` implements `BinaryRelation`, so hosts and tests read it as before.
  The JSON encoding is unchanged (same row order).
- Backfill no longer redirects below-floor reads inside `Ctx::delta`. It copies the
  integral of each below-floor node a new node reads into that node's delta slot, so
  `Ctx` has one delta type. That is a copy per backfill, which runs at boot.
- Fix regions iterate on batches too; `prev` is a batch and Exit is a batch
  subtraction.

Measured with P-3 alone (P-4's columns switched off), native release, median of 5,
P-2b → P-3: Run(10k) 63.6 → 27.0 ms, replace-10k 126 → 48.2, Clear 64 → 29.9,
Add(1k) 4.5 → 2.7, Update 2.5 → 2.2 (P-2b figures are one run). TodoMVC: AddTodo
×2000 35.7 → 31.8, SetFilter(Active) 14.3 → 7.8, ClearCompleted on 2k 22.0 → 15.9.

### Phase 4: functional relations and dense entity columns

**The data-model change the question asks about. Yes to all three ideas, with one
simplification: ids are already type-scoped and dense.**

1. **Track functionality.** Add a multiplicity flag, "at most one value per key",
   inferred in lowering. `Field` inputs, `MapConst`, `Filter`/`InRel` of a functional
   relation, `Compose` of two functional relations, and aggregates are functional.
   `Inverse`, `Union` and general `Compose` are not. No user-facing syntax is needed;
   `rel` many-to-many stays general.

2. **A functional integral needs no weights.** In a consistent state every live
   `(key, value)` pair has weight exactly 1, so weight is presence. Store the value
   and nothing else. Weights stay in deltas, and a functional delta consolidates to a
   **change record** per key: `(key, old: Option<V>, new: Option<V>)`. That is exactly
   the mount/update/remove classification the shaper already derives from −/+ pairs.

3. **Entity-keyed functional relations become columns.** `seq` is already per-sort,
   dense and sequential, so no global-to-type-scoped id map is needed:
   - `Field(E, f)` becomes `Column<T> { base: u64, pages: Vec<Option<Box<[Slot<T>; 1024]>>> }`,
     indexed by `seq - base`.
   - `Identity(E)` becomes a liveness bitset over the same index space.
   - A probe is two array indexes. A retract clears a slot. A fully dead page is freed.

   Paging matters because ids are never reused: they are persisted in the log, and
   renumbering would break replay. The benchmark's replace-10k leaves a dead prefix
   of 10k ids each time. A page table frees dead pages, and `base` skips an all-dead
   prefix cheaply. A flat `Vec` would grow without bound under churn.

4. **Packing ids** into one `u64` (sort in the top 16 bits, seq in the low 48) shrinks
   `Value::Id`. Within a column, the sort is implied by the column, so the index *is*
   `seq`. The encoded form `#sort:seq` and the log are unchanged.

Only entity-keyed functional relations get columns. Anything keyed by `Unit`, a pair,
text or an aggregate group, and every non-functional relation, keeps a general
hashed index. Keep it narrow: in the benchmark and TodoMVC, the base fields and
level views are nearly all columns.

Side effect on writes: `push_set` on a column is "read the old slot, write the new
one", and `net_rows` composition inside one transaction becomes a per-slot
overwrite. The O(N²) retract bug fixed in S-91 cannot recur by construction.

Trade-offs:
- Two integral representations, general and column, dispatched per node. The kernel
  code path count grows. Mitigate by specializing only `Input`, `Compose` (R-side
  probe) and `CoKeyed` (both sides) at first.
- Memory under id churn is bounded by live pages, not by ids ever minted, but each
  page is 1024 slots even when sparsely live. Accept that, or add compaction later.
- Determinism: a column iterates in `seq` order, not `Value::Ord` order. Output order
  must be sorted where tests or JSON depend on it. The shaper doesn't care.

**Landed as P-4 (2026-09-30)**, without packed ids and without carrying the flag to
`ShapeIR` (below). What was built:
- *Multiplicity inference* (`dbsp/props.rs`, which now also holds P-1's `anchor` and
  the coreflexive checks lowering uses). `functional` covers inputs, singletons,
  aggregates, per-row maps and filters, `Proj`, `Distinct`, semijoin and antijoin of
  a functional left side, and `Compose` or co-keyed ops of two functional sides. The
  wider rule lets one more P-1 rewrite fire: `Row[.num + .pos > 3]` drops its
  `Identity(Row)[…]`. `tests/lower_rewrites.rs` asserts that shape now, and its
  random-history oracle checks it.
- *Column integrals* (`dbsp/integral.rs`). `Integral` is `Rel(BTreeRelation)` or
  `Col(Column)`, and one read API covers both (`row_ref`, `weight`, `has_left`,
  `triples`, `keys`, plus `BinaryRelation`). A column is paged by `seq` into 1024-slot
  pages held in a `VecDeque` with a `base` page number: an empty page is freed, and
  a dead prefix pops off the front. It stores `(key, value)` per slot and no weights.
  Iterating in `seq` order *is* `Value` order, because every key has the same sort, so
  determinism needs nothing extra. `Circuit::view` and `input_integral` return
  `&Integral`.
- *Choosing a column.* `Circuit::keep_integral` picks the representation when a node
  starts keeping an integral (`props::column_sort`): a column **whenever the node's
  keys are proven to be one sort's ids** (`anchor`), without requiring a proof that
  it is functional. Any write a column cannot hold (a second value at a key, a
  weight other than ±1, a key of another sort) **demotes** it to a general relation
  in place, and demotion is one-way. So the choice decides speed only, never
  results. Waiting for a functionality proof left out what is functional only in
  practice: the union of a `match`'s disjoint branches (TodoMVC's `visible`) and
  every field read through it (TodoMVC: 11 columns with the proof, 21 without).
  Commit applies a batch's retractions before its assertions, because a
  consolidated batch can order a key's `+new` before its `−old`.
- *Tests.* `tests/dbsp.rs` gains two property tests over id-keyed data, mostly
  well-behaved writes mixed with arbitrary ones, so columns stay columns for a while
  and then demote partway through: a column against a `BTreeRelation` after every
  commit, and fork, semijoin, compose, `MapConst` and sum reading columns, checked
  against the batch oracle. A mutation that retracts a slot without matching its
  value fails both. `bench_create` prints columns vs general integrals, before and
  after the event sequence, so a benchmark demotion would show. None happens:
  js-framework-benchmark keeps 13 of 14 integrals as columns, TodoMVC 21 of 46.

Measured, native release, median of 5, P-3 → P-3 + P-4: Run(1k) 2.5 → 1.65 ms,
Run(10k) 27.0 → **16.7**, replace-10k 48.2 → 28.6, Add(1k) 2.7 → 1.5, Update 2.2 → 1.3,
Clear 29.9 → 14.7. TodoMVC: AddTodo ×2000 31.8 → 27.1, ToggleAll(True) 6.9 → 4.8,
SetFilter(Active) 7.8 → 4.0, ToggleAll(False) 10.8 → 6.8, ClearCompleted on 2k
15.9 → 9.4. Wasm in Chrome (the example's `engine cost` spec): Run(10k) about 52 ms.

Official harness after P-1 through P-4 (Chrome, medians, ms, total / script, against §1's
S-91 run, which predates P-1; vanillajs and elysium are §1's figures):

| Benchmark | Rex S-91 | Rex after P-4 | vanillajs | elysium |
|---|---|---|---|---|
| create 1,000 | 112.5 / 30.5 | 49.0 / 18.3 | 85.1 / 6.4 | 107.6 / 19.1 |
| replace 1,000 | | 61.5 / 31.2 | 99.9 / 18.0 | 124.7 / 31.5 |
| update every 10th | 28.4 / 6.4 | 27.8 / 5.3 | 20.2 / 1.1 | 36.7 / 6.0 |
| select | | 5.7 / 1.4 | 4.8 / 0.7 | 10.6 / 3.3 |
| swap | | 26.7 / 2.5 | 22.9 / 0.4 | 26.8 / 2.2 |
| remove one | | 19.6 / 0.6 | 18.8 / 0.7 | 26.0 / 1.1 |
| create 10,000 | 803.9 / 422.1 | **531.3 / 190.1** | 353.4 / 26.3 | 508.5 / 152.8 |
| append 1,000 | | 59.6 / 20.0 | 41.1 / 2.9 | 53.5 / 16.1 |
| clear 1,000 (4× throttle) | 103.4 / 100.4 | 64.0 / 61.1 | 19.3 / 15.4 | 34.5 / 30.8 |
| first paint | 1,580 | 1,595 | 58 | 142 |

Memory after run is 5.4 MB and after run-and-clear 4.6 MB; the saved
`results/rex-*` files had 19.9 and 19.0, but they came from a different run than §1.
Compressed size is 214.7 KB. Create 10k is now 1.5× vanillajs in total time and
about 190 ms of script, of which the wasm engine is about 52 ms. The rest is the
boundary and the shaper (P-6), and clear is the largest remaining relative gap.
Create 1k's paint (30 vs about 80 ms elsewhere) looks like a harness artifact; its
total is not comparable until it is re-run.

Not done, deliberately:
- **Packed ids.** `Value` stays 24 bytes while `Pair` holds two boxes, so packing
  `Id` alone saves nothing. Do it with P-5's single-box `Pair`.
- **The functional flag in `ShapeIR`.** Multiplicity is a property of the lowered
  circuit, and `rex build` does not lower. The only consumer is P-6's change-record
  boundary, so wire it there (codegen lowers, or the engine reports per view at boot).
  The proven `functional` is the flag to send, not `column_sort`'s optimistic choice.
- **Change records for functional deltas.** Deltas keep weights. That is P-6's
  wire format, not an engine need.

### Phase 5: a smaller, cheaper `Value`

- `Id` packed to `u64` (phase 4), and `Pair(Box<(Value, Value)>)` with one box instead
  of two: `Value` drops from 24 to 16 bytes.
- Optionally **intern pairs** too, making `Value` `Copy`, so clones and drops become
  plain copies. Pairs are rare (products, composite keys), but interned pairs never
  free their memory. Measure first; the boxed form may be enough.
- Replace the interner's global `Mutex` with a thread-local `RefCell`. The engine is
  single-threaded per circuit, and wasm is single-threaded. `Sym::as_str` currently
  locks on every JSON encode and every semantic compare.

### Phase 6: the boundary and the shaper

Sized by step 0. The likely shape:

- For **functional views**, send change records instead of −/+ pairs:
  `[key, newValue | null]`. The shaper's mirror for those views becomes
  `Map<key, value>` instead of `Map<key, Map<value, weight>>`, and `resolveOne`
  becomes a single `get`. Membership views are functional too (child → parent), so
  mounts and reparents need no weight bookkeeping.
- Reuse the engine's knowledge: when a key's membership and attribute changes arrive
  in one step (a mount), send them grouped per level row. The shaper then skips
  assembling the "wide row" from mirrors on mount. This couples the step result to
  the shape tree, which the engine does not know today, so it's lower priority.
- Defer a binary or columnar wire format (typed arrays instead of JSON strings) until
  step 0 shows JSON dominating after the above.

Trade-off: the shaper must still accept general (weighted) views for non-functional
relations, so this adds a second delta form beside the first. The functional flag
from phase 4 travels in `ShapeIR`, so codegen knows each view's form statically.

**Measured first (2026-09-30, after P-4).** A CPU profile of one click in Chrome
(`examples/js-framework-benchmark/e2e/profile.spec.ts`, run with `REX_PROFILE=1`),
split by P-0's `?profile` hook. Create 10k's click handler took 180 ms:

| Part | ms | What |
|---|---|---|
| wasm `dispatch` | 49 | arg JSON → `serde_json::Value`, the step, output JSON, then the persistence wrapper serializing and parsing the logged event (all 10k labels) **twice** |
| `JSON.parse` | 5.6 | the step result |
| shaper | 119 | DOM construction ~45 (8 `createElement` + `appendChild` per row), **order index ~27** (`compareEncoded` parsed a `BigInt` on every binary-search compare), mirrors and GC ~30, attribute applies ~11 |

Clear 10k took 164 ms, 130 of it in the shaper: `OrderIndex.remove` spliced the front
of a 10k array per row (O(n²), 52 ms), plus 10k separate `removeChild` calls (40 ms).

So against elysium (all TS, a less advanced engine) **the gap was the shaper, not the
boundary.** JSON parse is about 6 ms and the wasm engine step is well under half of
`dispatch`. Elysium does the same DOM work, but it doesn't pay an order index, a
second copy of the data in mirrors, or a double log read.

**First P-6 cut (2026-09-30)**, in order of measured payoff. None of it changes the
wire format:
- *Order index* (`rex-dom/src/order.ts`): keys are decoded once per entry
  (`sortKey`: a number when exact, a bigint otherwise, ids as pairs), and
  `insertMany`/`removeMany` take one merge or filter pass per parent. The shaper
  groups removes and mounts by parent. A bulk mount walks the parent's children last
  to first, so each new element's `insertBefore` reference is the element just
  placed; batches of 16 or fewer go one at a time.
- *Bulk clear:* when a batch removes every child a parent element has (at least 2,
  checked against `childNodes.length`), one `textContent = ""` replaces N
  `removeChild` calls. `DomDriver` gains `childCount`, `clear` and `clone`.
- *Single-value mirrors:* a mirror key holds its value directly when it has exactly
  one at weight 1, and a value → weight map only otherwise; it collapses back
  afterwards. This needs no functional flag, so the `ShapeIR` change can wait for
  the change-record wire format.
- *Template cloning* (codegen): each level builds a prototype skeleton once and
  rows `cloneNode` it. Listeners, `draggable`/`dropTarget` and autofocus's `focus()`
  are still applied per row, by child-index path.
- *Engine side:* view rows and event-log rows are written straight from borrowed
  triples into the output buffer, with one scratch `String` and no `Value` clones;
  `json_string` copies escape-free strings whole; `seed_deltas` resolves each input
  key once per transaction instead of with two hash lookups per row. The persistence
  wrapper keeps events it has read but not yet confirmed, so each event is
  serialized and parsed once rather than twice (a failed append still retries them).

Result, same profile: create 10k click handler 180 → **121 ms** (`dispatch` 49 → 40,
shaper 119 → 70); clear 10k 164 → **73 ms** (shaper 130 → 47, of which 30 ms is the
browser's own `textContent = ""`); create 1k 13.4 → 10.6 ms.

Official harness, CPU benchmarks only (Chrome, medians, ms, total / script, P-4 → this
cut; vanillajs and elysium are §1's figures):

| Benchmark | Rex after P-4 | Rex now | vanillajs | elysium |
|---|---|---|---|---|
| create 1,000 | 49.0 / 18.3 | 45.0 / 13.5 | 85.1 / 6.4 | 107.6 / 19.1 |
| replace 1,000 | 61.5 / 31.2 | 53.7 / 21.3 | 99.9 / 18.0 | 124.7 / 31.5 |
| update every 10th | 27.8 / 5.3 | 26.1 / 4.8 | 20.2 / 1.1 | 36.7 / 6.0 |
| create 10,000 | 531.3 / 190.1 | **483.6 / 126.7** | 353.4 / 26.3 | 508.5 / 152.8 |
| append 1,000 | 59.6 / 20.0 | 54.9 / 14.9 | 41.1 / 2.9 | 53.5 / 16.1 |
| clear 1,000 (4× throttle) | 64.0 / 61.1 | 41.3 / 37.5 | 19.3 / 15.4 | 34.5 / 30.8 |

Select, swap and remove-one are unchanged within noise. Rex's script time is now below
elysium's everywhere except clear (37.5 vs 30.8).

What is left, by size (create 10k): `template` self time 12 ms (child-index
navigation, `dataset.key`, two listener closures per row; event delegation on the
level's slot would remove the closures), `cloneNode` 11, mirrors plus `applyStep` about
15, GC 10, attribute applies 9, output JSON about 8, and the engine step itself. Next:
- **Event delegation** for row listeners: one listener per level slot, keyed by
  `data-key`.
- **Change records** for functional views (the plan above). They halve update
  traffic but do nothing for create, which is all `+` rows.
- **Engine:** SipHash still shows up (`Transaction::touched`, `HashMap<InputKey, _>`).
  Interning takes a global mutex per text (P-5). Event args go through a
  `serde_json::Value` tree.

### Phase 7: startup (first paint 1.58 s, 203 KB compressed)

The browser downloads the parser, checker, desugarer and lowering, parses the `.rex`
source at boot, and builds the circuit. Compiling the program ahead of time into a
serialized circuit (the "precompiled circuit" already listed as post-MVP) removes
both the startup work and most of the wasm. Before that, two cheap checks:
- Is the release profile still `opt-level = "z"`? §3.2 measured about 1.5× engine cost
  for about 27 KB.
- Stream the wasm compile (`instantiateStreaming`) in parallel with fetching the program.

Not measured: how the 1.58 s splits between download, compile and in-browser
compilation of the program. Measure before choosing.

### Phase 8: compiled circuits (proposal, not started)

Today every app ships one generic `rex_wasm` (825 KB). At boot it parses, checks and
lowers the `.rex` source (`RexApp::boot`), then interprets the resulting `Circuit` arena
every step (`Circuit::run`, `Node::compute`). Event handlers are interpreted too:
`events::expand` walks `MutationIR` into `DispatchOp`s, and `Engine::build_tx` turns
those into a `Transaction`. The idea: `rex build` already compiles the program, so it
can emit Rust that implements the circuit and handlers statically, calling the DBSP
operators as a library.

**Where the payoff is (measured 2026-09-30).** The benchmark circuit is 14 nodes
(8 inputs, 6 computed). The interpreter's `match` runs once per node per step, not per
row, so compiling the node graph alone saves almost nothing. A native Time Profiler run
of `tests/bench_create.rs` (about 60 samples, so rough) split `dispatch_event` like this:

| Part | Samples |
|---|---|
| `Engine::build_tx` (`push_retract` 19, `net_rows` 10, `push_new` 6) | 26 |
| `events::expand` | 14 |
| `Circuit::run`, the whole circuit | **11** |

Native totals: Run(10k) 21 ms, replace 28 ms, Clear 13 ms. In the browser, `dispatch`
is about 40 ms of the 121 ms create-10k click. The handler layer's cost is per row and
per field: SipHash, a `row_args.clone()` per row, `String` field names, `check_field`,
and `intern()` (a global mutex).

So compilation pays off through:
- compiled handlers, which write rows straight into each input's delta;
- typed rows instead of `Value`, which is P-5 done statically;
- fusion of linear chains, with no intermediate batches;
- startup and bundle size: no parser, checker or lowering in the wasm, and no compile
  at boot (Phase 7).

Some handler wins (pre-interned field syms, no per-row `HashMap` clone) are also
reachable inside the interpreter.

**Crate split.**
- `rex-core` stays the compiler and the oracle: parse, check, lower, the `Circuit`
  interpreter, batch `eval::algebra`, the REPL, `add_view`/backfill, node sharing.
- A new `rex-engine` runtime library, with no parser or checker, holds `Value`,
  `Sym`/intern, `Batch`/`BatchBuilder`, `Integral`/`Column`, `BTreeRelation`, the
  kernels, the event log and JSON encode. `rex-core` depends on it and re-exports it.
- Kernels become free functions extracted from the `Node::compute` arms: `compose`,
  `co_keyed`, `semijoin_delta` (already free), `aggregate`, `distinct`, `intersect`.
  `Node::compute` becomes a thin match over them, so the interpreter and generated
  code share one implementation per operator.
- `FixRegion::evaluate` stays a library call. Generated code embeds an interpreted
  inner `Circuit` for recursion groups (no example uses one yet).

**What `rex build` emits.** `rex build app.rex -o src/main.ts --engine engine/` also
writes a small cargo crate (`engine/{Cargo.toml, src/lib.rs}`) depending on
`rex-engine` and `wasm-bindgen`. `scripts/build-wasm.sh` builds that crate instead of
`rex-wasm`. The emitter:
- checks the program, then lowers it into a scratch `Circuit`, exactly as boot does
  today;
- walks the arena in topological order, reading `keep`, `integral_reads`,
  `column_sort` and `props::functional`;
- emits one Rust item per node. Ids bound by seed `new`s are deterministic
  (`#sort:0..`), so the emitter knows them too.

Sketch of the generated code for the benchmark (stage B, still `Value`-typed):

```rust
pub struct App {
    // kept integrals, representation fixed at compile time
    i_row: Column, i_row_num: Column, i_row_label: Column, i_row_pos: Column,
    i_row_selected: Column, i_state_next_id: Column, i_state_next_pos: Column,
    i_gate1: Column, i_on_update: Column, i_on_swap_a: Column, i_on_swap_b: Column,
    next_row: u64, log: EventLog,
}

// one builder per input, filled directly by handlers (no Transaction / DispatchOp)
#[derive(Default)] struct Tx { row: BatchBuilder, row_num: BatchBuilder, row_label: BatchBuilder,
                               row_pos: BatchBuilder, row_selected: BatchBuilder, /* … */ }

impl App {
    // `on Run(n, labels) { delete Row; new Row from labels as (i, label) {…}; set … }`
    pub fn on_run(&mut self, n: i64, labels: &[(i64, Sym)]) -> Step {
        let mut tx = Tx::default();
        self.retract_all_row(&mut tx);                         // generated per entity
        let next_id = self.state_int(&self.i_state_next_id);   // pre-event snapshot read
        for &(i, label) in labels {                            // sorted by key, typed
            let id = Value::Id(ROW, self.next_row); self.next_row += 1;
            tx.row.push(id.clone(), id.clone(), 1);
            tx.row_num.push(id.clone(), Value::Int(next_id + i), 1);
            tx.row_label.push(id.clone(), Value::Text(label), 1);
            tx.row_pos.push(id.clone(), Value::Int(i + 1), 1);
            tx.row_selected.push(id, Value::Atom(FALSE), 1);
        }
        self.set_state(&mut tx, NEXT_ID, Value::Int(next_id + n));
        /* … */
        self.step(tx.finish())
    }

    fn step(&mut self, d: Deltas) -> Step {
        // straight-line, topological; dirty checks only where a child can be empty
        let d_gate1 = filter_map(&d.row_selected, |k, v| (v == &TRUE_ATOM).then(|| k.clone()));
        let d_on_update = filter_map(&d.row_num, |k, v| (v.as_i64()? % 10 == 1).then(|| k.clone()));
        let d_member = map_const(&d.row, &Value::Unit);
        // commit: a direct Column::commit call, no Integral enum dispatch
        self.i_row.commit(&d.row); /* … */
        Step { member: d_member, order: d.row_pos, num: d.row_num, label: d.row_label,
               gate1: d_gate1, /* fixed fields, not a HashMap<String, _> */ }
    }
}
```

The generated `#[wasm_bindgen]` glue keeps the existing `RexApp` surface:
`dispatch(name, json)`, `snapshot`, `log_since`, `replay`, `base_snapshot`,
`restore`, `bound_id`. `main.ts`, `boot.ts` and the shaper work unchanged, so stage B
is a drop-in. Typed entry points (`run(n, keys: Int32Array, labels: string[])`) come
later; codegen already knows every event signature. `replay` calls the same generated
`on_*` functions silently, `restore` is one step from empty (as `Engine::restore` is
today), and the log format is unchanged, so interpreted and compiled engines can read
each other's stores.

**Stages,** each measured with the harness before the next:
- **A. Library split.** Create `rex-engine` and extract the kernels to free functions
  that `Node::compute` delegates to. No behaviour change.
- **B. Compiled engine, `Value`-typed.** Per-node kernel calls, static integral
  representations, static outputs, no per-node `match` or dirty-check dispatch.
  Compiled handlers write into per-input builders: no `DispatchOp`, no `HashMap` args,
  no `String` field names, pre-interned syms, and no `check_field` for values typed at
  compile time. `retract_all_<E>` and `set_<E>_<field>` are generated per entity.
  Wire up the js-framework-benchmark example only, behind a build flag. Measure
  dispatch, wasm size and first paint.
- **C. Fusion.** A chain of linear nodes (`Inverse`, `Filter`, `InRel`, `MapConst`,
  `FilterMap`, `Proj`, `Union`) feeding one consumer becomes one loop. Only nodes that
  are kept or have several consumers materialize a batch.
- **D. Typed rows.** Kernels become generic over `K: Key, V: Val`, with `Batch<K, V>`
  and `Column<V>`: ids as `u32` seqs, ints as `i64`, texts as `Sym`. Generated code
  picks concrete types from `ValueTy`, and `Value` survives only for pairs and
  heterogeneous nodes. This is the largest refactor; do it once B and C show where the
  per-row time remains.

**Correctness.** The interpreter stays the oracle. A new proptest
(`crates/rex-core/tests/compiled.rs`) generates the engine for each example program,
drives random event sequences through both engines, and asserts equal view deltas per
step and equal integrals. Generated crates are checked in like the codegen snapshot,
or rebuilt by a `build.rs`, so `cargo test` covers them.
`debug_assert_base_invariant` stays in the library, and compiled handlers call it in
debug builds.

**Costs and risks.**
- Every app needs its own Rust-to-wasm build: Rust and the wasm32 target at
  `rex build` time, and roughly 30–60 s release LTO builds.
- Keep the generic interpreted wasm for `vite dev` and the REPL; compiled is the
  production path, and both expose the same `RexApp` interface.
- Live `add_view` (the REPL, hot-reloading views) only works interpreted.

## 4. What not to do (yet)

- **Don't make the hidden keyset views lazy** (scanning at dispatch time). Phase 1
  makes them cheap, and laziness turns `SwapRows`/`Select` from O(1) into O(N).
- **Don't specialize the engine for bulk loads** (a separate "insert N rows" fast
  path). Phases 1–4 make the general path fast, and a second write path is a second
  place for replay bugs.
- **Don't drop the batch oracle.** Every representation change is checked against
  `BTreeRelation` batch evaluation in the existing property tests. That is the point
  of keeping the old type around.

## 5. Expected outcome (estimates, to be replaced by measurements)

| After | create 10k, engine native | Main reason |
|---|---|---|
| today | ~210 ms | 22 nodes × 10k rows through BTrees |
| phase 1 | ~60–80 ms | ~6 nodes, 4 views alias inputs |
| phases 2–3 | ~35–50 ms | no commits for linear nodes, flat deltas |
| phases 4–5 | ~10–20 ms | column probes, 16-byte values, no weights in functional state |

The end-to-end browser number also depends on phases 6 and 7 and on paint, which
alone is about 320–370 ms for every implementation on create 10k. A realistic
browser target after phases 1–5 is Rex's create 10k within about 1.3× of vanillajs.
That target is unmeasured.

## 6. Suggested story breakdown

| Story | Scope | Depends on |
|---|---|---|
| P-0 | Browser timing split (engine, parse, shaper, DOM) behind `?profile` | none |
| P-1 | Lowering rewrites and view aliasing, plus the no-orphan invariant and its property test | none |
| P-2 | `needs_integral` demand analysis | P-1 |
| P-2b | Node sharing, right identity, wider coreflexives (items 1–3, landed); then 4–6 | P-2 |
| P-3 | Flat consolidated delta batches (landed) | none (parallel with P-1) |
| P-4a | Multiplicity inference in lowering (landed); carried to `ShapeIR` (moved to P-6) | P-1 |
| P-4b | Column integrals for entity-keyed relations (landed); packed ids (moved to P-5) | P-3, P-4a |
| P-5 | `Value` shrink; thread-local interner | none |
| P-6 | Shaper bulk paths, single-value mirrors, template cloning (first cut landed); functional change records across the boundary | P-4a, P-0 |
| P-7 | Startup measurement, then profile and streaming fixes | none |
| P-8 | Compiled circuits: A library split, B compiled `Value`-typed engine and handlers, C fusion, D typed rows | P-7 measurement; D subsumes P-5 |
