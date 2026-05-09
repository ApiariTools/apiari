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
    // ── 1. Navigate and wait for app shell ─────────────────────────────
    await page.goto("/");
    await expect(page.getByRole("navigation", { name: "Sidebar" })).toBeVisible(
      { timeout: 15_000 },
    );

    // Switch to the apiari workspace if a switcher is visible.
    // isVisible() is non-waiting — only click if the button is already rendered.
    const workspaceBtn = page
      .getByRole("navigation", { name: "Sidebar" })
      .getByRole("button", { name: WORKSPACE })
      .first();
    if (await workspaceBtn.isVisible()) {
      await workspaceBtn.click();
    }

    // ── 2. Open Quick Dispatch and create a worker ─────────────────────
    await page.getByTestId("quick-dispatch-trigger").click();
    await expect(
      page.getByRole("dialog", { name: "Quick dispatch" }),
    ).toBeVisible({ timeout: 5_000 });

    await page.getByTestId("intent-textarea").fill(PROMPT);

    const repoPills = page.getByTestId("repo-pills").locator("button");
    await repoPills.first().click();

    await page.getByTestId("dispatch-btn").click();

    // Dialog should close after successful dispatch.
    await expect(
      page.getByRole("dialog", { name: "Quick dispatch" }),
    ).not.toBeVisible({ timeout: 10_000 });

    // ── 3. App auto-navigates to the new worker's detail ──────────────
    await expect(page.getByTestId("tab-timeline")).toBeVisible({
      timeout: 15_000,
    });

    // ── 4. Wait for the PR link to appear ─────────────────────────────
    // The mock agent emits PR_OPENED in its output; the reconciler picks it
    // up within one poll cycle (default: 5s).
    const prLink = page.locator(`a[href="${MOCK_PR_URL}"]`).first();
    await expect(prLink).toBeVisible({ timeout: 30_000 });
    await expect(prLink).toHaveAttribute("href", MOCK_PR_URL);

    // ── 5. Send a revision message ─────────────────────────────────────
    const chatInput = page.getByPlaceholder(/Send.*instruction/i);
    await expect(chatInput).toBeVisible({ timeout: 5_000 });
    await chatInput.fill(REVISION_MSG);
    await chatInput.press("Enter");

    // ── 6. Message appears in timeline ────────────────────────────────
    await expect(page.getByText(REVISION_MSG).first()).toBeVisible({
      timeout: 10_000,
    });

    // ── 7. Mock agent's revision response appears ─────────────────────
    await expect(page.getByText(/Got your feedback/i)).toBeVisible({
      timeout: 30_000,
    });
  });
});
