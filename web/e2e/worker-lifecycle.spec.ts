/**
 * End-to-end worker lifecycle test.
 *
 * Requires the daemon to be started with APIARI_E2E_AGENT pointing at
 * scripts/mock-agent (handled by playwright.e2e.config.ts webServer).
 *
 * Flow:
 *  1. Open the apiari workspace
 *  2. Create a worker via the Quick Dispatch dialog
 *  3. App auto-navigates to the new worker's detail view
 *  4. Assert the PR link is visible in the detail header
 *  5. Send a revision message via the instruction input
 *  6. Assert the message appears in the timeline
 *  7. Assert the mock agent's revision response appears
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
    const workspaceBtn = page.getByRole("button", { name: WORKSPACE }).first();
    if (await workspaceBtn.isVisible()) {
      await workspaceBtn.click();
    }

    // ── 2. Open Quick Dispatch and create a worker ─────────────────────
    // The "+" button in the Workers sidebar section has aria-label="New worker"
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

    // ── 3. App auto-navigates to the new worker's detail ──────────────
    // After dispatch App.tsx calls navigateTo('worker', id), which mounts
    // WorkerDetailV2. Wait for the Timeline tab as a sign the detail loaded.
    await expect(page.getByTestId("tab-timeline")).toBeVisible({
      timeout: 15_000,
    });

    // ── 4. Wait for the PR link to appear in the detail header ─────────
    // WorkerDetailV2 renders the PR as <a href="...pull/999">#999</a>.
    // The mock agent emits PR_OPENED: in its output so the reconciler
    // picks it up within one poll cycle.
    const prLink = page.locator(`a[href="${MOCK_PR_URL}"]`).first();
    await expect(prLink).toBeVisible({ timeout: 30_000 });
    await expect(prLink).toHaveAttribute("href", MOCK_PR_URL);

    // ── 5. Send a revision message ─────────────────────────────────────
    // The Timeline tab is active by default. The instruction input
    // placeholder is "Send async instruction…" (running/stalled) or
    // "Send an instruction…" (done).
    const chatInput = page.getByPlaceholder(/Send.*instruction/i);
    await expect(chatInput).toBeVisible();
    await chatInput.fill(REVISION_MSG);
    // The send button has title="Send"
    await page.locator('button[title="Send"]').click();

    // ── 6. Message appears in timeline immediately ─────────────────────
    await expect(page.getByText(REVISION_MSG).first()).toBeVisible({
      timeout: 5_000,
    });

    // ── 7. Mock agent's revision response appears ──────────────────────
    // The mock agent outputs: "Got your feedback: … Applying the fix now."
    await expect(page.getByText(/Got your feedback/i)).toBeVisible({
      timeout: 30_000,
    });
  });
});
