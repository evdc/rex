import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "e2e",
  use: { baseURL: "http://localhost:5198" },
  webServer: {
    command: "npx vite --port 5198",
    url: "http://localhost:5198",
    reuseExistingServer: true,
  },
});
