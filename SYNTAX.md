# Rex surface syntax, v1

**Status:** the reference for what `rex check` / `rex build` accept, as of
2026-10-01 (MVP-PLAN E1–E9 landed). The acceptance programs are written in it:
`examples/todomvc/src/app.rex`, `examples/js-framework-benchmark/src/app.rex`,
`examples/kanban/src/board.rex` and `examples/chat/src/app.rex` all build and
run. The grammar is whatever those programs need, and no more.

**Described here but not implemented** — each is marked where it appears:

- component arguments other than row binders (`Panel("Todo")`);
- `*`, `if`, `match` and aggregates in a handler *value* (§3);
- a write through a path (`b.owner.name := …`);
- `set` of a `state` directly from a DOM handler (a `local` works);
- the query sugar of ROADMAP M6.e — `select {…}` records, `group by`,
  `from E:` blocks — and hidden manual order (`pos: Text` and
  `endOf`/`dropPos` are still visible).

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
entity Like { msg: Message, user: User }             // a many-to-many is a link entity
rel CardList(Card, List)                             // a functional field declared on its own line

type Filter = All | Active | Completed               // named union type
```

- Scalar types: `Text`, `Int`, `Money`, `Date`; atoms `@foo` (anonymous,
  self-denoting); **named union types** `type T = A | B | C`, whose
  constructors are bare names (`set filter = All`, `match filter { All => …
  }`, `.kind in (Food | Toy)`). A constructor names its atom verbatim: `All`
  is `@All`. `Bool` is the built-in `type Bool = True | False`.
- A field whose type is an entity is a relation; access it as `.list`. Write
  the entity name (`list: List`); the sort name `ListID` is accepted too.
- A row need not have every field: `new E { … }` may leave one out, and that
  row simply has no value there (there is no NULL, and no default).
- `Unit` is a built-in sort with exactly one row. `unit : X -> Unit`
  is the constant relation to it, so whole-app values are ordinary relations
  keyed by `Unit`: `count(Todo by unit) : Unit -> Int`.

## 2. State

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

## 3. Events and handlers

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
| `new E { f: e, … }` | insert one row, all fields atomically |
| `let x = new E { f: e, … }` | the same, and `x` names the new row in the statements after it |
| `new E from R as (k, v) { f: e, … }` | one row per tuple of `R`; `k`/`v` name the key and value in the field expressions |
| `update T { f: e, … }` | set fields on every row of the keyset `T` (a binder, or `E [where P]`) |
| `x.f := e` | sugar for `update x { f: e }` |
| `delete T` | retract every row of `T` (`delete t`, `delete Todo where .completed`, `delete Row`) |
| `set s = e` | write a `state` or a component `local` |
| `do E(args)` | run another event's handler **in the same transaction** (only the outer event is logged; the static `do` graph must be acyclic) |

**Values** are a small expression language evaluated at one row, against the
pre-event snapshot: literals and constructors, the handler's params, field
paths read from a param or the target row (`t.text`, `.label`, `b.owner.name`),
a `state` name, `not`, `+ - / %`, `++`, and comparisons (which yield a `Bool`):
`not .completed`, `.label ++ " !!!"`, `nextId + i`, `.qty > limit`. `*`, `if`,
`match` and aggregates are **not implemented** in values (they are in
queries, §4). A value that reads something absent — a field of a row that is
gone, a `state` with no default that was never `set` — makes the dispatch
fail: the event is refused, nothing is written, nothing is logged. A plain
write to a row that is gone is not an error; it does nothing.

**Naming a new row.** `let x = new E { … }` makes `x` the new row's id for
the rest of the handler, so one event can build rows that refer to each other:

```rex
on SeedSynthetic() {
  let alice = new User { name: "Alice" }
  let m1    = new Message { text: "Welcome", sender: alice }
  new Like { msg: m1, user: alice }
  set current = alice
}
```

`x` can be a field value, a `do` argument, a `set` value, or a target
(`x.f := e`, `delete x` — writes compose with the row's creation). It cannot
be *read*: `x.name` is an error, because the row is not in the pre-event
snapshot every read sees; use the value you gave it. For the same reason a
`where` target does not find rows the handler has just created. `x` must not
repeat a param or an earlier binding, and a bulk `new … from` cannot be named.

**Targets.** `update`/`delete` take a param (`delete t`), a whole entity
(`delete Row`), or `E where P`. If `P` mentions no param, the target is a
hidden maintained view and dispatch reads its keys (O(|targets|), no scan); if
it does (`delete Item where .kind = k`), each row of `E` is tested at dispatch
(O(|E|)). Statements in one handler all read the pre-event snapshot, and their
writes to one cell compose in order — the last one wins, and a row deleted by
an earlier statement stays deleted.

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
let liker  : Like -> Text = .user.name
```

The core is SPEC.md's: point-free binary relations, `let name : A -> B`.
On top of it:

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
  the body sits at the implicit **`Unit` level**: one row, so static
  chrome, `if` blocks and `Unit`-keyed binds (`active`) live there.
