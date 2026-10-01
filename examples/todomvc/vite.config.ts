import { defineConfig } from "vite";

export default defineConfig({
  build: { target: "esnext" },
  // rex-runtime ships a wasm-bindgen glue file that locates its .wasm with
  // `new URL(..., import.meta.url)`, which dev-server pre-bundling would break.
  optimizeDeps: { exclude: ["rex-runtime"] },
  // The `file:../../js/*` links put the packages (and the .wasm) outside this
  // app's root; an installed copy under node_modules needs no such allowance.
  server: { fs: { allow: ["../.."] } },
});
