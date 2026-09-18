// TodoMVC in Rex surface v1 — an S-02 acceptance program (MVP-PLAN.md).
// Written before the grammar exists; every construct has a desugaring in
// SYNTAX.md. Ported from ../../../elysium26/examples/todomvc-views.ely.

entity Todo {
  text: Text
  completed: Bool
}

type Filter = All | Active | Completed
state filter : Filter = All

// --- Events: the only way state changes. One event = one logged, replayable
// transaction; every read in a handler sees the pre-event snapshot.

event AddTodo(text: Text)
event ToggleTodo(t: Todo)
event EditTodo(t: Todo, text: Text)
event DeleteTodo(t: Todo)
event ToggleAll(done: Bool)
event ClearCompleted()
event SetFilter(f: Filter)

on AddTodo(text)     => new Todo { text: text, completed: False }
on ToggleTodo(t)     => t.completed := not t.completed
on EditTodo(t, text) => t.text := text
on DeleteTodo(t)     => delete t
on ToggleAll(done)   => update Todo { completed: done }
on ClearCompleted()  => delete Todo where .completed
on SetFilter(f)      => set filter = f

// --- Derived relations, incrementally maintained. `filter` is a singleton
// read through `unit`, so changing it is one field delta, not a recompute.

let visible : Todo = match filter {
  All       => Todo
  Active    => Todo where not .completed
  Completed => Todo where .completed
}

let total     : Unit -> Int = count(Todo by unit)
let active    : Unit -> Int = count((Todo where not .completed) by unit)
let completed : Unit -> Int = count((Todo where .completed) by unit)

// --- UI. `main` is the mounted root; its body sits at the implicit `Unit`
// level, so static chrome and scalar binds (`active`) need no entity.
// Parens hold an element's own properties (attrs, handlers); braces hold
// its children.

view main =
  section(class="todoapp") {
    header(class="header") {
      h1 "todos"
      input(class="new-todo" placeholder="What needs to be done?" autofocus
        on keydown.enter(text = value) { do AddTodo(text); clear })
    }
    if (total > 0) {
      section(class="main") {
        input(id="toggle-all" class="toggle-all" type="checkbox" checked=(active = 0)
          on change(done = checked) => do ToggleAll(done))
        label(for="toggle-all") "Mark all as complete"
        ul(class="todo-list") {
          visible as t order by id select TodoItem(t)
        }
      }
      footer(class="footer") {
        span(class="todo-count") { strong { active } " items left" }
        ul(class="filters") {
          li { a(class.selected=(filter = All)       on click => do SetFilter(All))       "All" }
          li { a(class.selected=(filter = Active)    on click => do SetFilter(Active))    "Active" }
          li { a(class.selected=(filter = Completed) on click => do SetFilter(Completed)) "Completed" }
        }
        if (completed > 0) {
          button(class="clear-completed" on click => do ClearCompleted()) "Clear completed"
        }
      }
    }
  }

// A component: expands inline at each call site; `t` is the row key, so
// `t.completed` and `.completed` mean the same thing in its body. `local`
// state is a relation keyed by the instance (`Todo -> Bool`); setting it
// from a DOM handler is an implicit, logged event.

view TodoItem(t: Todo) =
  local editing = False
  li(class.completed=.completed class.editing=editing) {
    div(class="view") {
      input(class="toggle" type="checkbox" checked=.completed on change => do ToggleTodo(t))
      label(on dblclick { set editing = True; focus(.edit) }) { .text }
      button(class="destroy" on click => do DeleteTodo(t))
    }
    input(class="edit" value=.text
      on keydown.enter(v = value) { do EditTodo(t, v); set editing = False }
      on keydown.escape           => set editing = False
      on blur(v = value)          { do EditTodo(t, v); set editing = False })
  }
