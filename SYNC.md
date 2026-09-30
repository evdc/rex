# Rex sync: many replicas, one meaning

Status: **design, post-MVP** (sync is explicitly out of MVP scope, MVP-PLAN §1).
Written 2026-09-29. Decisions in §10 were made by the owner the same day.

The goal: a Rex app runs on many replicas — tabs, devices, an optional
server — that each accept writes, including offline, and converge to the
same state without the developer writing merge code. The two-tab data loss in
`js/rex-runtime/src/adapters/indexeddb.ts` (two engines numbering events from
the same `seq`) is the first symptom; a lock would hide it, this document fixes
the cause.

---

## 1. The model: state is a fold over a *set* of events

Rex already defines state as a deterministic fold over the event log
(MVP-PLAN §2.2, S-21). Today the log is a sequence produced by one writer.
Sync generalises it:

```
state = fold(apply, sort(events))
```

where `events` is a **set** that replicas exchange (a grow-only set — the
simplest CRDT there is) and `sort` is a deterministic total order every
replica computes the same way. Two replicas that hold the same set hold the
same state. That is strong eventual consistency, and it needs exactly two
properties: an agreed order (§3) and deterministic handlers (§2).

### Why replicate intents, not deltas

The tempting alternative is to replicate each event's *effect* — its Z-set
delta. Z-set addition is commutative and associative, so merging deltas
converges with no ordering at all. It converges to the wrong answer. Two
tabs move card `c` out of list `l1` concurrently:

```
A:  −(c,l1) +(c,l2)
B:  −(c,l1) +(c,l3)
Σ:  (c,l1):−1  (c,l2):+1  (c,l3):+1
```

The card is in two lists with a negative weight in a third. Every replica
agrees, and the functional dependency `Card -> List` is broken. Summing
deltas is a per-tuple counter, the wrong CRDT for a keyed relation.

Replicating the **named event** (`MoveCard(c, l2)`) and re-running its
handler at its place in the order gives last-writer-wins on the field, keeps
every key constraint, and re-checks the handler's `where` against the real
merged state. An intent that no longer applies (the target list was deleted
concurrently) matches nothing and has an empty effect, which the runtime can
report ("your move was dropped"). This is the Replicache / event-sourcing
model, and Rex already has every piece: named events, `replay`, and effects
kept out of handlers (MVP-PLAN §5 decision 2).

---

## 2. The determinism contract

A handler's effect must be a pure function of **(state at its position in
the order, its args, its event's metadata)**. Handlers may read state and
overwrite fields; they need not commute. Commutativity is something the
checker *detects* (§6), not a requirement.

What handlers give up is *hidden* nondeterminism. Everything nondeterministic
is captured when the event is created and travels with it:

| Need | Inside a handler | On replay / rebase |
|---|---|---|
| current time | the event's own origin time (§3), not a clock | same everywhere: "when the user did it" |
| randomness | a generator seeded from the event id | same values everywhere, unique per event |
| fresh entity ids | derived from the event id (§3) | stable, collision-free |
| UI-derived values (drop position, typed text) | args — `dropPos` already works this way | already deterministic |
| device state (selection, filter, drafts) | **not readable** by replicated handlers unless passed as an arg (§7) | — |
| I/O (fetch, payment, email) | not in handlers — the effects membrane (M4) | §2.1 |

Rex can **enforce** this rather than request it: handlers are Rex, not JS
callbacks, so there is simply no `now()` to call, only the event's time
(spelling TBD). Rex has no clock or random builtin today; no MVP story
should add one outside this design.

### 2.1 Effects run once, and irreversible ones wait for finality

If every replica performed the effects of every event it replays, a payment
would be charged once per replica. Rules:

- Only the **originating replica** performs an event's effects, and only on
  its first execution — never on replay or rebase.
- The response is a new event with `cause` = the request's **event id**
  (not its `seq`, which is not stable under sync — see §9), and it
  replicates like any other.
- A rebase can undo the conditions that made an effect fire, and an email
  cannot be unsent. So **irreversible effects fire only once their
  triggering event is final**: behind the stability frontier (§5) or
  committed by a `coordinated` handler (§6). Reversible or idempotent
  effects (a fetch, a cache fill) may fire optimistically.

