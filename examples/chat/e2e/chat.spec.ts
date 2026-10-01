import { test, expect, type Page } from "@playwright/test";

// The chat app: seed data created at genesis, a many-to-many through a link
// entity (`Like`), a state with no default (`current`), and handlers with
// guards — an event that does not apply is rejected, not half-applied.

const users = (page: Page) => page.locator(".users button");
const rows = (page: Page) => page.locator("table tr");
/** Each message, as the page shows it. */
const messages = (page: Page) =>
  page.evaluate(() =>
    [...document.querySelectorAll("table tr")].map((tr) => ({
      who: tr.children[0]!.textContent,
      text: tr.children[1]!.textContent,
      likes: [...tr.children[2]!.children].map((d) => d.textContent),
      canDelete: [...tr.querySelectorAll("button")].some((b) => b.textContent === "Delete"),
      canLike: [...tr.querySelectorAll("button")].some((b) => b.textContent === "Like!"),
      canUnlike: [...tr.querySelectorAll("button")].some((b) => b.textContent === "Unlike"),
    })),
  );
/** Dispatch straight to the engine, past the page's own buttons. */
const dispatch = (page: Page, name: string, args: Record<string, string>) =>
  page.evaluate(([n, a]) => (window as any).__rexApp.dispatch(n, a), [name, args] as const);
const select = (page: Page, name: string) => users(page).filter({ hasText: name }).click();

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(users(page)).toHaveCount(3);
});

test("the seed data is there on first load, with nobody selected", async ({ page }) => {
  await expect(users(page)).toHaveText(["Alice", "Bob", "Chloe"]);
  expect(await messages(page)).toEqual([
    { who: "Alice:", text: "Welcome to Rex chat", likes: ["Bob likes this!", "Chloe likes this!"], canDelete: false, canLike: false, canUnlike: false },
    { who: "Bob:", text: "Like messages to test many-to-many joins", likes: [], canDelete: false, canLike: false, canUnlike: false },
  ]);
  // No current user: nothing to send or like with.
  await expect(page.locator(".users button.selected")).toHaveCount(0);
  await expect(page.locator(".toolbar span")).toHaveText("");
  await expect(page.locator(".send-message")).toHaveCount(0);
});

test("the seed is genesis, not an event, and a reload does not run it again", async ({ page }) => {
  const names = await page.evaluate(() => (window as any).__rexApp.logSince(0).map((e: { name: string }) => e.name));
  // Seven `new`s and the state row.
  expect(names.every((n: string) => n === "@genesis")).toBe(true);
  expect(names).toHaveLength(8);
  await page.evaluate(() => (window as any).__rexApp.flush());
  await page.reload();
  await expect(users(page)).toHaveCount(3);
  await expect(rows(page)).toHaveCount(2);
});

test("selecting a user moves the selection, the label and who may delete", async ({ page }) => {
  await select(page, "Alice");
  await expect(page.locator(".toolbar span")).toHaveText("Current user: Alice");
  expect((await messages(page)).map((m) => [m.canLike, m.canDelete])).toEqual([[true, true], [true, false]]);
  expect((await messages(page)).map((m) => m.canUnlike)).toEqual([false, false]);
  const alice = await users(page).nth(0).elementHandle();
  await select(page, "Bob");
  await expect(page.locator(".users button.selected")).toHaveText("Bob");
  await expect(page.locator(".toolbar span")).toHaveText("Current user: Bob");
  expect((await messages(page)).map((m) => m.canDelete)).toEqual([false, true]);
  // Same buttons, not rebuilt ones.
  expect(await users(page).nth(0).evaluate((el, a) => el === a, alice)).toBe(true);
});

test("send appends a message from the current user and clears the input", async ({ page }) => {
  const first = await rows(page).nth(0).elementHandle();
  await select(page, "Chloe");
  await page.locator(".send-message").fill("hello, (world)");
  await page.locator(".send-message").press("Enter");
  await expect(rows(page)).toHaveCount(3);
  expect((await messages(page))[2]).toEqual({ who: "Chloe:", text: "hello, (world)", likes: [], canDelete: true, canLike: true, canUnlike: false });
  await expect(page.locator(".send-message")).toHaveValue("");
  expect(await rows(page).nth(0).evaluate((el, a) => el === a, first)).toBe(true);
});

