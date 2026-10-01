// Chat in Rex surface v1 — an S-02 acceptance program (MVP-PLAN.md).
// Ported from ../../../elysium26/examples/chat.ely (itself after
// https://www.scattered-thoughts.net/writing/relational-ui/).
//
// Things the other programs don't need:
//  - a many-to-many (`liked_by: many User`) — a link entity `Like` whose key
//    is the pair it links, so a user likes a message at most once;
//  - a state with no default (`current` user) — it starts EMPTY, a 0-or-1
//    row singleton, which gives optionality without runtime coproducts;
//  - handler guards: what an event requires of the state it meets.

entity User {
  name: Text
}
entity Message {
  text: Text
  sender: User
}
entity Like {
  msg: Message
  user: User
  key (msg, user)               // no two likes of one message by one user
}

state current : User            // no default: empty until a user is selected

// Seed data: created once, when the app first boots, and logged like any
// other change — so a reload restores it and never runs it twice.
let alice = new User { name: "Alice" }
let bob   = new User { name: "Bob" }
let chloe = new User { name: "Chloe" }
let m1 = new Message { text: "Welcome to Rex chat", sender: alice }
let m2 = new Message { text: "Like messages to test many-to-many joins", sender: bob }
let _  = new Like { msg: m1, user: bob }
let _  = new Like { msg: m1, user: chloe }

// A message's like by the current user, if there is one (and none at all
// while nobody is selected): `.msg` of those likes, read backwards.
let liked_by_me : Message -> Like = ~((Like where .user = current) . .msg)

event MessageSent(text: Text)    // the sender is read from `current` in the handler
event MessageLiked(msg: Message)
event MessageDeleted(msg: Message)
event UserSelected(user: User)

// A guard is the event's precondition, read from the state before it. If it
// does not hold the event is rejected: nothing is written, nothing is logged.
// The view below hides what a guard would reject, but the guard is the rule —
// it holds for any caller, not only for this page's buttons.
on MessageSent(text) where (current & text != "") else "pick a user and type something" =>
  new Message { text: text, sender: current }
// `Like`'s key already rules out a second like — an event that tried would
// be rejected. Liking again takes the like back instead: the event is
// accepted either way, and an `if` picks what it does.
on MessageLiked(msg) where (current & msg) else "pick a user first" {
  if (msg.liked_by_me) { delete Like where .msg = msg & .user = current }
  else                 { new Like { msg: msg, user: current } }
}
on MessageDeleted(msg) where (msg.sender = current) else "only the sender can delete a message" {
  delete Like where .msg = msg
  delete msg
}
on UserSelected(user) where (user) => set current = user

view main =
  div(class="chat-root") {
    h1 "Chat App"
    div(class="toolbar") {
      span { "Current user: " ++ current.name }
    }
    div(class="users") {
      User as u select
        button(class.selected=(u = current) on click => do UserSelected(u)) { .name }
    }
    table {
      Message as m order by id select MessageItem(m)
    }
    if (current) {
      input(class="send-message" placeholder="Say something ..."
        on keydown.enter(text = value) { do MessageSent(text); clear })
    }
  }

// Likes of this message: a nested level over the link entity, joined out to
// the liker's name through `l.user.name` (compose, spelled as a path).
view MessageItem(m: Message) =
  tr {
    td { .sender.name ":" }
    td { .text }
    td {
      Like as l where .msg = m select
        div { l.user.name " likes this!" }
    }
    td {
      if (current & not m.liked_by_me) { button(on click => do MessageLiked(m)) "Like!" }
      if (m.liked_by_me) { button(class="unlike" on click => do MessageLiked(m)) "Unlike" }
    }
    if (.sender = current) {
      td { button(on click => do MessageDeleted(m)) "Delete" }
    }
  }
