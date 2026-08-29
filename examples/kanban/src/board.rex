// The whole Kanban app — data model, 6NF views, and UI — in one relational
// program. The compiler (`rex build`) generates `main.ts` (the shape tree,
// templates, and event wiring) from the `view` below; nothing is hand-written
// per app. See ROADMAP M5 and SYNTAX.md for the view grammar.

entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: List }

let l_todo  = new List { title: "Todo",  pos: "a0" }
let l_doing = new List { title: "Doing", pos: "a1" }
let l_done  = new List { title: "Done",  pos: "a2" }

let c1 = new Card { title: "Design the schema", pos: "a0", list: l_todo }
let c2 = new Card { title: "Lower to circuits", pos: "a1", list: l_todo }
let c3 = new Card { title: "Ship the shaper",   pos: "a0", list: l_doing }

// `List as l` binds each list row to `l`; the nested `Card where :list = l`
// reads "the cards whose :list field points at this list row". Handlers name
// their DOM event (`on click`, `on drop`, `on change`) and may reference only
// their own level's binder (self) plus their declared params.
view board =
  List as l order by :pos select
    section.list dropTarget {
      header {
        span { :title }
        button "+ card" on click(pos: Text = endOf(card)) =>
          new Card { title: "New card", pos: pos, list: l }
      }
      on drop(card: Card = drag("card"), pos: Text = dropPos(card)) =>
        card:list := l ; card:pos := pos
      Card as c where :list = l order by :pos select
        div.card draggable {
          input value=:title on change(v: Text = value) => :title := v
          button "×" on click => delete c
        }
    }
