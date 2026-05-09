import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  testMatch: ["**/chat-demo.spec.ts", "**/reset-check.spec.ts"],
  fullyParallel: true,
  retries: 0,
  use: {
    baseURL: "http://127.0.0.1:5174",
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "npm run chat:demo",
    url: "http://127.0.0.1:5174",
    reuseExistingServer: true,
    timeout: 15_000,
  },
});
