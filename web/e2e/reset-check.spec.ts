import { expect, test } from "@playwright/test";

test("reset clears open windows and unreads", async ({ page }) => {
  await page.goto("http://127.0.0.1:5174/");
  await page.waitForSelector('[class*="launcherWrap"]', { timeout: 5000 });

  await page.screenshot({ path: "test-results/reset-1-initial.png", fullPage: true });

  // Open Research bot
  await page.locator('[class*="launcherWrap"] button').first().click();
  await page.locator('[class*="botItem"]').filter({ hasText: "Research" }).click();
  await expect(page.locator('[class*="windowHeader"]').first()).toBeVisible();

  // Trigger a streaming response so there's an in-flight simulation
  await page.getByRole("button", { name: /Stream.*Main/ }).click();

  await page.screenshot({ path: "test-results/reset-2-before-reset.png", fullPage: true });

  // Click Reset everything
  await page.getByRole("button", { name: "Reset everything" }).click();
  await page.waitForTimeout(200);

  await page.screenshot({ path: "test-results/reset-3-after-reset.png", fullPage: true });

  // Wait for any stale events to settle
  await page.waitForTimeout(2000);
  await page.screenshot({ path: "test-results/reset-4-after-settle.png", fullPage: true });

  // No chat windows open
  await expect(page.locator('[class*="windowHeader"]')).not.toBeVisible({ timeout: 1000 });

  // Badge shows Research: 2 again (seed restored); button shows icon (no active conversations)
  await expect(page.locator('[class*="launcherBadge"]')).toHaveText("2", { timeout: 3000 });
  await expect(page.locator('[class*="launcherCount"]')).not.toBeVisible();
});
