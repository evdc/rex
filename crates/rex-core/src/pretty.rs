//! Canonical s-expression printer for the AST. Used for readable test
//! assertions and for the CLI's `--ast` output. This is a *canonical* form:
//! two ASTs are equal iff their s-expressions are equal.

use crate::ast::*;

pub fn program_to_sexpr(program: &Program) -> String {
    program
        .stmts
        .iter()
        .map(stmt_to_sexpr)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn stmt_to_sexpr(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Entity(e) => {
            let fields = e
                .fields
                .iter()
                .map(|f| format!("(field {} {})", f.name, type_to_sexpr(&f.ty)))
                .collect::<Vec<_>>()
                .join(" ");
            if fields.is_empty() {
                format!("(entity {})", e.name)
            } else {
                format!("(entity {} {})", e.name, fields)
            }
        }
        Stmt::Let(l) => {
            let name = l.name.as_deref().unwrap_or("_");
            let ty = l
                .ty
                .as_ref()
                .map(type_to_sexpr)
                .unwrap_or_else(|| "_".to_string());
            let head = if l.recursive { "letrec" } else { "let" };
            format!("({} {} {} {})", head, name, ty, expr_to_sexpr(&l.body))
        }
        Stmt::Rel(r) => format!("(rel {} {} {})", r.name, r.from, r.to),
        Stmt::View(v) => format!("(view {} {})", v.name, select_to_sexpr(&v.body)),
        Stmt::State(s) => format!(
            "(state {} {} {})",
            s.name,
            type_to_sexpr(&s.ty),
            expr_to_sexpr(&s.default)
        ),
    }
}

fn select_to_sexpr(s: &SelectExpr) -> String {
    let mut parts = vec![match &s.binder {
        Some(b) => format!("select {} as {b}", s.entity),
        None => format!("select {}", s.entity),
    }];
    for w in &s.wheres {
        parts.push(format!("(where {})", expr_to_sexpr(w)));
    }
    if let Some(o) = &s.order_by {
        parts.push(format!("(order :{})", o.join(".")));
    }
    parts.push(element_to_sexpr(&s.body));
    format!("({})", parts.join(" "))
}

fn element_to_sexpr(e: &ElementExpr) -> String {
    let mut parts = vec![format!("el {}", e.tag)];
    for c in &e.classes {
        parts.push(format!(".{c}"));
    }
    for m in &e.modifiers {
        parts.push(format!("(mod {m})"));
    }
    for a in &e.attrs {
        let v = match &a.value {
            AttrValue::Static(s) => format!("{s:?}"),
            AttrValue::Bind(p) => format!(":{}", p.join(".")),
        };
        parts.push(format!("(attr {} {})", a.name, v));
    }
    for h in &e.handlers {
        parts.push(handler_to_sexpr(h));
    }
    for c in &e.children {
        parts.push(content_to_sexpr(c));
    }
    format!("({})", parts.join(" "))
}

fn content_to_sexpr(c: &Content) -> String {
    match c {
        Content::Element(e) => element_to_sexpr(e),
        Content::Select(s) => select_to_sexpr(s),
        Content::Text(t) => format!("(text {t:?})"),
        Content::Bind(p) => format!("(bind :{})", p.join(".")),
    }
}

fn handler_to_sexpr(h: &HandlerDecl) -> String {
    let mut ev = h.event.clone();
    for m in &h.modifiers {
        ev.push('.');
        ev.push_str(m);
    }
    let params = h
        .params
        .iter()
        .map(|p| format!("({} {})", p.name, extractor_to_sexpr(&p.extractor)))
        .collect::<Vec<_>>()
        .join(" ");
    let muts = h
        .body
        .iter()
        .map(mutation_to_sexpr)
        .collect::<Vec<_>>()
        .join(" ");
    format!("(on {ev} ({params}) ({muts}))")
}

fn extractor_to_sexpr(e: &Extractor) -> String {
    match e {
        Extractor::Value => "value".into(),
        Extractor::Checked => "checked".into(),
        Extractor::Drag(a) => format!("drag({a:?})"),
        Extractor::DropPos(a) => format!("dropPos({a})"),
        Extractor::EndOf(a) => format!("endOf({a})"),
        Extractor::Prompt(a) => format!("prompt({a:?})"),
    }
}

