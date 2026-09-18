// Chat in Rex surface v1 — an S-02 acceptance program (MVP-PLAN.md).
// Ported from ../../../elysium26/examples/chat.ely (itself after
// https://www.scattered-thoughts.net/writing/relational-ui/).
//
// Two things the other programs don't need:
//  - a many-to-many (`liked_by: many User`) — a link entity `Like`, the
//    honest 6NF form; `rel Liked(Message, User)` sugar for it is an open question;
//  - a state with no default (`current` user) — it starts EMPTY, a 0-or-1
//    row singleton, which gives optionality without runtime coproducts.

entity User {
  name: Text
}
entity Message {
  text: Text
  by: User
}
entity Like {
  msg: Message
  by: User
}

state current : User            // no default: empty until a user is selected

event SeedSynthetic()
event MessageSent(text: Text)    // the sender is read from `current` in the handler
event MessageLiked(msg: Message)
event MessageDeleted(msg: Message)
event UserSelected(user: User)

on SeedSynthetic() {
  let alice = new User { name: "Alice" }
  let bob   = new User { name: "Bob" }
  let chloe = new User { name: "Chloe" }
  let m1 = new Message { text: "Welcome to Rex chat", by: alice }
  let m2 = new Message { text: "Like messages to test many-to-many joins", by: bob }
  new Like { msg: m1, by: bob }
  new Like { msg: m1, by: chloe }
  set current = alice
}

// `current` is `Unit -> User` with 0 or 1 rows; when empty, `by: current`
// writes no `by` row (6NF: the field is simply absent) — the checker warns
// that the value may be empty.
on MessageSent(text)     => new Message { text: text, by: current }
on MessageLiked(msg)     => new Like { msg: msg, by: current }
on MessageDeleted(msg)   { delete Like where .msg = msg; delete msg }
on UserSelected(user)    => set current = user

view main =
  div(class="chat-root") {
    h1 "Chat App"
    div(class="toolbar") {
      button(on click => do SeedSynthetic()) "Load Synthetic Data"
      span { "Current user: " ++ current.name }
    }
    div(class="users") {
      User as u select
        button(class.selected=(u = current) on click => do UserSelected(u)) { .name }
    }
    table {
      Message as m order by id select MessageItem(m)
    }
    input(class="send-message" placeholder="Say something ..."
      on keydown.enter(text = value) { do MessageSent(text); clear })
  }

// Likes of this message: a nested level over the link entity, joined out to
// the liker's name through `l.by.name` (compose, spelled as a path).
view MessageItem(m: Message) =
  tr {
    td { .by.name ":" }
    td { .text }
    td {
      Like as l where .msg = m select
        div { l.by.name " likes this!" }
    }
    td { button(on click => do MessageLiked(m)) "Like!" }
    if (.by = current) {
      td { button(on click => do MessageDeleted(m)) "Delete" }
    }
  }
