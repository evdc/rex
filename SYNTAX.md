# Rex surface syntax, v1 (proposal)

**Status:** S-02 proposal, revised 2026-09-18 after owner review. This is the
surface the MVP acceptance programs are written in:
`examples/todomvc/src/app.rex`, `examples/js-framework-benchmark/src/app.rex`,
`examples/chat/src/app.rex`, `examples/kanban/src/board.rex`. Constructs
marked **(v1)** are not implemented yet (MVP-PLAN.md E1–E5); everything else
is what `rex build` accepts today. The programs are the acceptance tests: the
grammar is whatever they need, and no more.

Nothing here widens the narrow binary core (SPEC.md). Every construct
**desugars** to entities, relations and `let`s; §8 says how.

Three rules decided in review, which everything else follows:

1. **`.` is compose-join, everywhere, and a field is a relation you join
   with.** `.completed` is "the ambient row joined with `completed`";
   `t.completed` is `t . completed`; `.order.customer` is a two-hop join;
   `R . S` between named relations is the same operator. The old `:field`
   prefix is gone; `:` is now only type ascription.
2. **Parens hold an element's own properties; braces hold its children.**
   `input(class="edit" value=.text on blur(v = value) => …)`; `ul { … }`.
3. **Braces are bodies; `=> stmt` is the one-statement shorthand.** Handler
   bodies, DOM handler bodies and `match` arms all use them.

---

## 1. Schema

```rex
entity Todo {
  text: Text            // newline or comma separates fields
  completed: Bool
}
entity Card { title: Text, pos: Text, list: List }   // `list: List` IS a relation Card -> List
entity Like { msg: Message, by: User }               // a many-to-many is a link entity
rel CardList(Card, List)                             // a functional field declared on its own line

type Filter = All | Active | Completed               // (v1) named union type
```

- Scalar types: `Text`, `Int`, `Money`, `Date`; atoms `@foo` (anonymous,
  self-denoting); **named union types** **(v1)** `type T = A | B | C`, whose
  constructors are bare names resolved by the expected type (`set filter =
  All`, `match filter { All => … }`). `Bool` is the built-in
  `type Bool = True | False`.
- A field whose type is an entity is a relation; access it as `.list`. No
  `ID` suffix — write the entity name.
- `Unit` **(v1)** is a built-in sort with exactly one row. `unit : X -> Unit`
  is the constant relation to it, so whole-app values are ordinary relations
  keyed by `Unit`: `count(Todo by unit) : Unit -> Int`.

## 2. State **(v1)**

```rex
state filter  : Filter = All
state nextId  : Int = 1
state current : User          // no default: starts EMPTY (0 or 1 rows)
```

A `state` is a hidden entity (`State#`) with one row. A bare `filter` in an
expression means `unit . ~(State# . unit) . .filter` — a relation `X -> V`
from whatever the ambient domain is, built only from operators the engine
already maintains. A state with no default is the empty singleton until `set`, which is how
"no current user" is expressed without an option type. Changing state is one
field delta that flows through joins; nothing is recomputed from scratch.

## 3. Events and handlers **(v1)**

```rex
event AddTodo(text: Text)
event ToggleTodo(t: Todo)
event Run(n: Int, labels: Int -> Text)   // a relation-valued param

on ToggleTodo(t) => t.completed := not t.completed
on Run(n, labels) {
  delete Row
  new Row from labels as (i, label) { num: nextId + i, label: label, pos: i + 1, selected: False }
  set nextId = nextId + n
}
```

**Events are the only way state changes.** One event = one appended log
entry = one atomic engine transaction. A handler body is a list of
statements (newline or `;` separated); every read — targets *and* values —
sees the **pre-event snapshot**, which is why `SwapRows` in the benchmark is
two updates and not a double move. Params are id-typed (`t: Todo`), scalar,
union-typed, or relation-typed (`labels: Int -> Text`, crossing the boundary
as `[key, value, weight]` tuples — bulk data without list values).

Statements, one spelling per verb, verb first, target an expression:

| Statement | Meaning |
|---|---|
| `new E { f: e, … }` | insert one row, all fields atomically; `let x = new …` binds its id |
| `new E from R as (k, v) { f: e, … }` | one row per tuple of `R`; `k`/`v` name the key and value in the field expressions |
| `update T { f: e, … }` | set fields on every row of the keyset `T` (a binder, or `E [where P]`) |
| `x.f := e` | sugar for `update x { f: e }` |
| `delete T` | retract every row of `T` (`delete t`, `delete Todo where .completed`, `delete Row`) |
| `set s = e` | write a `state` or a component `local` |
| `do E(args)` | run another event's handler **in the same transaction** (only the outer event is logged; the static `do` graph must be acyclic) |

Values are ordinary Rex expressions with the target row as ambient domain:
`not .completed`, `.label ++ " !!!"`, `nextId + i`, `current`. An arg-free
`where P` target is a hidden maintained view (O(1) at dispatch); an
arg-dependent one is evaluated per event and the checker says so. A value
that may be empty (a defaultless state) writes no row for that field, and
the checker warns.

