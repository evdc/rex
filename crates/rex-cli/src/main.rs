//! `rex` — check, run and build `.rex` programs, or start the REPL.
//!
//! Exit codes: 0 success, 1 the program has errors (or warnings under
//! `--deny-warnings`), 2 usage or I/O trouble (an unreadable file, a bad flag).

mod repl;

use clap::{Parser, Subcommand};
use notify::{EventKind, RecursiveMode, Watcher};
use rex::diagnostic::Severity;
use rex::Diagnostic;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "rex", version, about = "The Rex language: check, run and build programs, or start the REPL")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and type-check files, printing diagnostics (no output when clean)
    Check {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Exit 1 if there are warnings, not just errors
        #[arg(long)]
        deny_warnings: bool,
    },
    /// Check a file, evaluate it in batch, and print every view's contents
    Run {
        file: PathBuf,
        /// Also print the parsed program as an s-expression
        #[arg(long)]
        ast: bool,
    },
    /// Compile a program's `view`s to a self-contained TypeScript module
    Build {
        file: PathBuf,
        /// Write the module here instead of stdout
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Import specifier for the raw program source (default `./<file>?raw`)
        #[arg(long)]
        import: Option<String>,
        /// Emit `console.debug` tracing at the engine boundary
        #[arg(long)]
        debug: bool,
        /// Rebuild whenever the file changes (needs `--out`)
        #[arg(short, long, requires = "out")]
        watch: bool,
    },
    /// Start the interactive REPL (also what plain `rex` does)
    Repl,
}

fn main() -> ExitCode {
    let cli = Cli::parse_from(args());
    match cli.command {
        None | Some(Command::Repl) => {
            repl::run();
            ExitCode::SUCCESS
        }
        Some(Command::Check { files, deny_warnings }) => check(&files, deny_warnings),
        Some(Command::Run { file, ast }) => run(&file, ast),
        Some(Command::Build { file, out, import, debug, watch }) => {
            let job = Build { file, out, import, debug };
            if watch { watch_build(&job) } else { job.run() }
        }
    }
}

/// The process arguments, with `rex file.rex` still meaning `rex run file.rex`
/// (how the CLI worked before it had subcommands).
fn args() -> Vec<OsString> {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    let bare_path = args
        .get(1)
        .and_then(|a| a.to_str())
        .is_some_and(|a| !a.starts_with('-') && !matches!(a, "check" | "run" | "build" | "repl" | "help"));
    if bare_path {
        args.insert(1, "run".into());
    }
    args
}

// --- shared front end -------------------------------------------------------

fn read(path: &Path) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("rex: cannot read {}: {e}", path.display());
        ExitCode::from(2)
    })
}

fn report(path: &Path, src: &str, diags: &[Diagnostic]) {
    for d in diags {
        eprintln!("{}\n", d.render_file(src, &path.display().to_string()));
    }
}

fn count(diags: &[Diagnostic], severity: Severity) -> usize {
    diags.iter().filter(|d| d.severity == severity).count()
}

