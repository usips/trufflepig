import { it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardViews } from "../board_views.js";
import { createDraftStores } from "../state/board_lru.js";
import { createBoardRouting, routeFromLocation } from "../board_routing.js";

installDomShim();
beforeEach(() => resetDomShim());

function harness(clockOffsetMs = 0) {
  const stores = createDraftStores();
  const state = {
    route: { view: "plan", ref: "P7", tab: "done" }, clockOffsetMs, seenAtOpen: null,
    formDrafts: stores.formDrafts, formStatuses: stores.formStatuses, pendingForms: new Set(),
    ingestPending: false, ingestTimer: null,
  };
  const dom = createBoardDom(state);
  return createBoardViews({
    state, dom, columns: ["todo", "doing", "review", "blocked", "done"],
    collection: (data, key) => data?.[key] || [], entryRecord: value => value,
    permitted: () => false, pageParams: (route, data) => ({ ...route, after: data.next_after }),
    nextAfter: data => data?.next_after, queryFilters: () => ({}), postKinds: [], entryKinds: [],
    errorMessage: error => error.message,
  });
}

function task(ordinal, doneAt) {
  return { id: `P7.${ordinal}`, title: `Completed ${ordinal}`, column: "done", done_at: doneAt };
}

function planView() {
  return {
    plan: { id: "P7", title: "Done history", head_revision: 1, owner_user: "josh" },
    revision: { id: "P7@1" }, tasks: [], claims: [], sections_without_tasks: [], commits: [],
  };
}

it("done_tab_groups_by_day_with_fake_clock", () => {
  const correctedNow = new Date(2026, 9, 7, 12).getTime();
  const clockOffsetMs = 9 * 86400000;
  const realNow = Date.now;
  Date.now = () => correctedNow - clockOffsetMs;
  try {
    const tasks = [
      task(3, new Date(2026, 9, 7, 10).getTime() / 1000),
      task(2, new Date(2026, 9, 6, 10).getTime() / 1000),
      task(1, new Date(2026, 9, 4, 10).getTime() / 1000),
    ];
    const page = harness(clockOffsetMs).renderPlan(planView(), { html: "", headings: [] },
      { tasks, next_before: null }, { view: "plan", ref: "P7", tab: "done" });
    assert.deepEqual(page.querySelectorAll(".done-group").map(group => group.querySelector("h2").textContent),
      ["Today", "This week", "Earlier"]);
    assert.deepEqual(page.querySelectorAll(".done-group").map(group =>
      group.querySelector(".task-card").querySelector(".small").textContent), ["P7.3", "P7.2", "P7.1"]);
    assert.ok(page.querySelectorAll("a").some(link => link.textContent === "Done" && link.className === "active"));
  } finally { Date.now = realNow; }
});

it("done_column_shows_five_and_more_link", () => {
  const recentDone = Array.from({ length: 5 }, (_, index) => task(25 - index, 1700000000 - index));
  const page = harness().renderOverview({ plans: [{ ...planView(), done_count: 25, recent_done: recentDone }] },
    { entries: [] }, { view: "overview" });
  assert.equal(page.querySelectorAll(".task-card").length, 5);
  const done = page.querySelector(".lane-done");
  assert.equal(done.tagName, "DETAILS");
  assert.equal(done.attributes.has("open"), false);
  assert.equal(done.querySelector("summary").textContent, "Done (25)");
  const more = done.querySelectorAll("a").find(link => link.textContent === "20 more →");
  assert.equal(more?.href, "/#/P7?tab=done");
});

it("done_load_older_link_keeps_the_returned_cursor", () => {
  const cursor = { seq: "9007199254741000", id: "P7.3" };
  const route = { view: "plan", ref: "P7", tab: "done", project: "W1" };
  const page = harness().renderPlan(planView(), { html: "", headings: [] },
    { tasks: [task(3, null)], next_before: cursor }, route);
  const older = page.querySelectorAll("a").find(link => link.textContent === "Load older");
  const query = new URLSearchParams(older.href.split("?")[1]);
  assert.equal(query.get("project"), "W1");
  assert.deepEqual(JSON.parse(query.get("before")), cursor);
  assert.equal(page.querySelector(".done-group").querySelector("h2").textContent, "Earlier");
});

it("done_column_preserves_active_cards_and_zero_more", () => {
  const view = planView();
  view.tasks = [ { id: "P7.6", title: "Active", column: "todo" } ];
  const page = harness().renderOverview({ plans: [{ ...view, done_count: 1, recent_done: [task(1, 1)] }] },
    { entries: [] }, { view: "overview" });
  assert.equal(page.querySelectorAll(".task-card").length, 2);
  const done = page.querySelector(".lane-done");
  assert.equal(done.querySelectorAll(".task-card").length, 1);
  assert.ok(!done.querySelectorAll("a").some(link => link.href.includes("tab=done")));
});

it("done_card_opens_the_actual_task_beyond_the_first_ordinal_page", () => {
  const page = harness().renderDone({ tasks: [task(99, 1700000000)], next_before: null },
    { view: "done" });
  const target = page.querySelector(".task-card").querySelector("a");
  const [path, query] = target.href.split("?");
  assert.equal(path, "/#/P7");
  assert.equal(new URLSearchParams(query).get("tab"), "tasks");
  assert.equal(new URLSearchParams(query).get("task"), "P7.99");
});

it("done_project_switch_clears_the_previous_completion_cursor", () => {
  const before = JSON.stringify({ seq: "9007199254741000", id: "P7.99" });
  const originals = ["location", "history"].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]);
  try {
    globalThis.location = { hash: `#/done?project=W1&before=${encodeURIComponent(before)}`, search: "" };
    globalThis.history = { state: null, replaceState: () => {} };
    assert.equal(routeFromLocation().before, before);
    const request = new AbortController(), refresh = new AbortController();
    const state = { route: routeFromLocation(), projects: [], generation: 0,
      request, globalRefresh: refresh, overview: {}, attention: {} };
    const routing = createBoardRouting({ state, routeUrl: createBoardDom(state).routeUrl,
      notice: () => {}, load: () => {} });
    routing.adoptRoute({ ...state.route });
    assert.equal(state.route.before, before, "same project retains its page");
    routing.adoptRoute({ ...state.route, project: "W2" });
    assert.equal(state.route.before, "");
    assert.ok(request.signal.aborted);
    assert.ok(refresh.signal.aborted);
    assert.equal(state.overview, null);
    assert.equal(state.attention, null);
  } finally {
    for (const [key, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  }
});
