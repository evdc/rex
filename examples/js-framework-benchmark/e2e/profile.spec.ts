import { test } from "@playwright/test";

// Where create-10k's script time goes, in Chrome: a CPU profile of one click,
// self time summed by function. Run on demand:
//   npx playwright test e2e/profile.spec.ts --reporter=line
// (skipped unless REX_PROFILE=1, since it is a measurement, not a check).
test.skip(!process.env.REX_PROFILE, "set REX_PROFILE=1 to profile");

async function profileClick(page: any, selector: string, label: string) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Profiler.enable");
  await cdp.send("Profiler.setSamplingInterval", { interval: 50 });
  await cdp.send("Profiler.start");
  const wall = await page.evaluate(async (sel: string) => {
    const t0 = performance.now();
    (document.querySelector(sel) as HTMLElement).click();
    const t1 = performance.now();
    await new Promise((r) => requestAnimationFrame(() => setTimeout(r, 0)));
    return { click: t1 - t0, toFrame: performance.now() - t0 };
  }, selector);
  const { profile } = await cdp.send("Profiler.stop");
  const timings = await page.evaluate(() => (window as any).__rexProfile?.timings.slice(-1)[0]);
  // Self time per node from samples.
  const dt = new Map<number, number>();
  for (let i = 0; i < profile.samples.length; i++) {
    dt.set(profile.samples[i], (dt.get(profile.samples[i]) ?? 0) + (profile.timeDeltas[i] ?? 0) / 1000);
  }
  const byFn = new Map<string, number>();
  for (const n of profile.nodes) {
    const cf = n.callFrame;
    const file = (cf.url as string).split("/").pop()?.split("?")[0] ?? "";
    const name = `${cf.functionName || "(anon)"} ${file}${file ? ":" + cf.lineNumber : ""}`;
    byFn.set(name, (byFn.get(name) ?? 0) + (dt.get(n.id) ?? 0));
  }
  const top = [...byFn].filter(([k]) => !k.startsWith("(idle)") && !k.startsWith("(program)")).sort((a, b) => b[1] - a[1]).slice(0, 30);
  console.log(`\n=== ${label}: click handler ${wall.click.toFixed(1)} ms, to next frame ${wall.toFrame.toFixed(1)} ms`);
  console.log(`    profile split: ${JSON.stringify(timings)}`);
  for (const [k, v] of top) console.log(`${v.toFixed(1).padStart(8)} ms  ${k}`);
}

test("profile create 10,000", async ({ page }) => {
  await page.goto("/?ephemeral&profile");
  await page.waitForSelector("#runlots");
  // Warm up the code paths once (JIT), then measure a create over existing rows
  // and one from empty, like the harness's create-10k (which starts empty).
  await page.click("#run");
  await page.click("#clear");
  await profileClick(page, "#runlots", "create 10k (from empty)");
  await profileClick(page, "#clear", "clear 10k");
  await profileClick(page, "#run", "create 1k");
});
