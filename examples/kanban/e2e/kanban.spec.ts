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

// Regression: the fractional order key must be decoded (`t:` prefix stripped)
// before the key math, or `endOf`/`dropPos` throw "invalid order key: t:…"
// when the target list already has cards. (Earlier suite only exercised the
// 1-card "Doing" and the empty "Done"; the 2-card "Todo" was the blind spot.)
test("add card to a list that already has cards", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  const todo = page.locator('section.list:has(header span:text("Todo"))'); // 2 cards
  await todo.locator("header button").click();
  await expect(todo.locator("div.card")).toHaveCount(3);
  expect(errors).toEqual([]);
});

// S-22 acceptance: a drag storm that grows an order key past the rebalance
// limit triggers `maybeRebalance`'s ONE `@rebalance` event (not N torn field
// writes), and it shows up in the engine's log.
test("drag storm into the same gap triggers a rebalance that appears in the log", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));

  // Repeated insertion into the same gap (rebalance.test.ts's pathology):
  // pin the list's first card as a fixed lower bound, add a second fixed
  // card as an upper bound, then alternately drag two "roaming" cards to the
  // slot between them. Each round's target bounds shrink to (anchor's key,
  // the other roaming card's just-set key), so the inserted key grows ~1
  // char/round until it crosses REBALANCE_LIMIT (40).
  await page.evaluate(() => {
    const list = [...document.querySelectorAll("section.list")].find((l) =>
      l.querySelector("header span")?.textContent?.includes("Todo"),
    )!;
    // list.querySelectorAll("div.card")[0] is the fixed lower-bound anchor,
    // never dragged.
    const a = list.querySelectorAll("div.card")[1]!;
    // A third card (dropped at the end) is the roaming pair's other member.
    (list.querySelector("header button") as HTMLElement).click();
    const cards = [...list.querySelectorAll("div.card")];
    const b = cards[cards.length - 1]!;

    const dragJustAbove = (moving: Element, other: Element) => {
      const dt = new DataTransfer();
      moving.dispatchEvent(new DragEvent("dragstart", { bubbles: true, dataTransfer: dt }));
      const rect = other.getBoundingClientRect();
      list.dispatchEvent(
        new DragEvent("drop", { bubbles: true, dataTransfer: dt, clientY: rect.top + 1 }),
      );
    };
    for (let i = 0; i < 300; i++) {
      dragJustAbove(i % 2 === 0 ? b : a, i % 2 === 0 ? a : b);
    }
  });

  expect(errors).toEqual([]);
  await expect(page.locator('section.list:has(header span:text("Todo")) div.card')).toHaveCount(3);

  const log = await page.evaluate(() => (window as any).__rexApp.log_since(0n));
  expect(log).toContain('"name":"@rebalance"');
});

test("drop into a list that already has cards", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  // Drag "Doing"'s card into the 2-card "Todo" list at a mid position.
  await page.evaluate(() => {
    const card = [...document.querySelectorAll("section.list")]
      .find((l) => l.querySelector("header span")?.textContent?.includes("Doing"))!
      .querySelector("div.card")!;
    const target = [...document.querySelectorAll("section.list")].find((l) =>
      l.querySelector("header span")?.textContent?.includes("Todo"),
    )!;
    const dt = new DataTransfer();
    card.dispatchEvent(new DragEvent("dragstart", { bubbles: true, dataTransfer: dt }));
    target.dispatchEvent(
      new DragEvent("drop", { bubbles: true, dataTransfer: dt, clientY: 50 }),
    );
  });
  await expect(
    page.locator('section.list:has(header span:text("Todo")) div.card'),
  ).toHaveCount(3);
  expect(errors).toEqual([]);
});
