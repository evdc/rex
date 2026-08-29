//! Rex CLI: read a `.rex` source file and run it through the front end,
//! printing tokens/AST or diagnostics.

mod repl;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        repl::run();
        return ExitCode::SUCCESS;
    };

    if path == "build" {
        return build(args);
    }

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
    // Only errors (which null `elaborated`) reject the program; warnings — the
    // §8 incrementality-cliff marks — are advisory and must not fail a run.
    if checked.elaborated.is_none() {
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

/// `rex build <app.rex> [-o out.ts] [--import <spec>]` — compile a `.rex`
/// program with views into a self-contained TS module.
fn build(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(path) = args.next() else {
        eprintln!("usage: rex build <app.rex> [-o out.ts] [--import <spec>] [--debug]");
        return ExitCode::FAILURE;
    };
    let mut out_path: Option<String> = None;
    let mut import: Option<String> = None;
    let mut debug = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "-o" | "--out" => out_path = args.next(),
            "--import" => import = args.next(),
            "--debug" => debug = true,
            other => {
                eprintln!("rex build: unknown argument `{other}`");
                return ExitCode::FAILURE;
            }
        }
    }

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rex: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Default the raw-source import to the input file's own name (`./board.rex?raw`).
    let import = import.unwrap_or_else(|| {
        let file = std::path::Path::new(&path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("program.rex");
        format!("./{file}?raw")
    });

    match rex_codegen::generate_with(&src, &import, debug) {
        Ok(module) => match &out_path {
            Some(out) => match std::fs::write(out, module) {
                Ok(()) => {
                    eprintln!("ok: wrote {out}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("rex build: cannot write {out}: {e}");
                    ExitCode::FAILURE
                }
            },
            None => {
                print!("{module}");
                ExitCode::SUCCESS
            }
        },
        Err(diags) => {
            for d in &diags {
                eprintln!("{}", d.render(&src));
            }
            ExitCode::FAILURE
        }
    }
}
