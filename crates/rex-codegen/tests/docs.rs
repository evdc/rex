//! The program in the README's introduction is a real program: it checks, and
//! `rex build` generates a module for it.

#[test]
fn the_readme_program_builds() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap();
    let start = readme.find("```rex\n").expect("a rex block") + "```rex\n".len();
    let program = &readme[start..start + readme[start..].find("```").expect("the block closes")];
    assert!(program.contains("view main"), "the first rex block should be the sample app");
    let module = rex_codegen::generate(program, "./app.rex?raw")
        .unwrap_or_else(|diags| panic!("the README program does not check:\n{diags:#?}\n{program}"));
    for needle in ["main#unit", "dispatch(\"Toggle\"", "classList.toggle(\"done\""] {
        assert!(module.contains(needle), "generated module lacks {needle}");
    }
}