---

## 3. Event identity, time, and order

```
Event {
  id:     (replica, counter)   // identity: immutable, unique, seeds ids + random
  origin: Hlc                  // valid time: when it was created; what the handler sees as "now"
  name, args,
  cause:  Option<EventId>,     // was Option<u64>
  intent: Option<String>,
}
```

- **`replica`** is a random id minted when a replica (a browser profile, a
  device, a server) first boots and persisted with its store. **`counter`**
  is that replica's own sequence. Together they replace today's global
  `seq` as the storage key, so an append is idempotent by id and two tabs can
  never overwrite each other.
- **`origin`** is a hybrid logical clock stamp (physical time + logical
  counter + replica): it tracks causality like a Lamport clock and stays
  close to wall time. A replica rejects stamps too far ahead of its own
  clock (§8).
- **Entity ids** minted by an event derive from its `id`
  (`#sort:replica.counter.i` for the `i`-th mint in the event), so they are
  collision-free across replicas and identical on every re-execution.
  Deriving them from anything order-dependent — today's per-sort counter,
  or an app-level `state nextId` — would change a row's id when a rebase
  re-runs the event, orphaning every later event that names it (§9).

### 3.1 Bitemporal order (decision 1)

The order an event is applied in is **not always its origin time**. Every
event has two times:

- **origin** (valid time): stored, immutable, what the handler reads.
- **order** (system time): where it sits in the fold. Normally equal to
  `origin`; different for *late* events (§5).

`order(e) = (max(e.origin, frontier_time(e)), e.origin, e.id)`, where
`frontier_time(e)` is the time of the newest declared frontier that does not
include `e` (none → −∞). The tuple is total and every replica computes it
from the same inputs (the event plus the replicated frontier declarations), so
re-stamping is a deterministic function, not a mutation someone has to
broadcast. Keeping `origin` means "when did the user do this" survives
re-stamping, and a late event's handler still sees its true time.

---

## 4. Replication

- **Exchange.** Each replica keeps a version vector `{replica → max
  counter}`. Sync is: swap vectors, send the events the other side lacks.
  The same scheme serves tabs (BroadcastChannel), devices (via a relay, or
  directly), and a server.
- **Storage.** The adapter keys events by `id`, so a retried or duplicate
  append is a no-op. `PersistenceAdapter.eventsSince(seq)` becomes
  `eventsNotIn(vv)`; snapshots record the version vector they cover rather
  than a cursor.
- **Rebase.** Applying a remote event whose `order` falls before local
  events means: roll back the suffix, apply it, re-run the suffix. Z-sets are
  invertible, so the rollback is **one** step feeding the negated sum of the
  suffix's input deltas — no snapshot restore, no replay from empty. The
  suffix is then re-executed event by event, since its handlers read state.
  Replicas keep per-event input deltas back to the stability frontier to do
  this.
- **Rendering.** The whole rebase (rollback, remote event, re-run suffix)
  is emitted as **one** delta batch, which the shaper already turns into one
  DOM transaction (nesting-draft §4). The DOM never shows the rolled-back
  state, and a remote event that commutes with the suffix nets out to just
  its own change.

---

## 5. Topology, stability, and compaction

**Local-first semantics; the server is an optional replica.** Every replica
computes state itself and applies its own writes optimistically, offline or
not. Correctness never depends on a server. An always-on **relay** replica,
when present, adds services, not authority:

1. durability and device-to-device transport,
2. **frontier declarations** (below),
3. the commit point for `coordinated` handlers (§6).

**Stability.** An event is *stable* once no event can ever be ordered before
it. Proving that needs every replica to have reported past it, which pure
local-first never guarantees: browser profiles are deleted silently and never
report again, and a laptop offline for a month returns with month-old events.

**Frontiers (decision 1).** The relay periodically declares a frontier
`F = (time, version vector)` as a replicated event, once every replica
holding a live **lease** has acknowledged the events it covers. A replica not
seen within the lease period (e.g. 30 days) is evicted from the calculation.
Then:

