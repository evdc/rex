# A Point-Free Relational View Language — v1 Design Sketch

*Working notes. Captures decisions reached so far, the open/parked items, and the rationale for the load-bearing choices. Not a spec — a stable artifact to attack next.*

*Kept current as of October 2026: everything in §§2–10 is implemented unless a paragraph says otherwise, and §14 records the application layer (`Unit`, `state`, events and the log) that the MVP added on top. The surface syntax as accepted today is [`SYNTAX.md`](SYNTAX.md); where an example here predates it (entity fields typed `CustID` rather than `Customer`, `→` for `->`), both spellings are accepted or the difference is typographic.*

---

## 1. Thesis

A point-free language in which programs are **standing views over changing data**. Every program denotes a binary relation; relations are composed like arrows in a category; the whole program lowers to a **DBSP circuit** so that incremental maintenance (inserts, deletes, corrections) is automatic. Built-in operations (`<`, `+`, `=`) are treated as infinite relations.

It is *not* a SQL clone, a Datalog clone, or a Prolog. The closest relatives: Tarski's relation algebra, the Bird–de Moor *Algebra of Programming* (fork/converse), RelationalAI's **Rel** (groundedness), and **DBSP/Feldera** (Z-sets, incremental maintenance).
(See also Alloy, for syntax inspiration, or Jamie Brandon's Imp at https://www.scattered-thoughts.net/#imp_v1 and linked pages)

The one-sentence identity: *point-free binary-relational view language; values are Z-sets with retractions; built-ins are infinite relations grounded by finite application; a single static analysis tracks groundedness, set-ness, linearity, and (later) stratification; columnar core, wide rows at the edge; lowered to DBSP.*

---

## 2. Values: Z-sets over a bicartesian value algebra

Core values are **Z-sets**: maps from elements to integer weights. A weight of 1 is "present once"; negative weights encode **retractions** (deletions/corrections); weights > 1 encode multiplicity (bags).

- A *set* is a Z-set with all weights in {0,1}. Set semantics is **not a mode** — it is the `distinct` operator applied where needed.
- This is forced by the DBSP lowering and is *good*: it unifies sets, bags, and deletions into one structure, and makes incremental maintenance natural.

**Value algebra** (the domain elements range over):

```
V ::= Unit | Scalar | ID-sort | (V × V) | (V + V)
```

- Products `(V × V)`: created by the fork combinator; the right column of a relation can be a nested pair.
- Coproducts `(V + V)`: included from v1 even though their main consumer (outer join → `Option`) is parked. **There is no NULL.** "Maybe a value" is `(V + Unit)`. Retrofitting coproducts later is painful; including them now is cheap. This makes the value algebra a **distributive/bicartesian** category.
  *Status:* coproducts of **atoms** are implemented and used throughout (`{@a | @b}`, `type T = A | B`, `Bool`); a general `(V + W)` has no injection or case form yet, so `(V + Unit)` is not expressible. What stands in for "maybe a value" today is the absence of a row (a field a row was never given, a `state` with no default), read through `except`-based defaulting.

### 2.1 Scalars: what the numbers do (decided 2026-09-30)

`Int` and `Money` are 64-bit signed integers (`Money` in minor units). The rules
below are pinned by `crates/rex-core/tests/adversarial.rs`.

- **Arithmetic wraps.** `+ - *`, and every `sum`, are arithmetic in ℤ/2⁶⁴. This
  is not a convenience: a maintained sum is *old + Δ*, and adding and
  subtracting deltas is exact only in a ring. Saturating or trapping
  arithmetic would make the incremental value differ from batch evaluation
  after an overflow and a retraction. It also makes debug, release and wasm
  builds agree.
- **Division and remainder are total**: by zero is `0`; they truncate toward
  zero (`-7 / 2 = -3`, `-7 % 3 = -1`), and `MIN / -1` wraps to `MIN`.
- **Comparison is exact.** `Int` and `Money` compare on a common scale (an
  `Int` is whole units: `Money > 2` reads "more than 2.00") computed without
  overflow, so order and equality are never artefacts of wrapping.
- **`Money * Money` is a type error** (money squared has no unit); one side
  must be an `Int`. `avg` of either numeric image is `Money` — the mean of
  `Int`s `1` and `2` is `1.50` — truncated toward zero to the minor unit.
- **Literals** out of range are errors, not truncations. A date is `yyyy-mm-dd`
  and must be a real calendar date; any other `a-b-c` is subtraction.

---

## 3. Relations and the categorical core

Everything is a **binary relation** `A → B`. Relations form a category: objects are value-algebra types, arrows are relations, composition is `.`, identities are diagonals.

**A "table" is not primitive.** An entity with fields is a family of binary relations sharing a common ID key:

```
Person.name    : PersonID → Text
Person.age     : PersonID → Int
Person.address : PersonID → Address
```

This is **columnar by construction** and maps directly onto vectorized/columnar execution. Wide rows are assembled only at the **edge** (serialization to a client), never required mid-computation.

### 3.1 Core combinators

| Combinator | Notation | Type | Meaning |
|---|---|---|---|
| compose | `R . S` | `(A→B), (B→C) ⟹ A→C` | relational composition; **keeps outer columns** `(A,C)` |
| semijoin | `R[S]` | `(A→B), (B→C) ⟹ A→B` | same join as `.` but **keeps R's columns** `(A,B)` — see §3.2 |
| inverse | `~R` | `(A→B) ⟹ B→A` | converse |
| fork | `R , S` | `(X→Y),(X→Z) ⟹ X→(Y×Z)` | tupling; the only combinator that *builds* nesting |
| union | `R \| S` | Z-set addition | additive; structural; **linear** (spelled `\|`, freeing `+` for arithmetic — surface v1, 2026-09-18) |
| intersect | `R & S` | `(A→B),(A→B) ⟹ A→B` | elementwise `min` of weights; **non-linear** (cost like `distinct`) |
| distinct | `distinct R` | `(A→B) ⟹ A→B` | clamp weights to {0,1}; **non-linear** |
| subtract | `R − S` | Z-set subtraction | **core-only, never user surface** — see §6 (`-` at the surface is arithmetic only; `R - S` on relations is a type error suggesting `except`) |

Constants are nullary columns: `1000 : X → Int` is the everywhere-1000 relation.

Comparisons/predicates desugar to **coreflexives** (sub-identity relations): `=` is the diagonal; `< 4` is `{v ↦ v | v < 4}`. A bare value or atom in filter position *is* its own coreflexive (`@west` means `{@west ↦ @west}`), so `= @west` and `@west` are interchangeable in filter position; comparisons like `> 30` still need their operator. Booleans need not be first-class — they are coreflexive relations.

### 3.2 The `.` / `[]` distinction (semijoin) — resolved

`R . S` and `R[S]` perform the **same join** (R's right column against S's left column); they differ only in **which columns survive**:

- `R . S : A→C` keeps the **outer** columns (projects away the join column).
- `R[S] : A→B` keeps **R's** columns (the semijoin — "filter R, keep R").

This single rule (due to treating entities as their identity relations, §3.3) **eliminates the earlier `where`-vs-`[]` confusion without any type-dispatch magic**: the two forms are distinct, both well-typed, and choosing between them is a meaningful question of *which columns you want to keep*, not a hidden mode. Crucially, the historically-confusable mistake now **fails to typecheck**: e.g. `custspend[custspend . >30]` joins `Money` (R's right) against `CustID` (S's left) → type error, instead of silently doing the wrong thing.

- **To filter on a value and keep the entity:** `Line[.product.name = "Widget"] : LineID→LineID`.
- **To filter on a value and move to the new column:** `Line . (.product.name = "Widget") : LineID→Text`.

`where` is retained only as **optional sugar**: `R where P ≡ R[P]`. It carries no distinct semantics and does no dispatch; it reads as English for the bracket. Use it or not.

### 3.3 Entities *are* their identity relations

`Customer : CustID → CustID` — an entity name denotes the **diagonal/identity relation** on its ID-sort. Consequences:

- "All customers" (a set) and "the identity on CustID" are literally the same object; a *subset* of customers is a *sub-diagonal* coreflexive. This is the set ≅ coreflexive correspondence used throughout.
- `id` is available as the generic identity when no entity name applies; `Customer` is just `id` at `CustID`.
- This identification is what makes §3.2's column-typing precise, so it is a **definition**, not an incidental convenience: filtering an entity (`Customer[.region = @west]`) yields a sub-diagonal `CustID→CustID`, i.e. exactly the set of matching customers.

---

## 4. Surface desugaring: applicative reads, point-free core

Humans write left-to-right applicative; the core stays point-free. **One desugar rule:**

```
A OP B   ≡   (A , B) . OP
```

Operators carry a **relational-vs-functional** tag:

- **Relational** operators (`=`, `<`, `>`, `⊆`, …) yield a **coreflexive** → used to *filter*. For these the desugarer applies the rewrite `(A,B).OP  ⟶  A . OP . ~B`, which avoids materializing the pair. (These two forms are provably equal for sub-identity `OP`; the converse form is the optimization.) Comparing two non-key columns is therefore **not** a missing primitive — it is `A . OP . ~B`. (No `guard₂` needed.)
- **Functional** operators (`+`, `-`, `*`, `/`, `%`, `++` text concat) yield a **value** → desugar to `(A,B) . OP` with `OP : (T×T)→T`.

**Precondition (must be a clear error):** `A OP B` requires both operands **co-keyed** (same left type). Correlated comparisons (e.g. `amount > cust.avg`) require rotating the RHS to the operand key first (via composition) — the rotation is the programmer's/compiler's job, and "operands not co-keyed" is a first-class diagnostic.

### Bindings and comparison: `let` vs `=`

**`let NAME = EXPR`** introduces a binding (a view definition *or* a data reference). Bare **`=`** is the **comparison** relation (the diagonal coreflexive). They never collide: every `=` in expression position is a comparison, every definition is `let`. (Rejected: single `=` for both — comparisons appear mid-expression constantly, so `=` must be theirs.)

```
let lineprice : Line → Money = .qty * .product.price
let widgetline : Line        = id where .product.name = "Widget"
```

(Surface v1, 2026-09-18: field paths are spelled `.f`, not `:f` — `.` is
compose-join everywhere and a field is a relation you join with, so
`t.completed` is literally `t . completed`. `:` now means only type
ascription. See SYNTAX.md.)

### Atoms

Two roles, deliberately kept syntactically distinct (conflating them was a real wart):

- **Scalar atoms `@foo`** — interned, self-denoting constants (cf. Lisp/Ruby symbols). Type is the singleton `{@foo}`. Used for enums/tags/coproduct discriminants: `region : CustID → {@north | @south | @east | @west}`. A scalar atom in filter position is its own coreflexive.
- **Entity-id bindings** — `let alice = new Customer { … }` binds `alice` to a freshly generated ID. This is a *bound name*, not a self-denoting literal, so it uses ordinary `let`/identifier syntax — visually distinct from `@foo`. First-use in `new E` fixes its sort (`alice : CustID`), so a later misuse at another entity is a **type error**, preserving the ID-sort discipline (§4).

### Entity definition, creation, and the `by` regrouping operator

```
entity Customer { name: Text, region: {@north | @south | @east | @west} }
entity Order    { customer: CustID, placed: Date }   // FK = a field valued in another ID-sort
entity Line     { order: OrderID, product: ProductID, qty: Int }
```

desugars to per-entity keyed relations (`Customer.name : CustID → Text`, …) plus a fresh `CustID` ID-sort, plus `Customer : CustID→CustID` (the entity-as-identity, §3.3).

**Creation sugar** populates all fields against one fresh key atomically (never N independent inserts that re-find the key):

```
let alice = new Customer { name: "Alice", region: @west }
let o1    = new Order { customer: alice, placed: 2026-01-15 }
let _     = new Line { order: o1, product: widget, qty: 3 }   // anonymous: no binding needed
```

**`X by Y ≡ ~Y . X`** — the regrouping operator, the language's `GROUP BY`. Names the otherwise-cryptic invert-then-compose idiom: `sum(lineprice by .order.customer)` reads as "regroup lineprice under customer, then sum." Best-in-class ergonomics for the most common aggregation shape, and (unlike SQL's `GROUP BY`) it is an ordinary composable combinator.

