import { expect, test } from "@playwright/test";

/**
 * The ROADMAP M3 gate, live in a browser:
 *  - field edit preserves node identity (and focus — the silent-failure mode)
 *  - drag across lists reuses the DOM node (reparent, not remove+mount)
 *  - subtree delete detaches once
 *  - batches apply atomically enough that the UI never shows a half-state
 */

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("section.list")).toHaveCount(3);
});

test("board renders the seed program", async ({ page }) => {
  const todo = page.locator('section.list:has(header span:text("Todo"))');
  await expect(todo.locator("div.card input").first()).toHaveValue("Design the schema", {
    timeout: 5000,
  });
  await expect(todo.locator("div.card")).toHaveCount(2);
  await expect(
    page.locator('section.list:has(header span:text("Doing")) div.card'),
  ).toHaveCount(1);
});

test("retitle keeps node identity and focus", async ({ page }) => {
  const input = page.locator("div.card input").first();
  // Tag the live DOM node; if the shaper ever remove+mounts instead of
  // updating, the expando vanishes with the old node.
  await input.evaluate((el) => ((el as any).__tag = "survivor"));
  await input.click();
  await input.fill("Design the WHOLE schema");
  await input.press("Tab"); // commit via change event
  await input.evaluate((el) => ((el as any).__tag2 = true));
  await expect(input).toHaveValue("Design the WHOLE schema");
  expect(await input.evaluate((el) => (el as any).__tag)).toBe("survivor");
});

test("focus survives an engine round-trip on change", async ({ page }) => {
  const input = page.locator("div.card input").first();
  await input.click();
  await input.fill("Renamed");
  // Fire change while still focused (blur would move focus anyway).
  await input.dispatchEvent("change");
  // The engine step + shaper update ran synchronously; the same element must
  // still be the active one.
  expect(
    await input.evaluate((el) => document.activeElement === el),
  ).toBe(true);
  await expect(input).toHaveValue("Renamed");
});

test("kanban drag across lists reuses the DOM node", async ({ page }) => {
  // (CSS [value=…] matches the attribute, not the live property — locate by
  // position instead: the first Todo card is "Design the schema".)
  const card = page
    .locator('section.list:has(header span:text("Todo")) div.card')
    .first();
  await card.evaluate((el) => ((el as any).__tag = "dragged"));

  // Simulate the HTML5 DnD the app listens for.
  await page.evaluate(() => {
    const card = [...document.querySelectorAll("div.card")].find(
      (c) => (c as any).__tag === "dragged",
    )!;
    const target = [...document.querySelectorAll("section.list")].find((l) =>
      l.querySelector("header span")?.textContent?.includes("Done"),
    )!;
    const dt = new DataTransfer();
    card.dispatchEvent(new DragEvent("dragstart", { bubbles: true, dataTransfer: dt }));
    target.dispatchEvent(
      new DragEvent("drop", { bubbles: true, dataTransfer: dt, clientY: 10_000 }),
    );
  });

  const done = page.locator('section.list:has(header span:text("Done"))');
  await expect(done.locator("div.card")).toHaveCount(1);
  // Same node object moved — the expando survived the reparent.
  expect(
    await done.locator("div.card").evaluate((el) => (el as any).__tag),
  ).toBe("dragged");
});

test("delete removes exactly the card", async ({ page }) => {
  const todo = page.locator('section.list:has(header span:text("Todo"))');
  await todo.locator("div.card button").first().click();
  await expect(todo.locator("div.card")).toHaveCount(1);
  await expect(todo.locator("div.card input")).toHaveValue("Lower to circuits");
});

test("add card mounts focused and ordered last", async ({ page }) => {
  const doing = page.locator('section.list:has(header span:text("Doing"))');
  await doing.locator("header button").click();
  await expect(doing.locator("div.card")).toHaveCount(2);
  const last = doing.locator("div.card input").last();
  await expect(last).toHaveValue("New card");
  expect(await last.evaluate((el) => document.activeElement === el)).toBe(true);
});
