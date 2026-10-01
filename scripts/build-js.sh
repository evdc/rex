#!/usr/bin/env bash
# Build the two JS packages to dist/ (rex-dom first: rex-runtime's types import it).
# The examples consume them through their `exports` maps, so run this — and
# scripts/build-wasm.sh for rex-runtime's pkg/ — before building an example.
set -euo pipefail

cd "$(dirname "$0")/../js"

for pkg in rex-dom rex-runtime; do
  echo "building $pkg ..."
  (cd "$pkg" && npm run build --silent)
done