### Type-directed field resolution (primary style)

The **idiomatic surface** is `let name : A → B = …`, where the declared domain `A` resolves bare `.field` references: the *leading* `.field` of each chain resolves in `A`, and each subsequent `.field` resolves against the running type at that point in the composition (so `.product.name` resolves `.product` in `Line`, then `.name` in `Product`). This is bidirectional type-checking, **not** macro substitution — the elaborator threads types through the chain. A leading `.` therefore means "field resolved by the left-hand type *here*," which can shift mid-expression.

`from E:` blocks (set an ambient *entry* entity for several single-entity definitions) are retained as **secondary** sugar only (*not implemented*). They read well for one-entity blocks but fight multi-entity expressions (an aggregation that re-keys crosses the block boundary, forcing awkward nesting), so they are the exception, not the backbone. The annotation style is primary because the signature `A→B` is information you want anyway (documentation + type + resolution seed in one).

**Decided: keyed-per-entity, not universal-relation.** The "everything that has a name" (ECS/archetype) model is rejected for v1 because (a) it forces all entity IDs into one sort, destroying static entity-typing; (b) it taxes *every join* to save on *one-time declaration* — backwards, since per-entity keying is exactly what makes group-by collapse into composition (§5). The legitimate expressiveness it gestured at ("query everything with fields A and B") is a **polymorphism** concern, properly served later by **row types / structural interfaces** over per-entity relations. (Parked; v1 must not foreclose it.)