Nothing non-deterministic runs in a handler. Random labels, timestamps and
DOM geometry are computed client-side and passed as event args (§7), so
replaying the log reproduces the state exactly.

## 4. Queries

```rex
let visible : Todo = match filter {
  All       => Todo
  Active    => Todo where not .completed
  Completed => Todo where .completed
}
let active : Unit -> Int  = count((Todo where not .completed) by unit)
let cards  : List -> Int  = count(Card by .list)
let liker  : Like -> Text = .by.name
```

Unchanged core (SPEC.md): point-free binary relations, `let name : A -> B`.
New in v1:

- `match e { pat => rel, … }` — each arm is a relation; the whole is the
  union of arms gated by `e = pat`; `_` matches the rest.
- `not P` in filter position is the complement within the ambient entity
  (`E except (E where P)`); a `Bool`-valued path in filter position
  (`where .completed`) means `where .completed = True`.
- `if c then a else b` as an expression = `(c . a) | (not c . b)`.
- Operators: `|` union, `&` intersect (binding tighter than `|`, as in
  logic), `except` difference; `+ - * / %`
  arithmetic on co-keyed value columns; `++` text concat; comparisons
  `= != < <= > >=`; `in`. (`R - S` on relations is a type error: use `except`.)

## 5. Views

```rex
view main =
  section(class="todoapp") {
    h1 "todos"
    ul(class="todo-list") {
      visible as t order by id select TodoItem(t)
    }
    span(class="todo-count") { strong { active } " items left" }
  }
```

- **Root level.** `view main` is the mounted root (`rex build` mounts
  `main` into `#app`). If its body is an element rather than a `select`,
  the body sits at the implicit **`Unit` level** **(v1)**: one row, so static
  chrome, `if` blocks and `Unit`-keyed binds (`active`) live there.
- **Levels.** `R as x [where …] [order by e [desc]] select <element>` renders
  one element per row of `R`. `R` is an entity or any sub-identity relation
  (`visible : Todo`) **(v1: derived sources)**. `x` is the row binder: a
  **key**, never a relation — it appears as the RHS of a membership
  `where .list = l`, as an event arg, or as the head of a path `x.f`; inside
  its own level `x.f` and `.f` mean the same thing.
- **Membership.** A nested level needs exactly one `where` equating a
  relation of its rows to the enclosing binder (`Card as c where .list = l`);
  other `where`s are plain restrictions.
- **Order.** `order by .pos` sorts in the shaper by the value's type
  (`desc` allowed; ties broken by key) **(v1: typed, `desc`)**; `order by id`
  is insertion order. A `Text` key written by drag handlers is manual order
  (see `endOf`/`dropPos`); hiding those keys is post-MVP.
- **`if (c) { … }`** **(v1)** — children present iff the coreflexive `c`
  holds at this level (`if (total > 0)` at the root, `if (.by = current)`
  inside a level); sugar for `X where c select …`, so it mounts/removes like
  any level.

### Elements

```
tag[( property* )] ["text"] [{ child* }]

property := attr="static" | attr=<bind> | class.name=<bind> | modifier | <dom handler>
child    := element | "text" | <bind> | R as x … select … | if (c) { … } | Name(args) [{ … }]
bind     := .path | x.path | name | ( expr )
```

- `class`, `id`, `type`, `aria-hidden`, … are ordinary attributes (keyword
  and hyphenated names are fine in attribute position); bare `modifier`s
  (`draggable`, `dropTarget`, `autofocus`) are presentation hooks lowered
  to `rex-dom` helpers or plain attributes.
- A **bind** is anything co-keyed with the level: a path (`.text`,
  `l.by.name`), a declared name (`active`, `cards`), or a parenthesised
  expression (`(active = 0)`). Coreflexive-valued binds toggle by
  **presence**: `class.selected=(filter = All)`, `checked=(active = 0)`,
  `class.done=.completed`. Value binds decode by type (`Int` renders `3`,
  not `i:3`) **(v1)**. A bare name that is not a declared relation is an error.
- Text and binds mix freely as children: `td { .sender.name ":" }`; a string
  followed by `++` starts a concat bind (`"Current user: " ++ current.name`).
- An element with neither properties nor children needs parens (`hr()`),
  since a bare name is always a bind.
- `by`, `in`, `id`, `not`, `type`, … are keywords and cannot be field names
  (`sender`, not `by`).

### Components **(v1)**

```rex
view TodoItem(t: Todo) =
  local editing = False
  li(class.editing=editing) { … }

view Panel(title: Text) = section { header { title } children }
Panel("Todo") { … }
```

A `view` with params is a component. A call `Name(args)` expands **inline**
at the call site with the binder substituted, so the shaper sees ordinary
levels (`main#todo#TodoItem`). `Name(args) { … }` passes a block that lands
at the component's single `children` slot. `local` declares per-instance
state: a hidden relation keyed by the instance (`Todo -> Bool`) with a
default (type inferred from it); `set editing = …` writes it.

## 6. DOM handlers

