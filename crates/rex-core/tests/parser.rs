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
    assert_eq!(expr(":qty"), ":qty");
    assert_eq!(expr(":product.price"), ":product.price");
    assert_eq!(expr(":order.customer.region"), ":order.customer.region");
}

// --- precedence -----------------------------------------------------------

#[test]
fn compose_binds_tighter_than_mul() {
    // `:qty * :product.price` = mul(:qty, :product.price), and the path hop
    // `.price` binds tightest of all.
    assert_eq!(expr(":qty * :product.price"), "(mul :qty :product.price)");
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
    assert_eq!(expr("a , b + c . d"), "(fork a (union b (compose c d)))");
}

#[test]
fn where_between_set_and_compose() {
    assert_eq!(expr("a where b . c"), "(where a (compose b c))");
    assert_eq!(expr("a where b + c"), "(union (where a b) c)");
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
        expr(":region in (@west + @east)"),
        "(in :region (union @west @east))"
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
        expr("lineprice by :order.customer"),
        "(by lineprice :order.customer)"
    );
}

#[test]
fn calls() {
    assert_eq!(expr("count(a)"), "(call count a)");
    assert_eq!(
        expr("sum(lineprice by :order.customer)"),
        "(call sum (by lineprice :order.customer))"
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
        sexpr("entity Customer { region: {@north + @south} }"),
        "(entity Customer (field region (coproduct @north @south)))"
    );
}

#[test]
fn let_with_and_without_annotation() {
    assert_eq!(sexpr("let x = 42"), "(let x _ 42)");
    assert_eq!(
        sexpr("let f : Line -> Money = :qty"),
        "(let f (-> Line Money) :qty)"
    );
}

#[test]
fn recursive_let() {
    assert_eq!(
        sexpr("let recursive path : Node -> Node = edge + edge . path"),
        "(letrec path (-> Node Node) (union edge (compose edge path)))"
    );
    // The annotation is optional at parse time; the checker requires it.
    assert_eq!(sexpr("let recursive p = e + e . p"), "(letrec p _ (union e (compose e p)))");
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
(let lineprice (-> Line Money) (mul :qty :product.price))
(let custspend (-> Customer Money) (call sum (by lineprice :order.customer)))
(let inregion Customer (where id (in :region (union @west @east))))
(let result (-> Customer Money) (compose inregion (where custspend (cmp > _ 30))))";
    assert_eq!(sx, expected);
}