---

## 5. Aggregation: homomorphism over a relation's image

Aggregation is **not** a fold (folds imply order; Z-sets are unordered and weighted). It is a **homomorphism from the image Z-set into a commutative monoid**, optionally a group:

```
agg( per_elem : A → M , M : commutative monoid )   :   (K → A)  ⟹  (K → M)
```

"Group by K" is just **keying by K** — there is no special group-by construct. The image of a customer's order amounts is `~cust . amount : CustID → Money`; summing is `agg_sum` over it. This is the design's happy surprise: group-by and correlated-subquery shapes collapse into composition and are *cleaner* than SQL's bolted-on `GROUP BY`.

**The monoid's algebra carries the cost model:**

- If `M` is a **group** (has inverse): SUM, COUNT, AVG (as (sum,count)) → cheap O(Δ) incremental maintenance, handles retractions by adding negative terms.
- If `M` is only a **monoid** (no inverse): MIN, MAX → cannot cheaply undo a retraction → maintained by recompute, O(DB). 

The presence/absence of an inverse *is* the line between "maintainable" and "recompute." Surface this to users.

**Per-aggregate weight semantics** must be declared: SUM wants weighted (multiplicity-respecting) image; COUNT-DISTINCT wants `distinct` first. The aggregation construct takes this as a parameter.

