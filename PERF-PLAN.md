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
- Profiling Run(10k) after P-1: of the roughly 60 ms, circuit compute is about 4 ms
  and commit about 18 ms, of which 15 ms is the five 10k-row inputs, which must commit.
  The other ~35 ms is outside `Circuit::run` (event evaluation, `build_tx`,
  seeding, `StepResult` clones) and is unprofiled. P-3 and P-4 target the input commits.
- TodoMVC still lowers to 114 nodes. P-1's rewrites fire little there, which makes
  it the next place to look for lowering wins.

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
| P-3 | Flat consolidated delta batches | none (parallel with P-1) |
| P-4a | Multiplicity inference in lowering, carried to `ShapeIR` | P-1 |
| P-4b | Column integrals for entity-keyed functional relations; packed ids | P-3, P-4a |
| P-5 | `Value` shrink; thread-local interner | none |
| P-6 | Functional change records across the boundary; `Map<key, value>` mirrors | P-4a, P-0 |
| P-7 | Startup measurement, then profile and streaming fixes | none |
