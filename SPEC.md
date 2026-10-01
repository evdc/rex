# A Point-Free Relational View Language — v1 Design Sketch

*Working notes. Captures decisions reached so far, the open/parked items, and the rationale for the load-bearing choices. Not a spec — a stable artifact to attack next.*

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

desugars to per-entity keyed relations (`Customer:name : CustID → Text`, …) plus a fresh `CustID` ID-sort, plus `Customer : CustID→CustID` (the entity-as-identity, §3.3).

**Creation sugar** populates all fields against one fresh key atomically (never N independent inserts that re-find the key):

```
let alice = new Customer { name: "Alice", region: @west }
let o1    = new Order { customer: alice, placed: 2026-01-15 }
let _     = new Line { order: o1, product: widget, qty: 3 }   // anonymous: no binding needed
```

**`X by Y ≡ ~Y . X`** — the regrouping operator, the language's `GROUP BY`. Names the otherwise-cryptic invert-then-compose idiom: `sum(lineprice by :order.customer)` reads as "regroup lineprice under customer, then sum." Best-in-class ergonomics for the most common aggregation shape, and (unlike SQL's `GROUP BY`) it is an ordinary composable combinator.

### Type-directed field resolution (primary style)

The **idiomatic surface** is `let name : A → B = …`, where the declared domain `A` resolves bare `.field` references: the *leading* `.field` of each chain resolves in `A`, and each subsequent `.field` resolves against the running type at that point in the composition (so `.product.name` resolves `.product` in `Line`, then `.name` in `Product`). This is bidirectional type-checking, **not** macro substitution — the elaborator threads types through the chain. A leading `.` therefore means "field resolved by the left-hand type *here*," which can shift mid-expression.

`from E:` blocks (set an ambient *entry* entity for several single-entity definitions) are retained as **secondary** sugar only. They read well for one-entity blocks but fight multi-entity expressions (an aggregation that re-keys crosses the block boundary, forcing awkward nesting), so they are the exception, not the backbone. The annotation style is primary because the signature `A→B` is information you want anyway (documentation + type + resolution seed in one).

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

Examples: `.`, `[]`, `~`, `+union` — monotone, linear. `distinct`, `except`, aggregation — non-monotone (and non-linear). Built-in `<`, `+arith` — monotone; infinite (need grounding).

---

## 8. Recursion — IMPLEMENTED

One combinator `fix : (Rel → Rel) → Rel` with **least-fixed-point** semantics (Kleene iteration from the empty Z-set, à la Feldera's `WITH RECURSIVE`). A recursive view is an equation `X = F(X)`; iterate to stability; **forced `distinct` at the knot** guarantees termination (this also pins recursion firmly in the non-linear/expensive tier — a fixpoint is never "free" like a filter).

`fix` is well-typed only over the **monotone fragment** of the algebra: `F` may not use `distinct`, aggregation, `except`/negation below the recursive occurrence. This **monotonicity partition is the stratification check**, expressed *structurally on the combinator tree* (cleaner than Datalog's predicate-dependency-graph formulation — a dividend of the binary-relational core). Implemented in `src/types/strat.rs`, consuming the §7 `monotone` bits.

**Decisions made at implementation time:**

- **Surface form is `let recursive`, not an expression-level `fix`.** The binding name *is* the self-reference (`let recursive path : Node → Node = edge | edge . path`); an expression `fix` would need a binder form the point-free language otherwise lacks. `fix` exists only in the elaborated form: a recursion group is one `TStmt::LetRec`, with recursive occurrences as `RecVar` nodes.
- **Consecutive `let recursive` statements form one fixpoint group** (mutual recursion); any other statement ends the group. Each body may reference every member of its group. Semantics is the joint least fixpoint.
- **Type annotations are mandatory on recursive lets** — the declared type seeds the self-reference before the body is checked.
- **DBSP lowering** (`Circuit::fixes`): the outer arena stays strictly topological; the cycle lives in a nested inner circuit wrapped Enter/Exit-style. Enter (δ₀) presents each import's full current value at iteration 0; the per-member feedback slot is the z⁻¹ edge, fed `distinct(body) − integrated-so-far` each pass (inner integrals persisting across iterations = semi-naive evaluation); Exit diffs the converged fixpoint against the previous outer step's, so retractions are correct by construction. Cost: O(closure) per outer step touching the region — the recompute tier, honestly. Fully incremental nested deltas (an insert costing only newly derivable facts) are future work.

---

## 9. Static analysis (the heart of the implementation)

A single flow analysis over the combinator graph, accumulating responsibilities:

1. **Groundedness** (Rel-style): a variable is grounded only by application to a **finite** relation; infinite relations (`<`, `+`) ground their result only when enough arguments are already grounded. Evaluation is rejected if any variable is ungrounded. Sound but **incomplete** — some terminating programs get rejected (the accepted tax, same shape as a borrow checker). *Required in v1.*
2. **Set-ness**: track which relations are already sets so redundant `distinct` can be dropped. *Optimization; v1-optional.*
3. **Linearity**: per-operator, drives the incremental cost model surfaced to users. *v1 should at least record it.*
4. **Stratification / monotonicity**: live since `fix` landed — a structural walk over each recursion group's bodies (`src/types/strat.rs`) rejecting non-monotone operators above a recursive occurrence.

---

## 10. Execution & cost model

Lowers to **DBSP circuits**; the point-free program *is* the circuit (boxes = relations, wires = composition). Incremental view maintenance is therefore essentially free.

- **Linear** combinators (`.`, `[]`, `~`, union, project) incrementalize at O(Δ).
- **Non-linear** ones (`distinct`, aggregation, `except`, recursion) are the expensive tier.
- **Open problem to surface (§11):** beyond per-operator linearity, **delta fan-out amplification** — e.g. a per-customer average means one new order flips the pass/fail of *every* sibling order, producing O(orders-per-customer) output deltas. This cost is *invisible* in tidy composition syntax (`cust . avg` looks as cheap as `cust . name`). The linearity bit does not capture it. Decide how hard to fight this (lint/warning vs. cost-annotation vs. document-only).

---

## 11. Parked / open items

- ~~**Recursion** (`fix` + stratification)~~ — **implemented** (§8); what remains parked is *fully incremental* recursion (nested deltas rather than per-step re-derivation).
- **Row polymorphism / structural interfaces** — the principled, type-safe version of the ECS "query everything with fields A,B" idea. v1 must not foreclose it.
- **Top-N / ranked-filter** — *not* a pure edge concern (it gates downstream data); a first-class non-linear operator, expressible as an aggregation into a "sorted-list-of-length-N" monoid. Parked.
- **Pure sort / ORDER BY** — genuinely an edge/presentation concern (Z-sets are unordered); not incrementally natural. Lives at serialization.
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