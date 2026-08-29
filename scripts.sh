cargo build -p rex-wasm --target wasm32-unknown-unknown --release
wasm-bindgen … --target web      # TODO - what's passed as input output
cargo run -- build examples/kanban/src/board.rex -o examples/kanban/src/main.ts
cd examples/kanban && npm run preview
