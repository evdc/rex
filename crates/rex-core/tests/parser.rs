//! Parser tests: assert the canonical s-expression of the parsed AST.

use rex::{parse, program_to_sexpr};

/// Parse `src`, asserting no diagnostics, and return the canonical s-expr.
fn sexpr(src: &str) -> String {
    let result = parse(src);
    assert!(
        result.diagnostics.is_empty(),
        "unexpected diagnostics for {src:?}: {:?}",
        result.diagnostics
    );
    program_to_sexpr(&result.program)
}

/// Parse a single `let x = <expr>` and return just the body's s-expr.
fn expr(src: &str) -> String {
    let full = sexpr(&format!("let x = {src}"));
    // strip the `(let x _ ` prefix and trailing `)`
    full.strip_prefix("(let x _ ")
        .and_then(|s| s.strip_suffix(")"))
        .unwrap_or(&full)
        .to_string()
}

// --- primaries ------------------------------------------------------------

#[test]
fn primaries() {
    assert_eq!(expr("42"), "42");
    assert_eq!(expr("9.99"), "9.99");
    assert_eq!(expr(r#""hi""#), "\"hi\"");
    assert_eq!(expr("2026-01-15"), "2026-01-15");
    assert_eq!(expr("@west"), "@west");
    assert_eq!(expr("id"), "id");
    assert_eq!(expr("Customer"), "Customer");
    assert_eq!(expr(".qty"), ".qty");
    assert_eq!(expr(".product.price"), ".product.price");
    assert_eq!(expr(".order.customer.region"), ".order.customer.region");
}

// --- precedence -----------------------------------------------------------

#[test]
fn compose_binds_tighter_than_mul() {
    // `.qty * .product.price` = mul(.qty, .product.price), and the path hop
    // `.price` binds tightest of all.
    assert_eq!(expr(".qty * .product.price"), "(mul .qty .product.price)");
}

#[test]
fn compose_is_left_associative() {
    assert_eq!(expr("a . b . c"), "(compose (compose a b) c)");
}

#[test]
fn fork_is_loosest() {
    // fork(a, compose(b, c))
    assert_eq!(expr("a , b . c"), "(fork a (compose b c))");
}

#[test]
fn union_looser_than_compose_tighter_than_fork() {
    assert_eq!(expr("a , b | c . d"), "(fork a (union b (compose c d)))");
}

#[test]
fn where_between_set_and_compose() {
    assert_eq!(expr("a where b . c"), "(where a (compose b c))");
    assert_eq!(expr("a where b | c"), "(union (where a b) c)");
}

#[test]
fn prefix_comparison_filter() {
    assert_eq!(expr("> 30"), "(cmp > _ 30)");
    assert_eq!(expr("= @west"), "(cmp = _ @west)");
    assert_eq!(expr("a where > 30"), "(where a (cmp > _ 30))");
}

#[test]
fn binary_comparison() {
    assert_eq!(expr("a > b"), "(cmp > a b)");
}

#[test]
fn membership() {
    assert_eq!(
        expr(".region in (@west | @east)"),
        "(in .region (union @west @east))"
    );
}

#[test]
fn restrict_is_postfix_tight() {
    assert_eq!(expr("a[b]"), "(restrict a b)");
    // binds tighter than compose: `a . b[c]` = compose(a, restrict(b, c))
    assert_eq!(expr("a . b[c]"), "(compose a (restrict b c))");
}

#[test]
fn restrict_after_parenthesized_where() {
    assert_eq!(
        expr("(a where > 30)[b]"),
        "(restrict (where a (cmp > _ 30)) b)"
    );
}

#[test]
fn inverse_and_distinct_prefix() {
    assert_eq!(expr("~a"), "(inverse a)");
    assert_eq!(expr("~a . b"), "(compose (inverse a) b)");
    assert_eq!(expr("distinct a . b"), "(distinct (compose a b))");
}

#[test]
fn fst_snd_prefix_bind_tight() {
    assert_eq!(expr("fst a"), "(fst a)");
    assert_eq!(expr("snd a . b"), "(compose (snd a) b)");
    assert_eq!(expr("fst (a , b)"), "(fst (fork a b))");
}

#[test]
fn by_regrouping() {
    assert_eq!(
        expr("lineprice by .order.customer"),
        "(by lineprice .order.customer)"
    );
}

#[test]
fn calls() {
    assert_eq!(expr("count(a)"), "(call count a)");
    assert_eq!(
        expr("sum(lineprice by .order.customer)"),
        "(call sum (by lineprice .order.customer))"
    );
    // comma separates args; fork needs parens
    assert_eq!(expr("agg(a, b)"), "(call agg a b)");
    assert_eq!(expr("agg((a , b))"), "(call agg (fork a b))");
}

#[test]
fn except_and_antijoin() {
    assert_eq!(expr("a except b"), "(except a b)");
    assert_eq!(expr("a antijoin b"), "(antijoin a b)");
}

// --- statements -----------------------------------------------------------

#[test]
fn entity_declaration() {
    assert_eq!(
        sexpr("entity Product { name: Text, price: Money }"),
        "(entity Product (field name Text) (field price Money))"
    );
}

#[test]
fn entity_with_coproduct_field() {
    assert_eq!(
        sexpr("entity Customer { region: {@north | @south} }"),
        "(entity Customer (field region (coproduct @north @south)))"
    );
}

#[test]
fn let_with_and_without_annotation() {
    assert_eq!(sexpr("let x = 42"), "(let x _ 42)");
    assert_eq!(
        sexpr("let f : Line -> Money = .qty"),
        "(let f (-> Line Money) .qty)"
    );
}

#[test]
fn recursive_let() {
    assert_eq!(
        sexpr("let recursive path : Node -> Node = edge | edge . path"),
        "(letrec path (-> Node Node) (union edge (compose edge path)))"
    );
    // The annotation is optional at parse time; the checker requires it.
    assert_eq!(sexpr("let recursive p = e | e . p"), "(letrec p _ (union e (compose e p)))");
}

#[test]
fn recursive_is_reserved() {
    // `recursive` is a keyword now, so it cannot be a binding name.
    let result = parse("let recursive = 42");
    assert!(!result.diagnostics.is_empty());
}

#[test]
fn anonymous_let() {
    assert_eq!(
        sexpr("let _ = new Line { qty: 3 }"),
        "(let _ _ (new Line (field qty 3)))"
    );
}

#[test]
fn new_creation() {
    assert_eq!(
        sexpr(r#"let a = new Customer { name: "Alice", region: @west }"#),
        "(let a _ (new Customer (field name \"Alice\") (field region @west)))"
    );
}

#[test]
fn arrow_type_is_right_associative() {
    assert_eq!(
        sexpr("let f : A -> B -> C = x"),
        "(let f (-> A (-> B C)) x)"
    );
}

// --- error handling -------------------------------------------------------

#[test]
fn reports_and_recovers_between_statements() {
    // First `let` is missing its `=`; the parser should recover and still parse
    // the second statement.
    let result = parse("let x 42\nlet y = 7");
    assert!(!result.diagnostics.is_empty());
    let sx = program_to_sexpr(&result.program);
    assert!(sx.contains("(let y _ 7)"), "got: {sx}");
}

// --- capstone: SPEC §12 ---------------------------------------------------

#[test]
fn parses_spec12_fixture() {
    let src = include_str!("fixtures/spec12.rex");
    let result = parse(src);
    assert!(
        result.diagnostics.is_empty(),
        "diagnostics: {:?}",
        result.diagnostics
    );
    let sx = program_to_sexpr(&result.program);
    let expected = "\
(entity Customer (field name Text) (field region (coproduct @north @south @east @west)))
(entity Product (field name Text) (field price Money))
(entity Order (field customer CustomerID) (field placed Date))
(entity Line (field order OrderID) (field product ProductID) (field qty Int))
(let alice _ (new Customer (field name \"Alice\") (field region @west)))
(let bob _ (new Customer (field name \"Bob\") (field region @east)))
(let widget _ (new Product (field name \"Widget\") (field price 9.99)))
(let gizmo _ (new Product (field name \"Gizmo\") (field price 24.50)))
(let o1 _ (new Order (field customer alice) (field placed 2026-01-15)))
(let o2 _ (new Order (field customer alice) (field placed 2026-02-03)))
(let o3 _ (new Order (field customer bob) (field placed 2026-02-20)))
(let _ _ (new Line (field order o1) (field product widget) (field qty 3)))
(let _ _ (new Line (field order o1) (field product gizmo) (field qty 1)))
(let _ _ (new Line (field order o2) (field product widget) (field qty 2)))
(let _ _ (new Line (field order o3) (field product gizmo) (field qty 5)))
(let lineprice (-> Line Money) (mul .qty .product.price))
(let custspend (-> Customer Money) (call sum (by lineprice .order.customer)))
(let inregion Customer (where id (in .region (union @west @east))))
(let result (-> Customer Money) (compose inregion (where custspend (cmp > _ 30))))";
    assert_eq!(sx, expected);
}

// --- views (surface UI) ---------------------------------------------------

#[test]
fn view_state_smoke() {
    // A view with membership `where`, `order by`, nested select, attr binding,
    // and an inline handler that chains two mutations parses cleanly.
    let src = r#"
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: ListID }

state filter : {@all | @active} = @all

view board =
  List as l order by .pos select
    section(class="list" dropTarget
      on drop(card = drag(Card), pos = dropPos(c, card)) { update card { list: l, pos: pos } }) {
      header {
        span { .title }
        button(on click(pos = endOf(c)) => new Card { title: "New card", pos: pos, list: l }) "+ card"
      }
      Card as c where .list = l order by .pos select
        div(class="card" draggable) {
          input(value=.title on change(v = value) => .title := v)
          button(on click => delete c) "x"
        }
    }
"#;
    let out = sexpr(src);
    // Spot-check the load-bearing pieces survive the round-trip.
    assert!(out.contains("(state filter"), "state: {out}");
    assert!(out.contains("(view board"), "view: {out}");
    assert!(out.contains("(where (cmp = .list l))"), "membership: {out}");
    assert!(out.contains("(order .pos)"), "order: {out}");
    assert!(out.contains("(mod dropTarget)"), "modifier: {out}");
    assert!(out.contains("(mod draggable)"), "modifier: {out}");
    assert!(out.contains("(attr class \"card\")"), "static class: {out}");
    assert!(out.contains("(attr value .title)"), "attr bind: {out}");
    assert!(out.contains("(bind .title)"), "text bind: {out}");
    assert!(out.contains("(on change ((v value)) ((assign self.title v)))"), "change handler: {out}");
    assert!(out.contains("(delete c)"), "delete: {out}");
    assert!(out.contains("(update card (list l) (pos pos))"), "update: {out}");
    assert!(out.contains("(pos dropPos(c card))"), "extractor arg: {out}");
    assert!(out.contains("(card drag(Card))"), "drag extractor: {out}");
    assert!(out.contains("(new Card"), "new mutation: {out}");
    assert!(out.contains("(text \"+ card\")"), "text child: {out}");
}

// --- surface v1 (S-10) -----------------------------------------------------

#[test]
fn v1_field_paths_and_dot_compose() {
    // `.f` is a field path; `x.f` is compose with an identifier (the checker
    // resolves it as a field); `.a.b` is one greedy path.
    assert_eq!(expr(".qty"), ".qty");
    assert_eq!(expr(".product.price"), ".product.price");
    assert_eq!(expr("t.completed"), "(compose t completed)");
    assert_eq!(expr("l.user.name"), "(compose (compose l user) name)");
    assert_eq!(expr("Card . title"), "(compose Card title)");
    assert_eq!(expr("Card by .list"), "(by Card .list)");
}

#[test]
fn v1_operators() {
    // `|` union, `&` intersect; `+ - * / %` arithmetic; `++` concat; `!=`.
    assert_eq!(expr("a | b & c"), "(union a (intersect b c))");
    assert_eq!(expr("a & b | c"), "(union (intersect a b) c)");
    assert_eq!(expr("a except b | c"), "(union (except a b) c)");
    assert_eq!(expr("a where b | c"), "(union (where a b) c)");
    assert_eq!(expr("a + b * c"), "(add a (mul b c))");
    assert_eq!(expr("a - b - c"), "(sub (sub a b) c)");
    assert_eq!(expr("a / b % c"), "(mod (div a b) c)");
    assert_eq!(expr("a ++ b"), "(concat a b)");
    assert_eq!(expr(".num % 10 = 1"), "(cmp = (mod .num 10) 1)");
    assert_eq!(expr("a != b"), "(cmp != a b)");
    // Arithmetic binds tighter than comparison, looser than compose.
    assert_eq!(expr("nextId + i"), "(add nextId i)");
    assert_eq!(expr("x.f + 1 > 2"), "(cmp > (add (compose x f) 1) 2)");
    assert_eq!(expr("-5"), "-5");
    assert_eq!(expr("-0.50"), "-0.50");
    assert_eq!(expr("in (@west | @east)"), "(in _ (union @west @east))");
}

#[test]
fn v1_not_match_if() {
    assert_eq!(expr("Todo where not .completed"), "(where Todo (not .completed))");
    assert_eq!(expr("not .a = 1"), "(not (cmp = .a 1))");
    assert_eq!(
        expr("match filter { All => Todo, Active => Todo where not .completed\n _ => Todo }"),
        "(match filter (All Todo) (Active (where Todo (not .completed))) (_ Todo))"
    );
    assert_eq!(expr("if c then a else b"), "(if c a b)");
}

#[test]
fn v1_types_and_state() {
    assert_eq!(
        sexpr("entity Customer { region: {@north | @south} }"),
        "(entity Customer (field region (coproduct @north @south)))"
    );
    assert_eq!(
        sexpr("entity Todo {\n  text: Text\n  completed: Bool\n}"),
        "(entity Todo (field text Text) (field completed Bool))"
    );
    assert_eq!(sexpr("type Filter = All | Active | Completed"), "(type Filter All Active Completed)");
    assert_eq!(sexpr("state filter : Filter = All"), "(state filter Filter All)");
    assert_eq!(sexpr("state current : User"), "(state current User _)");
    assert_eq!(sexpr(r#"import js "./utils.js" as utils"#), "(import \"./utils.js\" utils)");
}

#[test]
fn v1_events_and_handlers() {
    assert_eq!(
        sexpr("event Run(n: Int, labels: Int -> Text)"),
        "(event Run ((n Int) (labels (-> Int Text))))"
    );
    assert_eq!(
        sexpr("on ToggleTodo(t) => t.completed := not t.completed"),
        "(on ToggleTodo (t) ((assign t.completed (not (compose t completed)))))"
    );
    assert_eq!(
        sexpr("on Run(n, labels) {\n  delete Row\n  new Row from labels as (i, label) { num: nextId + i, label: label }\n  set nextId = nextId + n\n}"),
        "(on Run (n labels) ((delete Row) (new Row (from labels i label) (num (add nextId i)) (label label)) (set nextId (add nextId n))))"
    );
    assert_eq!(
        sexpr("on SwapRows() {\n  update Row where .pos = 2 { pos: 999 };\n  update Row where .pos = 999 { pos: 2 }\n}"),
        "(on SwapRows () ((update (where Row (cmp = .pos 2)) (pos 999)) (update (where Row (cmp = .pos 999)) (pos 2))))"
    );
    assert_eq!(
        sexpr("on Seed() { let a = new User { name: \"A\" }; new Like { user: a } }"),
        "(on Seed () ((new a = User (name \"A\")) (new Like (user a))))"
    );
    assert_eq!(
        sexpr("on ClearCompleted() => delete Todo where .completed"),
        "(on ClearCompleted () ((delete (where Todo .completed))))"
    );
}

#[test]
fn v1_elements_props_and_children() {
    // Parens hold properties (attrs, keyword/hyphenated names, class toggles,
    // modifiers, handlers); braces hold children; a string is a text child.
    let out = sexpr(
        r#"view main =
  section(class="todoapp") {
    h1 "todos"
    input(id="toggle-all" type="checkbox" aria-hidden="true" autofocus checked=(active = 0)
      on change(done = checked) => do ToggleAll(done))
    label(for="toggle-all") "Mark all"
    span(class="count") { strong { active } " items left" }
    li { a(class.selected=(filter = All) on click => do SetFilter(All)) "All" }
    td { .sender.name ":" }
    span { "Current user: " ++ current.name }
    if (total > 0) { footer(class="footer") { button(on click { do ClearCompleted(); clear }) "Clear" } }
    ul { visible as t order by id desc select TodoItem(t) }
    Panel("Todo") { children }
    td(class="col-md-6")
  }"#,
    );
    let expect = [
        "(view main (el section (attr class \"todoapp\")",
        "(el h1 (text \"todos\"))",
        "(el input (mod autofocus) (attr id \"toggle-all\") (attr type \"checkbox\") (attr aria-hidden \"true\") (attr checked (cmp = active 0)) (on change ((done checked)) ((do ToggleAll done))))",
        "(el label (attr for \"toggle-all\") (text \"Mark all\"))",
        "(el span (attr class \"count\") (el strong (bind active)) (text \" items left\"))",
        "(el li (el a (attr class.selected (cmp = filter All)) (on click () ((do SetFilter All))) (text \"All\")))",
        "(el td (bind .sender.name) (text \":\"))",
        "(el span (bind (concat \"Current user: \" (compose current name))))",
        "(if (cmp > total 0) ((el footer (attr class \"footer\") (el button (on click () ((do ClearCompleted ) (clear))) (text \"Clear\")))))",
        "(el ul (select visible as t (order id desc) (component TodoItem (t))))",
        "(component Panel (\"Todo\") (children))",
        "(el td (attr class \"col-md-6\"))",
    ];
    for e in expect {
        assert!(out.contains(e), "missing {e}\nin {out}");
    }
}

#[test]
fn v1_components_locals_and_focus() {
    let out = sexpr(
        "view TodoItem(t: Todo) =\n  local editing = False\n  li(class.editing=editing) {\n    label(on dblclick { set editing = True; focus(.edit) }) { .text }\n    button(on click(pos = endOf(c)) { do AddCard(l, pos); focus(c) }) \"+\"\n  }",
    );
    assert!(out.starts_with("(view TodoItem ((t Todo)) (local editing _ False) (el li"), "{out}");
    assert!(out.contains("(on dblclick () ((set editing True) (focus .edit)))"), "{out}");
    assert!(out.contains("(on click ((pos endOf(c))) ((do AddCard l pos) (focus c)))"), "{out}");
}

#[test]
fn v1_select_over_derived_relation_and_drag_extractors() {
    let out = sexpr(
        "view main =\n  List as l order by .pos select\n    section(dropTarget on drop(card = drag(Card), pos = dropPos(c, card)) => do MoveCard(card, l, pos)) {\n      Card as c where .list = l order by .pos select div(draggable) { .title }\n    }",
    );
    assert!(out.contains("(on drop ((card drag(Card)) (pos dropPos(c card))) ((do MoveCard card l pos)))"), "{out}");
    assert!(out.contains("(select Card as c (where (cmp = .list l)) (order .pos) (el div (mod draggable) (bind .title)))"), "{out}");
}

#[test]
fn v1_rejects_old_spellings() {
    // `:field` paths and `||` are gone; `R - S` parses (as arithmetic) and is
    // left to the checker to reject.
    assert!(!parse("let x = :qty").diagnostics.is_empty());
    assert!(!parse("let x = a || b").diagnostics.is_empty());
}
