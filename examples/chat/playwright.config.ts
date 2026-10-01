import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "e2e",
  use: { baseURL: "http://localhost:5203" },
  webServer: {
    command: "npx vite --port 5203",
    url: "http://localhost:5203",
    reuseExistingServer: true,
  },
});
