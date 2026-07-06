//! Tests for the elaboration pass: the checker emits a typed AST with resolved
//! field paths, resolved identifiers, and grounded filter predicates.

use rex::parse;
use rex::types::check;
use rex::types::ty::ValueTy;
use rex::types::typed::{AggKind, Lit, Pred, TExpr, TExprKind, TStmt};

fn elaborate(src: &str) -> rex::types::typed::TProgram {
    let parsed = parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse errors: {:?}", parsed.diagnostics);
    let checked = check::check(&parsed.program);
    assert!(
        checked.diagnostics.is_empty(),
        "type errors: {:?}",
        checked.diagnostics
    );
    checked.elaborated.expect("elaborated program")
}

/// Find the body of the view `let name = ...`.
fn view_body<'a>(prog: &'a rex::types::typed::TProgram, name: &str) -> &'a TExpr {
    prog.stmts
        .iter()
        .find_map(|s| match s {
            TStmt::Let { name: Some(n), body } if n == name => Some(body),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no view `{name}`"))
}

const SCHEMA: &str = "\
entity Customer { name: Text, region: {@north + @south + @east + @west} }
entity Product  { name: Text, price: Money }
entity Order    { customer: CustomerID, placed: Date }
entity Line     { order: OrderID, product: ProductID, qty: Int }
";

#[test]
fn ill_typed_program_yields_no_elaboration() {
    let parsed = parse(&format!("{SCHEMA}let bad : Line -> Money = :nonesuch\n"));
    let checked = check::check(&parsed.program);
    assert!(!checked.diagnostics.is_empty());
    assert!(checked.elaborated.is_none());
}

#[test]
fn field_path_resolves_to_hops_with_sorts() {
    let prog = elaborate(&format!(
        "{SCHEMA}let cust : Line -> CustomerID = :order.customer\n"
    ));
    let body = view_body(&prog, "cust");
    let TExprKind::Field(hops) = &body.kind else {
        panic!("expected a resolved field path, got {:?}", body.kind);
    };
    // Two hops: `order` in Line, then `customer` in Order.
    assert_eq!(hops.len(), 2);
    assert_eq!(hops[0].field, "order");
    assert_eq!(hops[1].field, "customer");
    // The two hops resolve in *different* sorts (Line, then Order).
    assert_ne!(hops[0].sort, hops[1].sort);
    // The result type is Line -> CustomerID.
    assert!(matches!(body.ty.from, ValueTy::Id(_)));
    assert!(matches!(body.ty.to, ValueTy::Id(_)));
}

#[test]
fn where_comparison_folds_into_a_filter() {
    let prog = elaborate(&format!(
        "{SCHEMA}\
         let lineprice : Line -> Money = :qty * :product.price\n\
         let custspend : Customer -> Money = sum(lineprice by :order.customer)\n\
         let big : Customer -> Money = custspend where > 30\n"
    ));
    let body = view_body(&prog, "big");
    let TExprKind::Filter(inner, pred) = &body.kind else {
        panic!("expected `where > 30` to fold into a Filter, got {:?}", body.kind);
    };
    assert!(matches!(inner.kind, TExprKind::View(_)));
    assert_eq!(*pred, Pred::Cmp(rex::ast::CmpOp::Gt, Lit::Int(30)));
}

#[test]
fn key_semijoin_stays_a_semijoin() {
    // Restricting by a computed customer-set is a real semijoin, not a filter.
    let prog = elaborate(&format!(
        "{SCHEMA}\
         let inregion : Customer = id where :region in (@west + @east)\n\
         let named : Customer -> Text = inregion . :name\n"
    ));
    // `inregion`'s body: `id where (:region in ...)` — the `in` is a coreflexive
    // relation, so `where` is a Semijoin over it (not a value Filter).
    let body = view_body(&prog, "inregion");
    assert!(
        matches!(body.kind, TExprKind::Semijoin(_, _)),
        "expected a Semijoin, got {:?}",
        body.kind
    );
}

#[test]
fn aggregation_and_by_are_resolved() {
    let prog = elaborate(&format!(
        "{SCHEMA}\
         let lineprice : Line -> Money = :qty * :product.price\n\
         let custspend : Customer -> Money = sum(lineprice by :order.customer)\n"
    ));
    let body = view_body(&prog, "custspend");
    let TExprKind::Agg(kind, arg) = &body.kind else {
        panic!("expected an aggregation, got {:?}", body.kind);
    };
    assert_eq!(*kind, AggKind::Sum);
    assert!(matches!(arg.kind, TExprKind::By(_, _)));
}

// --- recursion (§8) ---------------------------------------------------------

#[test]
fn recursion_group_elaborates_to_letrec_with_recvar() {
    let prog = elaborate(
        "entity Node { name: Text }\n\
         entity Edge { src: NodeID, dst: NodeID }\n\
         let srcof : Edge -> Node = :src\n\
         let dstof : Edge -> Node = :dst\n\
         let edge : Node -> Node = dstof by srcof\n\
         let recursive path : Node -> Node = edge + edge . path\n",
    );
    let bindings = prog
        .stmts
        .iter()
        .find_map(|s| match s {
            TStmt::LetRec { bindings } => Some(bindings),
            _ => None,
        })
        .expect("a LetRec group");
    assert_eq!(bindings.len(), 1);
    let (name, body) = &bindings[0];
    assert_eq!(name, "path");
    // edge + edge . path  ==>  Union(View(edge), Compose(View(edge), RecVar(path)))
    let TExprKind::Union(l, r) = &body.kind else {
        panic!("expected Union, got {:?}", body.kind)
    };
    assert!(matches!(&l.kind, TExprKind::View(n) if n == "edge"));
    let TExprKind::Compose(cl, cr) = &r.kind else {
        panic!("expected Compose, got {:?}", r.kind)
    };
    assert!(matches!(&cl.kind, TExprKind::View(n) if n == "edge"));
    assert!(matches!(&cr.kind, TExprKind::RecVar(n) if n == "path"));
}

#[test]
fn mutual_group_is_one_letrec_in_binding_order() {
    let prog = elaborate(
        "entity Node { name: Text }\n\
         entity Edge { src: NodeID, dst: NodeID }\n\
         let srcof : Edge -> Node = :src\n\
         let dstof : Edge -> Node = :dst\n\
         let edge : Node -> Node = dstof by srcof\n\
         let recursive odd : Node -> Node = edge + edge . even\n\
         let recursive even : Node -> Node = edge . odd\n",
    );
    let groups: Vec<_> = prog
        .stmts
        .iter()
        .filter_map(|s| match s {
            TStmt::LetRec { bindings } => Some(bindings),
            _ => None,
        })
        .collect();
    assert_eq!(groups.len(), 1, "consecutive recursive lets form ONE group");
    let names: Vec<&str> = groups[0].iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["odd", "even"]);
}
