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
        Stmt::View(v) => {
            let mut parts = vec![format!("view {}", v.name)];
            if !v.params.is_empty() {
                parts.push(format!("({})", params_to_sexpr(&v.params)));
            }
            for l in &v.locals {
                let ty = l.ty.as_ref().map(type_to_sexpr).unwrap_or_else(|| "_".into());
                parts.push(format!("(local {} {} {})", l.name, ty, expr_to_sexpr(&l.default)));
            }
            parts.push(match &v.body {
                ViewBody::Select(s) => select_to_sexpr(s),
                ViewBody::Element(e) => element_to_sexpr(e),
            });
            format!("({})", parts.join(" "))
        }
        Stmt::State(s) => format!(
            "(state {} {} {})",
            s.name,
            type_to_sexpr(&s.ty),
            s.default.as_ref().map(expr_to_sexpr).unwrap_or_else(|| "_".into())
        ),
        Stmt::Event(e) => format!("(event {} ({}))", e.name, params_to_sexpr(&e.params)),
        Stmt::On(o) => format!(
            "(on {} ({}) ({}))",
            o.event,
            o.params.join(" "),
            hstmts_to_sexpr(&o.body)
        ),
        Stmt::Type(t) => format!("(type {} {})", t.name, t.ctors.join(" ")),
        Stmt::Import(i) => format!("(import {:?} {})", i.path, i.alias),
    }
}

fn params_to_sexpr(params: &[Param]) -> String {
    params
        .iter()
        .map(|p| format!("({} {})", p.name, type_to_sexpr(&p.ty)))
        .collect::<Vec<_>>()
        .join(" ")
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
        let dir = if o.desc { " desc" } else { "" };
        parts.push(format!("(order {}{dir})", expr_to_sexpr(&o.expr)));
    }
    parts.push(content_to_sexpr(&s.body));
    format!("({})", parts.join(" "))
}

fn element_to_sexpr(e: &ElementExpr) -> String {
    let mut parts = vec![format!("el {}", e.tag)];
    for m in &e.modifiers {
        parts.push(format!("(mod {m})"));
    }
    for a in &e.attrs {
        let v = match &a.value {
            AttrValue::Static(s) => format!("{s:?}"),
            AttrValue::Bind(x) => expr_to_sexpr(x),
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
        Content::Bind(x) => format!("(bind {})", expr_to_sexpr(x)),
        Content::If { cond, children, .. } => {
            let kids = children.iter().map(content_to_sexpr).collect::<Vec<_>>().join(" ");
            format!("(if {} ({kids}))", expr_to_sexpr(cond))
        }
        Content::Component { name, args, children, .. } => {
            let args = args.iter().map(expr_to_sexpr).collect::<Vec<_>>().join(" ");
            match children {
                Some(kids) => {
                    let kids = kids.iter().map(content_to_sexpr).collect::<Vec<_>>().join(" ");
                    format!("(component {name} ({args}) ({kids}))")
                }
                None => format!("(component {name} ({args}))"),
            }
        }
        Content::ChildrenSlot(_) => "children".into(),
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
        .map(|p| match &p.ty {
            Some(t) => format!("({} {} {})", p.name, type_to_sexpr(t), extractor_to_sexpr(&p.extractor)),
            None => format!("({} {})", p.name, extractor_to_sexpr(&p.extractor)),
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("(on {ev} ({params}) ({}))", hstmts_to_sexpr(&h.body))
}

fn extractor_to_sexpr(e: &Extractor) -> String {
    match e {
        Extractor::Value => "value".into(),
        Extractor::Checked => "checked".into(),
        Extractor::Drag(a) => format!("drag({a})"),
        Extractor::DropPos { level, exclude } => format!("dropPos({level} {exclude})"),
        Extractor::EndOf(a) => format!("endOf({a})"),
        Extractor::Js { module, func, args } => {
            let args = args.iter().map(expr_to_sexpr).collect::<Vec<_>>().join(" ");
            format!("(js {module}.{func} {args})")
        }
    }
}

fn hstmts_to_sexpr(stmts: &[HStmt]) -> String {
    stmts.iter().map(hstmt_to_sexpr).collect::<Vec<_>>().join(" ")
}

fn hstmt_to_sexpr(m: &HStmt) -> String {
    match m {
        HStmt::New { bind, entity, from, fields, .. } => {
            let fs = field_inits_to_sexpr(fields);
            let mut s = format!("(new {entity}");
            if let Some(b) = bind {
                s = format!("(new {b} = {entity}");
            }
            if let Some(f) = from {
                s.push_str(&format!(" (from {} {} {})", expr_to_sexpr(&f.source), f.key, f.value));
            }
            if !fs.is_empty() {
                s.push(' ');
                s.push_str(&fs);
            }
            s.push(')');
            s
        }
        HStmt::Assign { binder, field, value, .. } => format!(
            "(assign {}.{} {})",
            binder.as_deref().unwrap_or("self"),
            field,
            expr_to_sexpr(value)
        ),
        HStmt::Update { target, sets, .. } => {
            format!("(update {} {})", expr_to_sexpr(target), field_inits_to_sexpr(sets))
        }
        HStmt::Delete { target, .. } => format!("(delete {})", expr_to_sexpr(target)),
        HStmt::Set { name, value, .. } => format!("(set {name} {})", expr_to_sexpr(value)),
        HStmt::Do { event, args, .. } => {
            let args = args.iter().map(expr_to_sexpr).collect::<Vec<_>>().join(" ");
            format!("(do {event} {args})")
        }
        HStmt::Clear { .. } => "(clear)".into(),
        HStmt::Focus { target, .. } => match target {
            FocusTarget::Level(l) => format!("(focus {l})"),
            FocusTarget::Class(c) => format!("(focus .{c})"),
        },
    }
}

fn field_inits_to_sexpr(fields: &[FieldInit]) -> String {
    fields
        .iter()
        .map(|f| format!("({} {})", f.name, expr_to_sexpr(&f.value)))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn expr_to_sexpr(expr: &Expr) -> String {
    use ExprKind::*;
    match &expr.kind {
        Ident(s) => s.clone(),
        Id => "id".to_string(),
        FieldPath(parts) => format!(".{}", parts.join(".")),
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
        Not(a) => format!("(not {})", expr_to_sexpr(a)),
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
        Add(a, b) => bin("add", a, b),
        Sub(a, b) => bin("sub", a, b),
        Mul(a, b) => bin("mul", a, b),
        Div(a, b) => bin("div", a, b),
        Mod(a, b) => bin("mod", a, b),
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

        Match { scrutinee, arms } => {
            let arms = arms
                .iter()
                .map(|a| format!("({} {})", pattern_to_sexpr(&a.pat), expr_to_sexpr(&a.body)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("(match {} {arms})", expr_to_sexpr(scrutinee))
        }
        If { cond, then, els } => format!(
            "(if {} {} {})",
            expr_to_sexpr(cond),
            expr_to_sexpr(then),
            expr_to_sexpr(els)
        ),

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

fn pattern_to_sexpr(p: &Pattern) -> String {
    match p {
        Pattern::Ident(s) => s.clone(),
        Pattern::Atom(a) => format!("@{a}"),
        Pattern::Int(n) => n.to_string(),
        Pattern::Str(s) => format!("{s:?}"),
        Pattern::Wildcard => "_".into(),
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
