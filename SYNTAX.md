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
entity Like { msg: Message, user: User, key (msg, user) }   // a key: at most one row per pair

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

### Keys

```rex
entity User { name: Text, age: Int, key (name) }
entity Like { msg: Message, user: User, key (msg, user) }
```

`key (f, …)` is a constraint on the state: **no two live rows agree on all of
the key's fields**. It is enforced where state changes —

- a `new` whose key a live row already holds **rejects the event** (§3), with
  the reason "a `Like` with this `msg` and `user` already exists". The whole
  transaction is checked in statement order: two `new`s with one key in one
  event collide, and a row deleted by an earlier statement has given its key
  up (`delete Like where …; new Like { … }` re-creates it);
- every `new` of a keyed entity must give the whole key, and a key field
  cannot be assigned afterwards (re-keying a row is `delete` and `new`);
- two seed rows with one key are a compile error; a snapshot or a log that
  breaks a key is not loaded.

A key says a duplicate *cannot exist*; what an event does about one is the
handler's business — reject (the default), or test first and do something
else: `if (Like where .msg = m & .user = u) { delete … } else { new … }` is a
toggle. A row still has its id: the key is a constraint, not the row's
identity, and views and handlers refer to rows as before. An entity has at
most one key. `key` is a word only here, before a `(` — a field may be called
`key`.

## 2. State

```rex
state filter  : Filter = All
state nextId  : Int = 1
state current : User          // no default: starts EMPTY (0 or 1 rows)
```

A default is a literal, a constructor, or a seed row named by a top-level
`let` (`let ada = new User { … }` … `state current : User = ada`).

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
| `if (c) { … } else { … }` | run one block or the other; `else` is optional and `else if` chains |
| `reject "reason"` | the event does not apply: nothing it wrote is kept, nothing is logged (the reason is optional) |

**Values** are a small expression language evaluated at one row, against the
pre-event snapshot: literals and constructors, the handler's params, field
paths read from a param or the target row (`t.text`, `.label`, `b.owner.name`),
a `state` name, `not`, `+ - / %`, `++`, comparisons (which yield a `Bool`) and
`&`/`|` between `Bool`s: `not .completed`, `.label ++ " !!!"`, `nextId + i`,
`.qty > limit`, `kind = Urgent`. `*`, `if`,
`match` and aggregates are **not implemented** in values (they are in
queries, §4). A value that reads something absent — a field of a row that is
gone, a `state` with no default that was never `set` — **rejects** the event:
nothing is written, nothing is logged. A plain write to a row that is gone is
not an error; it does nothing.

**Guards.** `where (cond)` on the handler's header is the event's
precondition; `else "reason"` says why, for whoever is told:

```rex
on MessageSent(text) where (current & text != "") else "pick a user and type something" =>
  new Message { text: text, sender: current }
on MessageDeleted(msg) where (msg.sender = current) else "only the sender can delete a message" {
  delete Like where .msg = msg
  delete msg
}
on AddCard(l, title) where (not l.card_count >= 5) else "that list is full" =>
  new Card { list: l, title: title }
```

If the guard does not hold in the state **before** the event, the event is
**rejected**: nothing is written and nothing is logged, so it is not replayed
either. The host gets the reason (`{ rejected: "…" }` from `dispatch`; the
default is "the guard of `E` does not hold"). A guard belongs to its handler
wherever it is run from: an event that `do`es a guarded one is rejected with
it, whole.

The condition is a filter (§4) on the handler's params rather than on a row:
comparisons and `Bool`s, joined with `&`, `|` and `not`, over params
(`text != ""`), paths from them (`msg.sender = current`, `n.locked`), `state`s
and any `let` (`l.card_count`, `total < 100`). A name alone asks whether it is
there: `where (current)` — the state has a value — and `where (msg)` — the row
exists. The parentheses are required, as on a view's `if`. It has no row of
its own, so no leading `.field`, and it cannot mention a row the handler
creates.

A condition can also be a **row test**: a target (below), asked only whether
it has any rows. Inside it `.field` is the row under test, as in `delete`:

```rex
on Tag(u, t) where (not (Has where .user = u & .tag = t)) else "already tagged" =>
  new Has { user: u, tag: t }
```

Beside other conjuncts it goes in parentheses, so that its `&`s stay its own:
`where (current & (Has where .tag = t & .user = current))`. Like a target, it
is a maintained view when it names no param and a scan of the entity when it
does.

Absence is false. `msg.sender = current` does not hold when `msg` is gone or
nobody is selected — which is what you want. A count is not absent for an
empty group, though: `l.card_count < 5` holds for a list with no cards,
because every list is a group of `count(Card by .list)` (§4, aggregates).

**`if`** branches inside a body. The event is accepted either way; the
condition picks what it does:

