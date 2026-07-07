# A Relational Language for Incremental UIs — Design Notes

A surface language that `select`s into **nested shapes** (à la EdgeQL/Gel), compiled to a
**flat, incremental relational core** (à la DBSP/Differential Dataflow), driving a **live DOM**
(no VDOM) and **external effects** (HTTP, etc.) — all maintained *surgically* under updates.

> **Reading note.** Confidence is marked inline: ✅ solid, 🟡 plausible-but-unvalidated,
> ⚠️ known-hard / open. Two 🟡/⚠️ assumptions carry the whole design (§8) — validate them
> against a real engine and a real app *before* building much.
>
> **Status (July 2026): Parts of this are now BUILT and validated against Rex's own
> DBSP engine** (the "out of scope" engine below is exactly what `crates/rex-core/src/dbsp`
> implements). §8.1 is resolved ✅ (the engine is snapshot-consistent by construction);
> the shaper/bridge of §§3–5 exist as `js/rex-dom` with the §10 build-order test
> obligations green (vitest spy-driver counts + Playwright, `examples/kanban`). §8.2
> (ordered/top-k in-engine) remains open — ordering currently lives app-level as
> fractional-key data. Part II (effects, §§6–7) is unbuilt; see the M4 note in §7.

---

## 1. The core bet

Nesting is **sugar over composite keys**, not a new primitive. ✅

```
User select { name, posts := Post filter .author == User select { title } }
```
is not a nested *value*; it is two flat relations plus an index:
- `User(user_id, name)`
- `Post(author_id, post_id, title)` **indexed by `author_id`**

DBSP's `group-by` already produces an `IndexedZSet` — a *flat* Z-set of tuples plus a
hashmap — **never a set nested inside a column**. Navigation = index lookup; aggregation =
fold to scalar. So every nesting layer is `group` (add a key), every consumer is `unnest`
(read by key) or `aggregate` (fold by key). These are closed under composition, so **surface
nesting composes arbitrarily and stays flat/incremental**. This mirrors EdgeQL→Postgres:
Postgres has no nested relations either; nesting is reconstructed at the edge (`jsonb_agg`).

**The one cliff** ⚠️: treating a nested collection as an *opaque value* (pass whole array to a
UDF, `array_agg` into a real ordered array, compare collections by value) leaves the flat
algebra → degrades that subtree from surgical to recompute-per-group. Keep nested things as
**shapes** (navigable/foldable), not first-class collection *values*, and you keep full
incrementality. `array_agg` may exist but is explicitly marked an incrementality cliff.

---

## 2. Architecture (one picture)

```
UI event ─insert─▶ base tables ─▶ [ CORE: flat, pure, sync, incremental ] ─▶ per-node Z-set deltas
                                     (DBSP-style; OUT OF SCOPE to build)          │
   ┌─────────────────────────────────────────────────────────────────────────────┤
   ▼                                                                              ▼
[ SHAPER ]  classify deltas → ops; ordering; coalescing            [ SHAPER ] derive Request rows
   │  holds nodeMap, per-parent order index, attr indexes             │
   ▼                                                                  ▼
[ DOM BRIDGE ] apply ops as DOM mutations, sync, one txn      [ EFFECT DRIVER ] do real HTTP,
   ▼                                                            write outcome facts back as base tables
 live DOM                                                              │
                                                                       └──▶ (loops back as base insert)
```

**Invariant that makes it all work** ✅ (given §8): the core never touches DOM or network. It
reads/writes tables. Everything impure — DOM mutation, HTTP — is an **external driver at the
edge** that consumes deltas and produces base-table facts. The DOM bridge and the HTTP driver
are *the same architectural pattern*.

---

## 3. 6NF decomposition → fine-grained reactivity ✅

Each renderable attribute is its own relation; structural membership is its own relation.

```
Card_title(card_id, title)       -- attribute
Card_pos(card_id, order_key)     -- ordering (fractional; §5)
Card_list(card_id, list_id)      -- STRUCTURAL: which parent
```

Payoff: **structural-vs-attribute is syntactic.** A `-/+` on `Card_list` is a mount/move/
reparent; a `-/+` on `Card_title` is a field update. A retitle touches one text node.

