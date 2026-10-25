import { afterEach, beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardViews } from "../board_views.js";
import { createBoardReader } from "../board_reader.js";
import { createBoardRenderLoop } from "../board_render_loop.js";
import { routeFromLocation } from "../board_routing.js";
import { boardRefRoute } from "../board_refs.js";
import { revealRouteTarget, routeTargetId } from "../board_route_target.js";

installDomShim();
const saved = ["window", "location"].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]);
beforeEach(() => resetDomShim());
afterEach(() => {
  for (const [key, descriptor] of saved) {
    if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key];
  }
});

const actor = { user: "josh", host: "board.test", harness: "codex", session: "s1" };

function setup(route = {}) {
  const state = {
    route, clockOffsetMs: 0, seenAtOpen: null, navigation: 0, formDrafts: new Map(),
    formStatuses: new Map(), pendingForms: new Set(), refreshTimer: null, generation: 0,
    watermark: "1", stream: {}, projects: [],
  };
  const dom = createBoardDom(state), noop = () => {};
  const context = {
    state, dom, columns: ["todo", "doing", "review", "blocked", "done"],
    collection: (data, key) => data?.[key] || [], entryRecord: value => value?.entry || value,
    permitted: () => false, pageParams: page => page, nextAfter: () => null,
    queryFilters: () => ({}), postKinds: ["note"], entryKinds: ["note"],
    errorMessage: error => error.message, board: noop, jsonFetch: noop, navigate: noop,
    scheduleRefresh: noop, submitMutation: noop, notice: noop, apiVersion: 1,
  };
  return { state, dom, context, views: createBoardViews(context) };
}

describe("board reference routes", () => {
  it("routes every reference pattern and leaves other text inert", () => {
    const { dom } = setup();
    const href = (ref, context) => dom.refLink(ref, ref, "mono", context).href;
    const sha = "A".repeat(40);
    assert.equal(href("E485"), "/#/E485");
    assert.equal(href("P7.3"), "/#/P7?tab=tasks&task=P7.3");
    assert.equal(href("P7.3", { focus: "entry-E9" }), "/#/P7?tab=tasks&task=P7.3&focus=entry-E9");
    assert.equal(href("P7.3/parser-2"), "/#/P7?tab=tasks&task=P7.3&focus=worker-P7.3-parser-2");
    assert.equal(href("P7"), "/#/P7");
    assert.equal(href("P7@3"), "/#/P7@3");
    assert.equal(href("P7@1..3"), "/#/P7@1..3");
    assert.equal(href(sha, { plan: "P7", repo_key: "r1" }),
      `/#/P7?tab=commits&oid=${sha.toLowerCase()}&repo=r1`);
    assert.equal(href(sha), `/#/search?q=${sha}`);
    for (const text of ["P7.3/Bad", "P7.3/", "E0", "hello", `P7.3/${"a".repeat(33)}`]) {
      const node = dom.refLink(text);
      assert.equal(node.tagName, "SPAN", `${text} stays text`);
      assert.equal(boardRefRoute(text), null);
    }
  });

  it("parses focus from the hash and resolves targets in priority order", () => {
    globalThis.location = { hash: "#/P7?tab=tasks&task=P7.3&focus=worker-P7.3-parser", search: "" };
    const route = routeFromLocation();
    assert.equal(route.focus, "worker-P7.3-parser");
    assert.equal(routeTargetId(route), "worker-P7.3-parser");
    assert.equal(routeTargetId({ ...route, focus: "" }), "P7.3");
    assert.equal(routeTargetId({ oid: "ab", task: "P7.3" }), "commit-ab");
    assert.equal(routeTargetId({ heading: "intro", oid: "ab" }), "intro");
  });
});

