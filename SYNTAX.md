# Rex view syntax

The `view` surface lets you write a whole app's UI as one relational query over
your schema. `rex build app.rex -o app.ts` compiles it to a self-contained
TypeScript module that drives the DOM through `rex-dom`. This file documents the
surface; see `examples/kanban/src/board.rex` for a worked example.

Nothing here widens the narrow core: every construct below **desugars** to
ordinary entities, relations, and `let`s (see `crates/rex-core/src/types/view.rs`).

---

## Schema

```rex
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: List }   // `list` relates Card -> List
```

A field whose type is an entity **is** a relation. `list: List` means "each Card
points at one List"; you access it as `:list` (a field path, `Card -> List`).
There is no `ID` suffix — write the entity name (`List`), not `ListID`.

You can also declare a relation on its own line, which reads better when the
relation deserves its own name or you prefer entities to hold only scalars:

```rex
entity Card { title: Text, pos: Text }
rel CardList(Card, List)             // a named relation Card -> List
```

`rel CardList(Card, List)` is exactly sugar for adding a `CardList: List` field
to `Card`. Reference it by name (`CardList`, `new Card { CardList: l }`), whereas
a field is referenced by path (`:list`). Both forms are equivalent — pick per
relation.

> **Ordering note.** `pos: Text` here holds a *fractional order key* (`a0`, `a1`,
> `aV`, …), not a number — dense insert-between with no renumbering. This is an
> implementation detail slated to be hidden; see `ROADMAP` / the ordering design
> note. Ordering by an intrinsic field (`order by :dueDate`) is a plain sort and
> needs no such key.

---

## `view` and `select`

A `view` is a nested `select`. Each `select` is one nesting level of the UI:

```rex
view board =
  List as l order by :pos select        // level: one <section> per List row
    section.list {
      header { span { :title } }
      Card as c where :list = l order by :pos select   // nested level: cards of this list
        div.card { input value=:title }
    }
```

- **`List as l`** binds each row of `List` to the name `l`. The alias
  disambiguates the *row* (`l`) from the *entity relation* (`List`); a nested
  level's membership then reads `where :list = l`. The `as` clause is optional
  (the binder defaults to the entity name), but aliases are clearer.
- **`order by :pos`** designates the level's order relation; the shaper sorts by
  it. (It does not sort in the engine.)
- **`where :list = l`** on a nested select is the **membership** conjunct: the
  cards whose `:list` points at *this* list row `l`. Exactly one `where` per
  nested level must equate a child relation (`:field` or a named `rel`) to the
  parent binder; other `where`s are ordinary restrictions.

## Elements

```
tag.class1.class2 modifier* attr* handler* { children }   |   tag "static text"
```

- `.class` adds a CSS class; bare `modifier`s are presentation hooks
  (`draggable`, `dropTarget`) that lower to `rex-dom` runtime helpers.
- `attr=value`: a static string, or a `:field` bind (`value=:title` keeps the
  input's value in sync with the `:title` relation; `class.done=:completed`
  toggles a class).
- `{ … }` holds children: nested elements, a nested `select` (a child level),
  static text, or a `:field` text bind (`span { :title }`).

## Handlers — named events

A handler fires on a **named DOM event**: `on click`, `on change`, `on drop`, …
The event name is the DOM event; there is no separate handler name.

```rex
button "+ card" on click(pos: Text = endOf(card)) =>
  new Card { title: "New card", pos: pos, list: l }
```

```rex
input value=:title on change(v: Text = value) => :title := v
```

- **Params** are declared `name: Type = extractor`. Extractors are a fixed
  vocabulary of client-side event projections (how a value comes off the DOM
  event — deliberately *not* relational):
  - `value` — the target input's value
  - `checked` — a checkbox's state (as `@true`/`@false`)
  - `drag("card")` — the dragged row's key
  - `dropPos(card)` — a fractional key at the pointer, excluding `card`
  - `endOf(card)` — a fresh key after the last child
  - `prompt("…")` — a `window.prompt` (temporary)
- **Body** is a `;`-separated list of relational mutations, run as **one atomic
  engine transaction** (all reads see the pre-event snapshot):
  - `:field := expr` or `binder:field := expr` — set a field
  - `new Entity { field: expr, … }` — insert a row
  - `delete binder` — retract a row
- **Scope rule:** a handler may reference only its own level's binder (**self**)
  and its declared params — never an enclosing level's binder. So the Kanban
  drop handler lives on the list level (`section.list`), where `l` is self.

---

## What desugars to what

| Surface | Desugars to |
|---|---|
| `entity Card { list: List }` | field `list : Card -> ListID` |
| `rel CardList(Card, List)` | field `CardList` on `Card` + `let CardList = Card . :CardList` |
| `List as l order by :pos select …` | `let board#list = List`, `let board#list#order = List . :pos` |
| `Card where :list = l …` (nested) | `let board#list#card = Card . :list` (composite membership) |
| `span { :title }` | attribute view `let … = Card . :title` + a text applier |
| `on click(…) => new Card { … }` | a checked handler; one engine transaction per event |

Build with debug tracing (logs each dispatch at the engine boundary):

```
rex build examples/kanban/src/board.rex -o examples/kanban/src/main.ts --debug
```