**Empty groups and total groups (decided S-50; extended 2026-10-01).** An aggregate is a fold, and a fold over nothing is its initial value — but only where there is a key to hold it. Which keys exist whatever the data holds is decided by the **type of the key** (the image's domain): the one `Unit` point (§14); the **live rows of an entity**; the **constructors of an enum** (an atom coproduct, `Bool` included). Over such a key `count` and `sum` are *total*: an empty group yields the monoid identity `0` (`Money` for a `Money` sum), a key's row appears when the key does and returns to `0`, not to absent, when its image empties. So "customers with zero orders" is `Customer where orders = 0`, and `count < n` admits the empty group. The rule is about the image's type, not its syntax: `count(Card by .list)` and `count(~(Card . .list))` are the same total relation. Three limits. (1) `min`/`max` have no identity in `Int`, `Money` or `Date` (a sentinel such as `i64::MAX` would print, compare and overflow as a number) and `avg` is not a monoid: they have no row for an empty group; an explicit `else d` default is the open form. (2) A scalar key (`Int`, `Text`, `Date`, a product) has no enumerable domain; its groups exist only as the image produces them. (3) Presence is "the group is non-empty **or** the key is live": rows that still point at a deleted row keep that key's group — deletion does not cascade.

**As implemented:** `sum`/`count`/`avg` fold deltas into per-key `(sum, count)` state (O(Δ)); an entity-keyed aggregate also reads that entity's identity input, so a row arriving or leaving visits its key (state is one entry per live key rather than per non-empty group); `min`/`max` re-fold the affected group from its integral on any change to it (O(group), not O(DB)). The scalar rules — wrapping, `avg`'s result type — are §2.1.

---

## 6. Negation / anti-join (stress-tested)

Anti-join needs **no new core primitive**:

```
antijoin(A, B)  ≡  A − A[B]
```

Because `A[B] ⊆ A` with matching weights, subtracting the **semijoin** (not the raw image) yields the non-matching part with **correct non-negative weights**. This is *why* `restrict []` is promoted to load-bearing core: without it, naive set difference corrupts weights (a customer with 3 orders would get weight `1 − 3 = −2`).

**Surface/core split:** raw `−` (Z-set subtract) is **core-only**, never a user surface operator — exposing it invites weight-corruption footguns. Users get `except` / `antijoin` (internally `A − A[B]`, always safe). 

`antijoin`/`except` is **anti-monotone** → carries the non-monotone bit → may not appear below `fix` except across a stratum boundary (see §8).

---

## 7. The operator metadata table

Each operator/combinator carries four bits that together encode much of the semantics:

| Property | Values | Drives |
|---|---|---|
| **arity / kind** | relational (coreflexive-valued) vs functional (value-valued) | desugar form (§4) |
| **monotone** | yes / no | legality below `fix` (§8) |
| **linear** | yes / no | incremental cost model |
| **groundedness** | finite / infinite | evaluation safety (§9) |

Examples: `.`, `[]`, `~`, `|` union — monotone, linear. `distinct`, `except`, aggregation — non-monotone (and non-linear). Built-in `<`, arithmetic `+` — monotone; infinite (need grounding).

*As implemented* (`crates/rex-core/src/operator.rs`): the table exists and a test keeps it in step with the combinators. `monotone` is read by the stratification check (§8) and `groundedness` by the grounding pass (§9); `linear` is recorded and has no consumer yet.

---

## 8. Recursion — IMPLEMENTED

One combinator `fix : (Rel → Rel) → Rel` with **least-fixed-point** semantics (Kleene iteration from the empty Z-set, à la Feldera's `WITH RECURSIVE`). A recursive view is an equation `X = F(X)`; iterate to stability; **forced `distinct` at the knot** guarantees termination (this also pins recursion firmly in the non-linear/expensive tier — a fixpoint is never "free" like a filter).

`fix` is well-typed only over the **monotone fragment** of the algebra: `F` may not use `distinct`, aggregation, `except`/negation below the recursive occurrence. This **monotonicity partition is the stratification check**, expressed *structurally on the combinator tree* (cleaner than Datalog's predicate-dependency-graph formulation — a dividend of the binary-relational core). Implemented in `crates/rex-core/src/types/strat.rs`, consuming the §7 `monotone` bits.

**Decisions made at implementation time:**

- **Surface form is `let recursive`, not an expression-level `fix`.** The binding name *is* the self-reference (`let recursive path : Node → Node = edge | edge . path`); an expression `fix` would need a binder form the point-free language otherwise lacks. `fix` exists only in the elaborated form: a recursion group is one `TStmt::LetRec`, with recursive occurrences as `RecVar` nodes.
- **Consecutive `let recursive` statements form one fixpoint group** (mutual recursion); any other statement ends the group. Each body may reference every member of its group. Semantics is the joint least fixpoint.
- **Type annotations are mandatory on recursive lets** — the declared type seeds the self-reference before the body is checked.
- **DBSP lowering** (`Circuit::fixes`): the outer arena stays strictly topological; the cycle lives in a nested inner circuit wrapped Enter/Exit-style. Enter (δ₀) presents each import's full current value at iteration 0; the per-member feedback slot is the z⁻¹ edge, fed `distinct(body) − integrated-so-far` each pass (inner integrals persisting across iterations = semi-naive evaluation); Exit diffs the converged fixpoint against the previous outer step's, so retractions are correct by construction. Cost: O(closure) per outer step touching the region — the recompute tier, honestly. Fully incremental nested deltas (an insert costing only newly derivable facts) are future work.

---

## 9. Static analysis (the heart of the implementation)

A single flow analysis over the combinator graph, accumulating responsibilities:

1. **Groundedness** (Rel-style): a variable is grounded only by application to a **finite** relation; infinite relations (`<`, `+`) ground their result only when enough arguments are already grounded. Evaluation is rejected if any variable is ungrounded. Sound but **incomplete** — some terminating programs get rejected (the accepted tax, same shape as a borrow checker). *Implemented* (`types/ground.rs`): a literal or comparison with no enumerable domain is a check-time error.
2. **Set-ness**: track which relations are already sets so redundant `distinct` can be dropped. *Not implemented as a checker analysis.* Lowering does prove narrower facts where it needs them — which relations are functional, and which are keyed by one entity's ids — to pick rewrites and dense column storage (`dbsp/props.rs`).
3. **Linearity**: per-operator, drives the incremental cost model surfaced to users. *Recorded (§7), not yet surfaced.* The one cost the checker does report is the composite cliff: aggregating or comparing a pair opaquely is a warning.
4. **Stratification / monotonicity**: live since `fix` landed — a structural walk over each recursion group's bodies (`types/strat.rs`) rejecting non-monotone operators above a recursive occurrence.

---

## 10. Execution & cost model

Lowers to **DBSP circuits**; the point-free program *is* the circuit (boxes = relations, wires = composition). Incremental view maintenance is therefore essentially free.

*As implemented* (`crates/rex-core/src/dbsp/`): one node per combinator, in topological order; a `step` pushes a transaction's base-table deltas through every node's delta rule against one frozen snapshot, so all views change together. Lowering applies algebraic rewrites and shares equal nodes; only nodes whose rule needs their history keep an integral. **The batch interpreter defines the semantics**: for every operator, and for whole programs under random histories, the circuit's integrated output must equal batch evaluation over the same base tables (`tests/dbsp.rs`, `histories.rs`, `gen_queries.rs`).

- **Linear** combinators (`.`, `[]`, `~`, union, project) incrementalize at O(Δ).
- **Non-linear** ones (`distinct`, aggregation, `except`, recursion) are the expensive tier.
- **Open problem to surface (§11):** beyond per-operator linearity, **delta fan-out amplification** — e.g. a per-customer average means one new order flips the pass/fail of *every* sibling order, producing O(orders-per-customer) output deltas. This cost is *invisible* in tidy composition syntax (`cust . avg` looks as cheap as `cust . name`). The linearity bit does not capture it. Decide how hard to fight this (lint/warning vs. cost-annotation vs. document-only).

---

## 11. Parked / open items

- ~~**Recursion** (`fix` + stratification)~~ — **implemented** (§8); what remains parked is *fully incremental* recursion (nested deltas rather than per-step re-derivation).
- **Row polymorphism / structural interfaces** — the principled, type-safe version of the ECS "query everything with fields A,B" idea. v1 must not foreclose it.
- **General coproducts at run time** — injections and a case form for `(V + W)` (§2).
- **Per-key aggregate defaults** — an `else`-style form for groups with an empty image (§5).
- **Top-N / ranked-filter** — *not* a pure edge concern (it gates downstream data); a first-class non-linear operator, expressible as an aggregation into a "sorted-list-of-length-N" monoid. Parked.
- **Pure sort / ORDER BY** — genuinely an edge/presentation concern (Z-sets are unordered); not incrementally natural. Lives at serialization: a view's `order by e` designates an order *relation* (row → key) and the shaper sorts by it, by the key's type.
- ~~**The `+` naming collision**~~ — **decided 2026-09-18 (surface v1):** union is `|`, intersect `&` (binding tighter than `|`, as in logic — union of coreflexives is disjunction), difference is `except`; `+ - * / %` are arithmetic only and `++` is text concat. Coproduct types are `{@a | @b}`.
- **Delta fan-out cost surfacing** (§10) — how aggressively to expose.
- **Outer join sugar** — deferred, but its requirement (coproducts in the value algebra) is already satisfied in v1 (§2).

---

## 12. Worked end-to-end example

Schema, data, and a realistically-stacked query (multi-hop join + arithmetic-in-aggregation + group-by + having + enum filter), in final surface syntax.

```
entity Customer { name: Text, region: {@north | @south | @east | @west} }
entity Product  { name: Text, price: Money }
entity Order    { customer: CustID, placed: Date }
entity Line     { order: OrderID, product: ProductID, qty: Int }

let alice  = new Customer { name: "Alice", region: @west }
let bob    = new Customer { name: "Bob",   region: @east }
let widget = new Product  { name: "Widget", price: 9.99 }
let gizmo  = new Product  { name: "Gizmo",  price: 24.50 }
let o1 = new Order { customer: alice, placed: 2026-01-15 }
let o2 = new Order { customer: alice, placed: 2026-02-03 }
let o3 = new Order { customer: bob,   placed: 2026-02-20 }
let _  = new Line { order: o1, product: widget, qty: 3 }
let _  = new Line { order: o1, product: gizmo,  qty: 1 }
let _  = new Line { order: o2, product: widget, qty: 2 }
let _  = new Line { order: o3, product: gizmo,  qty: 5 }

// "West/East customers who spent over 30, with their total spend (qty × price)."
let lineprice : Line → Money     = .qty * .product.price
let custspend : Customer → Money = sum(lineprice by .order.customer)
let inregion  : Customer         = id where .region in (@west | @east)
let result    : Customer → Money = (custspend where > 30)[inregion]
```

> **Correction (reflected in `tests/fixtures/spec12.rex`):** the `result` line as
> written above is ill-typed under the strict §3.2 rule — `R[S]` joins `R`'s
> *right* column (here `Money`) against `S`'s left (`CustID`), which fails to
> typecheck. Restricting by a customer key-set is composition on the shared
> `Customer` key: `let result : Customer → Money = inregion . (custspend where > 30)`.
> This is the `[]`/value-column hazard the stress-test was designed to catch,
> now enforced by the checker.

Four lines of view definitions for what is ~25 lines of SQL (two joins, group-by, having, IN-filter). What the example demonstrated, and the findings it produced, are folded into the relevant sections above:

- multi-hop join (`.order.customer`, `.product.price`) needs no rotation gymnastics — each hop's right column is the next hop's key (§3, §5);
- `by` makes the aggregation regrouping legible (§4);
- the `where` (value-filter, keep entity) vs `[]` (key-semijoin) distinction is used naturally and correctly — `where > 30` filters on value, `[inregion]` restricts by a key-set (§3.2);
- `id where …` is the idiom for "subset of an entity," which is why `id`/entity-as-identity (§3.3) is needed;
- `let` vs `=` keeps `region: @west` (binding) and `.region in (…)`/`> 30` (comparisons) unambiguous (§4).

**Bugs the exercise caught (both instances of the same hazard, now designed out):** reaching for `[]` when the discriminating data is in the *value* column. Under the §3.2 rule this is now a **type error**, not a silent wrong answer — the join columns fail to match. This was the single most valuable result of the whole stress-test sequence.

---

## 13. Stress-test ledger (what's been validated)

| Query shape | Result |
|---|---|
| join + filter + project | clean; the 80% case, no plumbing |
| fork (name + manager's name) | clean; fork justified (real pair data) |
| column-vs-column comparison | `A . OP . ~B`; no new primitive |
| group-by + aggregate + having | **cleaner than SQL**; group-by = keying |
| correlated aggregate (> cust avg) | works via composition; but fan-out cost hidden (§10) |
| anti-join / "never ordered" | `A − A[B]`; promotes `[]` to core; `−` core-only |
| negation + aggregation (non-max) | stratifies trivially when non-recursive |
| outer join (probe) | forces coproducts into value algebra (now included) |
| **end-to-end (multi-hop + agg + having + enum)** | **passed; surfaced & fixed the `[]`/value-filter hazard, now a type error** |

The core held under every seam. Each test sharpened a decision rather than breaking the design.

---

## 14. The application layer: `Unit`, state, events and the log

The core above describes standing views over data. An application also needs somewhere for whole-app values to live, a way for the data to change, and a record of how it changed. The MVP added all three **without adding anything to the core**: each is sugar that desugars to entities, `let`s and base-table transactions before the checker runs (`types/view.rs`), so none of it reaches the typed IR, either evaluator, or the circuit. SYNTAX.md is the surface; this section is the semantics.

### 14.1 `Unit`

`Unit` is a built-in sort with exactly one point, and `unit : X → Unit` is the constant relation to it, grounded by the ambient domain like any constant. Nothing else is needed for "scalars": a whole-app value is a relation keyed by `Unit` (`count(Todo by unit) : Unit → Int`), and a view's static chrome sits at a root level over the one-point relation. `unit` is a built-in *name*, not a reserved word; a `let` may shadow it.

### 14.2 State is a singleton relation

`state s : T [= d]` is a field of one hidden entity, `State#`, with one row. A bare `s` in an expression is `unit . ~(State# . unit) . .s` — to the `Unit` point, back to the one row, to the field — built entirely from operators the engine already maintains. So **a state change is one field delta flowing through joins**, never a recompute: a view that does not read the state does not move when it changes, and one that does changes by exactly the rows whose membership flips. A `state` with no default has no value until `set`; that is the language's "nothing selected".

Per-instance component state (`local`) is the same idea keyed by an entity instead of by `Unit`: a hidden field on that entity, read through its default.

### 14.3 Events are the semantic boundary

- An **event** is `(seq, name, args)`. Args are values, or — for bulk data — a relation `K → V` passed as `(key, value, weight)` rows; there are no list values.
- **Every write is an event.** A handler body is a list of mutations (`new`, `update`, `delete`, `set`, and `do` to inline another handler) that runs as **one transaction**, one `step` of the circuit. Program-setup `new`s are events too (`@genesis`), as is a manual-order key rebalance (`@rebalance`), so the log is complete from an empty engine.
- **Handlers are deterministic functions of (pre-event state, args).** Nothing non-deterministic runs inside one: a timestamp, a random label, a pointer position is computed by the DOM layer and *passed as an argument*, so it is in the log.
- **Reads see the pre-event snapshot; writes compose.** Every target and every value in a handler is evaluated against the state before the event — which is why swapping two rows is two updates, not a double move. Writes to one cell within the transaction compose in statement order: the last wins, and a row deleted earlier in the handler stays deleted.
- **A dispatch has three outcomes.** *Accepted*: its writes are applied and it is logged. *Rejected*: the event does not apply to the state it met — a guard does not hold, or a value reads something absent; nothing is written, nothing is logged, and the caller gets a reason (a string, for now). *Invalid*: the call itself is wrong — no such event, a missing or mistyped argument; an error, and likewise nothing happens. Because nothing rejected is logged, a rejection during replay means the log does not belong to that state, and is an error. (Sync will instead log a rejected event as a recorded no-op, so replicas agree that it was tried — SYNC.md §6.1.)
- **`if` branches; `reject` ends the event; a guard is both.** `if (c) { … } else { … }` in a body picks which statements run. `reject "reason"` makes the outcome *rejected*: a transaction's writes are collected and applied only if the handler completes, so nothing before the `reject` is kept, and a handler reached by `do` rejects its caller, whole. `on E(p) where (c) else "r" { body }` is exactly `if (c) { body } else { reject "r" }` — one mechanism, and no separate guard in the IR. Both conditions are filters at the handler's params and read the pre-event state like every other read: two `if`s in one handler test the same state. Conditions are compiled like targets (below): a conjunct that names no param is a hidden view tested at the `Unit` point; one that names a single entity param is a hidden keyset view tested at that argument; anything else is evaluated at dispatch, where a read of something absent is false. A write to a row that no longer exists is not an error; it does nothing, and it never resurrects the row.
- **A handler can name a row it creates** (`let x = new E { … }`) and use the name in later statements as a value or a target. The id is not minted early: it is *predicted* — the sort's next id plus the `new`s of that sort already in the transaction — which the sequential minting below makes exact. The row itself is not in the pre-event snapshot, so it cannot be read, and a `where` target does not find it.
- **A key is a constraint on the state, not an identity.** `entity E { …, key (f, g) }` says no two live rows of `E` agree on `f` and `g`. A transaction that would break it is *rejected* like any other event that does not apply: its `new`s are checked in order against the pre-event state (through a hidden index view, `key value → row`, maintained like any view) and against the rows the transaction has already created; a row it has already deleted has given its key up. Every row of a keyed entity is created with its whole key and keeps it (a key field cannot be assigned). Rows are still identified by id — in views, in handlers, on the wire — so nothing downstream changes; the key only narrows which states exist. Restore checks it with the rest of the base invariant.
- **Ids are minted per sort, in sequence, and never reused** — deterministic given event order, which is what makes replay exact, and what makes `order by id` insertion order. (Sync will need a replica component in the id — SYNC.md; nothing is reserved for it yet.)

**Targets are views; argument-dependent values are point evaluations.** A `where`-targeted mutation whose predicate mentions no event argument has a static target set, so it is desugared to a hidden maintained view and dispatch reads its keys — O(|targets|), not a scan. A predicate that does mention an argument is tested per row at dispatch. Values are evaluated at one row by a small expression evaluator over the base tables (`events.rs`), not by the relational evaluator: a value needs one row, not a relation.

### 14.4 The log, replay and snapshots

The engine appends each accepted event to an append-only log. **Replaying the log onto an empty engine reproduces the state exactly** — one event per step, since each handler reads the snapshot the previous one left — and can skip collecting deltas when nobody is listening. A **snapshot** is the base tables plus the id counters and the log cursor; views are not stored, they are re-derived on load in one step. A snapshot is only an optimisation: the log alone suffices.

Stored state is not trusted. A snapshot is loaded only if it is a state the program could have reached — every table belongs to a declared sort and field, an identity row is `(id, id)` at weight 1 for a minted id, a field holds at most one well-typed value per live row — and a replayed event's arguments are type-checked as a live dispatch's are. A refused snapshot or log leaves the engine untouched; the runtime then falls back to less (js/rex-runtime).

These properties are tested as properties: random histories replay and restore to the same state as the live engine, against the batch oracle and against hand-written models of what each handler means (`tests/histories.rs`, `models.rs`; `rex-wasm/tests/boundary.rs` for damaged storage).
