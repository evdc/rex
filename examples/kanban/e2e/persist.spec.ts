import { expect, test } from "@playwright/test";

/** S-80: state survives a reload — snapshot + log replay, no seed doubling. */

const seedCards = 3;

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("section.list")).toHaveCount(3);
});

test("an edit survives a reload and the seed does not run twice", async ({ page }) => {
  const input = page.locator("div.card input").first();
  await input.fill("Persisted title");
  await input.press("Tab");
  await expect(input).toHaveValue("Persisted title");
  // Let the async append reach IndexedDB before we leave.
  await page.evaluate(() => (window as any).__rexApp.flush());

  await page.reload();
  await expect(page.locator("section.list")).toHaveCount(3);
  await expect(page.locator("div.card")).toHaveCount(seedCards);
  await expect(page.locator("div.card input").first()).toHaveValue("Persisted title");
});

test("an event after the last snapshot is replayed", async ({ page }) => {
  // The snapshot only happens every 50 events or on page hide; kill the page
  // without either, so the last edit exists only in the event log.
  const input = page.locator("div.card input").first();
  await input.fill("Log only");
  await input.press("Tab");
  await expect(input).toHaveValue("Log only");
  await page.evaluate(() => (window as any).__rexApp.flush());

  const second = await page.context().newPage();
  await second.goto("/");
  await expect(second.locator("section.list")).toHaveCount(3);
  await expect(second.locator("div.card input").first()).toHaveValue("Log only");
  await second.close();
});

test("?ephemeral does not persist", async ({ page }) => {
  await page.goto("/?ephemeral");
  await expect(page.locator("section.list")).toHaveCount(3);
  const input = page.locator("div.card input").first();
  await input.fill("Gone");
  await input.press("Tab");
  await page.reload();
  await expect(page.locator("div.card input").first()).not.toHaveValue("Gone");
});
