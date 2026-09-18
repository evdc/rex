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
  List order by :pos select
    section.list dropTarget {
      header {
        span { :title }
        button "+ card" on click(pos: Text = endOf(card)) =>
          new Card { title: "New card", pos: pos, list: List }
      }
      on drop(card: Card = drag("text/rex-card"), pos: Text = dropPos(card)) =>
        card:list := List ; card:pos := pos
      Card where :list = List order by :pos select
        div.card draggable {
          input value=:title on change(v: Text = value) => :title := v
          button "x" on click => delete Card
        }
    }