**Caveat** ✅ (amended by implementation): the "every leaf renders one attribute, no wide
rows" thesis holds for *updates* but **not for mount** — you can't mount half a card. At
mount, the shaper assembles the wide row from **its own mirrors**: it receives every
rendered view's delta every step anyway, so it integrates them into per-view maps and
mount becomes local reads. (The earlier "point-lookups across the decomposed relations"
idea would be boundary chatter with the engine behind WASM; `read_view` remains as an
escape hatch for values deliberately not mirrored.) Cost lands only on insertions.

---

## 4. Delta → DOM: the classifier + phased apply

**One `step()` = one delta batch = one DOM transaction, applied synchronously (no yields).** ✅
This is what guarantees glitch-freedom — *provided the core actually emits a consistent batch*
(⚠️ see §8.1).

### Classifier — by **view role**, not composite-key parsing ✅ (amended by implementation)

In Rex's 6NF binary form each view's *role* is declared in the shape tree (structural
membership / order / attribute), so classification never inspects composite keys — it
groups each **role-tagged view's** delta by child key:

| condition (per child key) | op |
|---|---|
| membership view: positive only | `mount` (assemble wide row from mirrors; suppress own attr deltas) |
| membership view: negative only | `remove` (coalesce to highest dead ancestor) |
| membership view: −/+ with new parent value | `reparent` (reuse DOM node!) |
| order view: −/+ | `move` |
| attribute view: −/+ | `update(field)` |

**The #1 rule** ✅: a `−old/+new` sharing the entity key is **one** op (update/move/reparent),
**never** remove+mount. This preserves DOM node identity — focus, selection, scroll, in-flight
animation. Getting this wrong is a *silent* bug (works functionally; every edit nukes the
node). **Build and stress-test this first.**

**Parent-change ≡ order-change**, one level up ✅: Kanban card moving lists is a `reparent`
that reuses the same DOM node (`insertBefore` of an attached node = move). No flicker.

### Phased apply (static order, derived from shape tree) ✅

```
1. REMOVES   (top-down, subtree-coalesced: dead user → one removeChild, not 5000)
2. MOUNTS    (depth ascending: parent before child; assemble wide row)
3. UPDATES   (field-masked; skip keys mounted this batch)
4. MOVES/REPARENTS (computed against FINAL membership, for correct sibling refs)
```

---

## 5. Ordering machinery

DOM children are ordered; Z-sets are not. Order must become surgical `move` ops.

- **Fractional order keys, not dense ranks** ✅. Dense ranks shift every following sibling per
  reorder → O(n) moves. Fractional keys = **one** move per reorder.
- **Do NOT hand-roll the midpoint function** ⚠️ (learned the hard way — the recursion has
  nasty unbounded-side / same-gap edge cases; three naive attempts all broke monotonicity or
  grew keys linearly). **Use a published `fractional-indexing` lib.** Validated behavior:
  random insertion holds keys to ~8 chars at 50k items; the *only* pathology is repeated
  insertion into the identical gap (linear key growth) → needs a **rebalance hook** (re-space
  a parent's children when a key exceeds ~40 chars; a batch of `move`s, rare, amortized).
- **Per-parent sorted index** with `successor()` to compute the `insertBefore` reference. ✅
- **Every positional `order by` MUST be a total order** — append entity id as tiebreak, or
  top-k / moves become nondeterministic. ✅

---

## 6. External effects: intent / claim / outcome

Effects break three core properties: determinism, sync fixpoint, retractability. You cannot
`−1` a charged credit card. So effects live **outside** the core as three relations: ✅

| relation | owner | mutability | meaning |
|---|---|---|---|
| `EffectIntent(idem, kind, url, body, once)` | **derived view** | retractable | "we wish this to happen" |
| `EffectClaim(idem, status)` | **driver (base)** | never deleted while in-flight | "driver owns this" |
| `EffectOutcome(idem, for_url, status, body)` | **driver (base)** | append-only, immutable | "the world responded; history" |

**Two rules that prevent the double-charge** ✅:
1. **Source truth from `EffectOutcome`, never `EffectIntent`.** A wish can be retracted; a fact
   cannot. (GETs may be sourced from the intent side — re-issuing is harmless. POSTs must not.)
2. **`idem` is stable across re-derivations**, tied to the *triggering event's identity* (a
   click nonce), **not** to mutable payload (a cart total). Same `idem` → server
   `Idempotency-Key` header, so the server dedups too (defense in depth).

