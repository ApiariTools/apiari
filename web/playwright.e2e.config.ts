import { defineConfig } from "@playwright/test";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const IS_CI = !!process.env.CI;
const UI_PORT = 4298;

// All e2e tests use page.route() to mock the API — no real daemon required.
// This prevents test runs from creating real workers, touching real repos,
// or polluting the live workspace state.

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/*.spec.ts",
  fullyParallel: false,
  retries: IS_CI ? 1 : 0,
  timeout: 60_000,
  use: {
    baseURL: `http://127.0.0.1:${UI_PORT}`,
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  webServer: IS_CI
    ? {
        command: `npx vite preview --host 127.0.0.1 --port ${UI_PORT}`,
        url: `http://127.0.0.1:${UI_PORT}`,
        cwd: __dirname,
        timeout: 30_000,
        reuseExistingServer: false,
      }
    : {
        command: `npm run dev -- --host 127.0.0.1 --port ${UI_PORT}`,
        url: `http://127.0.0.1:${UI_PORT}`,
        cwd: __dirname,
        timeout: 30_000,
        reuseExistingServer: true,
      },
});
