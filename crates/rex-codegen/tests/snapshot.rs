//! S-03: pin codegen's output for the shared Kanban-shaped fixture program
//! (also used by `crates/rex-core/tests/contract_fixtures.rs`) so codegen has
//! a regression test independent of `examples/kanban`.
//!
//! Regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p rex-codegen --test snapshot`.

use std::path::{Path, PathBuf};

const BOARD_SRC: &str = include_str!("../../rex-core/tests/fixtures/board.rex");

fn snapshot_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/board.ts")
}

#[test]
fn codegen_output_for_board_matches_the_snapshot() {
    let generated = rex_codegen::generate(BOARD_SRC, "./board.rex?raw").expect("clean codegen");
    let path = snapshot_path();

    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &generated).unwrap_or_else(|e| panic!("write {path:?}: {e}"));
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("missing snapshot {path:?} — run with UPDATE_SNAPSHOTS=1 to generate it")
    });
    assert_eq!(
        generated, expected,
        "codegen output for board.rex changed — rerun with UPDATE_SNAPSHOTS=1 if intentional"
    );
}
