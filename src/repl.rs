//! An interactive REPL over the incremental engine: statements
//! (`entity`/`let`) accumulate into a growing session program; bare
//! expressions are evaluated against the live state without being kept, so
//! querying doesn't clutter the environment.
//!
//! The *front end* stays batch — each committed line re-parses and re-checks
//! the whole accumulated source (trivial at REPL scale, and it keeps SortIds
//! stable since entities mint in source order). *Evaluation* is incremental:
//! only the newly added statements are applied — a `new` becomes one atomic
//! base-table transaction, a `let` extends the circuit and backfills over the
//! data already integrated. Prior statements are never re-evaluated. Data
//! changes print each affected view's delta; `/retract` feeds the negated base
//! rows of an entity back through the same circuit.

use crate::dbsp::Engine;
use crate::eval::{self, BinaryRelation, BTreeRelation, Value};
use crate::types::env::{Binding, Env};
use crate::types::typed::{TStmt, TValue};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::collections::HashMap;

const MAX_ROWS: usize = 50;

pub fn run() {
    println!("rex REPL — enter `entity`/`let` statements or expressions. /help for commands.");
    let mut session = Session::new();
    let history_path = history_path();

    let mut rl = match DefaultEditor::new() {
        Ok(rl) => rl,
        Err(e) => {
            eprintln!("rex: could not start line editor: {e}");
            return;
        }
    };
    let _ = rl.load_history(&history_path);

    loop {
        match rl.readline("rex> ") {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with("//") {
                    continue;
                }
                let _ = rl.add_history_entry(&line);
                match trimmed {
                    "/quit" | "/exit" => break,
                    "/help" => print_help(),
                    "/env" => session.print_env(),
                    "/reset" => {
                        session = Session::new();
                        println!("session reset");
                    }
                    _ if trimmed.starts_with("/import") => {
                        session.import(trimmed["/import".len()..].trim());
                    }
                    _ if trimmed.starts_with("/retract") => {
                        session.retract(trimmed["/retract".len()..].trim());
                    }
                    _ if trimmed.starts_with('/') => {
                        println!("unknown command `{trimmed}` — try /help");
                    }
                    _ => session.eval_line(&line),
                }
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("rex: readline error: {e}");
                break;
            }
        }
    }
    let _ = rl.save_history(&history_path);
}

fn history_path() -> String {
    match std::env::var("HOME") {
        Ok(home) => format!("{home}/.rex_history"),
        Err(_) => ".rex_history".to_string(),
    }
}

fn print_help() {
    println!("commands:");
    println!("  <expr>            evaluate an expression (not kept in the session)");
    println!("  entity ...        define an entity (kept in the session)");
    println!("  let ...           define a binding (kept in the session)");
    println!("  /import <path>    load a .rex file's statements into the session");
    println!("  /retract <name>   retract a `new`-bound entity's rows (views update)");
    println!("  /env              list entities and bindings in the current session");
    println!("  /reset            clear the session");
    println!("  /help             show this message");
    println!("  /quit, /exit      leave the REPL");
}

/// Accumulated session state: committed source (the front end's input), how
/// much of it has already been applied, the type environment, and the live
/// incremental engine holding all data and views.
struct Session {
    src: String,
    /// AST statements already committed (for printing only the new ones).
    ast_count: usize,
    /// Typed statements already applied to the engine.
    typed_count: usize,
    env: Env,
    engine: Engine,
    /// `new`-bound entity ids, resolved incrementally as statements apply.
    values: HashMap<String, Value>,
}

/// [`eval::Store`] over the live engine: scratch expressions batch-evaluate
/// against the circuit's integrated base tables and views.
struct EngineStore<'a> {
    engine: &'a Engine,
    values: &'a HashMap<String, Value>,
}

