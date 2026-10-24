import { it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardViews } from "../board_views.js";
import { createBoardRenderLoop } from "../board_render_loop.js";

installDomShim();
const windowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");
beforeEach(() => resetDomShim());
afterEach(() => {
  if (windowDescriptor) Object.defineProperty(globalThis, "window", windowDescriptor);
  else delete globalThis.window;
});

const NOW = 10_000;
const actor = handle => ({ user: handle, host: "board.test", harness: "codex", session: handle });

function endedWorker(handle) {
  return {
    address: { task: "P1.2", handle }, entry: `E-${handle}`, actor: actor(handle), role: "code",
    scope: handle, files: [], state: "done", last_active: NOW - 4_000, ended_at: NOW - 4_000,
  };
}

function taskView() {
  const claim = (entry, extra = {}) => ({
    task: "P1.2", entry, actor: actor("lead"), vendor: "codex", scope: "lead scope",
    claimed_at: NOW - 5_000, last_active: NOW - 10, stale: false, workers: [endedWorker(`w-${entry}`)],
    ...extra,
  });
  return {
    task: { id: "P1.2", title: "Task", column: "doing" },
    current_claim: claim("E10"),
    claim_history: [claim("E5", { ended_at: NOW - 3_000, end_reason: "stopped" })],
    entries: [], commits: [], server_now: NOW, claim_ttl_secs: 7_200,
  };
}

function harness() {
  const state = {
    route: { view: "plan", ref: "P1", tab: "tasks", task: "P1.2" }, clockOffsetMs: NOW * 1000 - Date.now(),
    seenAtOpen: null, navigation: 0, formDrafts: new Map(), formStatuses: new Map(),
    pendingForms: new Set(), refreshTimer: null, generation: 0, watermark: "1", stream: {},
  };
  const dom = createBoardDom(state), noop = () => {};
  const views = createBoardViews({
    state, dom, columns: ["todo", "doing", "review", "blocked", "done"],
    collection: (data, key) => data?.[key] || [], entryRecord: value => value?.entry || value,
    permitted: () => false, pageParams: route => route, nextAfter: () => null,
    queryFilters: () => ({}), postKinds: [], entryKinds: [], errorMessage: error => error.message,
    board: noop, jsonFetch: noop, navigate: noop, scheduleRefresh: noop, notice: noop,
  });
  const page = () => views.taskDetails({
    plan: { id: "P1", lifecycle_state: "closed" }, tasks: [], claims: [], task_view: taskView(),
  }, []);
  const main = dom.el("main"); document.body.append(main);
  const count = dom.el("span"); count.id = "attention-count"; document.body.append(count);
  const wiring = {
    fetchRoute: async () => ({ page: page(), serverNow: null, watermark: "1", warnings: [] }),
    attentionItems: () => [], startStream: noop,
  };
  const loop = createBoardRenderLoop({
    state, dom, main, apiVersion: 1, seenStorage: sessionStorage, seenKey: "seen-test",
    getToken: () => "fixture", isExpired: () => false, enterExpired: noop,
    notice: noop, setConnection: noop, noteSeen: noop, wiring,
  });
  return { main, page, loop, dom, wiring };
}

const keyed = (main, key) => main.querySelectorAll(`details[data-disclosure-key="${key}"]`);

it("task disclosures keep their open choice across live refreshes", async () => {
  const { main, page, loop } = harness();
  main.append(page());
  const history = keyed(main, "claim-history:P1.2")[0];
  const ended = keyed(main, "ended-workers:E10")[0];
  const snapshot = keyed(main, "agent-snapshot:E5")[0];
  assert.ok(history && ended && snapshot, "fixture renders every task disclosure");
  assert.equal(history.querySelector("summary").dataset.focusKey, "disclosure:claim-history:P1.2");
  history.open = true; ended.open = true; snapshot.open = true;
  await loop.loadRoute();
  assert.equal(keyed(main, "claim-history:P1.2")[0].open, true);
  assert.equal(keyed(main, "ended-workers:E10")[0].open, true);
  const restored = keyed(main, "agent-snapshot:E5")[0];
  assert.equal(restored.open, true);
  assert.equal(restored.dataset.loaded, "true", "a restored lazy snapshot renders its agent tree");
  assert.equal(restored.querySelectorAll(".agent-tree").length, 1);
  restored.boardLoad();
  assert.equal(restored.querySelectorAll(".agent-tree").length, 1, "the lazy tree renders once");
  keyed(main, "ended-workers:E10")[0].open = false;
  await loop.loadRoute();
  assert.equal(Boolean(keyed(main, "ended-workers:E10")[0].open), false);
  assert.equal(keyed(main, "claim-history:P1.2")[0].open, true);
});

it("repeated disclosure keys keep independent choices by occurrence", async () => {
  const { main, loop, dom, wiring } = harness();
  const build = () => dom.add(dom.el("div"), dom.disclosure("ended-workers:E1", "First"),
    dom.disclosure("ended-workers:E1", "Second"));
  wiring.fetchRoute = async () => ({ page: build(), serverNow: null, watermark: "1", warnings: [] });
  main.append(build());
  keyed(main, "ended-workers:E1")[1].open = true;
  await loop.loadRoute();
  const [first, second] = keyed(main, "ended-workers:E1");
  assert.equal(Boolean(first.open), false);
  assert.equal(second.open, true);
});

it("non-focus refreshes keep the page scroll position", async () => {
  const { main, page, loop } = harness();
  const scrolls = [];
  globalThis.window = { scrollY: 640, scrollTo: ({ top }) => scrolls.push(top) };
  main.append(page());
  const original = main.replaceChildren.bind(main);
  main.replaceChildren = (...nodes) => { globalThis.window.scrollY = 0; original(...nodes); };
  await loop.loadRoute();
  assert.deepEqual(scrolls, [640], "the refresh restores the reader's scroll offset");
  globalThis.window.scrollY = 300;
  await loop.loadRoute(true);
  assert.ok(!scrolls.includes(300), "a focus navigation starts at its own target, not the old offset");
});