describe("focus navigation", () => {
  it("reveals the target inside a closed disclosure and marks it once", () => {
    const { dom } = setup();
    const root = dom.el("main"); document.body.append(root);
    const ended = dom.disclosure("ended-workers:E1", "Ended workers (1)");
    let loads = 0; ended.boardLoad = () => { loads++; };
    const row = dom.el("li"); row.dataset.anchor = "worker-P7.3-old";
    const scrolled = []; row.scrollIntoView = options => scrolled.push(options);
    const stale = dom.el("div"); stale.dataset.targeted = "true";
    root.append(stale, dom.add(ended, row));
    assert.equal(revealRouteTarget(root, { focus: "worker-P7.3-old" }), row);
    assert.equal(ended.open, true);
    assert.equal(loads, 1);
    assert.equal(row.dataset.targeted, "true");
    assert.equal(stale.dataset.targeted, undefined, "only the current target is marked");
    assert.deepEqual(scrolled, [{ block: "start" }]);
    const task = dom.el("article"); task.id = "P7.4"; root.append(task);
    assert.equal(revealRouteTarget(root, { task: "P7.4" }), task, "ids remain fallback targets");
    assert.equal(revealRouteTarget(root, { focus: "entry-E404" }), null);
  });

  it("applies focus on navigation but not on later live refreshes", async () => {
    const { state, dom } = setup({ view: "plan", ref: "P7", focus: "entry-E5" });
    globalThis.window = { scrollY: 0, scrollTo: () => {} };
    const main = dom.el("main"); document.body.append(main);
    const count = dom.el("span"); count.id = "attention-count"; document.body.append(count);
    const scrolls = [];
    const page = () => {
      const card = dom.el("article"); card.dataset.anchor = "entry-E5";
      card.scrollIntoView = () => scrolls.push("entry-E5");
      return dom.add(dom.el("div"), card);
    };
    const noop = () => {};
    const loop = createBoardRenderLoop({
      state, dom, main, apiVersion: 1, seenStorage: sessionStorage, seenKey: "seen",
      getToken: () => "fixture", isExpired: () => false, enterExpired: noop, notice: noop,
      setConnection: noop, noteSeen: noop, wiring: {
        fetchRoute: async () => ({ page: page(), serverNow: null, watermark: "1", warnings: [] }),
        attentionItems: () => [], startStream: noop,
      },
    });
    const navLink = route => { const item = dom.el("a"); item.dataset.route = route; return item; };
    const brand = navLink("plan"), nav = dom.add(dom.el("nav"), navLink("plan"), navLink("done"));
    nav.children[1].setAttribute("aria-current", "page");
    document.body.append(brand, nav);
    await loop.loadRoute(true);
    assert.deepEqual(scrolls, ["entry-E5"]);
    assert.deepEqual([brand, ...nav.children].map(item => item.attributes.get("aria-current") || ""),
      ["", "page", ""], "only the active navigation item is the current page");
    assert.equal(main.querySelector('[data-anchor="entry-E5"]').dataset.targeted, "true");
    await loop.loadRoute(false);
    assert.deepEqual(scrolls, ["entry-E5"], "a refresh keeps the reader's place");
    assert.equal(main.querySelector('[data-anchor="entry-E5"]').dataset.targeted, undefined);
  });

  it("anchors entries, workers, claim leads, and commits", () => {
    const { views } = setup();
    assert.equal(views.entryCard({ id: "E5", kind: "note", actor, body: "x", created_at: 1 })
      .dataset.anchor, "entry-E5");
    const claim = { task: "P7.3", entry: "E9", actor, scope: "lead", last_active: 1, workers: [
      { address: { task: "P7.3", handle: "parser" }, actor, role: "code", scope: "s", last_active: 1 },
    ] };
    const card = views.workingCard(claim);
    assert.equal(card.querySelector(".lead-row").dataset.anchor, "entry-E9");
    assert.equal(card.querySelector(".worker-row").dataset.anchor, "worker-P7.3-parser");
  });
});

