import { expect, test } from "@playwright/test";

// Tests run against the chat demo (mock mode, no daemon needed).
// The playwright config starts `npm run chat:demo` automatically.

test.describe("ChatLauncher demo", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("http://127.0.0.1:5174/");
    await page.waitForSelector('[class*="launcherWrap"]', { timeout: 5000 });
  });

  test("launcher button is visible", async ({ page }) => {
    await expect(page.locator('[class*="launcherWrap"] button').first()).toBeVisible();
  });

  test("unread badge shows on initial load", async ({ page }) => {
    // Mock seeds Research with 2 unread on load
    const badge = page.locator('[class*="launcherBadge"]');
    await expect(badge).toBeVisible({ timeout: 3000 });
    await expect(badge).toHaveText("2");
  });

  test("clicking launcher opens bot list popover", async ({ page }) => {
    await page.locator('[class*="launcherWrap"] button').first().click();
    await expect(page.locator('[class*="popoverHeader"]')).toBeVisible();
    await expect(page.locator('button[class*="botItem"]').filter({ hasText: "Main" })).toBeVisible();
    await expect(page.locator('button[class*="botItem"]').filter({ hasText: "Research" })).toBeVisible();
  });

  test("opening a bot clears its unread badge", async ({ page }) => {
    const badge = page.locator('[class*="launcherBadge"]');
    await expect(badge).toBeVisible({ timeout: 3000 });

    await page.locator('[class*="launcherWrap"] button').first().click();
    await page.locator('button[class*="botItem"]').filter({ hasText: "Research" }).click();

    await expect(badge).not.toBeVisible({ timeout: 2000 });
  });

  test("triggering unread increments badge", async ({ page }) => {
    // Open and close Research to clear its initial unread
    await page.locator('[class*="launcherWrap"] button').first().click();
    await page.locator('button[class*="botItem"]').filter({ hasText: "Research" }).click();
    await expect(page.locator('[class*="windowHeader"]').first()).toBeVisible();
    await page.locator('[class*="windowHeaderBtn"]').last().click();
    await expect(page.locator('[class*="launcherBadge"]')).not.toBeVisible({ timeout: 2000 });

    // Trigger an incoming message for Main via sidebar
    await page.getByRole("button", { name: "+ msg → Main" }).click();

    const badge = page.locator('[class*="launcherBadge"]');
    await expect(badge).toBeVisible({ timeout: 3000 });
    await expect(badge).toHaveText("1");
  });

  test("opening a bot shows active conversation count in button", async ({ page }) => {
    // Initially no bots opened → button shows icon, no launcherCount
    await expect(page.locator('[class*="launcherCount"]')).not.toBeVisible();

    await page.locator('[class*="launcherWrap"] button').first().click();
    await page.locator('button[class*="botItem"]').filter({ hasText: "Research" }).click();

    // One bot opened → launcherCount shows "1"
    await expect(page.locator('[class*="launcherCount"]')).toHaveText("1", { timeout: 3000 });
  });

  test("closing a bot window decrements active conversation count", async ({ page }) => {
    await page.locator('[class*="launcherWrap"] button').first().click();
    await page.locator('button[class*="botItem"]').filter({ hasText: "Research" }).click();
    await expect(page.locator('[class*="launcherCount"]')).toHaveText("1", { timeout: 3000 });

    // Close the window
    await page.locator('[class*="windowHeaderBtn"]').last().click();

    // Count goes back to 0 → icon returns, launcherCount gone
    await expect(page.locator('[class*="launcherCount"]')).not.toBeVisible({ timeout: 2000 });
  });
});