- **Levels.** `R as x [where …] [order by e [desc]] select <element>` renders
  one element per row of `R`. `R` is an entity or any sub-identity relation
  (`visible : Todo`). `x` is the row binder: a
  **key**, never a relation — it appears as the RHS of a membership
  `where .list = l`, as an event arg, or as the head of a path `x.f`; inside
  its own level `x.f` and `.f` mean the same thing.
- **Membership.** A nested level needs exactly one `where` equating a
  relation of its rows to the enclosing binder (`Card as c where .list = l`);
  other `where`s are plain restrictions.
- **Order.** `order by .pos` sorts in the shaper by the value's type
  (`desc` allowed; ties broken by key); `order by id`
  is insertion order. A `Text` key written by drag handlers is manual order
  (see `endOf`/`dropPos`); hiding those keys is post-MVP.
- **`if (c) { … }`** — children present iff the coreflexive `c` holds at
  this level (`if (total > 0)` at the root, `if (.user = current)` inside a
  level, `if (current)` for "the state `current` has a value"); sugar for
  `X where c select …`, so it mounts/removes like any level.
  Whatever is nested inside comes and goes with it: a `select` under an `if`
  re-appears, with its current rows, when the condition holds again.
- **Source order is DOM order.** Levels, `if`s and static markup that share an
  element stay in the order written, whichever mounts first.

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
  `l.user.name`), a declared name (`active`, `cards`), or a parenthesised
  expression (`(active = 0)`). Coreflexive-valued binds toggle by
  **presence**: `class.selected=(filter = All)`, `checked=(active = 0)`,
  `class.done=.completed`. Value binds decode by type (`Int` renders `3`,
  not `i:3`; a `Money` renders its minor units, `250` for 2.50 — formatting
  is not done for you). A bare name that is not a declared relation is an error.
- Text and binds mix freely as children: `td { .sender.name ":" }` (a bind
  beside other children gets a text node of its own, in its place); a string
  followed by `++` starts a concat bind (`"Current user: " ++ current.name`).
- An element with neither properties nor children needs parens (`hr()`),
  since a bare name is always a bind.
- `by`, `in`, `id`, `not`, `type`, … are keywords and cannot be field names
  (`sender`, not `by`).

### Components

```rex
view TodoItem(t: Todo) =
  local editing = False
  li(class.editing=editing) { … }

view ListBox(l: List) = section { h2 { .title } children }

ul { visible as t select TodoItem(t) }
List as l select ListBox(l) { Card as c where .list = l select … }
```

A `view` with params is a component. A call `Name(args)` expands **inline**
at the call site with the params renamed to the caller's binders, so the
generated code is identical to writing the body in place — a component adds no
level and nothing at run time. `Name(args) { … }` passes a block that lands at
the component's single `children` slot. Arguments are **row binders in scope**,
checked against the declared entity; scalar arguments (`Panel("Todo")`) are
**not implemented**. Components may call components; recursion is an error.

`local` declares per-instance state: a hidden field on the entity of the
component's first param (so `Todo -> Bool` here), absent until first `set`
and read through its default (whose type is inferred, or annotated). It lives
and dies with the row, and `set editing = …` from a DOM handler is a real,
logged event.

## 6. DOM handlers

```rex
on keydown.enter(text = value) { do AddTodo(text); clear }
on click(pos = endOf(c))       { do AddCard(l, pos); focus(c) }
on dblclick                    { set editing = True; focus(.edit) }
on change                      => do ToggleTodo(t)
```

```
on <domEvent>[.<modifier>][( name = extractor, … )] ( { stmt* } | => stmt )
stmt := do E(args) | set local = e | clear | revert | focus(level | .class)
```

A DOM handler is a property of its element. It names the DOM event
(`click`, `change`, `keydown.enter`, `drop`), materialises args through
**extractors**, and runs statements in order: each `do` dispatches one named
event (two `do`s are two sequential, separately logged events); `set` of a
component `local` is sugar for an implicit, logged event (a `state` is set
from an `on` handler: declare an event and `do` it); `clear`, `revert` and
`focus` are presentation actions and see the DOM *after* any preceding `do`.
An event the engine **refuses** (§3) ends the handler there — later `do`s and
actions do not run, so a `clear` does not throw away what was typed — and is
reported as a console warning, not an exception.
A DOM handler may not mutate directly — that is what events are for. It may
reference its own level's binder, any **enclosing** binder, and its params;
param types come from the event signature. `keydown.enter` (and `.escape`,
`.tab`, `.space`, arrows, single characters) runs only for that key.

## 7. Extractors and actions (the DOM-layer vocabulary)

Extractors are how a value comes off the DOM — deliberately *not*
relational, implemented once in `rex-dom`:

| Extractor | Value |
|---|---|
| `value` / `checked` | the target input's value / checkbox state (`Bool`) |
| `drag(E)` | the dragged row's key, typed `E` |
| `dropPos(c, x)` | a fractional key at the pointer among level `c`'s rows, excluding `x` |
| `endOf(c)` | a fresh key after the last row of level `c` |
| `utils.fn(args)` | a JS function from `import js "./utils.js" as utils`; the only escape hatch, and it lives entirely in the DOM layer — its result is an event arg, so the engine stays pure |

