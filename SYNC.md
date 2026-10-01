# Rex sync: many replicas, one meaning

Status: **design, post-MVP** (sync is explicitly out of MVP scope, MVP-PLAN §1).
Written 2026-09-29; revised 2026-09-30 after comparing with Firmament
(`../firmament/DESIGN.md`), a parallel design for running the Rex core as a
server. §13 covers that relationship. Decisions in §10 were made by the owner.

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
concurrently) is **rejected**: every event has exactly one outcome, `accepted`
or `rejected` with a reason (Firmament's `A.accepted` / `A.rejected`), rather
than a silently empty effect. The outcome is data, so views and the UI can
react to it ("your move was dropped"). This is the Replicache / event-sourcing
model, and Rex already has every piece: named events, `replay`, and effects
kept out of handlers (MVP-PLAN §5 decision 2).

### Intents while tentative, facts once final

Re-running handlers is right only for the **tentative** part of the log:
events not yet behind the stability frontier (§5), whose position can still
change. Once an event is final, its decision is **frozen** (Firmament D2): the
replica stores its outcome, its resolved facts (the Z-set delta), its minted
ids, and the `code_version` that produced them, and never re-runs its handler
again. Replay of the final prefix applies facts; replay from args survives as a
test that the two agree.

Without this, fixing a bug in a handler would silently rewrite history that
effects have already acted on, and a changed program could not read an old
log at all. Today `programKey` sidesteps the second problem by starting a
fresh log whenever the program changes, which is impossible once logs are
shared between replicas.

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

Determinism must also hold **below the language**: two replicas running the
same `code_version` must produce identical ids, row order and view
encodings. Firmament's review found places in rex-core where `HashMap`
iteration order or interning order could leak into a transaction (DESIGN
§11, "Known rex issues"). Those are convergence bugs for browser replicas
too, so the audit and a determinism fuzz target belong in rex-core (§11).

**Trusted time.** A handler's time is its event's `origin`, taken from the
creating replica's clock. That is right for "when did the user do this", but
a client can lie about it. Anything that enforces time, such as a deadline
("expire after 24h") or an ordering like "the latest reset", must use time
from the relay's order, not `origin`. A `coordinated` handler (§6) sees the
relay's time. Scheduled work enters as Clock events the relay appends
(Firmament D3, D14), never as a replica reading its own clock.

### 2.1 Effects run once, and irreversible ones wait for finality

If every replica performed the effects of every event it replays, a payment
would be charged once per replica. Rules:

- Each effect has a key: `effect_key = hash(event id, n)` for the event's
  `n`-th effect. The key is the idempotency key passed to the external
  system.
- The **originating replica** normally performs an event's effects, on
  first execution only, never on replay or rebase. Another replica (the relay)
  may retry an effect whose originator disappeared; the effect key makes the
  retry safe.
- The response is a new event with `cause` = the request's **event id**
  (not its `seq`, which is not stable under sync, see §9), and its own id
  is `uuidv5(effect_key)` (Firmament D8). Duplicate responses, from retries
  or from two replicas, then collapse into one event for free.
- A rebase can undo the conditions that made an effect fire, and an email
  cannot be unsent. So **irreversible effects fire only once their
  triggering event is final**: behind the stability frontier (§5) or
  committed by a `coordinated` handler (§6). Reversible or idempotent
  effects (a fetch, a cache fill) may fire optimistically.

---

## 3. Event identity, time, and order

```
Event {
  id:           (replica, counter)  // identity: immutable, unique, seeds ids + random
  origin:       Hlc                 // valid time: when it was created; what the handler sees as "now"
  code_version: Hash                // program source + engine build that must run it
  name, args,
  cause:        Option<EventId>,    // was Option<u64>
  intent:       Option<String>,
}

// Once final (§1), stored alongside the event:
Decision { outcome: Accepted | Rejected(reason), facts: ZSetDelta, minted: [Id] }
```

- **`code_version`** covers the program *and* the engine build (Firmament
  D13). Two tabs running different builds of an app would otherwise re-run
  the same tentative event differently and diverge. A replica that cannot
  run an event's `code_version` does not guess: it holds the event unapplied
  and asks to be upgraded. Final events need no matching version, because
  their facts are replayed, not their handlers.

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
  When the relay is Firmament, ids also need a **home partition** for
  routing (Firmament D7 packs `(partition, seq, i)`). An offline client
  cannot know a `seq`, so the shared scheme should be `(home partition,
  event id, i)`: known before sequencing, and still routable (§13).

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

Firmament defers valid time (D12) because it "breaks the notion of a sealed
frontier". It only does so if valid time takes part in *ordering*. Here
`origin` is metadata a handler reads, `order` is what frontiers seal, and a
late event lands after the frontier, so a sealed frontier stays sealed.

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

- Everything inside `F` is final: its decisions are frozen as facts (§1),
  its rollback deltas are dropped, and it can be folded into a snapshot. New replicas bootstrap from the latest snapshot
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

**The relay can be Firmament.** Firmament's per-partition validator already
provides what the relay needs: its decision log is an authoritative order,
its `decide` frontier is a stability frontier, and it decides guarded Actions
in that order. With Firmament as the relay, clients rebase their tentative
events onto its decisions, and re-stamping (§3.1) happens by itself: a
client event that reaches the appender simply gets the next `seq`, keeping its
`origin`. The hybrid-clock order is then needed only between peers that have
not reached the server yet (offline devices, tabs on one machine) and in
deployments with no server at all. See §13.

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

### 6.1 Declared invariants

The classifier needs something to check against, so invariants are declared,
in the two forms Firmament uses:

- **Guards** (`require`, spelling TBD): preconditions on the state before
  the event, plus its args. A failing guard rejects the event with the
  guard's reason (§1).
- **Invariant views**: a query that must gain no rows from any step ("walkers
  booked beyond capacity", "cards in a deleted list"). A step that adds a
  row is rejected. Invariant views need no new language: they are ordinary
  views with a role.

Keys and functional dependencies in the schema are invariants too.

### 6.2 Coordination by default (decision 2, revised 2026-09-30)

Static invariant-confluence analysis is incomplete: some handlers are safe
but cannot be proven so. A classifier that only *flags dangers* therefore
fails silently exactly where it matters. So the default is safe (Firmament
D10):

**A handler that the classifier cannot prove confluent against every
declared invariant must be marked `coordinated`** (spelling TBD), or it does
not compile. Proofs cover simple sufficient conditions: the handler is
monotone, or it only overwrites keyed fields and no guard or invariant view
reads those fields non-monotonically. The error names the invariant and
the concurrent handler the proof failed against, so the marker is a decision
the developer makes knowingly.

A coordinated event is tentative until the relay decides it in the relay's
order. If the relay rejects it, or the replica is offline, the UI shows it as
pending or failed. Later options, not in v1: a *surface-as-conflict* mode
(converge, and let views show the violation) and escrow (split a budget such
as stock across replicas, the demarcation protocol).

### 6.3 When a local decision is already final

A replica may treat its own tentative decision as final, and show it as
such, only when no later-arriving event can change it. Firmament's rule for
reading stale replicas (DESIGN §5.2) gives the test:

- the handler's guards read only **monotone views, in monotone positions**
  (`exists(...)` over a view that only gains rows). Staleness can then only
  cause a false *rejection*, never a false acceptance; and
- its writes are proven confluent (§6.2).

Such an event is accepted on apply and is never later rejected. That is
Firmament's coordination-free fast path (§5.4) seen from the client, and it
is what makes those events safe to apply offline and submit later.

### 6.4 Availability is chosen per handler

Coordination-free handlers are available under partition (AP): they work
offline and converge. Coordinated handlers are consistent (CP): they need
the relay to finalize. Neither Rex nor Firmament picks one for the whole
system; the classifier picks per handler, and the developer sees the choice
(§12, open question on how it is shown).

The classifier also drives the UI: events final by §6.3 are styled final on
apply; everything else is tentative until the frontier passes it.

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

**Scope is the general concept.** Firmament rejects `state` outright, because
on a server one singleton would be shared by every client, "until per-actor
state is designed" (DESIGN §11). Device-local and per-actor state are the
same idea at different scopes: state keyed by an implicit principal, either
this **device**, this **actor** (the signed-in user, on every device), or
**global** (today's `state`). Design the three scopes once, for both
projects; `local` is the device scope.

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
- **`SortId` is declaration order.** Encoded ids embed it (`#3:7`), so
  reordering entity declarations changes every id in a stored log. Sort ids
  must be stable across program changes (found by Firmament, DESIGN §11).
- **Replay re-runs handlers.** `rex::events::replay` re-derives every event
  from its args. That stays correct for the tentative suffix only; the final
  prefix must replay facts (§1).

---

## 10. Decisions (2026-09-29, revised 2026-09-30)

1. **Late events are re-stamped** after the frontier (§5), with the
   original time kept as `origin` (bitemporal, §3.1).
2. **`coordinated` is required unless the classifier proves the handler
   confluent**; there is no surface-as-conflict mode in v1 (§6.2).
   *Revised 2026-09-30:* originally "required when the classifier finds a
   danger", flipped to the safe default because the classifier is
   incomplete.
3. **`local` marks device-local state**, explicitly; revisit once it has
   been used (§7).

---

## 11. Plan

Each layer is useful on its own.

- **Core: shared rex-core work (do first).** Firmament needs the same
  changes to the core (DESIGN §11, M0-PLAN T1), so they are done once,
  upstream in rex-core, before either project builds on them:
  - ids minted by the caller (from the event id), not by an engine counter;
  - handlers that **return** their delta instead of applying it, with an
    explicit accepted/rejected outcome and guard support;
  - transaction negation (rollback here, undo of a rejected step there);
  - replay from facts, keeping replay from args as a test;
  - the determinism audit (`HashMap` order, interner order) and a
    determinism fuzz target;
  - `SortId`s that stay stable across schema changes, so encoded ids
    (`#3:7`) survive a program change;
  - `code_version` on events.
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
  classes; guards and invariant views; `coordinated` required unless
  proven confluent (§6.2); the final-on-apply test (§6.3).
- **L3: relay.** Transport, leases, frontier declarations, compaction
  (freezing decisions as facts), late-event re-stamping, `coordinated`
  commit, finality for irreversible effects. Runs the same engine natively
  (the reason MVP kept Rust). **Prefer Firmament as this relay** rather than
  building a second server (§13); a small relay of our own is only for
  deployments without Firmament, or for tests.

---

## 12. Open questions

- Spellings: the event's time inside a handler, `coordinated`, `local
  state`.
- Lease length and what a replica sees when it returns after eviction.
- **Annotations vs a derived Plan.** Firmament's principle is that the
  model never says sync or async: coordination is derived by the compiler
  and shown in a *Plan*. `coordinated` is an annotation. A possible middle
  ground: the compiler derives the coordination class, and the developer
  acknowledges it through a checked-in Plan (a diff in review) rather than
  a keyword. "Needs to be online" is a product decision, which arguably
  passes Firmament's admission test for the model; so does `local`, since
  "this is my device's filter" is something a domain expert recognises.
- Guard and invariant-view spelling, and whether they share Firmament's
  surface (Firmament forks the front end; the typed IR is the shared
  interface, so the IR has to carry them either way).
- How a replica asks to be upgraded when it holds events whose
  `code_version` it cannot run (§3), and how long it keeps them.
- Text: concurrent edits to one `Text` field are last-writer-wins; rich
  collaborative text needs a sequence CRDT and is out of scope.

---

## 13. Relationship to Firmament

Firmament (`../firmament`) is a server host for the Rex core: per-partition
appenders assign an order, deterministic validators decide Actions in that
order, and Postgres holds the logs. It shares rex-core and forks the front
end. The two designs answer the same question from opposite ends of the
network.

| | Firmament | This design |
|---|---|---|
| Who orders events | one appender per partition assigns `seq` (a sequencer) | every replica computes the same hybrid-clock order |
| When an event is final | at intake (one fsync) | when a declared frontier passes it |
| Availability | CP: a partition's writes stop if its store is down | AP for confluent handlers; CP for `coordinated` ones (§6.4) |
| What replay applies | facts (D2) | facts once final, intents while tentative (§1) |
| Coordination default | coordinate unless proven free (D10) | the same (§6.2) |
| Time a handler sees | `at` from the appender; the Clock as log records | `origin`; relay time for anything enforced (§2) |
| Valid time | deferred (D12) | kept as metadata, not order (§3.1) |
| Surface | no annotations; a derived Plan | `local` and `coordinated` keywords (§12) |

**Local-first is not "many validators reaching consensus".** Consensus among
validators (Raft, Paxos) still agrees on one order *before* deciding. It is a
replicated sequencer that survives node failure, but an offline client still
cannot finalize anything. Local-first is **speculation ahead of the order**:
each client is a tentative validator whose decisions hold until the
authoritative order arrives. Firmament already speculates in a small way (its
validator decides before the intake commit, made safe by determinism, D13);
local-first moves the same idea to the network edge.

The mappings that do hold:

- **A client is a tentative validator; Firmament is the relay (§5).** Its
  decision log is the authoritative order and its `decide` frontier is the
  stability frontier. Clients rebase onto its decisions. This is Replicache's
  architecture with Firmament as the server.
- **Partitions are ownership.** Data only one device writes (`local` state,
  drafts) is its own ordering domain, trivially totally ordered. Shared data
  either merges by hybrid-clock order (confluent handlers) or goes to its
  owning partition's validator, which is what `coordinated` means. So
  `coordinated` is Firmament's "partition-ordered" coordination class.

### What this design took from Firmament

Frozen decisions and facts replay (§1), `code_version` (§3), accepted/rejected
outcomes (§1), guards and invariant views (§6.1), coordination by default
(§6.2), the monotone stale-read rule (§6.3), effect keys and `uuidv5` result
ids (§2.1), trusted time from the relay and Clock events (§2), and the
determinism audit (§2, §11).

### What Firmament could take from this design

1. **A client.** Firmament's M5 plans "a rex-wasm client subscribing to view
   deltas". This design is that client: tentative events, rebase by negated
   deltas, one shaper batch per rebase.
2. **Offline writes on the fast path.** An Action proven coordination-free is
   "never later rejected" (Firmament §5.4), so a client can apply it offline
   and submit it later without risk (§6.3).
3. **Ids a client can mint offline.** Firmament's D7 ids, `pack(p, seq, i)`,
   need a `seq` from the appender. Firmament already has a client-generated
   UUIDv7 `action_id`; deriving ids from `(home partition, action_id, i)`
   keeps them routable and makes them known before sequencing (§3).
4. **Valid time without breaking frontiers** (§3.1), if D12 is revisited.
5. **Scoped state** (§7) as the design for Firmament's missing per-actor
   state.

### Shared work

The rex-core changes in §11 ("Core") are the concrete point where the
projects converge. Changes to the typed IR need agreement on both sides,
since it is the interface between Firmament's front end and rex-core.

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
- Firmament, `../firmament/DESIGN.md` and `LANGUAGE.md` (2026-09-29).
- Thomson et al., *Calvin: Fast Distributed Transactions for Partitioned Database Systems* (SIGMOD 2012).
