import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "e2e",
  use: { baseURL: "http://localhost:5197" },
  webServer: {
    command: "npx vite --port 5197",
    url: "http://localhost:5197",
    reuseExistingServer: true,
  },
});
