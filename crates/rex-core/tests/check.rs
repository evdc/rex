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
fn fst_snd_project_fork_components() {
    assert_ok(&with_schema(
        "let pair : Line -> Int * Money = (:qty , :product.price)\n\
         let qty  : Line -> Int   = fst pair\n\
         let prc  : Line -> Money = snd pair\n",
    ));
}

#[test]
fn aggregating_a_pair_warns_of_the_incrementality_cliff_but_compiles() {
    let src = with_schema(
        "let pair : Line -> Int * Money = (:qty , :product.price)\n\
         let n : Line -> Int = count(pair)\n",
    );
    let parsed = rex::parse(&src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    let warnings: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| d.severity == rex::diagnostic::Severity::Warning)
        .collect();
    assert!(
        warnings.iter().any(|d| d.message.contains("incrementality cliff")),
        "expected a cliff warning, got: {:?}",
        checked.diagnostics
    );
    // A warning marks the cliff; it never blocks the program.
    assert!(checked.elaborated.is_some(), "warnings must not block elaboration");
}

#[test]
fn fst_on_non_pair_is_rejected() {
    assert_error_contains(
        &with_schema("let bad : Line -> Int = fst :qty\n"),
        "needs a pair-valued relation",
    );
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

// --- recursion (§8) ---------------------------------------------------------

const GRAPH: &str = "\
entity Node { name: Text }
entity Edge { src: NodeID, dst: NodeID }
let srcof : Edge -> Node = :src
let dstof : Edge -> Node = :dst
let edge : Node -> Node = dstof by srcof
";

fn with_graph(body: &str) -> String {
    format!("{GRAPH}{body}")
}

#[test]
fn recursive_self_reference_resolves() {
    assert_ok(&with_graph(
        "let recursive path : Node -> Node = edge + edge . path\n",
    ));
}

#[test]
fn non_recursive_self_reference_still_errors() {
    assert_error_contains(
        &with_graph("let path : Node -> Node = edge + edge . path\n"),
        "unknown name `path`",
    );
}

#[test]
fn recursive_view_requires_annotation() {
    assert_error_contains(
        &with_graph("let recursive path = edge + edge . path\n"),
        "needs a type annotation",
    );
}

#[test]
fn recursive_new_is_rejected() {
    assert_error_contains(
        &with_graph("let recursive x : Node -> Node = new Node { name: \"x\" }\n"),
        "`new` cannot be `recursive`",
    );
}

#[test]
fn recursive_type_mismatch_is_reported() {
    // Body is Edge -> Node, declared Node -> Node.
    assert_error_contains(
        &with_graph("let recursive p : Node -> Node = dstof\n"),
        "type mismatch",
    );
}

#[test]
fn mutual_recursion_group_checks() {
    // Consecutive recursive lets form one group; `odd` may reference the
    // not-yet-defined `even`.
    assert_ok(&with_graph(
        "let recursive odd : Node -> Node = edge + edge . even\n\
         let recursive even : Node -> Node = edge . odd\n",
    ));
}

#[test]
fn non_recursive_statement_breaks_the_group() {
    // `mid` separates the two recursive lets, so `odd` cannot see `even`.
    assert_error_contains(
        &with_graph(
            "let recursive odd : Node -> Node = edge + edge . even\n\
             let mid : Node -> Node = edge\n\
             let recursive even : Node -> Node = edge . odd\n",
        ),
        "unknown name `even`",
    );
}

// --- rejection: §8 stratification -------------------------------------------

#[test]
fn distinct_over_recursive_occurrence_is_rejected() {
    assert_error_contains(
        &with_graph("let recursive p : Node -> Node = edge + distinct(edge . p)\n"),
        "not monotone",
    );
}

#[test]
fn except_over_recursive_occurrence_is_rejected() {
    assert_error_contains(
        &with_graph("let recursive p : Node -> Node = edge except p\n"),
        "not monotone",
    );
}

#[test]
fn intersect_over_recursive_occurrence_is_allowed() {
    // `&` is elementwise min of weights — monotone in both arguments — so it
    // is legal under recursion (unlike distinct/except/aggregation).
    assert_ok(&with_graph("let recursive p : Node -> Node = edge & p\n"));
}

#[test]
fn aggregation_over_recursive_occurrence_is_rejected() {
    assert_error_contains(
        &with_graph("let recursive w : Node -> Int = sum(w by edge)\n"),
        "not monotone",
    );
}

#[test]
fn duplicate_names_in_a_group_are_rejected() {
    assert_error_contains(
        &with_graph(
            "let recursive p : Node -> Node = edge + edge . p\n\
             let recursive p : Node -> Node = edge . p\n",
        ),
        "duplicate recursive binding `p`",
    );
}

#[test]
fn non_monotone_off_the_recursive_path_is_fine() {
    // `distinct(edge)` contains no recursive occurrence (a constant of the
    // iteration), and applying `distinct` to the *converged* view in a later
    // statement is the stratum boundary — both legal.
    assert_ok(&with_graph(
        "let recursive p : Node -> Node = distinct(edge) + edge . p\n\
         let q : Node -> Node = distinct(p)\n",
    ));
}

// --- S-04: small checker/codegen bugs from README --------------------------

#[test]
fn where_field_eq_atom_on_a_view_level_is_accepted() {
    // `:region = @west` used to be rejected ("not co-keyed") because the
    // atom literal never grounded to the ambient entity domain the way
    // `:field` does; only `where :region in @west` worked.
    assert_ok(&with_schema(
        "view board =\n  Customer where :region = @west select\n    li { :name }\n",
    ));
}

#[test]
fn bare_identifier_in_element_body_is_an_error() {
    // A bare identifier naming a field of the enclosing entity, with no
    // attrs/children of its own, is almost always a forgotten `:` — used to
    // silently compile to a `<name>` tag instead.
    assert_error_contains(
        &with_schema("view board =\n  Customer select\n    li { name }\n"),
        "unknown element `name`; did you mean `{ :name }`?",
    );
}

#[test]
fn bare_identifier_that_is_a_real_tag_is_unaffected() {
    // A genuine element tag (not a field name of the enclosing entity)
    // still compiles as an element.
    assert_ok(&with_schema(
        "view board =\n  Customer select\n    li { span { :name } }\n",
    ));
}

#[test]
fn in_over_integer_literals_is_accepted() {
    // `expect_subset` used to require `.atoms()` on both sides, so `x in (1 +
    // 2 + 3)` over an `Int` field was rejected even though `collect_lits`
    // gathers the int literals fine.
    assert_ok(&with_schema(
        "view board =\n  Line where :qty in (1 + 2 + 3) select\n    li { :order }\n",
    ));
}

#[test]
fn in_over_mismatched_scalar_types_is_still_rejected() {
    assert_error_contains(
        &with_schema("let bad : Line -> Line = id[:qty in (\"x\" + \"y\")]\n"),
        "is not within the value type",
    );
}
