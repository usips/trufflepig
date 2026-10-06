import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardViews } from "../board_views.js";

installDomShim();
beforeEach(() => resetDomShim());

function blankState() {
  return {
    clockOffsetMs: 0, formDrafts: new Map(), formStatuses: new Map(), pendingForms: new Set(),
    seenAtOpen: null,
  };
}

function viewsContext(state, dom) {
  const noop = () => {};
  return {
    state, dom, board: noop, jsonFetch: noop, navigate: noop, scheduleRefresh: noop,
    submitMutation: noop, notice: noop, apiVersion: 1,
    entryRecord: record => (record?.entry && typeof record.entry === "object" ? record.entry : record),
    collection: (data, key) => {
      if (!Array.isArray(data?.[key])) throw new Error(`Board reply is missing its ${key} collection.`);
      return data[key];
    },
    permitted: () => false, nextAfter: () => null, pageParams: route => ({ ...route }),
    queryFilters: () => ({}), postKinds: ["note"], entryKinds: ["note"],
    columns: ["todo", "doing", "review", "done", "blocked"],
    errorMessage: error => error?.message || String(error),
  };
}

// Two cards linking the same plan: duplicate hrefs share one focus key.
function duplicatePage(dom) {
  const root = document.createElement("div");
  const first = dom.link("Plan P1", "plan", { ref: "P1" });
  const second = dom.link("Plan P1 again", "plan", { ref: "P1" });
  root.append(first, second);
  return { root, first, second };
}

function tickerEvent(seq, subject) {
  return {
    seq: String(seq), kind: "note", subject, summary: `event ${seq}`,
    actor: "saoirse", created_at: 1700000000 + seq,
  };
}

describe("board focus restore", () => {
  it("returns focus to the second of two same-href links", () => {
    const dom = createBoardDom(blankState());
    const before = duplicatePage(dom);
    document.activeElement = before.second;
    const focus = dom.focusedControl(before.root);
    const after = duplicatePage(dom);
    document.activeElement = null;
    dom.restoreFocus(after.root, focus);
    assert.equal(document.activeElement, after.second);
  });

  it("returns focus to the second duplicate panel", () => {
    const dom = createBoardDom(blankState());
    const build = () => {
      const root = document.createElement("div");
      const panels = [0, 1].map(() => dom.panel("Duplicates", dom.link("Plan P1", "plan", { ref: "P1" })));
      root.append(panels);
      return { root, panels };
    };
    const before = build();
    document.activeElement = before.panels[1].querySelector("a");
    const focus = dom.focusedControl(before.root);
    const after = build();
    document.activeElement = null;
    dom.restoreFocus(after.root, focus);
    assert.equal(document.activeElement, after.panels[1].querySelector("a"));
  });

  it("falls back to the nearest keyed ancestor when the link is gone", () => {
    const dom = createBoardDom(blankState());
    const root = document.createElement("div");
    const card = document.createElement("section");
    dom.focusKey(card, "task-card:P1.1");
    const gone = dom.link("Plan P1", "plan", { ref: "P1" });
    card.append(gone);
    root.append(card);
    document.activeElement = gone;
    const focus = dom.focusedControl(root);
    const rebuilt = document.createElement("div");
    const survivor = document.createElement("section");
    dom.focusKey(survivor, "task-card:P1.1");
    rebuilt.append(survivor);
    document.activeElement = null;
    dom.restoreFocus(rebuilt, focus);
    assert.equal(document.activeElement, survivor);
  });

  it("falls back to the enclosing form's first control when link and ancestor are gone", () => {
    const dom = createBoardDom(blankState());
    const root = document.createElement("div");
    const form = document.createElement("form");
    form.dataset.draftKey = "filters:entries:all";
    const gone = dom.link("Older entries", "entries", { after: "7" });
    form.append(gone);
    root.append(form);
    document.activeElement = gone;
    const focus = dom.focusedControl(root);
    const rebuilt = document.createElement("div");
    const survivor = document.createElement("form");
    survivor.dataset.draftKey = "filters:entries:all";
    const control = document.createElement("input");
    control.name = "plan";
    const second = document.createElement("input");
    second.name = "kind";
    survivor.append(control, second);
    rebuilt.append(survivor);
    document.activeElement = null;
    dom.restoreFocus(rebuilt, focus);
    assert.equal(document.activeElement, control);
  });

  it("keys ticker rows so a new row doesn't move focus", () => {
    const state = blankState();
    const dom = createBoardDom(state);
    const views = createBoardViews(viewsContext(state, dom));
    const before = views.eventList([tickerEvent(2, "P1"), tickerEvent(1, "P1")]);
    const rows = before.querySelectorAll("li");
    assert.equal(rows.length, 2);
    assert.equal(rows[0].dataset.focusKey, "event:2");
    assert.equal(rows[1].dataset.focusKey, "event:1");
    const target = rows[1].querySelector("a");
    document.activeElement = target;
    const focus = dom.focusedControl(before);
    const after = views.eventList([tickerEvent(3, "P1"), tickerEvent(2, "P1"), tickerEvent(1, "P1")]);
    document.activeElement = null;
    dom.restoreFocus(after, focus);
    const expected = after.querySelector('[data-focus-key="event:1"]').querySelector("a");
    assert.equal(document.activeElement, expected);
  });

  it("gives cards and sections focusable keys", () => {
    const state = blankState();
    const dom = createBoardDom(state);
    const views = createBoardViews(viewsContext(state, dom));
    const task = { id: "P1.1", title: "Do it", column: "todo" };
    const entry = { id: "E1", kind: "note", actor: "saoirse", created_at: 1700000000, body: "hello" };
    const claim = { actor: "saoirse", task: "P1.1", last_active: 1700000000, scope: "wave", stale: null };
    const taskCard = views.taskCard(task, []);
    assert.equal(taskCard.dataset.focusKey, "task-card:P1.1");
    assert.equal(views.entryCard(entry).dataset.focusKey, "entry-card:E1");
    assert.equal(views.workingCard(claim).dataset.focusKey, "working-card:P1.1");
    assert.equal(
      views.attentionCard({ type: "proposal", record: entry }).dataset.focusKey, "attention-card:E1");
    const details = views.taskDetails({ plan: { id: "P1" }, tasks: [task], claims: [] }, []);
    assert.equal(details.querySelector("article").dataset.focusKey, "task-detail:P1.1");
    const section = dom.panel("Tasks", dom.el("div"));
    assert.equal(section.dataset.focusKey, "section:Tasks");
    const overview = views.renderOverview(
      { plans: [{ plan: { id: "P1", title: "T", head_revision: 1 }, tasks: [], claims: [] }], events: [] },
      { entries: [] }, { after: "" });
    const lane = overview.querySelector('[data-focus-key="swimlane:P1"]');
    assert.ok(lane);
    for (const item of [taskCard, section, lane]) {
      document.activeElement = null;
      item.focus();
      assert.equal(document.activeElement, item);
    }
  });
});
