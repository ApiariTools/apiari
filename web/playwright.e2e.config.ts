import { defineConfig } from "@playwright/test";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");
const MOCK_AGENT = path.join(REPO_ROOT, "scripts", "mock-agent");

// In CI we use a pre-built binary for the daemon and `vite preview` to serve
// the built frontend.  Locally we use cargo run + Vite dev server for hot-reload.
const IS_CI = !!process.env.CI;
const DAEMON_PORT = 4299;
const UI_PORT = 4298;

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
          // CI: pre-built daemon binary serves the API.
          command: `APIARI_E2E_AGENT=${MOCK_AGENT} ${REPO_ROOT}/target/debug/apiari daemon restart --foreground --port ${DAEMON_PORT}`,
          url: `http://127.0.0.1:${DAEMON_PORT}/api/workspaces`,
          cwd: REPO_ROOT,
          timeout: 30_000,
          reuseExistingServer: false,
        },
        {
          // CI: vite preview serves the pre-built web/dist/ and proxies /api + /ws to daemon.
          command: `VITE_API_PORT=${DAEMON_PORT} npx vite preview --host 127.0.0.1 --port ${UI_PORT}`,
          url: `http://127.0.0.1:${UI_PORT}`,
          cwd: __dirname,
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
