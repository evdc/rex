`then` and `else` as short circuiting logical operators - "falsey" = "empty relation", "truthy" = "any nonempty relation" ??

`let are_there_west_customers = Customer:region[@west] then "yes" else "no"` 
- evalutes `Customer:region[@west]` -- intersect RHS of `Customer:region` with the singleton set `{@west}` -- to either a nonempty set of `(CustomerID, Region)` or the empty set
- `<nonempty set> then "yes"` evaluates to "yes" (evaluate to RHS, if LHS is truthy)
    - `"yes" else "no"` evalues to "yes" (evaluate to LHS, if LHS is truthy)
- `<empty set> then "yes"` evaluates to `<empty set>` (evaluate to LHS, if LHS is falsey)
    - `"<empty set> else "no"` evaluates to `"no"` (evaluate to RHS, if LHS is falsey)
This isn't a ternary conditional, the operators can be used independently

---

What is the overall goal and plan here
-> Build a relational programming language/framework that supports developing a whole app, data model to frontend, based on event driven reactive queries that are incrementally maintained.
- (Also) extend this to declarative, relational/ontology driven distributed systems (Firmament)

What are the pieces
- Elysium - prototype language/framework for reactive relational web apps
- Ripple - relational programming language impl. in Rust, currently compiles to SQLite
- Reactor-ts -- minimal DBSP runtime + events, in TS, as the engine for the frontend. Includes a minimal "platform" + handwritten (well, Claude-written) "compiled" code, showing what the target code using the platform *could* look like; no compiler/frontend.
- This Claude chat (https://claude.ai/chat/64e524e6-9a48-429c-8fb3-c4fdce30948c) and spec doc: incremental maintenance of nested structures = composite keys (+ a prefix op on composite keys?). 

