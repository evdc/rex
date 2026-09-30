// Build this app into the official js-framework-benchmark repo layout:
//
//   node scripts/build-harness.mjs ../js-framework-benchmark
//
// writes <harness>/frameworks/keyed/rex/{index.html,assets/…,package.json}.
// The harness loads exactly `frameworks/keyed/rex/index.html` (no query
// string), so persistence is switched off at build time
// (`VITE_REX_PERSIST=0`, the equivalent of `?ephemeral`), and the app's own
// stylesheet is swapped for the harness's shared `/css/currentStyle.css` so
// every implementation is measured against the same CSS.
import { execFileSync } from "node:child_process";
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(dirname(fileURLToPath(import.meta.url)));
const harness = resolve(process.argv[2] ?? "../js-framework-benchmark");
const dest = join(harness, "frameworks", "keyed", "rex");
const out = join(here, "dist-harness");

rmSync(out, { recursive: true, force: true });
execFileSync("npx", ["vite", "build", "--base", "./", "--outDir", out, "--emptyOutDir"], {
  cwd: here,
  stdio: "inherit",
  env: { ...process.env, VITE_REX_PERSIST: "0" },
});

let html = readFileSync(join(out, "index.html"), "utf8");
html = html.replace(/<style>[\s\S]*?<\/style>/, '<link href="/css/currentStyle.css" rel="stylesheet" />');
html = html.replace("<title>Rex (keyed)</title>", '<title>Rex-"keyed"</title>');
writeFileSync(join(out, "index.html"), html);

rmSync(dest, { recursive: true, force: true });
mkdirSync(dest, { recursive: true });
cpSync(out, dest, { recursive: true });
writeFileSync(
  join(dest, "package.json"),
  JSON.stringify(
    {
      name: "rex",
      version: "0.1.0",
      description: "Rex (relational UI language, DBSP engine in WASM), built from examples/js-framework-benchmark",
      main: "index.html",
      "js-framework-benchmark": { frameworkVersion: "", frameworkHomeURL: "", language: "Rex" },
      scripts: { dev: "exit 0", "build-prod": "exit 0" },
      author: "evdc",
    },
    null,
    2,
  ) + "\n",
);
// The harness reads a framework's version from its lockfile.
writeFileSync(
  join(dest, "package-lock.json"),
  JSON.stringify(
    {
      name: "rex",
      version: "0.1.0",
      lockfileVersion: 3,
      requires: true,
      packages: { "": { name: "rex", version: "0.1.0", license: "Apache-2.0" } },
    },
    null,
    2,
  ) + "\n",
);
console.log(`wrote ${dest}`);
