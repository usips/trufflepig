import { it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardViews } from "../board_views.js";
import { createBoardRenderLoop } from "../board_render_loop.js";

installDomShim();
beforeEach(() => resetDomShim());

function harness() {
  const navigations = [];
  const state = {
    route: { view: "overview" }, clockOffsetMs: 0, seenAtOpen: null, navigation: 0,
    formDrafts: new Map(), formStatuses: new Map(), pendingForms: new Set(),
    ingestPending: false, ingestTimer: null, refreshTimer: null, generation: 0,
    watermark: "1", stream: {}, projects: [],
  };
  const dom = createBoardDom(state), noop = () => {};
  const views = createBoardViews({
    state, dom, columns: ["todo", "doing", "review", "blocked", "done"],
    collection: (data, key) => data?.[key] || [], entryRecord: value => value?.entry || value,
    permitted: () => false, pageParams: route => route, nextAfter: () => null,
    queryFilters: () => ({}), postKinds: [], entryKinds: [], errorMessage: error => error.message,
    board: noop, jsonFetch: noop, navigate: (...args) => navigations.push(args),
    scheduleRefresh: noop, notice: noop,
  });
  const data = { plans: [{
    plan: { id: "P7", title: "Readable board", head_revision: 1 }, tasks: [], claims: [],
    done_count: 25, recent_done: Array.from({ length: 5 }, (_, at) => ({
      id: `P7.${25 - at}`, title: `Completed ${25 - at}`, column: "done",
    })),
  }], events: [] };
  const overview = () => views.renderOverview(data, { entries: [] }, state.route);
  const main = dom.el("main"); document.body.append(main);
  const count = dom.el("span"); count.id = "attention-count"; document.body.append(count);
  const wiring = {
    fetchRoute: async () => ({ page: overview(), serverNow: null, watermark: "1", warnings: [] }),
    attentionItems: views.attentionItems, startStream: noop,
  };
  const loop = createBoardRenderLoop({
    state, dom, main, apiVersion: 1, seenStorage: sessionStorage, seenKey: "seen-test",
    getToken: () => "fixture", isExpired: () => false, enterExpired: noop,
    notice: noop, setConnection: noop, noteSeen: noop, wiring,
  });
  return { state, dom, views, data, overview, main, wiring, loop, navigations };
}

it("closed_done_is_a_compact_footer", () => {
  const { data, overview } = harness();
  data.plans[0].tasks = [{ id: "P7.26", title: "Active", column: "doing" }];
  const page = overview(), lane = page.querySelector(".swimlane");
  const done = page.querySelector(".lane-done"), grid = lane.querySelector(".lane-columns");
  assert.ok(done.parentElement === lane, "Done must be a sibling below the active grid");
  assert.ok(lane.children.indexOf(done) > lane.children.indexOf(grid));
  assert.equal(grid.querySelectorAll(".lane-column").length, 4);
  assert.equal(Boolean(done.open), false);
  assert.equal(done.querySelector("summary").textContent, "Done (25)");
});

it("Done previews at most five cards and always links the complete history", () => {
  const { data, overview } = harness();
  data.plans[0].recent_done.push({ id: "P7.20", title: "Sixth", column: "done" });
  const done = overview().querySelector(".lane-done");
  assert.equal(done.querySelectorAll(".task-card").length, 5);
  assert.equal(done.querySelector(".done-history-link").href, "/#/P7?tab=done");
  data.plans[0].done_count = 1; data.plans[0].recent_done.length = 1;
  assert.equal(overview().querySelector(".done-history-link").href, "/#/P7?tab=done");
});

it("a completed plan has a compact empty state and accurate active counts", () => {
  const { data, overview } = harness();
  assert.ok(!overview().querySelector(".lane-columns"), "completed plans omit the empty active grid");
  assert.equal(overview().querySelector(".lane-empty").textContent, "No active tasks.");
  data.plans[0].tasks = [{ id: "P7.26", title: "Active", column: "doing" }];
  data.plans[0].tasks_omitted = 6;
  assert.equal(overview().querySelector(".count").textContent, "7 active · 25 done");
  assert.ok(overview().querySelectorAll("a").some(node => node.textContent === "Browse all tasks"));
  data.plans[0].tasks = [];
  assert.ok(overview().querySelector(".lane-columns"), "omitted active work is not described as empty");
});