/// Parse and type-check `src`, printing every diagnostic. The program comes
/// back only when it is free of errors; the second value is the warning count.
fn front_end(path: &Path, src: &str) -> (Option<rex::ast::Program>, usize) {
    let parsed = rex::parse(src);
    report(path, src, &parsed.diagnostics);
    if count(&parsed.diagnostics, Severity::Error) > 0 {
        return (None, count(&parsed.diagnostics, Severity::Warning));
    }
    let checked = rex::check(&parsed.program);
    report(path, src, &checked.diagnostics);
    let warnings = count(&parsed.diagnostics, Severity::Warning) + count(&checked.diagnostics, Severity::Warning);
    // Errors null `elaborated`; warnings (the §8 incrementality-cliff marks)
    // are advisory and never reject a program.
    let ok = checked.elaborated.is_some();
    (ok.then_some(parsed.program), warnings)
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

// --- check -------------------------------------------------------------------

fn check(files: &[PathBuf], deny_warnings: bool) -> ExitCode {
    let (mut failed, mut unreadable) = (false, false);
    for path in files {
        let Ok(src) = read(path) else {
            unreadable = true;
            continue;
        };
        let (program, warnings) = front_end(path, &src);
        let denied = deny_warnings && warnings > 0;
        failed |= program.is_none() || denied;
        let verdict = match (&program, warnings) {
            (None, _) => "has errors".to_string(),
            (Some(_), 0) => "ok".to_string(),
            (Some(_), w) => format!("ok, {}{}", plural(w, "warning"), if denied { " (denied)" } else { "" }),
        };
        eprintln!("{}: {verdict}", path.display());
    }
    if failed {
        ExitCode::FAILURE
    } else if unreadable {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

// --- run ---------------------------------------------------------------------

fn run(path: &Path, ast: bool) -> ExitCode {
    let src = match read(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let (Some(program), _) = front_end(path, &src) else {
        return ExitCode::FAILURE;
    };
    if ast {
        println!("{}\n", rex::program_to_sexpr(&program));
    }
    let result = rex::eval::run(&program);
    let mut names: Vec<&String> = result.views.keys().collect();
    names.sort();
    for name in names {
        println!("{name}:");
        for (l, r, w) in rex::eval::BinaryRelation::iter(&result.views[name]) {
            if w == 1 {
                println!("  {l} -> {r}");
            } else {
                println!("  {l} -> {r}  (x{w})");
            }
        }
    }
    ExitCode::SUCCESS
}

// --- build -------------------------------------------------------------------

struct Build {
    file: PathBuf,
    out: Option<PathBuf>,
    import: Option<String>,
    debug: bool,
}

impl Build {
    /// Compile once. Failures are reported and returned as an exit code, so a
    /// watching build can keep going.
    fn run(&self) -> ExitCode {
        let src = match read(&self.file) {
            Ok(s) => s,
            Err(code) => return code,
        };
        // Default the raw-source import to the input file's own name (`./board.rex?raw`).
        let import = self.import.clone().unwrap_or_else(|| {
            let name = self.file.file_name().and_then(|f| f.to_str()).unwrap_or("program.rex");
            format!("./{name}?raw")
        });
        let module = match rex_codegen::generate_with(&src, &import, self.debug) {
            Ok(m) => m,
            Err(diags) => {
                report(&self.file, &src, &diags);
                return ExitCode::FAILURE;
            }
        };
        match &self.out {
            None => {
                print!("{module}");
                ExitCode::SUCCESS
            }
            Some(out) => match std::fs::write(out, module) {
                Ok(()) => {
                    eprintln!("ok: wrote {}", out.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("rex build: cannot write {}: {e}", out.display());
                    ExitCode::from(2)
                }
            },
        }
    }
}

/// How long a burst of file events must stay quiet before rebuilding (editors
/// write a file as several events, or via a temp file and a rename).
const SETTLE: Duration = Duration::from_millis(80);

/// Build, then rebuild each time the source file changes, until interrupted.
/// A failed build is reported and the watch carries on.
fn watch_build(job: &Build) -> ExitCode {
    let (tx, rx) = mpsc::channel();
    let mut watcher = match notify::recommended_watcher(tx) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("rex build: cannot watch files: {e}");
            return ExitCode::from(2);
        }
    };
    // Watch the directory, not the file: editors that save by renaming a
    // temp file over it would otherwise drop the watch.
    let dir = match job.file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
        eprintln!("rex build: cannot watch {}: {e}", dir.display());
        return ExitCode::from(2);
    }
    let name = job.file.file_name();
    let touches_source = |ev: &notify::Event| {
        !matches!(ev.kind, EventKind::Access(_)) && ev.paths.iter().any(|p| p.file_name() == name)
    };

    job.run();
    eprintln!("watching {} (ctrl-c to stop)", job.file.display());
    while let Ok(event) = rx.recv() {
        match event {
            Ok(ev) if touches_source(&ev) => {
                // Absorb the rest of the burst, then build once.
                while rx.recv_timeout(SETTLE).is_ok() {}
                job.run();
            }
            Ok(_) => {}
            Err(e) => eprintln!("rex build: watch error: {e}"),
        }
    }
    ExitCode::SUCCESS
}
