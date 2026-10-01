// `npm pack` must not produce a runtime without its engine: the wasm glue in
// pkg/ is a build artifact (scripts/build-wasm.sh), not checked in.
import { existsSync } from "node:fs";

const missing = ["rex_wasm.js", "rex_wasm.d.ts", "rex_wasm_bg.wasm"].filter(
  (f) => !existsSync(new URL(`../pkg/${f}`, import.meta.url)),
);
if (missing.length > 0) {
  console.error(`rex-runtime: pkg/ is missing ${missing.join(", ")} — run scripts/build-wasm.sh first.`);
  process.exit(1);
}