test("a user likes a message once: liking again takes it back", async ({ page }) => {
  await select(page, "Bob");
  // Bob already likes the first message (seed data), so it offers "Unlike".
  expect((await messages(page)).map((m) => [m.canLike, m.canUnlike])).toEqual([[false, true], [true, false]]);
  await rows(page).nth(1).getByText("Like!").click();
  expect((await messages(page))[1]!.likes).toEqual(["Bob likes this!"]);
  expect((await messages(page))[1]).toMatchObject({ canLike: false, canUnlike: true });
  // The same event again takes it back, rather than adding a second like.
  await rows(page).nth(1).getByText("Unlike").click();
  expect((await messages(page))[1]!.likes).toEqual([]);
  await rows(page).nth(0).getByText("Unlike").click();
  expect((await messages(page))[0]!.likes).toEqual(["Chloe likes this!"]);
  // Whose likes they are follows the selection.
  await select(page, "Chloe");
  expect((await messages(page)).map((m) => m.canUnlike)).toEqual([true, false]);
});

test("delete removes the message and its likes", async ({ page }) => {
  await select(page, "Alice");
  await rows(page).nth(0).getByText("Delete").click();
  expect((await messages(page)).map((m) => m.text)).toEqual(["Like messages to test many-to-many joins"]);
  // No likes are left pointing at the deleted message.
  const likes = await page.evaluate(() => JSON.parse((window as any).__rexApp.wasm.read_view("main#unit#message#like")));
  expect(likes).toEqual([]);
});

test("a reload restores messages, likes and the current user", async ({ page }) => {
  await select(page, "Bob");
  await page.locator(".send-message").fill("persist me");
  await page.locator(".send-message").press("Enter");
  await rows(page).nth(2).getByText("Like!").click();
  const before = await messages(page);
  await page.evaluate(() => (window as any).__rexApp.flush());

  await page.reload();
  await expect(rows(page)).toHaveCount(3);
  expect(await messages(page)).toEqual(before);
  await expect(page.locator(".users button.selected")).toHaveText("Bob");
  await expect(page.locator(".toolbar span")).toHaveText("Current user: Bob");
});

test("a guard rejects an event the page would never send, with its reason", async ({ page }) => {
  const before = await messages(page);
  // Nobody is selected.
  expect(await dispatch(page, "MessageSent", { text: "t:from nobody" })).toEqual({
    ids: [], deltas: {}, rejected: "pick a user and type something",
  });
  expect((await dispatch(page, "MessageLiked", { msg: "#2:0" })).rejected).toBe("pick a user first");
  // Bob may not delete Alice's message, whatever the buttons show.
  await select(page, "Bob");
  expect((await dispatch(page, "MessageDeleted", { msg: "#2:0" })).rejected).toBe("only the sender can delete a message");
  // A user, or a message, that does not exist.
  expect((await dispatch(page, "UserSelected", { user: "#1:99" })).rejected).toBeDefined();
  expect((await dispatch(page, "MessageLiked", { msg: "#2:99" })).rejected).toBe("pick a user first");
  expect(await messages(page)).toEqual(
    before.map((m) => ({ ...m, canLike: m.who === "Bob:", canUnlike: m.who === "Alice:", canDelete: m.who === "Bob:" })),
  );
  // Nothing rejected was logged.
  const names = await page.evaluate(() => (window as any).__rexApp.logSince(0).map((e: { name: string }) => e.name));
  expect(names.filter((n: string) => n !== "@genesis")).toEqual(["UserSelected"]);
});

test("a rejected event from the page stops its handler and is not an error", async ({ page }) => {
  const errors: string[] = [];
  const notes: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("console", (m) => notes.push(`${m.type()}: ${m.text()}`));
  await select(page, "Alice");
  // An empty message: the guard says no.
  await page.locator(".send-message").press("Enter");
  await expect(rows(page)).toHaveCount(2);
  expect(notes).toContain("info: [rex] MessageSent rejected: pick a user and type something");
  expect(errors).toEqual([]);
  expect(notes.filter((n) => n.startsWith("error") || n.startsWith("warning"))).toEqual([]);
  // A mistyped call, on the other hand, throws.
  await expect(dispatch(page, "MessageSent", { text: "not encoded" })).rejects.toThrow();
});
