# Rex

A point-free, binary-relational **view language**. Every program denotes a binary
relation `A → B`; relations compose like arrows in a category; entities are
families of keyed columnar relations. The long-term goal (per [`SPEC.md`](SPEC.md))
is to lower programs to **DBSP circuits** so that incremental view maintenance
(inserts, deletes, corrections over Z-sets) is automatic.

This repository is the **compiler front end plus a batch reference interpreter**.
The design is captured in `SPEC.md` (working notes, not a spec — read it first);
`drafts.md` holds looser ideas and project context.

> **Status:** the non-recursive core parses, type-checks, elaborates, and
> evaluates end-to-end. The §12 worked example from the spec runs and produces
> correct results. Everything is **batch/eager** — there is no DBSP lowering,
> no incremental maintenance, and no recursion yet.

---

## Building & running

```sh
cargo build
cargo test                     # 60-odd tests across lexer/parser/check/elaborate/eval
cargo run                      # start the REPL
cargo run path/to/prog.rex     # parse, type-check, and batch-evaluate a file
```

Running a file prints the canonical s-expression AST, a type-check result, and
each view's materialized contents. `cargo run tests/fixtures/spec12.rex` is the
best end-to-end demo.

The REPL (`src/repl.rs`) accepts `entity`/`let` statements (committed to the
session) and bare expressions (evaluated in a scratch copy and discarded).
Commands: `/import <path>`, `/env`, `/reset`, `/help`, `/quit`.

---

## Pipeline & architecture

Source flows through five phases, each in its own module:

```
src → lex → parse → check/elaborate → eval
      │      │            │              │
   Vec<Token>  Program   TProgram   HashMap<name, BTreeRelation>
              (untyped)  (typed)
```

| Phase | Module(s) | What it does |
|---|---|---|
| **Lex** | `src/lexer.rs`, `src/token.rs` | ASCII-only, hand-written. Maximal-munch operators; distinguishes decimal `.` from compose `.`; dates, atoms `@foo`, strings with escapes. Recovers and collects diagnostics. |
| **Parse** | `src/parser.rs`, `src/ast.rs` | Recursive descent for statements, Pratt (binding-power) for expressions. Produces a surface AST that mirrors what was written — **no desugaring here**. Recovers to the next `let`/`entity` on error. |
| **Check / elaborate** | `src/types/` | The heart. A single bidirectional, type-directed walk validates the program *and* produces the elaborated `TProgram`: field paths resolved to `(sort, field)` hops, identifiers resolved to their kind (view / entity-identity / value), filter built-ins lowered to `Filter` nodes. Threads an ambient domain through composition to resolve `:field`. |
| **Eval** | `src/eval/` | Batch interpreter over the typed AST. `interp.rs` walks `TProgram`; `algebra.rs` is the relational algebra; `relation.rs` is the Z-set store; `value.rs` is the runtime domain element. |
| **Support** | `src/diagnostic.rs`, `src/span.rs`, `src/pretty.rs`, `src/operator.rs` | Spans + rendered diagnostics; canonical s-expr printer (used for test assertions and `--ast`-style output); the §7 operator-metadata table. |

Key design choices realized in code:

- **Entities are identity relations** (§3.3). `entity Customer {…}` mints a fresh
  `SortId`, per-field keyed relations `(sort, field) → BTreeRelation`, and treats
  the bare name `Customer` as the diagonal `CustID → CustID`. `id` is the generic
  version, resolved against the ambient domain.
- **`.` vs `[]`** (§3.2). Both are the same join; `Compose` keeps outer columns,
  `Semijoin` keeps the left's columns. The historically-confusable mistake
  (`[]` against a value column) is a genuine **type error** — see the test
  `semijoin_on_value_column_is_a_type_error`.
- **One desugar rule surfaces in the typed AST**, not the parser: `by` stays as
  the idiom `~Y . X`; filter comparisons fold into `Filter`; `except`/`antijoin`
  both become `Antijoin` (evaluated as `A − A[B]`, the weight-safe form, §6).
