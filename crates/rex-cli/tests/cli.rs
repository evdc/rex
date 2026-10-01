//! The `rex` binary end to end: subcommands, exit codes, diagnostics, `--watch`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A scratch directory unique to one test, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new(test: &str) -> Dir {
        let path = std::env::temp_dir().join(format!("rex-cli-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rex(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rex"))
        .args(args.iter().map(|a| a.as_ref()))
        .output()
        .expect("run rex")
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exited normally")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const CLEAN: &str = "entity A { x: Int }\nlet a = new A { x: 1 }\nlet xs = A . .x\n";
const BAD: &str = "entity A { x: Int }\nlet y = A . .zz\n";
const WARNS: &str = "entity Line { qty: Int, price: Money }\n\
                     let pair : Line -> Int * Money = (.qty , .price)\n\
                     let n : Line -> Int = count(pair)\n";
const VIEW: &str = "entity Todo { text: Text }\nview main = ul { Todo as t select li { .text } }\n";

// --- check ---------------------------------------------------------------------

#[test]
fn check_is_quiet_about_a_clean_file_and_exits_zero() {
    let d = Dir::new("check-clean");
    let f = d.file("ok.rex", CLEAN);
    let o = rex(&[&"check", &f]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).contains("ok.rex: ok"), "{}", stderr(&o));
}

#[test]
fn check_points_at_the_error_with_file_line_and_column() {
    let d = Dir::new("check-bad");
    let f = d.file("bad.rex", BAD);
    let o = rex(&[&"check", &f]);
    assert_eq!(code(&o), 1);
    let err = stderr(&o);
    assert!(err.contains("error: unknown field `zz`"), "{err}");
    assert!(err.contains("bad.rex:2:13"), "{err}");
    assert!(err.contains("2 | let y = A . .zz"), "{err}");
    assert!(err.contains("|             ^^^"), "{err}");
    assert!(err.contains("bad.rex: has errors"), "{err}");
}

#[test]
fn check_reports_every_file_and_fails_if_any_does() {
    let d = Dir::new("check-many");
    let good = d.file("good.rex", CLEAN);
    let bad = d.file("bad.rex", BAD);
    let o = rex(&[&"check", &good, &bad]);
    assert_eq!(code(&o), 1);
    let err = stderr(&o);
    assert!(err.contains("good.rex: ok") && err.contains("bad.rex: has errors"), "{err}");
}

#[test]
fn check_a_missing_file_is_a_usage_error() {
    let d = Dir::new("check-missing");
    let o = rex(&[&"check", &d.0.join("nope.rex")]);
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("cannot read"), "{}", stderr(&o));
}

#[test]
fn check_with_no_file_is_a_usage_error() {
    assert_eq!(code(&rex(&[&"check"])), 2);
}

#[test]
fn warnings_do_not_fail_check_unless_denied() {
    let d = Dir::new("check-warn");
    let f = d.file("w.rex", WARNS);
    let o = rex(&[&"check", &f]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).contains("warning: ") && stderr(&o).contains("w.rex: ok, 1 warning"), "{}", stderr(&o));
    let denied = rex(&[&"check", &"--deny-warnings", &f]);
    assert_eq!(code(&denied), 1);
    assert!(stderr(&denied).contains("(denied)"), "{}", stderr(&denied));
}

#[test]
fn a_parse_error_is_reported_and_stops_before_type_checking() {
    let d = Dir::new("check-parse");
    let f = d.file("p.rex", "entity A { x: }\n");
    let o = rex(&[&"check", &f]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("p.rex:1:"), "{}", stderr(&o));
}

#[test]
fn errors_inside_view_bodies_point_into_the_view() {
    // The bind, the handler, the order key and the `if` each report where
    // they are written, not at the top of the file.
    let d = Dir::new("check-views");
    let head = "entity Todo { text: Text }\nevent Edit(t: Todo, text: Text)\non Edit(t, text) => t.text := text\n";
    for (view, needle) in [
        ("view main = ul { Todo as t select li { .nope } }", ".nope"),
        ("view main = ul { Todo as t select li(on click => do Edit(t)) { .text } }", "do Edit(t)"),
        ("view main = ul { Todo as t order by .nope select li { .text } }", ".nope"),
        ("view main = div { if (nope > 0) { p \"x\" } }", "nope"),
    ] {
        let f = d.file("v.rex", &format!("{head}{view}\n"));
        let o = rex(&[&"check", &f]);
        assert_eq!(code(&o), 1, "{view}");
        let col = view.find(needle).unwrap() + 1;
        let want = format!("v.rex:4:{col}");
        assert!(stderr(&o).contains(&want), "{view}\nwanted {want} in:\n{}", stderr(&o));
    }
}