it("overview filters persist in the route and reset plan paging when toggled", () => {
  const { state, overview, navigations } = harness();
  state.route = { view: "overview", after: JSON.stringify({ seq: "80", plan: "P8" }), through: "100" };
  const page = overview(), filter = page.querySelector("input"), label = filter.parentElement;
  const disclosure = page.querySelector("details");
  const summary = disclosure.querySelector("summary");
  assert.ok(summary.textContent.includes("Filters"));
  assert.equal(summary.dataset.focusKey, "overview:filters");
  assert.equal(summary.attributes.has("tabindex"), false, "the native summary remains a tab stop");
  assert.equal(filter.dataset.focusKey, "overview:show-inactive");
  assert.equal(filter.checked, false);
  assert.equal(label.children[1].textContent, "Show plans without active tasks");
  assert.equal(page.querySelector(".overview-filter-status").textContent,
    "Showing plans with active tasks, sorted by most recently updated.");
  disclosure.open = true;
  disclosure.listeners.get("toggle")[0]();
  assert.equal(state.overviewFiltersOpen, true);
  filter.checked = true;
  filter.listeners.get("change")[0]({ target: filter });
  assert.deepEqual(navigations[0], ["overview", {
    view: "overview", after: "", through: "", showInactive: "1",
  }, false, false], "filter changes preserve the focused checkbox while reloading the overview");
  state.route = { view: "overview", showInactive: "1" };
  const allPlans = overview();
  assert.equal(allPlans.querySelector("details").open, true,
    "the filter disclosure remains open across the navigation rerender");
  assert.equal(overview().querySelector("details").open, true,
    "the filter disclosure remains open across a refresh rerender");
  assert.equal(allPlans.querySelector("input").checked, true);
  assert.equal(allPlans.querySelector(".overview-filter-status").textContent,
    "Showing all plans, sorted by most recently updated.");
});

it("the active-plan empty state leaves a visible recovery filter", () => {
  const { state, data, overview } = harness();
  data.plans = [];
  const activeOnly = overview();
  assert.equal(activeOnly.querySelector(".empty").textContent, "No plans with active tasks.");
  assert.ok(activeOnly.querySelector("input"), "the filter stays available when no active plans match");
  assert.equal(activeOnly.querySelector(".overview-filter-status").textContent,
    "Showing plans with active tasks, sorted by most recently updated.");
  state.route = { view: "overview", showInactive: "1" };
  assert.equal(overview().querySelector(".empty").textContent, "No plans yet.");
});

function visibleText(node) {
  return `${node.textContent || ""}${node.children.map(visibleText).join("")}`;
}