impl eval::Store for EngineStore<'_> {
    fn field_rel(&self, sort: crate::types::ty::SortId, field: &str) -> BTreeRelation {
        self.engine
            .circuit
            .input_integral(&crate::dbsp::InputKey::Field(sort, crate::eval::intern(field)))
            .cloned()
            .unwrap_or_default()
    }

    fn identity_rel(&self, sort: crate::types::ty::SortId) -> BTreeRelation {
        self.engine
            .circuit
            .input_integral(&crate::dbsp::InputKey::Identity(sort))
            .cloned()
            .unwrap_or_default()
    }

    fn view_rel(&self, name: &str) -> BTreeRelation {
        self.engine.circuit.view(name).cloned().unwrap_or_default()
    }

    fn value(&self, name: &str) -> Value {
        self.values
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("unbound value `{name}`"))
    }
}

impl Session {
    fn new() -> Session {
        Session {
            src: String::new(),
            ast_count: 0,
            typed_count: 0,
            env: Env::new(),
            engine: Engine::new(),
            values: HashMap::new(),
        }
    }

    /// Evaluate one line of input. `entity`/`let` statements are committed to
    /// the session on success; anything else is checked, batch-evaluated
    /// against the engine's live state, and discarded afterward.
    fn eval_line(&mut self, line: &str) {
        let trimmed = line.trim();
        let is_decl = trimmed.starts_with("entity") || trimmed.starts_with("let");
        if is_decl {
            let candidate = format!("{}\n{}\n", self.src, line);
            self.commit(candidate);
            return;
        }

        let candidate = format!("{}\nlet __repl_result = {}\n", self.src, line);
        let Some((env, typed)) = check_source(&candidate) else {
            return;
        };
        let Some(TStmt::Let { body, .. }) = typed.stmts.last() else {
            println!("(nothing to evaluate)");
            return;
        };
        let store = EngineStore { engine: &self.engine, values: &self.values };
        let rel = eval::eval_expr_with(&store, body);
        if let Some(Binding::Rel(rt)) = env.binding("__repl_result") {
            println!("__repl_result : {}", env.show_rel(rt));
        }
        print_relation(&rel);
    }

