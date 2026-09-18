// A minimal Kanban-shaped program, self-contained (no dependency on
// examples/kanban), used as the S-03 codegen/shaper contract fixture: two
// entities, one `view` with a nested `select`, order, attrs, and handlers
// covering create/update/move/delete.

entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: ListID }

let l_todo  = new List { title: "Todo",  pos: "a0" }
let l_doing = new List { title: "Doing", pos: "a1" }

let c1 = new Card { title: "Design", pos: "a0", list: l_todo }
let c2 = new Card { title: "Lower",  pos: "a1", list: l_todo }
let c3 = new Card { title: "Ship",   pos: "a0", list: l_doing }

view board =
  List as l order by .pos select
    section(class="list" dropTarget
      on drop(card = drag(Card), pos = dropPos(c, card)) { update card { list: l, pos: pos } }) {
      header {
        span { .title }
        button(on click(pos = endOf(c)) => new Card { title: "New card", pos: pos, list: l }) "+ card"
      }
      Card as c where .list = l order by .pos select
        div(class="card" draggable) {
          input(value=.title on change(v = value) => .title := v)
          button(on click => delete c) "x"
        }
    }
