import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

export default defineConfig({
  resolve: {
    // The README's example imports "rex-dom" like an app would; in this repo
    // that is the source, not a built dist/.
    alias: { "rex-dom": fileURLToPath(new URL("./src/index.ts", import.meta.url)) },
  },
});
