import { test, expect, type Page } from "@playwright/test";

// The chat app: a many-to-many through a link entity (`Like`), a state with
// no default (`current`), and one event that builds a graph of rows which
// refer to each other (`let x = new …`).

const users = (page: Page) => page.locator(".users button");
const rows = (page: Page) => page.locator("table tr");
/** Each message as [first cell, text, likes…]. */
const messages = (page: Page) =>
  page.evaluate(() =>
    [...document.querySelectorAll("table tr")].map((tr) => ({
      who: tr.children[0]!.textContent,
      text: tr.children[1]!.textContent,
      likes: [...tr.children[2]!.children].map((d) => d.textContent),
      canDelete: [...tr.querySelectorAll("button")].some((b) => b.textContent === "Delete"),
      canLike: [...tr.querySelectorAll("button")].some((b) => b.textContent === "Like!"),
    })),
  );
const seed = async (page: Page) => {
  await page.getByText("Load Synthetic Data").click();
  await expect(users(page)).toHaveCount(3);
};

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("h1")).toHaveText("Chat App");
});

test("with no user selected there is nothing to send or like with", async ({ page }) => {
  await expect(users(page)).toHaveCount(0);
  await expect(rows(page)).toHaveCount(0);
  await expect(page.locator(".send-message")).toHaveCount(0);
  await expect(page.locator(".toolbar span")).toHaveText("");
});

test("one event seeds users, messages that name them, and likes that name both", async ({ page }) => {
  await seed(page);
  await expect(users(page)).toHaveText(["Alice", "Bob", "Chloe"]);
  expect(await messages(page)).toEqual([
    { who: "Alice:", text: "Welcome to Rex chat", likes: ["Bob likes this!", "Chloe likes this!"], canDelete: true, canLike: true },
    { who: "Bob:", text: "Like messages to test many-to-many joins", likes: [], canDelete: false, canLike: true },
  ]);
  // The seed also selected Alice.
  await expect(page.locator(".users button.selected")).toHaveText("Alice");
  await expect(page.locator(".toolbar span")).toHaveText("Current user: Alice");
  await expect(page.locator(".send-message")).toBeVisible();
});

test("the seed is one logged event", async ({ page }) => {
  await seed(page);
  const names = await page.evaluate(() => (window as any).__rexApp.logSince(0).map((e: { name: string }) => e.name));
  expect(names.filter((n: string) => n === "SeedSynthetic")).toHaveLength(1);
  expect(names.filter((n: string) => !n.startsWith("@"))).toEqual(["SeedSynthetic"]);
});

test("selecting a user moves the selection, the label and who may delete", async ({ page }) => {
  await seed(page);
  const alice = await users(page).nth(0).elementHandle();
  await users(page).nth(1).click();
  await expect(page.locator(".users button.selected")).toHaveText("Bob");
  await expect(page.locator(".toolbar span")).toHaveText("Current user: Bob");
  expect((await messages(page)).map((m) => m.canDelete)).toEqual([false, true]);
  // Same buttons, not rebuilt ones.
  expect(await users(page).nth(0).evaluate((el, a) => el === a, alice)).toBe(true);
});

test("send appends a message from the current user and clears the input", async ({ page }) => {
  await seed(page);
  const first = await rows(page).nth(0).elementHandle();
  await users(page).nth(2).click();
  await page.locator(".send-message").fill("hello, (world)");
  await page.locator(".send-message").press("Enter");
  await expect(rows(page)).toHaveCount(3);
  expect((await messages(page))[2]).toEqual({ who: "Chloe:", text: "hello, (world)", likes: [], canDelete: true, canLike: true });
  await expect(page.locator(".send-message")).toHaveValue("");
  expect(await rows(page).nth(0).evaluate((el, a) => el === a, first)).toBe(true);
});

test("like adds a like by the current user; a second like is a second row", async ({ page }) => {
  await seed(page);
  await users(page).nth(1).click();
  const like = rows(page).nth(1).getByText("Like!");
  await like.click();
  await like.click();
  expect((await messages(page))[1]!.likes).toEqual(["Bob likes this!", "Bob likes this!"]);
  expect((await messages(page))[0]!.likes).toEqual(["Bob likes this!", "Chloe likes this!"]);
});

test("delete removes the message and its likes, and only the sender may", async ({ page }) => {
  await seed(page);
  await rows(page).nth(0).getByText("Delete").click();
  expect((await messages(page)).map((m) => m.text)).toEqual(["Like messages to test many-to-many joins"]);
  // No likes are left pointing at the deleted message.
  const likes = await page.evaluate(() => JSON.parse((window as any).__rexApp.wasm.read_view("main#unit#message#like")));
  expect(likes).toEqual([]);
});

test("a reload restores users, messages, likes and the current user", async ({ page }) => {
  await seed(page);
  await users(page).nth(1).click();
  await page.locator(".send-message").fill("persist me");
  await page.locator(".send-message").press("Enter");
  await rows(page).nth(2).getByText("Like!").click();
  const before = await messages(page);
  await page.evaluate(() => (window as any).__rexApp.flush());

  await page.reload();
  await expect(users(page)).toHaveCount(3);
  expect(await messages(page)).toEqual(before);
  await expect(page.locator(".users button.selected")).toHaveText("Bob");
  await expect(page.locator(".toolbar span")).toHaveText("Current user: Bob");
  // Ids keep counting from where the log left off: a second seed adds three
  // new users rather than colliding with the first three.
  await page.getByText("Load Synthetic Data").click();
  await expect(users(page)).toHaveCount(6);
  expect(await messages(page)).toHaveLength(5);
});

test("an event the engine refuses changes nothing and does not throw", async ({ page }) => {
  const errors: string[] = [];
  const warnings: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("console", (m) => m.type() === "warning" && warnings.push(m.text()));
  // No user is selected, so `MessageSent` has no sender to read.
  const refused = await page.evaluate(() => {
    try {
      (window as any).__rexApp.dispatch("MessageSent", { text: "t:nobody" });
      return false;
    } catch {
      return true;
    }
  });
  expect(refused).toBe(true);
  await expect(rows(page)).toHaveCount(0);
  expect(errors).toEqual([]);
});
