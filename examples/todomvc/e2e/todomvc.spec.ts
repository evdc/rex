import { test, expect, type Page } from "@playwright/test";

const add = async (page: Page, ...texts: string[]) => {
  for (const t of texts) {
    await page.locator(".new-todo").fill(t);
    await page.locator(".new-todo").press("Enter");
  }
};
const labels = (page: Page) => page.locator(".todo-list li label");

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".new-todo")).toBeVisible();
});

test("chrome is hidden until there is a todo", async ({ page }) => {
  await expect(page.locator(".main")).toHaveCount(0);
  await expect(page.locator(".footer")).toHaveCount(0);
  await add(page, "a");
  await expect(page.locator(".footer")).toBeVisible();
});

test("add clears the input, focuses it, and keeps id order (past 9)", async ({ page }) => {
  await expect(page.locator(".new-todo")).toBeFocused();
  const texts = Array.from({ length: 12 }, (_, i) => `t${i + 1}`);
  await add(page, ...texts);
  await expect(page.locator(".new-todo")).toHaveValue("");
  await expect(labels(page)).toHaveText(texts);
  await expect(page.locator(".todo-count")).toHaveText("12 items left");
});

test("other keys do not add", async ({ page }) => {
  await page.locator(".new-todo").fill("nope");
  await page.locator(".new-todo").press("a");
  await expect(page.locator(".todo-list li")).toHaveCount(0);
});

test("toggle, count, filters, clear completed", async ({ page }) => {
  await add(page, "a", "b", "c");
  await page.locator(".todo-list li").nth(1).locator(".toggle").check();
  await expect(page.locator(".todo-list li").nth(1)).toHaveClass(/completed/);
  await expect(page.locator(".todo-count")).toHaveText("2 items left");

  await page.getByText("Active", { exact: true }).click();
  await expect(labels(page)).toHaveText(["a", "c"]);
  await expect(page.locator(".filters a.selected")).toHaveText("Active");

  await page.getByText("Completed", { exact: true }).click();
  await expect(labels(page)).toHaveText(["b"]);

  await page.getByText("All", { exact: true }).click();
  await expect(labels(page)).toHaveText(["a", "b", "c"]);

  await page.locator(".clear-completed").click();
  await expect(labels(page)).toHaveText(["a", "c"]);
  await expect(page.locator(".clear-completed")).toHaveCount(0);
});

test("toggle all, and delete", async ({ page }) => {
  await add(page, "a", "b");
  await page.locator("#toggle-all").check();
  await expect(page.locator(".todo-list li.completed")).toHaveCount(2);
  await expect(page.locator(".todo-count")).toHaveText("0 items left");
  await page.locator("#toggle-all").uncheck();
  await expect(page.locator(".todo-list li.completed")).toHaveCount(0);

  await page.locator(".todo-list li").first().hover();
  await page.locator(".todo-list li").first().locator(".destroy").click();
  await expect(labels(page)).toHaveText(["b"]);
  await page.locator(".destroy").click();
  await expect(page.locator(".main")).toHaveCount(0);
});

test("edit: double-click focuses the field; enter commits; the row node survives", async ({ page }) => {
  await add(page, "a", "b");
  const row = page.locator(".todo-list li").first();
  const handle = await row.elementHandle();
  await row.locator("label").dblclick();
  await expect(row).toHaveClass(/editing/);
  await expect(row.locator(".edit")).toBeFocused();
  await row.locator(".edit").fill("a2");
  await row.locator(".edit").press("Enter");
  await expect(row).not.toHaveClass(/editing/);
  await expect(labels(page)).toHaveText(["a2", "b"]);
  expect(await handle!.evaluate((n) => n.isConnected)).toBe(true);
});

test("edit: escape cancels; blur commits", async ({ page }) => {
  await add(page, "a");
  const row = page.locator(".todo-list li").first();
  await row.locator("label").dblclick();
  await row.locator(".edit").fill("zzz");
  await row.locator(".edit").press("Escape");
  await expect(row).not.toHaveClass(/editing/);
  await expect(labels(page)).toHaveText(["a"]);

  await row.locator("label").dblclick();
  await row.locator(".edit").fill("kept");
  await page.locator(".new-todo").click();
  await expect(labels(page)).toHaveText(["kept"]);
});

test("reload restores todos, completion and filter", async ({ page }) => {
  await add(page, "a", "b", "c");
  await page.locator(".todo-list li").first().locator(".toggle").check();
  await page.getByText("Active", { exact: true }).click();
  await expect(labels(page)).toHaveText(["b", "c"]);
  await page.reload();
  await expect(labels(page)).toHaveText(["b", "c"]);
  await expect(page.locator(".filters a.selected")).toHaveText("Active");
  await page.getByText("All", { exact: true }).click();
  await expect(page.locator(".todo-list li").first()).toHaveClass(/completed/);
  await expect(page.locator(".todo-count")).toHaveText("2 items left");
});
