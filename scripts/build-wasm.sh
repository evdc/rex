#!/usr/bin/env bash
# Build rex-wasm and generate the wasm-bindgen JS/TS glue.
#
# Output lands in js/rex-runtime/pkg/ (the `rex-runtime/wasm` export); the
# generated apps and every example import it from there, so nothing is copied.
set -euo pipefail

cd "$(dirname "$0")/.."

OUT_DIR="js/rex-runtime/pkg"

# wasm-bindgen's CLI must match the `wasm-bindgen` crate version pinned in
# Cargo.lock exactly, or the generated glue fails to load at runtime with an
# opaque "schema version mismatch" error. Check this up front.
if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "error: wasm-bindgen CLI not found on PATH." >&2
  echo "  install with: cargo install wasm-bindgen-cli --version <version>" >&2
  echo "  (version must match the wasm-bindgen crate version in Cargo.lock)" >&2
  exit 1
fi

CRATE_VERSION=$(grep -A2 '^name = "wasm-bindgen"$' Cargo.lock | grep '^version' | head -1 | sed -E 's/version = "(.*)"/\1/')
CLI_VERSION=$(wasm-bindgen --version | awk '{print $2}')

if [ -z "$CRATE_VERSION" ]; then
  echo "error: could not find wasm-bindgen crate version in Cargo.lock" >&2
  exit 1
fi

if [ "$CRATE_VERSION" != "$CLI_VERSION" ]; then
  echo "error: wasm-bindgen CLI version ($CLI_VERSION) does not match the" >&2
  echo "  wasm-bindgen crate version pinned in Cargo.lock ($CRATE_VERSION)." >&2
  echo "  fix with: cargo install wasm-bindgen-cli --version $CRATE_VERSION" >&2
  exit 1
fi

echo "building rex-wasm (release profile, wasm32-unknown-unknown)..."
cargo build -p rex-wasm --profile wasm-release --target wasm32-unknown-unknown

WASM_FILE="target/wasm32-unknown-unknown/wasm-release/rex_wasm.wasm"

echo "running wasm-bindgen (target web) -> $OUT_DIR ..."
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
wasm-bindgen --target web --out-dir "$OUT_DIR" "$WASM_FILE"

SIZE=$(wc -c < "$OUT_DIR"/*_bg.wasm | tr -d ' ')
GZIP_SIZE=$(gzip -c "$OUT_DIR"/*_bg.wasm | wc -c | tr -d ' ')
echo "done: $OUT_DIR (wasm: ${SIZE} bytes, gzipped: ${GZIP_SIZE} bytes)"
