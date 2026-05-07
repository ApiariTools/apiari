/**
 * End-to-end worker lifecycle test.
 *
 * Requires the daemon to be started with APIARI_E2E_AGENT pointing at
 * scripts/mock-agent (handled by playwright.e2e.config.ts webServer).
 *
 * Flow:
 *  1. Open the apiari workspace workers view
 *  2. Create a worker via the Quick Dispatch dialog
 *  3. Wait for the worker to appear and finish (mock agent exits fast)
 *  4. Assert the PR link is visible on the worker card
 *  5. Open the worker detail
 *  6. Send a revision message via the chat input
 *  7. Assert the message appears in the timeline
 *  8. Assert the mock agent's revision response appears
 */

import { expect, test } from "@playwright/test";

const WORKSPACE = "apiari";
const PROMPT = "e2e test: add a comment to main.rs";
const REVISION_MSG = "please also update the tests";
const MOCK_PR_URL = "https://github.com/ApiariTools/apiari/pull/999";

test.describe("worker lifecycle", () => {
  test("create worker → PR appears → send revision → response visible", async ({
    page,
  }) => {
    // ── 1. Navigate to the apiari workspace ────────────────────────────
    await page.goto("/");

    // Switch to the apiari workspace if not already there.
    // The workspace selector is in the sidebar.
    const workspaceBtn = page.getByRole("button", { name: WORKSPACE }).first();
    if (await workspaceBtn.isVisible()) {
      await workspaceBtn.click();
    }

    // Open Workers mode.
    await page.getByRole("button", { name: /Workers/i }).first().click();

    // ── 2. Open Quick Dispatch and create a worker ─────────────────────
    await page.getByRole("button", { name: "New worker" }).click();
    await expect(
      page.getByRole("dialog", { name: "Quick dispatch" }),
    ).toBeVisible();

    // Fill in the prompt.
    await page.getByTestId("intent-textarea").fill(PROMPT);

    // Select the first available repo pill.
    const repoPills = page.getByTestId("repo-pills").locator("button");
    await repoPills.first().click();

    // Submit.
    await page.getByTestId("dispatch-btn").click();

    // Dialog should close.
    await expect(
      page.getByRole("dialog", { name: "Quick dispatch" }),
    ).not.toBeVisible({ timeout: 5_000 });

    // ── 3. Wait for the worker to appear in the list ───────────────────
    // The worker card shows the prompt or a truncated version of it.
    const workerCard = page
      .locator("[data-worker-id]")
      .or(page.getByText("e2e test"))
      .first();
    await expect(workerCard).toBeVisible({ timeout: 15_000 });

    // ── 4. Wait for the PR link to appear ─────────────────────────────
    // The mock agent outputs PR_OPENED: https://...pull/999
    // The reconciler picks it up within one poll cycle (≤60s, but mock is fast).
    const prLink = page.getByRole("link", { name: /PR/i }).first();
    await expect(prLink).toBeVisible({ timeout: 30_000 });
    await expect(prLink).toHaveAttribute("href", MOCK_PR_URL);

    // ── 5. Open worker detail ──────────────────────────────────────────
    await workerCard.click();

    // Worker detail panel should show the prompt somewhere.
    await expect(page.getByText(PROMPT)).toBeVisible({ timeout: 5_000 });

    // Switch to the Chat tab.
    await page.getByRole("button", { name: "Chat" }).last().click();

    // ── 6. Send a revision message ─────────────────────────────────────
    const chatInput = page.getByPlaceholder("Message worker...");
    await expect(chatInput).toBeVisible();
    await chatInput.fill(REVISION_MSG);
    await page.getByRole("button", { name: "Send message" }).click();

    // ── 7. Message appears in timeline immediately ─────────────────────
    await expect(page.getByText(REVISION_MSG)).toBeVisible({ timeout: 5_000 });

    // ── 8. Mock agent's revision response appears ──────────────────────
    // The mock agent outputs: "Got your feedback: ... Applying the fix now."
    await expect(
      page.getByText(/Got your feedback/i),
    ).toBeVisible({ timeout: 30_000 });
  });
});