fn mutation_to_sexpr(m: &Mutation) -> String {
    match m {
        Mutation::Set { binder, field, value, .. } => format!(
            "(set {}:{} {})",
            binder.as_deref().unwrap_or("self"),
            field.join("."),
            expr_to_sexpr(value)
        ),
        Mutation::Delete { target, .. } => format!("(delete {target})"),
        Mutation::New { entity, fields, .. } => {
            let fs = fields
                .iter()
                .map(|f| format!("({} {})", f.name, expr_to_sexpr(&f.value)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("(new {entity} {fs})")
        }
    }
}

pub fn expr_to_sexpr(expr: &Expr) -> String {
    use ExprKind::*;
    match &expr.kind {
        Ident(s) => s.clone(),
        Id => "id".to_string(),
        FieldPath(parts) => format!(":{}", parts.join(".")),
        Atom(s) => format!("@{s}"),
        Int(n) => n.to_string(),
        Decimal(s) => s.clone(),
        Str(s) => format!("{s:?}"),
        Date { year, month, day } => format!("{year:04}-{month:02}-{day:02}"),

        Compose(a, b) => bin("compose", a, b),
        Fork(a, b) => bin("fork", a, b),
        Union(a, b) => bin("union", a, b),
        Intersect(a, b) => bin("intersect", a, b),
        Restrict(a, b) => bin("restrict", a, b),
        Inverse(a) => format!("(inverse {})", expr_to_sexpr(a)),
        Distinct(a) => format!("(distinct {})", expr_to_sexpr(a)),
        Proj(side, a) => {
            let name = match side {
                crate::ast::ProjSide::Fst => "fst",
                crate::ast::ProjSide::Snd => "snd",
            };
            format!("({name} {})", expr_to_sexpr(a))
        }
        Where(a, b) => bin("where", a, b),
        By(a, b) => bin("by", a, b),
        Except(a, b) => bin("except", a, b),
        Antijoin(a, b) => bin("antijoin", a, b),
        Mul(a, b) => bin("mul", a, b),
        Concat(a, b) => bin("concat", a, b),

        Compare { op, lhs, rhs } => {
            let l = lhs
                .as_ref()
                .map(|e| expr_to_sexpr(e))
                .unwrap_or_else(|| "_".to_string());
            format!("(cmp {} {} {})", op.symbol(), l, expr_to_sexpr(rhs))
        }
        In { lhs, rhs } => {
            let l = lhs
                .as_ref()
                .map(|e| expr_to_sexpr(e))
                .unwrap_or_else(|| "_".to_string());
            format!("(in {} {})", l, expr_to_sexpr(rhs))
        }

        Call { func, args } => {
            let args = args
                .iter()
                .map(expr_to_sexpr)
                .collect::<Vec<_>>()
                .join(" ");
            if args.is_empty() {
                format!("(call {func})")
            } else {
                format!("(call {func} {args})")
            }
        }
        New { entity, fields } => {
            let fields = fields
                .iter()
                .map(|f| format!("(field {} {})", f.name, expr_to_sexpr(&f.value)))
                .collect::<Vec<_>>()
                .join(" ");
            if fields.is_empty() {
                format!("(new {entity})")
            } else {
                format!("(new {entity} {fields})")
            }
        }
    }
}

pub fn type_to_sexpr(ty: &Type) -> String {
    match &ty.kind {
        TypeKind::Named(s) => s.clone(),
        TypeKind::AtomSingleton(s) => format!("@{s}"),
        TypeKind::Arrow(a, b) => format!("(-> {} {})", type_to_sexpr(a), type_to_sexpr(b)),
        TypeKind::Product(a, b) => format!("(* {} {})", type_to_sexpr(a), type_to_sexpr(b)),
        TypeKind::Coproduct(elems) => {
            let elems = elems
                .iter()
                .map(type_to_sexpr)
                .collect::<Vec<_>>()
                .join(" ");
            format!("(coproduct {elems})")
        }
    }
}

fn bin(head: &str, a: &Expr, b: &Expr) -> String {
    format!("({} {} {})", head, expr_to_sexpr(a), expr_to_sexpr(b))
}