```rex
on ToggleLike(msg) where (current) {
  if (msg.liked_by_me) { delete Like where .msg = msg & .user = current }
  else                 { new Like { msg: msg, user: current } }
}
```

The condition is the same language as a guard and, like every read, sees the
pre-event state: two `if`s in one handler test the same state, whatever the
first one's block wrote. Both blocks are checked; a row named in a block
(`let x = new …`) is that block's own.

**`reject`** is what a guard is made of. It ends the event as rejected,
wherever it is: a handler's writes are collected and applied only if it
finishes, so a `reject` after other statements discards them, and one in a
handler reached by `do` rejects the caller too. The header form is sugar —

```rex
on E(p) where (c) else "r" { body }      ≡      on E(p) { if (c) { body } else { reject "r" } }
```

— and writing it out gives each precondition its own reason:

```rex
on MessageSent(text) {
  if (not current)    { reject "pick a user first" }
  else if (text = "") { reject "type something" }
  new Message { text: text, sender: current }
}
```

(The sugar is `if (c) … else reject`, not `if (not c) reject`: on a row that
does not exist neither `c` nor `not c` holds, and such an event must be
rejected.) Keep the header for the plain "this event requires …"; a statement
straight after a `reject` in the same block is an error, since it cannot run.

**Naming a new row.** `let x = new E { … }` makes `x` the new row's id for
the rest of the handler, so one event can build rows that refer to each other:

```rex
on Reply(to, text) where (current) {
  let m = new Message { text: text, sender: current }
  new Like { msg: to, user: current }
  new Thread { head: to, reply: m }
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
(O(|E|)); a row with nothing where `P` looks does not match. Statements in one handler all read the pre-event snapshot, and their
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
- **Aggregates** — `count`, `sum`, `min`, `max`, `avg` — fold each key's
  image. A fold over nothing is its initial value, where there is a key to
  hold it, and which keys exist is read off the key's **type**:
  `count(Card by .list) : List -> Int` has a row for every live list (`0` for
  an empty one), `count(Todo by .status)` one for every constructor of
  `Status`, `count(Todo by unit)` its one row. So "lists with no cards" is
  `List where cards = 0`. This holds for `count` and `sum`; `min`, `max` and
  `avg` have no initial value and have no row for an empty group. A key of a
  scalar type (`count(Card by .title) : Text -> Int`) has no domain to range
  over: its groups exist only as the data produces them.
- `where p & q`: conjuncts of the same type are one relation, intersected
  (`.sender & current` — the sender *is* the current user); conjuncts of
  different types are each a condition, and all must hold
  (`current & not m.liked_by_me & .pinned`). `E where p & q` and
  `E where (p & q)` are the same rows.
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
An event that is **rejected** (§3 — its guard does not hold, or it reads
something absent) ends the handler there: later `do`s and actions do not run,
so a `clear` does not throw away what was typed. It is an outcome, not an
exception; the generated code notes the reason with `console.info`.
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

1. **Empty groups — settled** (2026-10-01): `count`/`sum` are total over a
   key that is `Unit`, an entity or an enum, and give `0` for an empty group
   (§4). Still open: a default for `min`/`max`/`avg` (`min(…) else d` —
   there is no ∞ in `Int`, `Money` or `Date` to use as one), and for a
   scalar key. Also: a row that other rows still point at after it is
   deleted keeps its group (`cards` for a deleted list with cards left in
   it) — deleting does not cascade, and whether it should is a schema
   question (`rel`, keys), not an aggregate one.
2. **Many-to-many sugar**: `rel Liked(Message, many User)` as a keyed link
   entity with `new Liked(m, u)` / `delete Liked(m, u)`, vs. writing the
   entity and its `key` out as `chat/app.rex` does now. The key (§1) is the
   mechanism either way; open are the spelling (today's `rel R(A, B)` is a
   functional field), whether adding an existing pair is a no-op (a set) or
   a rejection (a constraint), and how a view iterates the pairs.
2a. **Keys, further**: more than one key per entity; changing a key field in
   place; looking a row up by its key in O(log n) (a row test with params
   scans the entity today, though the key's index is already maintained);
   and what deleting a row does to rows that point at it.
3. **Empty state as a field value** (`user: current` when nothing is
   selected): the event is rejected at run time, and `where (current)` says so
   in the program. Should the checker require the guard?
3a. **Guards, further**: showing the reason in the page; error values richer
   than a string. A view shares a guard's condition by naming it — `let
   deletable : Message -> Message = Message where .sender = current`, then
   `where (m.deletable)` on the handler and `if (m.deletable)` in the view —
   which covers every condition over an entity or `Unit`; `can E(args)` would
   only save knowing which `let` goes with which event.
4. **`order by id`** for insertion order relies on id keys sorting by mint
   order — true today, worth stating in SPEC.
5. **Type aliases beyond unions** (`type Money2 = Money`) — not needed yet.
