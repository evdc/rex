entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: ListID }

let l_todo  = new List { title: "Todo",  pos: "a0" }
let l_doing = new List { title: "Doing", pos: "a1" }
let l_done  = new List { title: "Done",  pos: "a2" }

let c1 = new Card { title: "Design the schema",  pos: "a0", list: l_todo }
let c2 = new Card { title: "Lower to circuits",  pos: "a1", list: l_todo }
let c3 = new Card { title: "Ship the shaper",    pos: "a0", list: l_doing }

let lists      : List -> ListID = id
let list_title : List -> Text   = :title
let list_pos   : List -> Text   = :pos
let card_list  : Card -> ListID = :list
let card_title : Card -> Text   = :title
let card_pos   : Card -> Text   = :pos