describe("specific links", () => {
  it("ticker claim and worker events open the task and worker", () => {
    const { views } = setup();
    const event = (seq, kind, subject, summary) => ({
      seq: String(seq), kind, subject, summary, actor, created_at: 1, plan: "P7",
    });
    const list = views.eventList([
      event(3, "claim", "E9", "took P7.3: parser work"),
      event(2, "worker_add", "P7.3", "added worker P7.3/parser (code)"),
      event(1, "commit", "b".repeat(40), "commit"),
    ]);
    const [claimRow, workerRow, commitRow] = list.querySelectorAll(".event-row");
    const claimLinks = claimRow.querySelectorAll("a");
    assert.equal(claimLinks[0].href, "/#/P7?tab=tasks&task=P7.3&focus=entry-E9");
    assert.equal(claimLinks[1].href, "/#/E9", "the claim entry stays reachable");
    assert.equal(workerRow.querySelector("a").textContent, "P7.3/parser");
    assert.equal(workerRow.querySelector("a").href, "/#/P7?tab=tasks&task=P7.3&focus=worker-P7.3-parser");
    assert.match(commitRow.querySelector("a").href, /^\/#\/P7\?tab=commits&oid=b+$/);
  });

  it("pins the selected task detail above the task list", () => {
    const { views } = setup();
    const task = id => ({ id, title: id, column: "todo" });
    const details = views.taskDetails({
      plan: { id: "P7", lifecycle_state: "closed" }, tasks: [task("P7.1"), task("P7.2"), task("P7.3")],
      claims: [], task_view: { task: task("P7.3"), entries: [], commits: [] },
    }, []);
    const articles = details.querySelectorAll(".task-detail");
    assert.deepEqual(articles.map(item => item.id), ["P7.3", "P7.1", "P7.2"]);
    assert.ok(articles[0].className.includes("task-detail-pinned"));
    assert.ok(articles[0].querySelector(".task-view-detail"));
  });

  it("an entry page links back into its task, focused on the entry", () => {
    const { views } = setup({ view: "entry", ref: "E5" });
    const entry = { id: "E5", kind: "note", actor, body: "x", created_at: 1, plan: "P7",
      refs: ["P7", "P7.3"] };
    const page = views.entryPage({ entry, replies: [], backrefs: [] }, null, { view: "entry", ref: "E5" });
    assert.equal(page.querySelector(".entry-task-link").href, "/#/P7?tab=tasks&task=P7.3&focus=entry-E5");
    const plain = views.entryPage({ entry: { ...entry, refs: [] }, replies: [], backrefs: [] }, null, {});
    assert.equal(plain.querySelector(".entry-task-link"), null);
  });

  it("pager links built from a focused route drop the focus target", () => {
    const { views } = setup({ view: "entry", ref: "E5", focus: "entry-E9" });
    const entry = { id: "E5", kind: "note", actor, body: "x", created_at: 1, plan: "P7", refs: [] };
    const page = views.entryPage({ entry, replies: [], backrefs: [], replies_next_after: { seq: "4" },
      backrefs_next_after: { seq: "3" }, through: "9" }, null,
    { view: "entry", ref: "E5", focus: "entry-E9" });
    const pagers = page.querySelectorAll("a.button").filter(item => /Next/.test(item.textContent));
    assert.equal(pagers.length, 2);
    for (const pager of pagers) assert.doesNotMatch(pager.href, /focus=/, pager.textContent);
  });

  it("a stored revision links its neighbours, diff, plan, and history", async () => {
    const { context, views } = setup();
    const reader = createBoardReader({
      ...context, views,
      board: async op => ({ seq: "1", warnings: [], data: op.op === "show"
        ? op.target.includes("@") ? { id: "P7@3", actor, source: "direct", created_at: 1 }
          : { plan: { id: "P7", head_revision: 5 }, repo_keys: [] } : [] }),
      jsonFetch: async () => ({ api: 1, html: "", headings: [], snapshot_seq: "1" }),
      readOp: (op, options) => ({ op, ...options }), snapshotSeq: reply => reply.seq || null,
      minimumSeq: () => "1", cursorFromRoute: () => null, parseBoardJson: JSON.parse,
    });
    const { page } = await reader.fetchRoute({ view: "plan", ref: "P7@3" }, null, false);
    const nav = page.querySelector(".revision-nav");
    assert.deepEqual(nav.querySelectorAll("a").map(item => item.href),
      ["/#/P7@2", "/#/P7@4", "/#/P7@2..3", "/#/P7", "/#/P7?tab=history"]);
  });

  it("marks the selected plan tab as the current page", () => {
    const { views } = setup();
    const tabs = views.planTabs("P7", "tasks").querySelectorAll("a");
    assert.deepEqual(tabs.map(tab => tab.attributes.get("aria-current") || ""),
      ["", "page", "", "", "", "", ""]);
  });
});