    fn import(&mut self, path: &str) {
        if path.is_empty() {
            println!("usage: /import <path.rex>");
            return;
        }
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                println!("rex: cannot read {path}: {e}");
                return;
            }
        };
        let candidate = format!("{}\n{}\n", self.src, src);
        if self.commit(candidate) {
            println!("imported {path}");
        }
    }

    /// Retract a `new`-bound entity: negate its base rows through the circuit
    /// and show each view's delta.
    fn retract(&mut self, name: &str) {
        if name.is_empty() {
            println!("usage: /retract <binding>");
            return;
        }
        let Some(id) = self.values.get(name).cloned() else {
            println!("no entity binding `{name}`");
            return;
        };
        let result = self.engine.retract_entity(&id);
        println!("retracted {name} = {id}");
        print_view_deltas(&result.view_deltas);
    }

    /// Parse and check `candidate` as the whole session source; on success,
    /// apply ONLY the newly added statements to the engine (a `new` is one
    /// atomic transaction, a `let` extends the circuit and backfills) and
    /// commit `candidate` as the new session state. Prior statements are
    /// never re-evaluated.
    fn commit(&mut self, candidate: String) -> bool {
        let parsed = crate::parse(&candidate);
        if !parsed.diagnostics.is_empty() {
            for d in &parsed.diagnostics {
                println!("{}", d.render(&candidate));
            }
            return false;
        }
        let checked = crate::check(&parsed.program);
        if !checked.diagnostics.is_empty() {
            for d in &checked.diagnostics {
                println!("{}", d.render(&candidate));
            }
            return false;
        }
        let typed = checked
            .elaborated
            .expect("no diagnostics implies an elaborated program");
        self.env = checked.env;

        // Entity declarations appear only in the AST (they produce no typed
        // statement); walk the new AST suffix pairing each `let` with the next
        // typed statement so prints come out in source order.
        let mut ti = self.typed_count;
        for stmt in &parsed.program.stmts[self.ast_count..] {
            match stmt {
                crate::ast::Stmt::Entity(e) => println!("entity {} defined", e.name),
                crate::ast::Stmt::Let(_) => {
                    self.apply_stmt(&typed.stmts[ti]);
                    ti += 1;
                }
            }
        }
        debug_assert_eq!(ti, typed.stmts.len());

        self.ast_count = parsed.program.stmts.len();
        self.typed_count = typed.stmts.len();
        self.src = candidate;
        true
    }

    /// Apply one newly committed typed statement to the engine.
    fn apply_stmt(&mut self, stmt: &TStmt) {
        match stmt {
            TStmt::New { name, sort, fields } => {
                let resolved: Vec<(String, Value)> = fields
                    .iter()
                    .map(|(f, tv)| {
                        let v = match tv {
                            TValue::Lit(lit) => eval::interp::lit_value(lit),
                            TValue::Ref(n) => self.values[n].clone(),
                        };
                        (f.clone(), v)
                    })
                    .collect();
                let (id, result) = self.engine.apply_new(*sort, &resolved);
                match name {
                    Some(name) => {
                        self.values.insert(name.clone(), id);
                        self.print_binding(name);
                    }
                    None => println!("(anonymous) {id}"),
                }
                print_view_deltas(&result.view_deltas);
            }
            TStmt::Let { name: Some(name), body } => {
                self.engine.add_view(name, body, &self.values);
                self.print_binding(name);
            }
            // An anonymous view has no observable effect; don't grow the
            // circuit for it.
            TStmt::Let { name: None, .. } => println!("(anonymous binding ignored)"),
        }
    }

    fn print_binding(&self, name: &str) {
        match self.env.binding(name) {
            Some(Binding::Value(vt)) => {
                let v = &self.values[name];
                println!("{name} : {} = {v}", self.env.show(vt));
            }
            Some(Binding::Rel(rt)) => {
                println!("{name} : {}", self.env.show_rel(rt));
                match self.engine.circuit.view(name) {
                    Some(rel) => print_relation(rel),
                    None => println!("  (empty)"),
                }
            }
            None => println!("{name} = ()"),
        }
    }

    fn print_env(&self) {
        let mut entities: Vec<&String> = self.env.entities().collect();
        entities.sort();
        if entities.is_empty() {
            println!("(no entities)");
        } else {
            println!("entities:");
            for name in entities {
                println!("  {name}");
            }
        }

        let mut names: Vec<&String> = self.env.bindings().map(|(n, _)| n).collect();
        names.sort();
        if names.is_empty() {
            println!("(no bindings)");
        } else {
            println!("bindings:");
            for name in names {
                match self.env.binding(name).unwrap() {
                    Binding::Value(vt) => println!("  {name} : {}", self.env.show(vt)),
                    Binding::Rel(rt) => println!("  {name} : {}", self.env.show_rel(rt)),
                }
            }
        }
    }
}

/// Parse and check a full source string, printing diagnostics; returns the
/// environment and elaborated program on success.
fn check_source(candidate: &str) -> Option<(Env, crate::types::typed::TProgram)> {
    let parsed = crate::parse(candidate);
    if !parsed.diagnostics.is_empty() {
        for d in &parsed.diagnostics {
            println!("{}", d.render(candidate));
        }
        return None;
    }
    let checked = crate::check(&parsed.program);
    if !checked.diagnostics.is_empty() {
        for d in &checked.diagnostics {
            println!("{}", d.render(candidate));
        }
        return None;
    }
    let typed = checked
        .elaborated
        .expect("no diagnostics implies an elaborated program");
    Some((checked.env, typed))
}

/// Print each view's nonzero delta from a step — the incremental engine's
/// whole point made visible.
fn print_view_deltas(deltas: &HashMap<String, BTreeRelation>) {
    let mut names: Vec<&String> = deltas.iter().filter(|(_, d)| !d.is_empty()).map(|(n, _)| n).collect();
    names.sort();
    for name in names {
        println!("  Δ{name}:");
        for (l, r, w) in deltas[name].iter() {
            let sign = if w > 0 { "+" } else { "-" };
            let n = w.abs();
            if n == 1 {
                println!("    {sign} {l} -> {r}");
            } else {
                println!("    {sign} {l} -> {r}  (x{n})");
            }
        }
    }
}