```rex
on keydown.enter(text = value) { do AddTodo(text); clear }
on click(pos = endOf(c))       { do AddCard(l, pos); focus(c) }
on dblclick                    { set editing = True; focus(.edit) }
on change                      => do ToggleTodo(t)
```

```
on <domEvent>[.<modifier>][( name = extractor, … )] ( { stmt* } | => stmt )
stmt := do E(args) | set local = e | clear | focus(level | .class)
```

A DOM handler is a property of its element. It names the DOM event
(`click`, `change`, `keydown.enter`, `drop`), materialises args through
**extractors**, and runs statements in order **(v1)**: each `do` dispatches
one named event (two `do`s are two sequential, separately logged events);
`set` of a `local` or `state` is sugar for an implicit, logged event; `clear`
and `focus` are presentation actions and see the DOM *after* any preceding
`do`. A DOM handler may not mutate directly — that is what events are for.
It may reference its own level's binder, any **enclosing** binder **(v1)**,
and its params; param types come from the event signature.

## 7. Extractors and actions (the DOM-layer vocabulary)

Extractors are how a value comes off the DOM — deliberately *not*
relational, implemented once in `rex-dom`:

| Extractor | Value |
|---|---|
| `value` / `checked` | the target input's value / checkbox state (`Bool`) |
| `drag(E)` | the dragged row's key, typed `E` |
| `dropPos(c, x)` | a fractional key at the pointer among level `c`'s rows, excluding `x` |
| `endOf(c)` | a fresh key after the last row of level `c` |
| `utils.fn(args)` **(v1)** | a JS function from `import js "./utils.js" as utils`; the only escape hatch, and it lives entirely in the DOM layer — its result is an event arg, so the engine stays pure |

Actions: `clear` resets the handler's target input; `focus(c)` focuses the
first input of the row the preceding `do` created at level `c`;
`focus(.cls)` focuses a child element of this row.

---

## 8. What desugars to what

| Surface | Core |
|---|---|
| `entity Card { list: List }` | field `list : Card -> List` + identity `Card` |
| `rel R(A, B)` | field `R` on `A` + `let R = A . R` |
| `.f` / `x.f` / `R . S` | compose-join; `.f` composes the ambient row with field `f` |
| `type T = A \| B` ; `Bool` | coproduct of atoms `{@A \| @B}`; `where .done` ≡ `where .done = True`; `not P` ≡ `except (where P)` |
| `Unit`, `unit` | built-in one-row sort; `unit : X -> Unit` constant |
| `state s : T [= d]` | hidden entity `State#` (one genesis row, field `s` seeded iff a default); `s` ≡ `unit . s` |
| `match e { p => r, … }` | `(e = p₁) . r₁ \| (e = p₂) . r₂ \| …` |
| `if c then a else b` | `(c . a) \| (not c . b)` |
| `event E(p: T…)` / `on E(p…) { … }` | an `EventDef`; handler = checked statement list run as ONE transaction |
| `update E where P {…}` (arg-free `P`) | hidden `let on#E#k = E where P`; dispatch reads the keyset |
| `x.f := e` | `update x { f: e }` |
| `new E from R as (k, v) {…}` | one `new` per tuple, `k` ≡ `id`, `v` ≡ `R`, in one transaction |
| `do F(args)` in a handler | inline `F`'s body into the same transaction |
| `view main = <element>` | implicit root level over `Unit`; `let main#unit = Unit` |
| `R as x order by e select …` | `let main#x = R`, `let main#x#order = R . e` |
| `Card as c where .list = l …` (nested) | `let …#c = Card . list` (composite membership) |
| `if (c) { … }` in a view | a level `X where c select …` |
| `{ e }` / `attr=e` | attribute view `let …#attr = X . e`; coreflexives bind by presence |
| `Name(args) { … }` | inline expansion with binder substitution; block at the `children` slot |
| `local s = d` in `Name(x: E)` | hidden entity keyed by `E` with field `s`, defaulted via `except` |
| `on click(p = ex) { do E(a); focus(c) }` | listener: run extractors, `dispatch("E", {…})`, then actions |

## 9. Open questions

1. **Empty groups**: `count(Card by .list)` has no row for an empty list, so
   the Kanban count text vanishes. **Settled for `by unit` (S-50, landed):**
   grouping by `unit` is a *total* group — its key exists by construction —
   so it yields the monoid identity, `0`, rather than no row. `Count`/`Sum`
   have identities; `min`/`max`/`avg` do not and stay absent. Per-key
   defaults for an ordinary group key still need an `else`-style form
   (`drafts.md`) — not in v1.
2. **Many-to-many sugar**: `rel Liked(Message, User)` as a link entity with
   `new Liked(m, u)` / `delete Liked(m, u)`, vs. writing the entity out as
   `chat/app.rex` does now.
3. **Empty state as a field value** (`by: current` when nothing is
   selected): warn, or reject and require an `if (current)` guard?
4. **`order by id`** for insertion order relies on id keys sorting by mint
   order — true today, worth stating in SPEC.
5. **Type aliases beyond unions** (`type Money2 = Money`) — not needed yet.