- **Z-sets everywhere** (§2). `BTreeRelation` maps `left → right → i64 weight`,
  prunes zero-weight entries, and supports negative weights (retractions) — the
  substrate incremental maintenance will eventually need.
- **Money is exact**, stored as integer cents.

---

## What exists

- ✅ Full lexer, parser, s-expression pretty-printer.
- ✅ Entity declarations → minted ID-sorts + keyed field columns.
- ✅ `new` creation sugar (atomic per-key population; anonymous `let _`).
- ✅ Bidirectional checker with type-directed `:field` resolution, ambient-domain
  threading, co-keyed/join-column diagnostics, atom/coproduct subtyping.
- ✅ Combinators: compose, semijoin/`where`, inverse, fork, union, intersect,
  distinct, `by`, `except`/`antijoin`.
- ✅ Functional ops `*` and `||`; comparisons (prefix-filter and binary
  column-vs-column); `in`.
- ✅ Aggregations `sum`/`count`/`avg`/`min`/`max` as monoid homomorphisms over
  a key's image Z-set, respecting weights.
- ✅ Batch interpreter; REPL with session state and `/import`.
- ✅ The §12 worked example type-checks and evaluates correctly.

---

## What's left to do (roughly in spec order)

1. **Groundedness analysis (§9.1) — required by the spec for v1, not yet built.**
   The operator table records `Infinite` for `<`, `*`, etc., but nothing enforces
   that infinite relations are grounded by finite application. Today an ungrounded
   program isn't rejected; it just silently evaluates to something empty/wrong
   (e.g. a standalone coreflexive over a non-entity domain — see below).
2. **DBSP lowering + incremental maintenance — the entire thesis.** The evaluator
   is batch/eager (`algebra.rs`: "every operation fully materializes"). There are
   no circuits, no delta processing, no IVM. This is the biggest gap between the
   code and the design.
3. **Recursion `fix` + stratification (§8).** Parked by design; the monotonicity
   bits in `operator.rs` are forward-compat only, with no consumer.
4. **Analysis consumers.** Set-ness (drop redundant `distinct`), linearity cost
   model surfaced to users, delta fan-out warning (§10) — all unimplemented; the
   metadata table has no readers.
5. **Value-algebra completeness.** Coproducts exist in the *type* system but have
   no runtime injection/case forms — there's no `inl/inr`, no way to construct or
   match `(V + Unit)`, so the "no NULL, use `V + Unit`" story isn't realizable at
   runtime yet. Fork builds `Pair` values but there are **no projection
   combinators** (`fst`/`snd`/`outl`/`outr`), so a forked pair is a dead end
   except at serialization.
6. **Edge/serialization layer.** Wide-row assembly, ORDER BY, top-N (§11) — none
   exist. Views print as raw `left → right` pairs.
