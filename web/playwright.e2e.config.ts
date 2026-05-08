import { defineConfig } from "@playwright/test";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");
const MOCK_AGENT = path.join(REPO_ROOT, "scripts", "mock-agent");

// In CI the binary is pre-built with the embedded frontend, so we need only
// one server (the daemon itself).  Locally we use a Vite dev server on a
// separate port so hot-reload works during development.
const IS_CI = !!process.env.CI;
const DAEMON_PORT = 4299;
const UI_PORT = IS_CI ? DAEMON_PORT : 4298;

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/worker-lifecycle.spec.ts",
  fullyParallel: false,
  retries: IS_CI ? 1 : 0,
  timeout: 120_000,
  use: {
    baseURL: `http://127.0.0.1:${UI_PORT}`,
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  webServer: IS_CI
    ? [
        {
          // CI: daemon serves the embedded frontend — no Vite proxy needed.
          command: `APIARI_E2E_AGENT=${MOCK_AGENT} ${REPO_ROOT}/target/debug/apiari daemon restart --foreground --port ${DAEMON_PORT}`,
          url: `http://127.0.0.1:${DAEMON_PORT}/api/workspaces`,
          cwd: REPO_ROOT,
          timeout: 30_000,
          reuseExistingServer: false,
        },
      ]
    : [
        {
          // Local dev: cargo run + Vite dev server for hot-reload.
          command: `APIARI_E2E_AGENT=${MOCK_AGENT} cargo run -p apiari -- daemon restart --foreground --port ${DAEMON_PORT}`,
          url: `http://127.0.0.1:${DAEMON_PORT}/api/workspaces`,
          cwd: REPO_ROOT,
          timeout: 90_000,
          reuseExistingServer: false,
        },
        {
          command: `VITE_API_PORT=${DAEMON_PORT} npm run dev -- --host 127.0.0.1 --port ${UI_PORT}`,
          url: `http://127.0.0.1:${UI_PORT}`,
          cwd: __dirname,
          timeout: 30_000,
          reuseExistingServer: false,
        },
      ],
});
