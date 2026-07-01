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
            format!("(let {} {} {})", name, ty, expr_to_sexpr(&l.body))
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