// --- run -----------------------------------------------------------------------

#[test]
fn run_prints_each_view_and_not_the_ast() {
    let d = Dir::new("run");
    let f = d.file("ok.rex", CLEAN);
    let o = rex(&[&"run", &f]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("xs:\n  #0:0 -> 1"), "{}", stdout(&o));
    assert!(!stdout(&o).contains("(entity"), "{}", stdout(&o));
}

#[test]
fn run_ast_adds_the_s_expression() {
    let d = Dir::new("run-ast");
    let f = d.file("ok.rex", CLEAN);
    let o = rex(&[&"run", &"--ast", &f]);
    assert!(stdout(&o).starts_with("(entity A"), "{}", stdout(&o));
}

#[test]
fn run_refuses_a_program_with_errors() {
    let d = Dir::new("run-bad");
    let f = d.file("bad.rex", BAD);
    let o = rex(&[&"run", &f]);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
}

#[test]
fn a_bare_path_still_means_run() {
    let d = Dir::new("bare");
    let f = d.file("ok.rex", CLEAN);
    let o = rex(&[&f]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("xs:"), "{}", stdout(&o));
}

// --- build ---------------------------------------------------------------------

#[test]
fn build_writes_the_module_to_stdout_or_a_file() {
    let d = Dir::new("build");
    let f = d.file("app.rex", VIEW);
    let o = rex(&[&"build", &f]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("import PROGRAM from \"./app.rex?raw\""), "{}", stdout(&o));
    let out = d.0.join("app.ts");
    let o = rex(&[&"build", &f, &"-o", &out]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(std::fs::read_to_string(&out).unwrap().contains("ShapeNode"));
    assert!(stderr(&o).contains("ok: wrote"), "{}", stderr(&o));
}

#[test]
fn build_honours_import_and_debug() {
    let d = Dir::new("build-flags");
    let f = d.file("app.rex", VIEW);
    let o = rex(&[&"build", &f, &"--import", &"./other.rex?raw", &"--debug"]);
    assert!(stdout(&o).contains("from \"./other.rex?raw\""), "{}", stdout(&o));
    assert!(stdout(&o).contains("console.debug"), "{}", stdout(&o));
}

#[test]
fn build_reports_errors_with_positions_and_writes_nothing() {
    let d = Dir::new("build-bad");
    let f = d.file("bad.rex", BAD);
    let out = d.0.join("bad.ts");
    let o = rex(&[&"build", &f, &"-o", &out]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("bad.rex:2:13"), "{}", stderr(&o));
    assert!(!out.exists());
}

#[test]
fn watch_needs_an_output_file() {
    let d = Dir::new("watch-usage");
    let f = d.file("app.rex", VIEW);
    assert_eq!(code(&rex(&[&"build", &f, &"--watch"])), 2);
}

// --- build --watch ---------------------------------------------------------------

struct Watcher(Child);

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Poll until `done` holds or ten seconds pass.
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!("timed out waiting for {what}");
}

fn contains(path: &Path, needle: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.contains(needle))
}

fn page(text: &str) -> String {
    format!("entity Todo {{ text: Text }}\nview main = div {{ p \"{text}\" }}\n")
}

#[test]
fn watch_rebuilds_on_change_and_survives_a_broken_save() {
    let d = Dir::new("watch");
    let f = d.file("app.rex", &page("first"));
    let out = d.0.join("app.ts");
    let child = Command::new(env!("CARGO_BIN_EXE_rex"))
        .arg("build")
        .arg(&f)
        .arg("-o")
        .arg(&out)
        .arg("--watch")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rex");
    let mut watcher = Watcher(child);

    eventually("the first build", || contains(&out, "first"));

    std::fs::write(&f, page("second")).unwrap();
    eventually("the rebuild", || contains(&out, "second"));

    // A broken save is reported, leaves the last good output, and does not
    // stop the watch.
    std::fs::write(&f, BAD).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(contains(&out, "second"));
    assert!(watcher.0.try_wait().unwrap().is_none(), "the watch exited on an error");

    std::fs::write(&f, page("fixed")).unwrap();
    eventually("the build after the fix", || contains(&out, "fixed"));
}