**Driver asymmetry** ✅: on intent retraction, a GET aborts; a `once` POST **consults the claim
table** — if `SETTLED`, no-op (outcome persists); if `IN_FLIGHT`, do *not* assume cancel.

⚠️ **Crash windows are the genuinely-hard part.** Claim-written-then-crash, or
succeeded-then-crash-before-recording, force a **server contract**: the endpoint must both
honor `Idempotency-Key` *and* allow **retrieval of a prior result by that key**. If a backend
can't offer retrieval-by-key, the honest guarantee is *at-least-once*, and the type system
should say so (`ExactlyOnce<T>` vs `AtLeastOnce<T>`) rather than pretend.

**Structured concurrency is free** ✅ (the nicest result): an intent nested in a shape node is
*justified by that node's row*. Unmount the node → intent retracted → in-flight fetch aborted.
The *same retraction* that cascades DOM subtree-removal cascades effect cancellation. One
mechanism, two payoffs.

---

## 7. Surface sugar & desugaring

`await` is **not suspension** — it is a relational **join against a future fact**. Nothing ever
blocks a `step()`. Type: `Async<T> = Pending | Loaded(T) | Failed(E)`; the type forbids using it
as a bare `T`, so the loading/error UI is forced by the type. ✅

> **M4 prerequisite:** Rex has no runtime coproduct values yet — `Value` carries only
> `Atom` tags, no `Inj(tag, payload)` constructor/eliminator — so `Async<T>` cannot be
> represented. Building that is the first task of the effects milestone.

| surface | desugars to |
|---|---|
| `await e` | 1 intent + left-join to outcome |
| `await all { a, b }` | N intents + **inner** join (Loaded iff all present) |
| `await allSettled { }` | N intents + **outer** join (record of `Async<T>`) |
| `await race { }` | N intents + `argmin settled_at`; losers' intents retracted ⇒ aborted |
| `await search e` (latest-wins) | **slot-keyed** intent (replace on new query); stale outcomes anti-join out on `for_url != live` |
| `timeout(d)` | `now`-relation view derives `TimedOut`, driver aborts |
| `await cached e` (SWR) | last outcome for slot ∪ fresh intent |
| `scope { }` | synthetic justification row; drop it ⇒ cancel group (lexical scope is already automatic) |

**Cancellation is always "the intent stopped being justified"** — three reasons (parent gone /
superseded by newer sibling on a slot / clock past deadline), one mechanism. ✅

---

## 8. ⚠️ The two load-bearing unproven assumptions — validate FIRST

Everything above rests on these. They are asserted, not proven.

**8.1 Cross-output snapshot consistency — RESOLVED ✅ against Rex's engine.**
`Circuit::step()` computes every node's delta against one frozen pre-step integral
snapshot, commits, and returns *all* view deltas in a single synchronous `StepResult` —
no frontiers, no timestamps, no async. "Batch = tick = DOM transaction" holds by
construction; **no barrier is needed.** (The concern remains real for any future
multi-worker/Feldera-style backend and should be re-verified if the engine is swapped.)