it("Working now renders one dense row per claim with full detail in titles", () => {
  const { data, overview } = harness();
  const actor = { user: "josh", host: "archlinux", harness: "codex", session: "full-session-id" };
  const worker = (handle, extra = {}) => ({
    address: { task: "P7.26", handle }, role: "review", scope: `Review ${handle}`,
    actor: { ...actor, harness: "kimi", session: handle }, last_active: 5, files: ["src/long/path.rs"],
    state: null, ended_at: null, ...extra,
  });
  const claim = { task: "P7.26", actor, vendor: "codex", model: "gpt-6.1", effort: "max",
    scope: "src/deep/VeryLongFilename.js + geometry", last_active: 1, open_questions: 2,
    progress: { step: { value: { current: 2, total: 3 }, updated_at: 1 } },
    workers: [worker("parser"), worker("old", { state: "done", ended_at: 2 })] };
  data.working = [claim];
  data.plans[0].tasks = [{ id: claim.task, title: "Make active work readable", column: "doing" }];
  const page = overview(), row = page.querySelector(".working-row");
  const working = page.querySelector(".working-panel"), layout = page.querySelector(".overview-layout");
  assert.ok(working, "Working now must have its own prominent panel");
  assert.ok(page.children.indexOf(working) < page.children.indexOf(layout));
  assert.equal(working.querySelector("h2").textContent, "Working now (1)");
  assert.equal(working.querySelector(".working-all").href, "/#/claims");
  assert.equal(row.tagName, "DETAILS");
  assert.equal(Boolean(row.open), false, "rows start collapsed");
  assert.equal(row.dataset.disclosureKey, "working:P7.26");
  const summary = row.querySelector("summary");
  const task = summary.querySelector(".working-row-task");
  assert.equal(task.href, "/#/P7?tab=tasks&task=P7.26");
  assert.equal(task.querySelector(".working-row-id").textContent, "P7.26");
  assert.equal(task.querySelector(".working-row-title").textContent, "Make active work readable");
  const scope = summary.querySelector(".working-row-scope");
  assert.equal(scope.textContent, claim.scope);
  assert.equal(scope.title, claim.scope, "the full scope stays available on hover");
  const agent = summary.querySelector(".working-row-agent");
  assert.equal(agent.title, "josh@archlinux/codex/full-session-id · gpt-6.1 · max");
  const mark = agent.querySelector(".agent-mark");
  assert.equal(mark.dataset.vendor, "codex");
  assert.match(mark.attributes.get("aria-label"), /josh@archlinux\/full-session-id/);
  assert.equal(visibleText(summary.querySelector(".working-row-step")).includes("2/3"), true);
  assert.equal(summary.querySelector(".working-row-questions").title, "2 open questions");
  assert.equal(summary.querySelector(".working-row-workers").title, "1 live worker");
  assert.ok(summary.querySelector(".working-row-age").querySelector("time"));
  assert.equal(summary.querySelector(".working-row-stale").children.length, 0);
  const lines = row.querySelectorAll(".working-worker");
  assert.equal(lines.length, 1, "the body lists live workers only");
  assert.equal(lines[0].querySelector(".working-worker-handle").textContent, "/parser");
  assert.match(visibleText(row.querySelector(".working-row-footer")), /\(1 ended\)/);
  assert.equal(row.querySelector(".working-row-details").href, "/#/P7?tab=tasks&task=P7.26");
  for (const verbose of [".agent-tree", ".agent-files", ".worker-row", ".claim-signals"]) {
    assert.equal(page.querySelector(verbose), null, `Overview omits ${verbose}`);
  }
});

it("Working now hides zero counts and orders stale claims first, then by activity", () => {
  const { data, overview } = harness();
  const claim = (task, last_active, stale = false) => ({
    task, actor: "josh", vendor: "claude", scope: task, last_active, stale, open_questions: 0,
  });
  data.working = [claim("P7.1", 10), claim("P7.2", 30), claim("P7.3", 5, true), claim("P7.4", 20)];
  const rows = overview().querySelectorAll(".working-row");
  assert.deepEqual(rows.map(row => row.dataset.disclosureKey),
    ["working:P7.3", "working:P7.2", "working:P7.4", "working:P7.1"]);
  assert.equal(rows[0].dataset.stale, "true");
  assert.equal(rows[0].querySelector(".working-row-stale").querySelector(".badge").textContent, "stale");
  for (const empty of [".working-row-questions", ".working-row-workers", ".working-row-step"]) {
    assert.equal(rows[1].querySelector(empty).children.length, 0, `${empty} is empty at zero`);
  }
  assert.equal(rows[1].querySelector(".working-row-body").querySelector("p").textContent, "No live workers.");
});

it("Needs you bounds Unicode previews while the entry keeps the complete body", () => {
  const { views } = harness();
  const body = "🛰️".repeat(300) + "src/" + "LongFile".repeat(300) + ".js";
  const entry = { id: "E1", kind: "question", actor: "josh", created_at: 1, body };
  const card = views.attentionCard({ type: "question", record: entry });
  const preview = card.querySelector(".attention-preview").textContent;
  assert.ok([...preview].length <= 280);
  assert.ok(preview.endsWith("…"));
  assert.ok(!preview.includes("\uFFFD"));
  assert.equal(card.querySelector(".attention-entry-link").href, "/#/E1");
  assert.equal(views.entryCard(entry).querySelector(".entry-body").textContent, body);
});