7. **`from E:` blocks** (§4 secondary sugar) — not parsed.
8. **The `+` naming collision** (§11) is unresolved in surface syntax (`+` is
   union; arithmetic add isn't spelled at all — only `*` and `||` exist).

---

## Bugs & correctness issues

- ~~**Negative money parses and prints wrong.**~~ **Fixed.** The sign is now taken
  from the literal itself (`"-0.50" → Money(-50)`) and `Display` prints it
  (`-$0.50`). Covered by tests in `src/eval/value.rs`.
- ~~**Coreflexive-producing nodes ignore input weights.**~~ **Fixed.**
  `BinCompare` now combines weights bilinearly (`wa * wb`), and `InRel` /
  standalone `Coreflexive` carry the source row's weight, so they produce faithful
  Z-set coreflexives instead of clamped weight-1 rows.
- **Standalone `Coreflexive` silently yields empty off an entity domain.**
  `src/eval/interp.rs:166` only materializes when `te.ty.from` is `ValueTy::Id`;
  any other domain returns the empty relation with no error. This is the hole that
  a real groundedness check (item 1 above) is supposed to close.
- **Aggregation of an empty group produces no row.** `sum`/`count` over a key with
  no image emit nothing rather than `0` — there's no outer key to range over, so
  "count of customers with zero orders" is unrepresentable. Standard group-by
  behavior, but worth deciding deliberately given the incremental target.
- **`in` is atoms-only.** `expect_subset` (`src/types/check.rs:734`) uses
  `.atoms()`, so `x in (1 + 2 + 3)` over integers is rejected even though
  `collect_lits` happily gathers the int literals. Either generalize or reject
  earlier with a clearer message.
- **`min`/`max` over retractions is batch-only-correct.** They consider any value
  with net weight `> 0`; this is fine for batch but is exactly the recompute-tier
  hazard the spec (§5) flags — it will need real handling under IVM.

## Performance

- **The REPL re-checks and re-evaluates the whole accumulated session on every
  line** (`Session::commit` in `src/repl.rs`): O(n²) in session length, and every
  `new` re-executes (id sequences are deterministic so results are stable, but the
  work is repeated). Fine at prototype scale; a real incremental engine would
  make this the natural place to feed deltas.
- **The interpreter clones pervasively.** `eval` returns owned `BTreeRelation`s,
  `View`/`Identity`/field lookups `.clone()` whole relations, `Const` clones the
  entire id relation to iterate its domain. `BinaryRelation::iter`/`row` return
  boxed iterators that clone every `Value`. All acceptable for a reference
  interpreter; none of it survives into a columnar/vectorized backend.
- **Field lookups allocate a `String` key per access** (`(sort, name.to_string())`
  in `Interp::field`).

## Code style / cleanup

- ~~Duplicate interpreter entry points `run_typed`/`run_typed_values`.~~
  **Done** — collapsed to the single `run_typed_values`.
- ~~`operator.rs` is dead metadata that can silently drift.~~ **Partly addressed** —
  it's still forward-compat with no runtime consumer (by design), but a test now
  guards that every combinator has an entry and that names are unique, so the
  table can't drift from the operator set unnoticed. Wiring a `TExprKind → OpMeta`
  lookup for a real linearity/groundedness pass is still future work.
- ~~`Value::is_money` is unused.~~ **Removed.**
- ~~The `spec12.rex` fixture's `result` line deviates from SPEC §12.~~ **Done** — a
  correction note is now in SPEC §12 pointing at the fixture, so the two no longer
  drift silently. The fixture uses `inregion . (custspend where > 30)` because the
  spec's `(custspend where > 30)[inregion]` is (correctly) ill-typed under §3.2.

## Test coverage

Strong where it exists: lexer (18 cases), parser (precedence/associativity of
every operator), checker (positive cases + the important negative diagnostics:
unknown field/type, wrong `new` value type, atom-not-in-coproduct, ID-sort
discipline, the `[]`-on-value-column type error, non-co-keyed comparisons),
elaboration (field-hop resolution, `where`→`Filter` folding, semijoin preserved,
`by`/agg), and algebra primitives + the §12 end-to-end eval.

Gaps:

- **Interpreter coverage is thin beyond §12.** No eval tests drive `antijoin`/
  `except`, `distinct`, `intersect`, `inverse`, `in`-filter, `BinCompare`
  (column-vs-column), fork end-to-end, or `count`/`avg`/`min`/`max` — only `sum`
  is exercised at the interp level.
- **No retraction/negative-weight tests through the interpreter.** The Z-set store
  is tested for pruning, but no program-level test inserts a negative weight and
  checks that composition/antijoin/aggregation handle it — precisely the behavior
  the whole design rests on.
- ~~No tests for the money bugs (negative parse/display).~~ **Added** in
  `src/eval/value.rs`.
- ~~No test asserting the operator table stays in sync.~~ **Added** in
  `src/operator.rs`.
- **No REPL tests** (session commit, scratch eval, `/import`).
