import { test, expect, type Page } from "@playwright/test";

const rows = (page: Page) => page.locator("#tbody tr");
const label = (page: Page, i: number) => rows(page).nth(i).locator("td").nth(1).innerText();
const ids = (page: Page) => rows(page).locator("td:first-child").allInnerTexts();

test.beforeEach(async ({ page }) => {
  await page.goto("/?ephemeral");
  await expect(page.locator("#run")).toBeVisible();
});

test("create 1,000 rows, numbered from 1 in order", async ({ page }) => {
  await page.click("#run");
  await expect(rows(page)).toHaveCount(1000);
  const nums = await ids(page);
  expect(nums[0]).toBe("1");
  expect(nums[999]).toBe("1000");
});

test("create again replaces the rows and keeps counting", async ({ page }) => {
  await page.click("#run");
  await page.click("#run");
  await expect(rows(page)).toHaveCount(1000);
  expect((await ids(page))[0]).toBe("1001");
});

test("create 10,000 rows", async ({ page }) => {
  await page.click("#runlots");
  await expect(rows(page)).toHaveCount(10000);
  const nums = await ids(page);
  expect(nums[9999]).toBe("10000");
});

test("append 1,000 rows", async ({ page }) => {
  await page.click("#run");
  await page.click("#add");
  await expect(rows(page)).toHaveCount(2000);
  const nums = await ids(page);
  expect(nums[1000]).toBe("1001");
  expect(nums[1999]).toBe("2000");
});

test("update every 10th row", async ({ page }) => {
  await page.click("#run");
  const before = await page.locator("#tbody td:nth-child(2) a").allInnerTexts();
  await page.click("#update");
  await expect(rows(page).nth(0).locator("td").nth(1)).toContainText("!!!");
  const after = await page.locator("#tbody td:nth-child(2) a").allInnerTexts();
  const changed = after.flatMap((t, i) => (t !== before[i] ? [i] : []));
  // Rows numbered 1, 11, 21, … (`num % 10 = 1`), i.e. positions 0, 10, 20, …
  expect(changed.length).toBe(100);
  expect(changed.every((i) => i % 10 === 0)).toBe(true);
  expect(after[0]).toBe(before[0] + " !!!");
  await page.click("#update");
  await expect(rows(page).nth(0).locator("td").nth(1)).toContainText("!!! !!!");
});

test("swap rows moves exactly two rows", async ({ page }) => {
  await page.click("#run");
  const before = await ids(page);
  await page.evaluate(() => {
    const tb = document.getElementById("tbody")!;
    (window as any).__moves = 0;
    new MutationObserver((rs) => {
      for (const r of rs) (window as any).__moves += r.addedNodes.length;
    }).observe(tb, { childList: true });
  });
  await page.click("#swaprows");
  await expect.poll(() => ids(page).then((n) => n[1])).toBe(before[998]);
  const after = await ids(page);
  expect(after[998]).toBe(before[1]);
  expect(after.filter((n, i) => n !== before[i])).toHaveLength(2);
  expect(await page.evaluate(() => (window as any).__moves)).toBe(2);
});

test("select highlights one row at a time", async ({ page }) => {
  await page.click("#run");
  await rows(page).nth(3).locator("td").nth(1).locator("a").click();
  await expect(rows(page).nth(3)).toHaveClass(/danger/);
  await rows(page).nth(5).locator("td").nth(1).locator("a").click();
  await expect(rows(page).nth(5)).toHaveClass(/danger/);
  await expect(page.locator("#tbody tr.danger")).toHaveCount(1);
});

test("remove deletes that row only", async ({ page }) => {
  await page.click("#run");
  const before = await ids(page);
  await rows(page).nth(1).locator("td").nth(2).locator("a").click();
  await expect(rows(page)).toHaveCount(999);
  const after = await ids(page);
  expect(after).toEqual(before.filter((_, i) => i !== 1));
});

test("clear removes every row", async ({ page }) => {
  await page.click("#run");
  await page.click("#clear");
  await expect(rows(page)).toHaveCount(0);
});

test("reload restores the rows without re-randomising (S-80)", async ({ page }) => {
  await page.goto("/");
  await page.click("#run");
  await expect(rows(page)).toHaveCount(1000);
  const texts = await page.locator("#tbody td:nth-child(2) a").allInnerTexts();
  await page.reload();
  await expect(rows(page)).toHaveCount(1000);
  expect(await page.locator("#tbody td:nth-child(2) a").allInnerTexts()).toEqual(texts);
});

test("engine cost of create 10,000 (recorded, µs)", async ({ page }) => {
  // The engine alone: `dispatch` = one event, one transaction, one delta batch.
  const t = await page.evaluate(() => {
    // The raw wasm app, so the timing is the engine call alone (no
    // `Engine.dispatch` JSON encode/parse around it).
    const app = (window as any).__rexApp.wasm;
    const args = JSON.stringify({
      n: "i:10000",
      labels: Array.from({ length: 10000 }, (_, i) => [`i:${i}`, `t:row ${i}`, 1]),
    });
    const t0 = performance.now();
    app.dispatch("Run", args);
    return (performance.now() - t0) * 1000;
  });
  console.log(`engine: Run(10000) = ${Math.round(t)} µs`);
  expect(t).toBeGreaterThan(0);
});