Actions: `clear` resets the handler's target input; `revert` puts it back to the last value its bind set (so `Escape` cancels an edit — the blur that follows re-commits the old text); `focus(c)` focuses the
first input of the row the preceding `do` created at level `c`;
`focus(.cls)` focuses a child element of this row.

---

## 8. What desugars to what

| Surface | Core |
|---|---|
| `entity Card { list: List }` | field `list : Card -> List` + identity `Card` |
| `rel R(A, B)` | field `R` on `A` + `let R = A . R` |
| `.f` / `x.f` / `R . S` | compose-join; `.f` composes the ambient row with field `f` |
| `type T = A \| B` ; `Bool` | coproduct of atoms `{@A \| @B}` (a constructor names its atom verbatim); `where .done` ≡ `where .done = True`; `not P` ≡ `id except P` |
| `Unit`, `unit` | built-in one-row sort; `unit : X -> Unit` constant; `count(X by unit)` is total, so it is `0` when empty |
| `state s : T [= d]` | hidden entity `State#` (one genesis row, field `s` seeded iff a default); `s` ≡ `unit . ~(State# . unit) . .s` |
| `match e { p => r, …, _ => d }` | `(id where e = p₁) . r₁ \| … \| (id except ((id where e = p₁) \| …)) . d` |
| `if c then a else b` | `(id where c) . a \| (id except (id where c)) . b` |
| `class.x = e` | hidden gate view `Binder where e` (coreflexive); the driver toggles `x` by row presence, decoding nothing |
| `event E(p: T…)` / `on E(p…) { … }` | an `EventDef`; handler = checked statement list run as ONE transaction |
| `update E where P {…}` (arg-free `P`) | hidden `let on#E#k = E where P`; dispatch reads the keyset |
| `x.f := e` | `update x { f: e }` |
| `new E from R as (k, v) {…}` | one `new` per tuple, `k` ≡ `id`, `v` ≡ `R`, in one transaction |
| `let x = new E {…}` | the `new`, plus `x` bound to its id: the sort's next id, known at dispatch because ids are sequential |
| `do F(args)` in a handler | inline `F`'s body into the same transaction |
| `view main = <element>` | implicit root level over `Unit`; `let main#unit = unit#root` (the point `{unit ↦ unit}`); a bind is `unit#root . e`, a class gate `unit#root where e` |
| `R as x order by e select …` | `let main#x = R`, `let main#x#order = R . e` |
| `Card as c where .list = l …` (nested) | `let …#c = Card . list` (composite membership) |
| `if (c) { … }` in a view | one child level per element in the body, membership `Base where c` (coreflexive: child key = parent key), so it mounts/removes as `c` flips |
| `select` directly under a `Unit` level | membership `E . unit`; no `where` relating it to a parent |
| two `select`s of one entity in a level | second is named `…#entity2`, third `…#entity3` |
| `{ e }` / `attr=e` | attribute view `let …#attr = X . e`; coreflexives bind by presence |
| `Name(args) { … }` | inline expansion with binder substitution; block at the `children` slot |
| `local s = d` in `Name(x: E)` | hidden field `local#Name#s` on `E`, read as `(E . .f) \| ((E except .f) . d)`; `set s = v` is `do local#Name#s#set(x, v)` |
| `on click(p = ex) { do E(a); focus(c) }` | listener: run extractors, `dispatch("E", {…})`, then actions |

## 8a. Limits and name rules

- **Nesting is limited to 128 levels** — parentheses, prefix operators, the
  length of one operator chain (`a | b | c …` is a tree as deep as it is
  long), elements inside elements; an `if`/`match` counts as four. Past it the
  parser says so. Every later pass recurses over the tree, and the stack is
  1 MB where Rex runs; name a sub-expression with `let`, or use a component.
  A `match` may have any number of arms (its union is built balanced).
- **A name is declared once.** Entities, types, constructors, states, `rel`s
  and `let`s share one namespace; views (roots and components) another. An
  entity has each field once, and a `new`/`update` sets each field once. The
  built-in type names (`Int Text Money Date Bool Unit`) cannot be redefined.
  Two exceptions, both deliberate: a `let` may shadow `unit` or a constructor.
- **`.a . name`**: a `.name` hop is a field first. Where the entity has no
  such field but `name` is a relation in scope, it is the join it looks like
  (`.list . titles`). A field of that name still wins; write `(.list) . titles`
  to force the join.
- **A constructor is a literal** wherever an atom is: `.kind in (Food | Toy)`.
- A leading byte-order mark is ignored.

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
3. **Empty state as a field value** (`user: current` when nothing is
   selected): today the dispatch is refused at run time. Should the checker
   warn, or require an `if (current)` guard?
4. **`order by id`** for insertion order relies on id keys sorting by mint
   order — true today, worth stating in SPEC.
5. **Type aliases beyond unions** (`type Money2 = Money`) — not needed yet.
