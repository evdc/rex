//! Rex CLI: read a `.rex` source file and run it through the front end,
//! printing tokens/AST or diagnostics.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        rex::repl::run();
        return ExitCode::SUCCESS;
    };

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rex: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let parsed = rex::parse(&src);
    for diag in &parsed.diagnostics {
        eprintln!("{}", diag.render(&src));
    }
    println!("{}", rex::program_to_sexpr(&parsed.program));

    if !parsed.diagnostics.is_empty() {
        return ExitCode::FAILURE;
    }

    let checked = rex::check(&parsed.program);
    for diag in &checked.diagnostics {
        eprintln!("{}", diag.render(&src));
    }
    if !checked.diagnostics.is_empty() {
        return ExitCode::FAILURE;
    }
    eprintln!("ok: type-checked clean");

    // Batch-evaluate and print each view's contents.
    let result = rex::eval::run(&parsed.program);
    let mut names: Vec<&String> = result.views.keys().collect();
    names.sort();
    println!("\n--- views ---");
    for name in names {
        let rel = &result.views[name];
        println!("{name}:");
        for (l, r, w) in rex::eval::BinaryRelation::iter(rel) {
            if w == 1 {
                println!("  {l} -> {r}");
            } else {
                println!("  {l} -> {r}  (x{w})");
            }
        }
    }
    ExitCode::SUCCESS
}