it("activity_summary_is_compact_and_keeps_full_entry", () => {
  const { views } = harness();
  const body = "🛰️".repeat(300) + "src/" + "LongFile".repeat(300) + ".js";
  const event = { seq: "1", kind: "question", subject: "E1", summary: body,
    actor: "josh", created_at: 1 };
  const item = views.eventList([event]).querySelector(".event-row");
  const summary = item.querySelector(".event-summary").textContent;
  assert.ok([...summary].length <= 280, "Activity summaries must stay bounded");
  assert.ok(summary.endsWith("…"));
  assert.ok(!summary.includes("\uFFFD"));
  assert.equal(item.querySelector("a").href, "/#/E1");
  assert.equal(views.entryCard({ id: "E1", kind: "question", actor: "josh", created_at: 1, body })
    .querySelector(".entry-body").textContent, body);
});

it("latest activity renders newest first without mutating source events", () => {
  const { data, overview } = harness();
  const older = { seq: "9007199254740991", kind: "note", subject: "E1",
    summary: "Older", actor: "josh", created_at: 1 };
  const newest = { seq: "9007199254740993", kind: "note", subject: "E3",
    summary: "Newest", actor: "josh", created_at: 3 };
  const middle = { seq: "9007199254740992", kind: "note", subject: "E2",
    summary: "Middle", actor: "josh", created_at: 2 };
  data.events = [older, newest, middle];

  const rows = overview().querySelector(".ticker").querySelectorAll(".event-row");
  assert.deepEqual(rows.map(row => row.dataset.focusKey), [
    `event:${newest.seq}`, `event:${middle.seq}`, `event:${older.seq}`,
  ]);
  assert.deepEqual(data.events, [older, newest, middle]);
});

it("Done choices survive refresh before a native toggle event is delivered", async () => {
  const { main, overview, loop } = harness();
  main.append(overview());
  main.querySelector(".lane-done").open = true;
  await loop.loadRoute();
  assert.equal(main.querySelector(".lane-done").open, true);
  main.querySelector(".lane-done").open = false;
  await loop.loadRoute();
  assert.equal(Boolean(main.querySelector(".lane-done").open), false);
});

it("a choice made while a refresh is in flight wins over its prepared page", async () => {
  const { main, overview, loop, wiring } = harness();
  main.append(overview());
  let finish;
  wiring.fetchRoute = () => {
    const page = overview();
    return new Promise(resolve => { finish = () => resolve({ page, serverNow: null, watermark: "1", warnings: [] }); });
  };
  const refreshing = loop.loadRoute();
  main.querySelector(".lane-done").open = true;
  finish(); await refreshing;
  assert.equal(main.querySelector(".lane-done").open, true);
});

it("failed_refresh_preserves_latest_done_choice", async () => {
  const { main, overview, loop, wiring } = harness();
  main.append(overview());
  let fail;
  wiring.fetchRoute = () => new Promise((resolve, reject) => {
    fail = () => reject(new Error("Fixture read failed"));
  });
  const refreshing = loop.loadRoute();
  main.querySelector(".lane-done").open = true;
  fail(); await refreshing;
  assert.ok(main.querySelector(".callout"), "the failed refresh must replace the overview");
  wiring.fetchRoute = async () => ({ page: overview(), serverNow: null, watermark: "1", warnings: [] });
  await loop.loadRoute();
  assert.equal(main.querySelector(".lane-done").open, true);
});

it("Done choices survive leaving the overview and returning in the same tab", async () => {
  const { main, overview, loop, wiring, state, dom } = harness();
  main.append(overview());
  main.querySelector(".lane-done").open = true;
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "window");
  globalThis.window = { scrollTo: () => {} };
  try {
    state.route = { view: "attention" };
    wiring.fetchRoute = async () => ({ page: dom.el("div"), serverNow: null, watermark: "1", warnings: [] });
    await loop.loadRoute(true);
    state.route = { view: "overview" };
    wiring.fetchRoute = async () => ({ page: overview(), serverNow: null, watermark: "1", warnings: [] });
    await loop.loadRoute(true);
    assert.equal(main.querySelector(".lane-done").open, true);
  } finally {
    if (descriptor) Object.defineProperty(globalThis, "window", descriptor);
    else delete globalThis.window;
  }
});