fn print_relation(rel: &BTreeRelation) {
    let rows: Vec<_> = BinaryRelation::iter(rel).collect();
    if rows.is_empty() {
        println!("  (empty)");
        return;
    }
    for (l, r, w) in rows.iter().take(MAX_ROWS) {
        if *w == 1 {
            println!("  {l} -> {r}");
        } else {
            println!("  {l} -> {r}  (x{w})");
        }
    }
    if rows.len() > MAX_ROWS {
        println!("  ... ({} more rows)", rows.len() - MAX_ROWS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::relation::BinaryRelation;

    const SPEC12: &str = include_str!("../tests/fixtures/spec12.rex");

    /// Commit the fixture line by line (each committed line applies only its
    /// own statements) and compare every view against a single batch run.
    #[test]
    fn line_by_line_commits_match_batch() {
        let mut session = Session::new();
        for line in SPEC12.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with("//") {
                continue;
            }
            assert!(session.eval_committing_line(line), "line failed: {line}");
        }

        let batch = eval::run(&crate::parse(SPEC12).program);
        for name in ["lineprice", "custspend", "inregion", "result"] {
            assert_eq!(
                session.engine.circuit.view(name).unwrap(),
                batch.view(name).unwrap(),
                "view `{name}`"
            );
        }
    }

    /// A bare expression batch-evaluates against the live engine state.
    #[test]
    fn scratch_expression_reads_live_state() {
        let mut session = Session::new();
        for line in SPEC12.lines() {
            let t = line.trim();
            if !t.is_empty() && !t.starts_with("//") {
                session.eval_committing_line(line);
            }
        }
        // `custspend where > 30` re-derived as a scratch expression must match
        // the committed `result` view joined without the region filter — here
        // just check it against the engine's own custspend (all pass > 30).
        let (env, typed) = check_source(&format!(
            "{}\nlet __repl_result = custspend where > 30\n",
            session.src
        ))
        .expect("scratch check");
        let Some(TStmt::Let { body, .. }) = typed.stmts.last() else { panic!() };
        assert!(matches!(env.binding("__repl_result"), Some(Binding::Rel(_))));
        let store = EngineStore { engine: &session.engine, values: &session.values };
        let rel = eval::eval_expr_with(&store, body);
        assert_eq!(&rel, session.engine.circuit.view("custspend").unwrap());
    }

    /// `/retract` updates views through the circuit and is idempotent.
    #[test]
    fn retract_updates_views() {
        let mut session = Session::new();
        for line in SPEC12.lines() {
            let t = line.trim();
            if !t.is_empty() && !t.starts_with("//") {
                session.eval_committing_line(line);
            }
        }
        assert_eq!(session.engine.circuit.view("result").unwrap().len(), 2);
        session.retract("bob");
        let result = session.engine.circuit.view("result").unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.iter().all(|(l, _, _)| l == session.values["alice"]));
        // Idempotent: nothing left to retract.
        session.retract("bob");
        assert_eq!(session.engine.circuit.view("result").unwrap().len(), 1);
    }

    /// A line that fails the checker leaves the session state untouched.
    #[test]
    fn failed_line_leaves_state_untouched() {
        let mut session = Session::new();
        session.eval_committing_line("entity Customer { name: Text }");
        let before = (session.ast_count, session.typed_count, session.engine.circuit.node_count());
        assert!(!session.eval_committing_line("let x : Customer -> Text = :nosuchfield"));
        assert_eq!(
            before,
            (session.ast_count, session.typed_count, session.engine.circuit.node_count())
        );
    }

    impl Session {
        /// Test helper: commit a single declaration line, reporting success.
        fn eval_committing_line(&mut self, line: &str) -> bool {
            let candidate = format!("{}\n{}\n", self.src, line);
            self.commit(candidate)
        }
    }
}
