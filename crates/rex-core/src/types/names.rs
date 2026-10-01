//! Declarations that collide. Run on the source program, before desugaring:
//! every later pass assumes a name means one thing, and several of them
//! assume a row is given each field once.
//!
//! Left unchecked these were silent: the second `state s` wrote its default
//! into the same cell as the first (two values in a field the engine relies
//! on holding one); a second `view main` emitted a second shape with the same
//! name, which the shaper refuses at boot; `entity Bool` quietly replaced the
//! built-in.

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

/// Names the language itself gives a type; a program may not take them.
const BUILTIN_TYPES: &[&str] = &["Int", "Text", "Money", "Date", "Bool", "Unit"];

pub fn check_names(program: &Program) -> Vec<Diagnostic> {
    let mut cx = Names::default();
    for stmt in &program.stmts {
        cx.stmt(stmt);
    }
    cx.diagnostics
}

#[derive(Default)]
struct Names {
    /// Everything an expression can name: entities, types, constructors,
    /// states and views-as-relations (`let`s). One namespace — a name is
    /// resolved without knowing which of these it is.
    values: HashMap<String, &'static str>,
    /// `view`s: root views and components, named in element position.
    views: HashMap<String, ()>,
    diagnostics: Vec<Diagnostic>,
}

impl Names {
    fn error(&mut self, span: Span, message: String) {
        self.diagnostics.push(Diagnostic::error(span, message));
    }

    /// Claim `name` as a `kind`. Two entities or two types are already
    /// reported by the checker with their own wording, so `quiet` skips the
    /// same-kind case for those.
    fn declare(&mut self, name: &str, kind: &'static str, span: Span, quiet: bool) {
        match self.values.get(name).copied() {
            // A binding may shadow a constructor (S-50): the built-in meaning
            // of a name applies only while nothing else has bound it.
            Some(prev) if matches!((prev, kind), ("constructor", "binding") | ("binding", "constructor")) => {
                self.values.insert(name.to_string(), "binding");
            }
            Some(prev) if prev == kind && quiet => {}
            Some(prev) if prev == kind => self.error(span, format!("{kind} `{name}` is already defined")),
            Some(prev) => self.error(span, format!("`{name}` is already defined as {} {prev}", article(prev))),
            None => {
                self.values.insert(name.to_string(), kind);
            }
        }
    }

    fn type_name(&mut self, name: &str, kind: &'static str, span: Span) {
        if BUILTIN_TYPES.contains(&name) {
            self.error(span, format!("`{name}` is a built-in type and cannot be redefined as {} {kind}", article(kind)));
            return;
        }
        self.declare(name, kind, span, true);
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Entity(e) => {
                self.type_name(&e.name, "entity", e.span);
                let mut seen: HashMap<&str, ()> = HashMap::new();
                for f in &e.fields {
                    if seen.insert(&f.name, ()).is_some() {
                        self.error(f.span, format!("entity `{}` already has a field `{}`", e.name, f.name));
                    }
                }
            }
            Stmt::Type(t) => {
                self.type_name(&t.name, "type", t.span);
                for c in &t.ctors {
                    // Two types sharing a constructor is the checker's own
                    // error; here, a constructor against everything else.
                    if self.values.get(c.as_str()).is_some_and(|k| *k != "constructor") {
                        self.declare(c, "constructor", t.span, true);
                    } else {
                        self.values.insert(c.clone(), "constructor");
                    }
                }
            }
            Stmt::State(s) => self.declare(&s.name, "state", s.span, false),
            Stmt::Rel(r) => self.declare(&r.name, "relation", r.span, false),
            Stmt::Let(l) => {
                // `let unit = …` deliberately shadows the built-in (S-50), and
                // a `let x = new …` binds a value: neither is in the table
                // until here, so only a clash with a declaration is caught.
                if let Some(name) = &l.name {
                    self.declare(name, "binding", l.span, false);
                }
                self.expr(&l.body);
            }
            Stmt::View(v) => {
                if self.views.insert(v.name.clone(), ()).is_some() {
                    self.error(v.span, format!("view `{}` is already defined", v.name));
                }
            }
            Stmt::On(on) => on.body.iter().for_each(|h| self.hstmt(h)),
            _ => {}
        }
    }

    fn hstmt(&mut self, h: &HStmt) {
        match h {
            HStmt::New { entity, fields, .. } => self.fields(fields, &format!("`new {entity}`")),
            HStmt::Update { sets, .. } => self.fields(sets, "this `update`"),
            _ => {}
        }
    }

    /// A top-level `new E { … }`, wherever it sits in a `let` body.
    fn expr(&mut self, e: &Expr) {
        if let ExprKind::New { entity, fields } = &e.kind {
            self.fields(fields, &format!("`new {entity}`"));
        }
    }

    fn fields(&mut self, fields: &[FieldInit], what: &str) {
        let mut seen: HashMap<&str, ()> = HashMap::new();
        for f in fields {
            if seen.insert(&f.name, ()).is_some() {
                self.error(f.span, format!("{what} sets `{}` twice", f.name));
            }
        }
    }
}

fn article(kind: &str) -> &'static str {
    if kind.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" }
}
