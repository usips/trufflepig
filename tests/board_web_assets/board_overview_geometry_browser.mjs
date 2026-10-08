// Run against an existing, privately seeded Board Web listener; never starts a server.
// CLI: node <script> --playwright <absolute module> --chromium <absolute executable>
// --bootstrap-url-file <0600 URL file> --plan <P#> [--project <W#>].
// Fixture: an active plan, at least two claims, long paths, and a long Needs you entry.
import assert from "node:assert/strict";
import { readFile, stat } from "node:fs/promises";
import { createRequire } from "node:module";
import { isAbsolute, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

export async function board_overview_geometry(page, {
  width = 1440, height = 1000, colorScheme = "light", planRef,
} = {}) {
  await page.setViewportSize({ width, height });
  await page.emulateMedia({ colorScheme });
  await page.waitForFunction(() => document.querySelector(".swimlane")
    && document.getElementById("main")?.getAttribute("aria-busy") === "false");
  const observation = await page.evaluate(plan => {
    const bounds = element => {
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom,
        width: rect.width, height: rect.height };
    };
    const lane = plan ? document.querySelector(`[data-focus-key="swimlane:${plan}"]`)
      : [...document.querySelectorAll(".swimlane")].find(node => node.querySelector(".lane-columns"));
    const grid = lane?.querySelector(".lane-columns"), done = lane?.querySelector(".lane-done");
    const working = document.querySelector(".working-panel"), layout = document.querySelector(".overview-layout");
    const constrained = ".working-strip,.working-card,.lane-columns,.lane-column,.lane-done,.needs-rail,.attention-card,.event-row";
    return {
      viewport: { width: innerWidth, clientWidth: document.documentElement.clientWidth,
        scrollWidth: document.documentElement.scrollWidth },
      lane: bounds(lane), grid: bounds(grid), done: bounds(done),
      doneOpen: Boolean(done?.open), doneIsFooter: done?.parentElement === lane,
      recentDone: done?.querySelectorAll(".task-card").length,
      doneHistory: done?.querySelector(".done-history-link")?.getAttribute("href"),
      columnCount: grid ? getComputedStyle(grid).gridTemplateColumns.split(" ").length : 0,
      activeColumns: grid?.querySelectorAll(".lane-column").length,
      working: bounds(working), layout: bounds(layout),
      workingCards: [...document.querySelectorAll(".working-card")].map(card => ({
        ...bounds(card), scope: bounds(card.querySelector(".working-scope")),
        task: bounds(card.querySelector(".working-task")),
      })),
      previews: [...document.querySelectorAll(".attention-preview")].map(node => ({
        codepoints: Array.from(node.textContent).length, ...bounds(node),
      })),
      activitySummarySizes: [...document.querySelectorAll(".event-summary")]
        .map(node => Array.from(node.textContent).length),
      overflows: [...document.querySelectorAll(constrained)].filter(node => node.clientWidth
        && node.scrollWidth > node.clientWidth + 1).map(node => ({
        className: node.className, clientWidth: node.clientWidth, scrollWidth: node.scrollWidth,
      })),
    };
  }, planRef);
  observation.colorScheme = colorScheme;
  try {
    assert.ok(observation.grid && observation.done, "The fixture must contain an active plan and Done disclosure");
    assert.equal(observation.doneOpen, false, "Done must start collapsed");
    assert.equal(observation.doneIsFooter, true, "Done must sit below the active grid");
    assert.ok(observation.done.top >= observation.grid.bottom - 1, "Done must follow the complete active grid");
    assert.ok(observation.done.height > 0 && observation.done.height < 60, "Closed Done must be compact");
    assert.ok(Math.abs(observation.done.width - (observation.lane.width - 2)) <= 2,
      "Done must span the plan lane");
    assert.ok(observation.recentDone <= 5, "Done preview must contain at most five tasks");
    assert.match(observation.doneHistory || "", /tab=done/, "Done must link to complete history");
    assert.equal(observation.activeColumns, 4);
    assert.equal(observation.columnCount, width <= 560 ? 1 : width <= 1000 ? 2 : 4);
    assert.ok(observation.working && observation.layout, "Working now must be prominent");
    assert.ok(observation.working.bottom <= observation.layout.top, "Working now must precede the plan layout");
    assert.ok(observation.workingCards.length >= 2, "Seed multiple claims to exercise wrapping");
    for (const card of observation.workingCards) {
      assert.ok(card.left >= -1 && card.right <= observation.viewport.clientWidth + 1,
        "Every working agent must fit without horizontal scrolling");
      assert.ok(card.scope?.height > 0 && card.task?.height > 0, "Task title and scope must be visible");
    }
    assert.ok(observation.previews.length, "Seed a long Needs you entry");
    assert.ok(observation.previews.every(preview => preview.codepoints <= 280));
    assert.ok(observation.activitySummarySizes.every(size => size <= 280));
    assert.ok(observation.viewport.scrollWidth <= observation.viewport.clientWidth + 1,
      "Overview must not overflow the viewport");
    assert.deepEqual(observation.overflows, [], "Long paths must wrap inside cards and activity");
  } catch (error) {
    error.boardGeometry = observation;
    throw error;
  }
  return observation;
}

