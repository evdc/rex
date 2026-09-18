// A minimal Kanban-shaped program, self-contained (no dependency on
// examples/kanban), used as the S-03 codegen/shaper contract fixture: two
// entities, one `view` with a nested `select`, order, attrs, and named
// events covering create/update/move/delete.

entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: ListID }

let l_todo  = new List { title: "Todo",  pos: "a0" }
let l_doing = new List { title: "Doing", pos: "a1" }

let c1 = new Card { title: "Design", pos: "a0", list: l_todo }
let c2 = new Card { title: "Lower",  pos: "a1", list: l_todo }
let c3 = new Card { title: "Ship",   pos: "a0", list: l_doing }

event AddCard(list: List, pos: Text)
event MoveCard(card: Card, list: List, pos: Text)
event RenameCard(card: Card, title: Text)
event DeleteCard(card: Card)

on AddCard(list, pos)        => new Card { title: "New card", pos: pos, list: list }
on MoveCard(card, list, pos) => update card { list: list, pos: pos }
on RenameCard(card, title)   => card.title := title
on DeleteCard(card)          => delete card

view board =
  List as l order by .pos select
    section(class="list" dropTarget
      on drop(card = drag(Card), pos = dropPos(c, card)) => do MoveCard(card, l, pos)) {
      header {
        span { .title }
        button(on click(pos = endOf(c)) { do AddCard(l, pos); focus(c) }) "+ card"
      }
      Card as c where .list = l order by .pos select
        div(class="card" draggable) {
          input(value=.title on change(v = value) => do RenameCard(c, v))
          button(on click => do DeleteCard(c)) "x"
        }
    }
