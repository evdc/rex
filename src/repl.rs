//! An interactive REPL: statements (`entity`/`let`) accumulate into a growing
//! session program; bare expressions are evaluated against that program
//! without being kept, so querying doesn't clutter the environment.
//!
//! Since the evaluator is batch-mode (it re-runs the whole elaborated program
//! from scratch), each line simply re-checks and re-evaluates the full
//! accumulated source. That's wasteful at large scale, but REPL sessions are
//! small, so it keeps the REPL a thin wrapper with no incremental-eval logic
//! of its own.

use crate::eval::{self, BinaryRelation, BTreeRelation, Value};
use crate::types::env::{Binding, Env};
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
    println!("  /env              list entities and bindings in the current session");
    println!("  /reset            clear the session");
    println!("  /help             show this message");
    println!("  /quit, /exit      leave the REPL");
}

/// Accumulated session state: committed source, how many statements of it
/// have been parsed already, and the type environment they produced.
struct Session {
    src: String,
    stmt_count: usize,
    env: Env,
}

impl Session {
    fn new() -> Session {
        Session {
            src: String::new(),
            stmt_count: 0,
            env: Env::new(),
        }
    }

    /// Evaluate one line of input. `entity`/`let` statements are committed to
    /// the session on success; anything else is checked and run in a scratch
    /// copy of the session and discarded afterward.
    fn eval_line(&mut self, line: &str) {
        let trimmed = line.trim();
        let is_decl = trimmed.starts_with("entity") || trimmed.starts_with("let");
        if is_decl {
            let candidate = format!("{}\n{}\n", self.src, line);
            self.commit(candidate);
            return;
        }

        let candidate = format!("{}\nlet __repl_result = {}\n", self.src, line);
        let Some((env, views, values)) = Self::check_and_run(&candidate) else {
            return;
        };
        print_binding("__repl_result", &env, &views, &values);
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

    /// Parse, check, and evaluate `candidate` as the whole session source. On
    /// success, print every statement newly added beyond `self.stmt_count`
    /// and commit `candidate` as the new session state.
    fn commit(&mut self, candidate: String) -> bool {
        let Some((checked_env, program_stmts, views, values)) = Self::check_and_run_owned(&candidate)
        else {
            return false;
        };
        for stmt in &program_stmts[self.stmt_count..] {
            print_new_stmt(stmt, &checked_env, &views, &values);
        }
        self.stmt_count = program_stmts.len();
        self.env = checked_env;
        self.src = candidate;
        true
    }

    /// Shared parse/check/eval pipeline. Prints diagnostics and returns
    /// `None` on any failure.
    fn check_and_run_owned(
        candidate: &str,
    ) -> Option<(Env, Vec<crate::ast::Stmt>, HashMap<String, BTreeRelation>, HashMap<String, Value>)> {
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
        let elaborated = checked
            .elaborated
            .expect("no diagnostics implies an elaborated program");
        let (views, values) = eval::run_typed_values(&elaborated);
        Some((checked.env, parsed.program.stmts, views, values))
    }

    fn check_and_run(
        candidate: &str,
    ) -> Option<(Env, HashMap<String, BTreeRelation>, HashMap<String, Value>)> {
        let (env, _, views, values) = Self::check_and_run_owned(candidate)?;
        Some((env, views, values))
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

fn print_new_stmt(
    stmt: &crate::ast::Stmt,
    env: &Env,
    views: &HashMap<String, BTreeRelation>,
    values: &HashMap<String, Value>,
) {
    match stmt {
        crate::ast::Stmt::Entity(e) => println!("entity {} defined", e.name),
        crate::ast::Stmt::Let(l) => match &l.name {
            Some(name) => print_binding(name, env, views, values),
            None => println!("(anonymous binding evaluated)"),
        },
    }
}

fn print_binding(
    name: &str,
    env: &Env,
    views: &HashMap<String, BTreeRelation>,
    values: &HashMap<String, Value>,
) {
    match env.binding(name) {
        Some(Binding::Value(vt)) => {
            let v = &values[name];
            println!("{name} : {} = {v}", env.show(vt));
        }
        Some(Binding::Rel(rt)) => {
            println!("{name} : {}", env.show_rel(rt));
            print_relation(&views[name]);
        }
        None => println!("{name} = ()"),
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