**8.2 Ordered/top-k nesting stays cheap.** `posts order by … limit 5` per group: a deletion from
the top-5 must *pull up* the 6th (engine retains more than it emits); a single upstream change
can yield a two-element output delta. Incrementally maintainable in theory 🟡 — but the constant
factors and the shaper bookkeeping under *real* churn (drag-reorder storms, large lists) are
unmeasured. **Benchmark before trusting the "surgical and cheap" story.**
*Implementation note:* deferred, deliberately. Ordering today is app-level data
(fractional keys in an ordinary relation, written by event handlers; the engine has no
ordering concept — matches SPEC §11's parking of sort/Top-N). This assumption goes live
only when an in-engine `order by … limit` operator is built; the benchmark obligation
moves with it.

Secondary open items: non-incrementalizable aggregates (`median`, order-sensitive `array_agg`)
recompute per affected group — O(group), state it in the cost model; wall-clock-derived leaves
(`"3 minutes ago"`) change with *no* input delta — handle via a `now` relation or client timer,
never expect the delta stream to emit time passing.

---

## 9. Worked example — Kanban board with search + exactly-once order

```
Board select {
  results  := await search http.get("/search?q=" ++ SearchBox.query),   -- latest-wins GET
  cards    := Card filter .board==Board order by .pos select {
                title,
                author := await http.get("/user/" ++ .author_id),        -- structurally scoped
              },
  checkout := on ClickPlaceOrder(Board) =>
                await once http.post("/order", {cart: .cart_id}),         -- exactly-once POST
}
```

**Update A — retitle card c1.** `Card_title: −(c1,old)+(c1,new)` → same key, non-key col →
`update(c1,{title})`. One text-node mutation. Node identity preserved. ✅

**Update B — c1 todo→doing (Kanban drag).** `Card_list: −(c1,todo)+(c1,doing)` → parent portion
differs → `reparent(c1)`: reuse the DOM node, `insertBefore` into the new list. No flicker. ✅

**Update C — reorder c2.** `Card_pos: −(c2,k1)+(c2,k2)` → only order key differs → one `move`
(fractional key → single delta). ✅

**Update D — click Place Order (nonce k9) while a coupon changes the cart total, same tick:**

| tick | happens | result |
|---|---|---|
| T0 | click + total −40+36 | intent `idem=order:b1:k9` derived; no claim → **claim IN_FLIGHT**, POST w/ Idempotency-Key. checkout=Submitting |
| T1 | coupon re-touches cart | intent re-derived, **`idem` unchanged**; body churn ⇒ claim exists ⇒ **do nothing** — no 2nd charge ✅ |
| T2 | POST 200 "X7" | `+Outcome`, claim SETTLED; checkout=Placed("X7") |
| T3 | navigate away (`−click`) | `−intent`; claim SETTLED ⇒ **no-op**; Outcome persists ✅ |

Charged **exactly once** despite two re-derivations and a retraction — because truth is sourced
from `EffectOutcome` and `idem` is click-stable. Had `results`/`author` (GETs) been retracted,
the same `−intent` would *abort* the fetch instead.

---

## 10. Build order

1. **Same-key −/+ fusion classifier** (§4) — highest-risk, silent failure. Prove node identity
   survives edits (focus/scroll).
2. **Batch → DOM transaction + phased apply** (§4) — but **first validate §8.1** against the real
   engine.
3. **Ordering**: integrate published fractional-indexing + per-parent sorted index (§5). **Benchmark §8.2.**
4. Wide-row assembly + subtree-remove coalescing; then **reparent** (validate Kanban drag).
5. Effects: intent/claim/outcome + driver, GET path first; then `once` + server idempotency
   contract (§6) — validate the §9-D trace incl. simulated crash windows.
6. Combinators (`all`/`race`/`search`/`timeout`/`cached`) as desugar rules (§7).
7. Pagination/retry (async outer loop = nested timestamp) last.

## 11. Honest risk summary

- ~~**Biggest technical risk:** §8.1 snapshot consistency~~ — **retired**: Rex's engine
  is atomic per step by construction (§8.1); the barrier contingency is unnecessary.
- **Biggest cost-model unknown:** §8.2 ordered/top-k churn under real load — still open,
  but deferred along with in-engine ordering (order keys are app data today).
- **Biggest correctness-across-boundary risk:** effect crash windows (§6) — depends on backends
  you may not control; be willing to expose `AtLeastOnce<T>` honestly.
- **Most likely to be *pleasant* in practice:** structural cancellation and relational
  loading/error states — the parts where the model pays for itself.
- **Untested overall:** none of this has met a real, demanding app yet. The next best move may
  be to pick one hard app (collaborative editor, multiplayer board) and find which axiom bends
  first — cheaper than discovering it in code.