export async function board_done_disclosure_persists(page, { planRef } = {}) {
  assert.match(planRef || "", /^P[1-9]\d*$/, "Supply the active fixture plan");
  const selector = `[data-focus-key="swimlane:${planRef}"] .lane-done`;
  const originalHash = new URL(page.url()).hash;
  const waitOpen = open => page.waitForFunction(({ selector, open }) =>
    document.querySelector(selector)?.open === open, { selector, open });
  const refresh = async () => {
    await page.locator(selector).evaluate(node => { node.dataset.beforeRefresh = "true"; });
    await page.locator("#refresh").click();
    await page.waitForFunction(selector => {
      const done = document.querySelector(selector);
      return done && !done.dataset.beforeRefresh
        && document.getElementById("main").getAttribute("aria-busy") === "false";
    }, selector);
  };
  await page.locator(selector + " summary").click();
  await waitOpen(true);
  await refresh(); await waitOpen(true);
  const project = new URLSearchParams(originalHash.split("?")[1] || "").get("project");
  await page.evaluate(project => {
    location.hash = "#/attention" + (project ? "?project=" + encodeURIComponent(project) : "");
  }, project);
  await page.waitForFunction(() => !document.querySelector(".lane-done")
    && document.getElementById("main").getAttribute("aria-busy") === "false");
  await page.evaluate(hash => { location.hash = hash; }, originalHash);
  await waitOpen(true);
  await page.locator(selector + " summary").click(); await waitOpen(false);
  await refresh(); await waitOpen(false);
  return { planRef, retainedOpenAcrossRefresh: true, retainedOpenAcrossNavigation: true,
    retainedClosedAcrossRefresh: true };
}

function browserOptions(arguments_) {
  const options = {}, keys = {
    "--playwright": "playwright", "--chromium": "chromium",
    "--bootstrap-url-file": "bootstrapUrlFile", "--plan": "planRef", "--project": "project",
  };
  for (let index = 0; index < arguments_.length; index += 2) {
    const option = arguments_[index], value = arguments_[index + 1];
    assert.ok(Object.hasOwn(keys, option) && value && !value.startsWith("--"), "Invalid browser option");
    assert.equal(options[keys[option]], undefined, "Duplicate browser option");
    options[keys[option]] = value;
  }
  for (const key of ["playwright", "chromium", "bootstrapUrlFile"]) {
    assert.ok(options[key] && isAbsolute(options[key]), key + " requires an explicit absolute path");
  }
  assert.match(options.planRef || "", /^P[1-9]\d*$/);
  assert.ok(process.env.TMPDIR && isAbsolute(process.env.TMPDIR), "Set TMPDIR to on-disk scratch");
  const scratch = resolve(process.env.TMPDIR);
  assert.ok(scratch !== "/tmp" && !scratch.startsWith("/tmp/"));
  return options;
}

async function runOverviewGeometryBrowser(options) {
  const privateFile = await stat(options.bootstrapUrlFile);
  assert.equal(privateFile.mode & 0o777, 0o600, "Bootstrap URL file must have mode 0600");
  const bootstrapUrl = (await readFile(options.bootstrapUrlFile, "utf8")).trim();
  const address = new URL(bootstrapUrl);
  assert.equal(address.protocol, "http:");
  assert.ok(["127.0.0.1", "[::1]", "localhost"].includes(address.hostname));
  assert.match(address.hash, /^#token=[a-f0-9]{64}$/);
  const { chromium } = createRequire(import.meta.url)(options.playwright);
  const browser = await chromium.launch({ headless: true, executablePath: options.chromium });
  try {
    const page = await browser.newPage();
    await page.goto(bootstrapUrl);
    await page.waitForSelector(".swimlane");
    if (options.project) await page.locator("#project").selectOption(options.project);
    await page.waitForSelector(`[data-focus-key="swimlane:${options.planRef}"]`);
    for (const colorScheme of ["light", "dark"]) {
      for (const width of [1440, 768, 390]) {
        console.log(JSON.stringify(await board_overview_geometry(page, {
          width, colorScheme, planRef: options.planRef,
        })));
      }
    }
    console.log(JSON.stringify(await board_done_disclosure_persists(page, { planRef: options.planRef })));
  } finally { await browser.close(); }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  await test("board_overview_geometry_and_done_persistence", async () => {
    try { await runOverviewGeometryBrowser(browserOptions(process.argv.slice(2))); }
    catch (error) {
      if (error.boardGeometry) console.error(JSON.stringify(error.boardGeometry));
      const redact = text => text.replace(/([#?&]token=)[a-f0-9]{64}/gi, "$1[redacted]");
      const safeError = new Error(redact(error.message));
      safeError.name = error.name;
      safeError.stack = redact(error.stack || safeError.stack);
      throw safeError;
    }
  });
}
