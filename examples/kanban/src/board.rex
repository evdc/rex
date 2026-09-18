// The whole Kanban app — data model, named events, and UI — in one
// relational program. The compiler (`rex build`) generates `main.ts` (the
// shape tree, templates, and event wiring) from the `view` below; nothing is
// hand-written per app. DOM handlers never mutate directly: they `do` a
// declared event, whose `on` body is the checked, logged transaction. See
// SYNTAX.md for the grammar.

entity List {
  title: Text
  pos: Text
}
entity Card {
  title: Text
  pos: Text
  list: List
}

let l_todo  = new List { title: "Todo",  pos: "a0" }
let l_doing = new List { title: "Doing", pos: "a1" }
let l_done  = new List { title: "Done",  pos: "a2" }

let c1 = new Card { title: "Design the schema", pos: "a0", list: l_todo }
let c2 = new Card { title: "Lower to circuits", pos: "a1", list: l_todo }
let c3 = new Card { title: "Ship the shaper",   pos: "a0", list: l_doing }

// `pos: Text` is a fractional order key (manual/drag order); an intrinsic
// `order by .dueDate` would need no key. Hiding this is post-MVP (S-72).

event AddCard(list: List, pos: Text)
event MoveCard(card: Card, list: List, pos: Text)
event RenameCard(card: Card, title: Text)
event DeleteCard(card: Card)

on AddCard(list, pos)        => new Card { title: "New card", pos: pos, list: list }
on MoveCard(card, list, pos) => update card { list: list, pos: pos }
on RenameCard(card, title)   => card.title := title
on DeleteCard(card)          => delete card

let cards : List -> Int = count(Card by .list)

view main =
  List as l order by .pos select
    section(class="list" dropTarget
      on drop(card = drag(Card), pos = dropPos(c, card)) => do MoveCard(card, l, pos)) {
      header {
        span { .title }
        span(class="count") { cards }
        button(on click(pos = endOf(c)) { do AddCard(l, pos); focus(c) }) "+ card"
      }
      Card as c where .list = l order by .pos select
        div(class="card" draggable) {
          input(value=.title on change(v = value) => do RenameCard(c, v))
          button(on click => do DeleteCard(c)) "×"
        }
    }
