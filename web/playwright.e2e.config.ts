import { defineConfig } from "@playwright/test";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DAEMON_PORT = 4299;
const UI_PORT = 4298;
const REPO_ROOT = path.resolve(__dirname, "..");
const MOCK_AGENT = path.join(REPO_ROOT, "scripts", "mock-agent");

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/worker-lifecycle.spec.ts",
  fullyParallel: false,
  retries: 0,
  timeout: 60_000,
  use: {
    baseURL: `http://127.0.0.1:${UI_PORT}`,
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  webServer: [
    {
      // Start the apiari daemon with the mock agent instead of real codex/claude.
      command: `APIARI_E2E_AGENT=${MOCK_AGENT} cargo run -p apiari -- daemon restart --foreground --port ${DAEMON_PORT}`,
      url: `http://127.0.0.1:${DAEMON_PORT}/api/workspaces`,
      cwd: REPO_ROOT,
      timeout: 90_000,
      reuseExistingServer: false,
    },
    {
      // Vite dev server proxying to the test daemon.
      command: `VITE_API_PORT=${DAEMON_PORT} npm run dev -- --host 127.0.0.1 --port ${UI_PORT}`,
      url: `http://127.0.0.1:${UI_PORT}`,
      cwd: __dirname,
      timeout: 30_000,
      reuseExistingServer: false,
    },
  ],
});
