import { test, expect, type Page } from "@playwright/test";

/**
 * A seeded random walk through the real app — real wasm engine, generated
 * code, shaper, IndexedDB — against a twenty-line model of TodoMVC. After
 * every action the page must show exactly what the model holds, and every so
 * often the page is reloaded and must come back the same from its log.
 *
 * The unit-level suites check each layer against the next; this one checks
 * that the layers, stacked, still add up to the app.
 */

interface Todo { text: string; done: boolean }
type Filter = "All" | "Active" | "Completed";

const TEXTS = [
  "a", "buy milk", "", " ", "  padded  ", "x,y", "(paren)", "back\\slash", '"quoted"', "<b>not html</b>", "&amp;",
  "é 😀 日本", "t:looks encoded", "#1:0", "@True", "tab\there", "very ".repeat(40) + "long",
];

function rng(seed: number) {
  let s = seed >>> 0;
  const next = () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  return { int: (n: number) => Math.floor(next() * n), pick: <T>(xs: readonly T[]) => xs[Math.floor(next() * xs.length)]! };
}

const visible = (todos: Todo[], f: Filter) => todos.filter((t) => (f === "All" ? true : f === "Active" ? !t.done : t.done));

/** Everything the page shows, in one round trip. */
const read = (page: Page) =>
  page.evaluate(() => {
    const q = (s: string) => document.querySelector(s);
    return {
      rows: [...document.querySelectorAll(".todo-list li")].map((li) => ({
        text: li.querySelector("label")!.textContent,
        done: li.classList.contains("completed"),
        checked: (li.querySelector(".toggle") as HTMLInputElement).checked,
        editing: li.classList.contains("editing"),
        edit: (li.querySelector(".edit") as HTMLInputElement).value,
      })),
      count: q(".todo-count")?.textContent ?? null,
      selected: [...document.querySelectorAll(".filters a.selected")].map((a) => a.textContent),
      main: !!q(".main"),
      footer: !!q(".footer"),
      clear: !!q(".clear-completed"),
      toggleAll: (q("#toggle-all") as HTMLInputElement | null)?.checked ?? null,
      // Source order inside the app: header, then main, then footer.
      order: [...q(".todoapp")!.children].map((c) => c.className),
      input: (q(".new-todo") as HTMLInputElement).value,
    };
  });

function expected(todos: Todo[], filter: Filter) {
  const any = todos.length > 0;
  const active = todos.filter((t) => !t.done).length;
  return {
    rows: visible(todos, filter).map((t) => ({ text: t.text, done: t.done, checked: t.done, editing: false, edit: t.text })),
    count: any ? `${active} items left` : null,
    selected: any ? [filter] : [],
    main: any,
    footer: any,
    clear: todos.some((t) => t.done),
    toggleAll: any ? active === 0 : null,
    order: any ? ["header", "main", "footer"] : ["header"],
    input: "",
  };
}

async function walk(page: Page, seed: number, steps: number) {
  const r = rng(seed);
  let todos: Todo[] = [];
  let filter: Filter = "All";
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));

  await page.goto("/");
  await expect(page.locator(".new-todo")).toBeVisible();

  for (let step = 0; step < steps; step++) {
    const shown = visible(todos, filter);
    const row = shown.length ? r.int(shown.length) : -1;
    const li = page.locator(".todo-list li").nth(row);
    const action = r.int(todos.length ? 11 : 2);
    let what: string;
    switch (action) {
      case 0: case 1: case 2: {
        const text = r.pick(TEXTS);
        await page.locator(".new-todo").fill(text);
        await page.locator(".new-todo").press("Enter");
        todos.push({ text, done: false });
        what = `add ${JSON.stringify(text)}`;
        break;
      }
      case 3: case 4: {
        if (row < 0) continue;
        await li.locator(".toggle").click();
        shown[row]!.done = !shown[row]!.done;
        what = `toggle row ${row}`;
        break;
      }
      case 5: {
        if (row < 0) continue;
        await li.locator(".destroy").click();
        todos = todos.filter((t) => t !== shown[row]);
        what = `delete row ${row}`;
        break;
      }
      case 6: {
        if (row < 0) continue;
        const text = r.pick(TEXTS);
        // Dispatched, not clicked: a todo with empty text has a label with
        // no area to click (the example does not refuse empty todos).
        await li.locator("label").dispatchEvent("dblclick");
        await expect(li.locator(".edit")).toBeFocused();
        await li.locator(".edit").fill(text);
        if (r.int(3) === 0) {
          await li.locator(".edit").press("Escape");
          what = `edit row ${row}, then escape`;
        } else {
          await li.locator(".edit").press("Enter");
          shown[row]!.text = text;
          what = `edit row ${row} to ${JSON.stringify(text)}`;
        }
        break;
      }
      case 7: {
        const all = todos.every((t) => t.done);
        await page.locator("#toggle-all").click();
        todos.forEach((t) => (t.done = !all));
        what = "toggle all";
        break;
      }
      case 8: {
        if (!todos.some((t) => t.done)) continue;
        await page.locator(".clear-completed").click();
        todos = todos.filter((t) => !t.done);
        what = "clear completed";
        break;
      }
      default: {
        filter = r.pick(["All", "Active", "Completed"] as const);
        await page.locator(".filters a", { hasText: new RegExp(`^${filter}$`) }).click();
        what = `filter ${filter}`;
      }
    }
    expect(await read(page), `seed ${seed}, step ${step}: after ${what}`).toEqual(expected(todos, filter));

    if (step % 17 === 16) {
      // Everything so far is in the log; a reload must rebuild it exactly.
      await page.evaluate(() => (window as any).__rexApp.flush());
      await page.reload();
      await expect(page.locator(".new-todo")).toBeVisible();
      expect(await read(page), `seed ${seed}, step ${step}: after a reload`).toEqual(expected(todos, filter));
    }
  }
  expect(errors, `seed ${seed}: page errors`).toEqual([]);
}

for (const seed of [1, 2, 3]) {
  test(`random walk, seed ${seed}`, async ({ page }) => {
    await walk(page, seed, Number(process.env.REX_FUZZ_STEPS ?? 70));
  });
}
