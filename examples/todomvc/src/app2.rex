// TodoMVC in Rex surface v1 — an S-02 acceptance program (MVP-PLAN.md).
// Written before the grammar exists; every construct below has a desugaring
// in SYNTAX.md. Ported from ../../../elysium26/examples/todomvc-views.ely.

entity Todo { 
  text: Text          // Comma field-sep is optional if newline
  completed: Bool 
}

// Alternative: instead of "build sets of atoms", explicitly declared union types?
type Filter = All | Active | Completed
state filter: Filter = All    // we don't need e.g. `Filter.All` because the state is type-annotated (bidirectional check)

// --- Events: the only way state changes. One event = one logged, replayable
// transaction; every read in a handler sees the pre-event snapshot.

event AddTodo(text: Text)
event ToggleTodo(t: Todo)
event EditTodo(t: Todo, text: Text)
event DeleteTodo(t: Todo)
event ToggleAll(done: Bool)
event ClearCompleted()
event SetFilter(f: Filter)

on AddTodo(text) {
  new Todo { text: text, completed: @false }
}

on ToggleTodo(t) {
  t:completed := not t:completed
} 

on EditTodo(t, text) {
  t:text := text
} 

on DeleteTodo(t) {
  // in Elysium, we had to do `Todo where .id = t delete`
  // Is this desugaring consistent?
  delete t
}

on ToggleAll(done)  {
  Todo update { completed: done }
}

on ClearCompleted() {
  Todo where :completed delete
}

on SetFilter(f) {
  set filter = f
}

// --- Derived relations, incrementally maintained. `filter` is a singleton
// read through `unit`, so changing it is one field delta, not a recompute.

let visible : Todo = Todo where match filter {
  All       => true
  Active    => not :completed
  Completed => :completed
}

let total     : Unit -> Int = count(Todo by unit)
let active    : Unit -> Int = count((Todo where not :completed) by unit)
let completed : Unit -> Int = count((Todo where :completed) by unit)

// --- UI. `main` is the mounted root; its body sits at the implicit `Unit`
// level, so static chrome and scalar binds (`active`) need no entity.

// I think the `section.todoapp` CSS class syntax could be confusing with rex's own dot syntax?

view main =
  section(class="todoapp") {
    header(class="header") {
      h1 "todos"
      input(class="new-todo" placeholder="What needs to be done?" autofocus
        on keydown.enter(text = value) => {do AddTodo(text); clear}
      )
    }
    if (total > 0) {
      section(class="main") {
        input(id="toggle-all" class="toggle-all" type="checkbox" checked=(active = 0)
          on change(done = checked) => do ToggleAll(done)
        )
        label(for="toggle-all") "Mark all as complete"
        ul(class="todo-list") {
          visible as t order by id select TodoItem(t)
        }
      }
      footer(class="footer") {
        span(class="todo-count") { strong { active } " items left" }
        ul(class="filters") {
          li { a "All"       class.selected=(filter = @all)       on click => do SetFilter(@all) }
          li { a "Active"    class.selected=(filter = @active)    on click => do SetFilter(@active) }
          li { a "Completed" class.selected=(filter = @completed) on click => do SetFilter(@completed) }
        }
        if (completed > 0) {
          button(class="clear-completed") "Clear completed" on click => do ClearCompleted()
        }
      }
    }
  }

// A component: expands inline at each call site; `t` is the row key.
// `local` state is a relation keyed by the instance (`Todo -> Bool`).

view TodoItem(t: Todo) =
  local editing = False   // type inferred
  li(class="completed") {
    div(class="view") {
      input.toggle type="checkbox" checked=:completed on change => do ToggleTodo(t)
      label { :text } on dblclick => set editing = @true -> focus(.edit)
      button(class="destroy") on click => do DeleteTodo(t)
    }
    input(class="edit" value=:text) 
      on keydown.enter(v = value) => do EditTodo(t, v) ; set editing = @false
      on keydown.escape           => set editing = @false
      on blur(v = value)          => do EditTodo(t, v) ; set editing = @false
  }

// generally I think with this form, the boundary of "what is a tag attribute, what is a tag child, what is an inline Rex/Elysium expr" `
// is a bit unclear to me visually (maybe I'm too used to Python/TS/Rust-style explicit delimiters instead of Haskell-style whitespace?)
// At least in tag/JSX-style it's fairly explicit
// like should the `on ...` handlers inside a view component go inside the (), inside the {}, or ...?