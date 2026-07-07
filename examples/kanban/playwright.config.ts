import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "e2e",
  use: { baseURL: "http://localhost:5199" },
  webServer: {
    command: "npx vite --port 5199",
    url: "http://localhost:5199",
    reuseExistingServer: true,
  },
});
