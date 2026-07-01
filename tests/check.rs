//! Type-checker tests: well-typed acceptance (incl. SPEC §12) and the headline
//! rejection cases (§3.2 column hazard, §4 co-keying).

use rex::check_source;

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .map(|d| d.message)
        .collect()
}

fn assert_ok(src: &str) {
    let errs = errors(src);
    assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
}

/// Assert there is at least one error containing `needle`.
fn assert_error_contains(src: &str, needle: &str) {
    let errs = errors(src);
    assert!(
        errs.iter().any(|e| e.contains(needle)),
        "expected an error containing {needle:?}, got: {errs:?}"
    );
}

const SCHEMA: &str = "\
entity Customer { name: Text, region: {@north + @south + @east + @west} }
entity Product  { name: Text, price: Money }
entity Order    { customer: CustomerID, placed: Date }
entity Line     { order: OrderID, product: ProductID, qty: Int }
";

fn with_schema(body: &str) -> String {
    format!("{SCHEMA}{body}")
}

// --- acceptance -----------------------------------------------------------

#[test]
fn spec12_fixture_typechecks_clean() {
    let src = include_str!("fixtures/spec12.rex");
    assert_ok(src);
}

#[test]
fn entity_and_simple_view() {
    assert_ok(&with_schema("let lineprice : Line -> Money = :qty * :product.price\n"));
}

#[test]
fn multi_hop_field_path() {
    // :order.customer resolves `order` in Line, then `.customer` in Order.
    assert_ok(&with_schema(
        "let cust : Line -> CustomerID = :order.customer\n",
    ));
}

#[test]
fn aggregation_by_regrouping() {
    assert_ok(&with_schema(
        "let lineprice : Line -> Money = :qty * :product.price\n\
         let custspend : Customer -> Money = sum(lineprice by :order.customer)\n",
    ));
}

#[test]
fn subset_of_entity_via_id_where() {
    assert_ok(&with_schema(
        "let inregion : Customer = id where :region in (@west + @east)\n",
    ));
}

#[test]
fn new_creation_typechecks() {
    assert_ok(&with_schema(
        "let alice = new Customer { name: \"Alice\", region: @west }\n\
         let o1 = new Order { customer: alice, placed: 2026-01-15 }\n",
    ));
}

// --- rejection: field/type errors ----------------------------------------

#[test]
fn unknown_field_is_rejected() {
    assert_error_contains(
        &with_schema("let bad : Line -> Money = :nonesuch\n"),
        "unknown field `nonesuch`",
    );
}

#[test]
fn unknown_type_is_rejected() {
    assert_error_contains(
        &with_schema("let bad : Nope -> Money = :qty\n"),
        "unknown type `Nope`",
    );
}

#[test]
fn wrong_field_value_type_in_new() {
    assert_error_contains(
        &with_schema("let p = new Product { name: \"W\", price: \"cheap\" }\n"),
        "field `price` expects `Money` but got `Text`",
    );
}

#[test]
fn atom_not_in_coproduct_is_rejected() {
    assert_error_contains(
        &with_schema("let c = new Customer { name: \"A\", region: @middle }\n"),
        "region",
    );
}

#[test]
fn id_sort_discipline_across_entities() {
    // `alice : CustomerID` used where a ProductID is expected.
    assert_error_contains(
        &with_schema(
            "let alice = new Customer { name: \"A\", region: @west }\n\
             let bad = new Line { order: alice, product: alice, qty: 1 }\n",
        ),
        "expects `OrderID`",
    );
}

// --- rejection: the §3.2 semijoin hazard ----------------------------------

#[test]
fn semijoin_on_value_column_is_a_type_error() {
    // `custspend[custspend . > 30]`: R's right column is Money, but the inner
    // relation's left column is CustomerID -> the join columns don't match.
    let src = with_schema(
        "let lineprice : Line -> Money = :qty * :product.price\n\
         let custspend : Customer -> Money = sum(lineprice by :order.customer)\n\
         let bad : Customer -> Money = custspend[custspend . > 30]\n",
    );
    assert_error_contains(&src, "join column mismatch");
}

// --- rejection: §4 co-keying ----------------------------------------------

#[test]
fn comparison_operands_must_be_cokeyed() {
    // Comparing a Line-keyed column with a Customer-keyed view.
    let src = with_schema(
        "let lineprice : Line -> Money = :qty * :product.price\n\
         let custspend : Customer -> Money = sum(lineprice by :order.customer)\n\
         let bad : Line -> Line = :qty > custspend\n",
    );
    assert_error_contains(&src, "not co-keyed");
}

// --- rejection: §9.1 groundedness -----------------------------------------

#[test]
fn ungrounded_standalone_comparison_is_rejected() {
    // `> 30` standing alone over `Int` is an infinite relation with no finite
    // domain to enumerate.
    assert_error_contains("let bad : Int -> Int = > 30\n", "ungrounded");
}

#[test]
fn ungrounded_standalone_equality_is_rejected() {
    assert_error_contains("let bad : Text -> Text = = \"x\"\n", "ungrounded");
}

#[test]
fn grounded_filter_is_ok() {
    // The same comparison in filter position is grounded by the finite relation
    // it filters, so it must NOT be rejected.
    let src = with_schema(
        "let lineprice : Line -> Money = :qty * :product.price\n\
         let custspend : Customer -> Money = sum(lineprice by :order.customer)\n\
         let big : Customer -> Money = custspend where > 30\n",
    );
    assert_ok(&src);
}