- Everything inside `F` is final: its rollback deltas are dropped and it can
  be folded into a snapshot. New replicas bootstrap from the latest snapshot
  plus the events after it, not from the whole history.
- An event outside `F` with `origin < F.time` is **late**: it is re-stamped
  to order just after `F` (§3.1), like `git rebase` onto main rather than a
  merge into old history. Its intent is re-checked against current state —
  usually what a returning user expects.
- Without a relay there are no declarations: nothing is compacted, the log
  keeps growing, and every replica still converges. Fine for small apps and
  tabs on one device; compaction is what the relay buys.

Frontier declarations are the one centralised service in this design, and
it is deliberate: convergence needs no coordinator, finality does.

---

## 6. Invariants and the CALM classifier

The CALM theorem: a monotone program needs no coordination. Invariant
confluence (Bailis et al., VLDB 2014) generalises it: a set of operations
can run without coordination iff merging any two valid states preserves the
invariants. Rex's checker sees each handler's relational body and the
schema's declared keys and `rel`s, so it can classify every handler:

| Class | Example | Under concurrency |
|---|---|---|
| **monotone** — only inserts with fresh ids | `AddCard` | commutes with everything; final as soon as applied; never triggers a rebase |
| **keyed overwrite** — writes a functional field | `Rename`, `MoveCard` | last-writer-wins by `order`; confluent for key constraints |
| **order-sensitive** — `delete`, `not`/`except` reads, aggregates compared to a threshold | `DeleteList`, "≤ 5 cards" | may converge to a state that violates a declared invariant |

For each order-sensitive handler, the checker asks whether some pair of
concurrent executions can break a *declared* invariant (a uniqueness key, a
foreign key under concurrent delete, a declared bound).

**Decision 2: such a handler must be marked `coordinated`** (spelling TBD),
or it does not compile. A coordinated event is tentative until the relay
commits it in its order. If it is rejected, or the replica is offline, the UI
shows it as pending or failed. The error message names the invariant and the
concurrent handler that can break it, so the marker is a decision the
developer makes knowingly. Later options, not in v1: a *surface-as-conflict*
mode (converge, and let views show the violation) and escrow (split a
budget such as stock across replicas, the demarcation protocol).

The classifier also drives the UI: monotone and keyed events are final on
apply; order-sensitive ones may still change on sync, and can be styled as
tentative until the frontier passes them.

---

## 7. Device-local state (decision 3)

Some state is about *this device*, not the shared document: which item is
selected, the list filter, which row is being edited, the signed-in user.
It must not replicate: two tabs should not flip each other's filters. It
also must not feed replicated handlers implicitly, or the same event would
do different things on different replicas.

**The `local` keyword marks device-local state.** A `local` relation is
stored and logged on its own replica only (so it still survives reload),
never sent to peers, and never read by a replicated handler: the checker
rejects the read and suggests passing the value as an argument, which
records it in the event. Handlers that only write `local` state are
themselves local events.

This fits what S-62 already built. A component `local` (`local editing =
False` in TodoMVC) is per-instance UI state and should be device-local by
default under sync; it only needs to stop replicating. The new case is
top-level state: `local state filter : Filter = All`. Plain `state` stays
replicated. Today's examples would mostly become `local state`: TodoMVC's
`filter` and chat's `current`. The benchmark's `nextId`/`nextPos` counters
stay replicated. If the keyword ends up redundant in practice (everything
that is `state` is really local), drop it and flip the default.

---

## 8. Trust

Every replica can write any event, so handlers re-run on arrival, which
re-checks any authorisation logic they contain. That is a real advantage of
replicating intents. Timestamps are the new attack surface: a replica could
stamp far into the future to win every last-writer-wins race, or far into the
past to force deep rebases. Mitigations: reject HLC stamps more than a bound
ahead of the local clock; past stamps are bounded by frontiers (late → re-
stamped); sign events with a per-replica key once replicas belong to users.
Access control beyond "whoever holds the document" is out of scope here.

---

## 9. Hazards in the current design

These are things already built or planned that assume one writer. None is
a problem until sync lands; each needs handling when it does.

- **Engine id minting.** `Engine::next_id` is a per-sort counter
  (`dbsp/engine.rs`), so ids collide across replicas and change under
  rebase. §3 replaces it.
