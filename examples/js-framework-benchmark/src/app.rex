// js-framework-benchmark (keyed) in Rex surface v1 — an S-02 acceptance
// program (MVP-PLAN.md). Follows the structure of
// ../../../elysium26/bench/js-framework-benchmark/main.ely and the upstream
// vanillajs index.html (same element ids so the harness drives it).

import js "./utils.js" as utils      // randomLabels(n): Int -> Text, DOM-layer only

entity Row {
  num: Int
  label: Text
  pos: Int
  selected: Bool
}

state nextId  : Int = 1
state nextPos : Int = 1

// Bulk data enters as a relation-valued event param: the labels are made
// client-side (non-deterministic), logged with the event, and inserted in
// ONE transaction — replay never re-randomises.

event Run(n: Int, labels: Int -> Text)   // replace every row with n fresh ones
event Add(labels: Int -> Text)           // append 1,000 rows
event Update()                           // every 10th label gets " !!!"
event Clear()
event SwapRows()                         // display positions 2 and 999
event Select(r: Row)
event Delete(r: Row)

on Run(n, labels) {
  delete Row
  new Row from labels as (i, label) {
    num: nextId + i, label: label, pos: i + 1, selected: False
  }
  set nextId  = nextId + n
  set nextPos = n + 1
}

on Add(labels) {
  new Row from labels as (i, label) {
    num: nextId + i, label: label, pos: nextPos + i, selected: False
  }
  set nextId  = nextId + 1000
  set nextPos = nextPos + 1000
}

on Update() => update Row where .num % 10 = 1 { label: .label ++ " !!!" }

on Clear() => delete Row

// Both targets resolve against the pre-event snapshot, so this is a swap,
// not a double move. Arg-free targets are hidden maintained views (O(1)).
on SwapRows() {
  update Row where .pos = 2   { pos: 999 }
  update Row where .pos = 999 { pos: 2 }
}

on Select(r) {
  update Row where .selected { selected: False }
  r.selected := True
}

on Delete(r) => delete r

view main =
  div(class="container") {
    div(class="jumbotron") {
      div(class="row") {
        div(class="col-md-6") { h1 "Rex (keyed)" }
        div(class="col-md-6") {
          div(class="row") {
            div(class="col-sm-6 smallpad") {
              button(id="run" class="btn btn-primary btn-block" type="button"
                on click(labels = utils.randomLabels(1000)) => do Run(1000, labels))
                "Create 1,000 rows"
            }
            div(class="col-sm-6 smallpad") {
              button(id="runlots" class="btn btn-primary btn-block" type="button"
                on click(labels = utils.randomLabels(10000)) => do Run(10000, labels))
                "Create 10,000 rows"
            }
            div(class="col-sm-6 smallpad") {
              button(id="add" class="btn btn-primary btn-block" type="button"
                on click(labels = utils.randomLabels(1000)) => do Add(labels))
                "Append 1,000 rows"
            }
            div(class="col-sm-6 smallpad") {
              button(id="update" class="btn btn-primary btn-block" type="button"
                on click => do Update()) "Update every 10th row"
            }
            div(class="col-sm-6 smallpad") {
              button(id="clear" class="btn btn-primary btn-block" type="button"
                on click => do Clear()) "Clear"
            }
            div(class="col-sm-6 smallpad") {
              button(id="swaprows" class="btn btn-primary btn-block" type="button"
                on click => do SwapRows()) "Swap Rows"
            }
          }
        }
      }
    }
    table(class="table table-hover table-striped test-data") {
      tbody(id="tbody") {
        Row as r order by .pos select RowView(r)
      }
    }
    span(class="preloadicon glyphicon glyphicon-remove" aria-hidden="true")
  }

view RowView(r: Row) =
  tr(class.danger=.selected) {
    td(class="col-md-1") { .num }
    td(class="col-md-4") { a(on click => do Select(r)) { .label } }
    td(class="col-md-1") {
      a(on click => do Delete(r)) {
        span(class="glyphicon glyphicon-remove remove" aria-hidden="true")
      }
    }
    td(class="col-md-6")
  }