- **App-level counters as ids.** `state nextId` (js-framework-benchmark)
  re-executes deterministically, but any id derived from it changes when a
  rebase re-runs the event, breaking later events that carry the old value
  in their args. The classifier should flag a replicated counter read that
  flows into a key.
- **`order by id` means creation order.** TodoMVC relies on it, and S-70
  sorts `#sort:seq` numerically. With replica-scoped ids that is no longer
  creation order. Keep the id ordering stable (e.g. by origin time, then
  id), or make "creation order" an explicit origin-time sort.
- **Rebalance vs concurrent inserts.** `@rebalance` (S-71) re-keys the
  children its replica knew about. A concurrent insert keeps a key that was
  computed against the old neighbours and can jump position. The fix is
  S-72's direction: manual order as intents (`move x before y`), which the
  handler resolves against current state, not as client-computed keys.
- **Effect intents keyed by `seq`.** MVP-PLAN §5 decision 2 keys an
  `EffectIntent` row by `(outer seq, call site)`. Key it by event id.
- **Snapshot cursor.** `base_snapshot`'s `cursor` becomes a version vector
  (§4).

---

## 10. Decisions (2026-09-29)

1. **Late events are re-stamped** after the frontier (§5), with the
   original time kept as `origin` (bitemporal, §3.1).
2. **Invariants that are not confluent require `coordinated`**; there is no
   surface-as-conflict mode in v1 (§6).
3. **`local` marks device-local state**, explicitly; revisit once it has
   been used (§7).

---

## 11. Plan

Each layer is useful on its own.

- **L0: identity.** Event `id = (replica, counter)` + HLC `origin`; ids
  minted from the event id; adapter keyed by id with `eventsNotIn(vv)`;
  tabs exchange events over BroadcastChannel and apply them in `order`
  (full replay on an out-of-order arrival is acceptable at this layer).
  *Acceptance:* two tabs edit concurrently; both converge; no event lost.
  This replaces the two-tab stopgap.
- **L1: incremental rebase.** Per-event input deltas since the frontier,
  one-step rollback via negation, suffix re-execution, one shaper batch.
  *Acceptance:* property test — N replicas, random handlers, random delivery
  orders and duplications → identical base tables and views; rebase of a
  k-event suffix is O(k) handler runs plus one rollback step.
- **L2: `local` and the classifier.** `local state`; component locals stop
  replicating; checker rejects replicated reads of local state; handler
  classes; `coordinated` required for non-confluent handlers against
  declared invariants.
- **L3: relay.** Transport, leases, frontier declarations, compaction,
  late-event re-stamping, `coordinated` commit, finality for irreversible
  effects. Runs the same engine natively (the reason MVP kept Rust).

---

## 12. Open questions

- Spellings: the event's time inside a handler, `coordinated`, `local
  state`.
- Lease length and what a replica sees when it returns after eviction.
- Whether declared bounds ("≤ 5 cards") are part of the schema language or
  only inferred from `where` clauses; the classifier needs them declared.
- Schema/program evolution: `programKey` today starts a fresh log when the
  program changes, which is unacceptable once logs are shared.
- Text: concurrent edits to one `Text` field are last-writer-wins; rich
  collaborative text needs a sequence CRDT and is out of scope.

## References

- Hellerstein & Alvaro, *Keeping CALM: When Distributed Consistency Is Easy* (CACM 2020).
- Bailis et al., *Coordination Avoidance in Database Systems* (VLDB 2014) — invariant confluence.
- Kulkarni et al., *Logical Physical Clocks* (2014) — hybrid logical clocks.
- Lamport, *Time, Clocks, and the Ordering of Events* (1978).
- Shapiro et al., *Conflict-free Replicated Data Types* (2011).
- Budiu et al., *DBSP: Automatic Incremental View Maintenance* (VLDB 2023); McSherry et al., *Differential Dataflow* (CIDR 2013).
- Kleppmann et al., *Local-first software* (Onward! 2019).
- Barbará-Milló & Garcia-Molina, the demarcation protocol (1994) — escrow.
- Replicache's rebase-on-server model; Automerge's sync protocol.
