import { afterEach, beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createDraftStores } from "../state/board_lru.js";
import { createBoardViews } from "../board_views.js";
import { createBoardReader } from "../board_reader.js";
import { createBoardRouting } from "../board_routing.js";
import { createBoardRenderLoop } from "../board_render_loop.js";

installDomShim();
const globals = Object.fromEntries(["fetch", "history", "location", "window"].map(key => [key, globalThis[key]]));
beforeEach(() => resetDomShim());
afterEach(() => { for (const [key, value] of Object.entries(globals)) globalThis[key] = value; });

function dispatch(element, type) {
  const event = { type, target: element, preventDefault() {} };
  for (let target = element; target; target = target.parentElement) {
    for (const listener of target.listeners.get(type) || []) listener(event);
  }
}

function editorTab() {
  const state = {
    ...createDraftStores(), clockOffsetMs: 0, pendingForms: new Set(),
    navigation: 0, generation: 0, request: null, refreshTimer: null,
    refreshing: false, refreshAgain: false, resyncing: false, watermark: "41",
    stream: {}, reconnect: null, attention: { entries: [] }, route: {},
  };
  const dom = createBoardDom(state);
  const main = dom.el("main"); main.setAttribute("tabindex", "-1");
  const count = dom.el("span"); count.id = "attention-count";
  document.body.append(main, count);
  globalThis.location = { hash: "#/", href: "http://board.test/#/" };
  const changeUrl = (_, __, url) => { location.hash = new URL(url, location.href).hash; };
  globalThis.history = { pushState: changeUrl, replaceState: changeUrl };
  globalThis.window = { scrollTo() {} };
  const notices = [], sent = [];
  let failure = null, mutation = null, navigation = null;
  globalThis.fetch = async (_, options) => {
    const { op } = JSON.parse(options.body); sent.push(op);
    if (failure && ["edit", "new"].includes(op.op)) {
      return { ok: false, status: 409, text: async () => JSON.stringify({ error: failure }) };
    }
    const data = op.op === "show"
      ? { plan: { id: "P1" }, revision: { id: op.target, body: `Stored ${op.target}` }, can_edit: true }
      : op.op === "repositories" ? [] : { plan: "P1", revision: "P1@99" };
    return { ok: true, status: 200, text: async () => JSON.stringify({
      api: 1, result: { result: op.op, data }, snapshot_seq: "41",
    }) };
  };
  const wiring = {}, noop = () => {}, notice = (...args) => notices.push(args);
  const loop = createBoardRenderLoop({
    state, dom, main, apiVersion: 1, seenStorage: null, seenKey: "test",
    getToken: () => "test-token", isExpired: () => false, enterExpired: noop,
    notice, setConnection: noop, noteSeen: noop, wiring,
  });
  const { navigate } = createBoardRouting({ state, routeUrl: dom.routeUrl, notice,
    load: focus => (navigation = loop.loadRoute(focus)) });
  const views = createBoardViews({
    state, dom, ...loop, navigate, notice, apiVersion: 1,
    submitMutation: (...args) => (mutation = loop.submitMutation(...args)),
    columns: ["todo", "doing", "review", "done", "blocked"], postKinds: ["note"], entryKinds: ["note"],
  });
  const reader = createBoardReader({ state, dom, views, ...loop, apiVersion: 1 });
  Object.assign(wiring, { attentionItems: views.attentionItems, startStream: noop,
    fetchRoute: (route, ...args) => ["new", "edit"].includes(route.view)
      ? reader.fetchRoute(route, ...args)
      : { page: dom.el("div"), serverNow: null, watermark: "41", warnings: [] },
  });
  return {
    state, dom, main, sent, notices, navigate, loop,
    fail: error => { failure = error; },
    async open(mode = "edit", base = "P1@3") {
      state.navigation++; state.route = { view: mode, ref: base };
      await loop.loadRoute(true);
      const form = main.querySelector("form");
      assert.ok(form, `production ${mode} editor was mounted`);
      return form;
    },
    type(form, name, value) {
      const input = form.querySelectorAll("input,textarea,select").find(item => item.name === name);
      assert.ok(input, `editor has ${name}`); input.value = value; dispatch(input, "input");
    },
    async submit(form) {
      dispatch(form, "submit");
      assert.ok(mutation, "production submit handler called submitMutation");
      const saved = await mutation;
      if (navigation) await navigation;
      return saved;
    },
  };
}

function editorEntries(state, key) {
  return { formDraft: state.formDrafts.has(`editor:${key}`),
    status: state.formStatuses.has(`editor:${key}`), draft: state.drafts.has(key) };
}

describe("production editor draft lifecycle", () => {
  it("viewing_an_editor_pins_nothing", async () => {
    const tab = editorTab();
    for (const mode of ["new", "edit"]) {
      for (let index = 1; index <= 12; index++) {
        await tab.open(mode, `P1@${index}`);
        assert.deepEqual([tab.state.formDrafts.size, tab.state.formStatuses.size, tab.state.drafts.size],
          [0, 0, 0], `viewing ${mode} ${index} leaves no draft or status entries`);
      }
    }
  });

  it("saved_editor_leaves_no_pins", async () => {
    const tab = editorTab();
    for (const mode of ["new", "edit"]) {
      const form = await tab.open(mode);
      tab.type(form, "body", "Saved plan text");
      tab.type(form, mode === "new" ? "title" : "summary", "Save this");
      const key = mode === "new" ? "new" : "edit:P1@3";
      assert.deepEqual(editorEntries(tab.state, key), { formDraft: true, status: false, draft: true });
      assert.equal(await tab.submit(form), true);
      assert.equal(tab.state.route.view, "plan", "successful production submit navigates to the plan");
      assert.deepEqual(editorEntries(tab.state, key), { formDraft: false, status: false, draft: false });
      assert.deepEqual([tab.state.formDrafts.size, tab.state.formStatuses.size, tab.state.drafts.size], [0, 0, 0]);
      assert.equal(tab.state.pendingForms.size, 0);
    }
    assert.equal(tab.notices.filter(([text]) => text === "Plan saved.").length, 2);
  });

  it("keeps typed drafts through viewed editors, saves and ordinary form churn", async () => {
    const tab = editorTab();
    const original = await tab.open();
    tab.type(original, "body", "Unsaved original text"); tab.type(original, "summary", "Original summary");
    for (let index = 4; index <= 15; index++) {
      await tab.open("edit", `P1@${index}`);
      const saved = await tab.open("edit", `P2@${index}`);
      tab.type(saved, "body", `Saved elsewhere ${index}`); tab.type(saved, "summary", "Other change");
      assert.equal(await tab.submit(saved), true);
    }
    for (let tabIndex = 0; tabIndex < 2; tabIndex++) {
      const root = tab.dom.el("div");
      for (let index = 0; index < 40; index++) {
        const form = tab.dom.el("form");
        form.append(tab.dom.field("Plan", "plan", `P${index}`).wrapper);
        root.append(tab.dom.retainForm(form, `tasks:${tabIndex}:filter:${index}`));
      }
      tab.dom.captureForms(root);
    }
    const restored = await tab.open();
    const values = Object.fromEntries(restored.querySelectorAll("textarea,input").map(input => [input.name, input.value]));
    assert.deepEqual(values, { body: "Unsaved original text", summary: "Original summary" });
    assert.equal(tab.state.drafts.get("edit:P1@3").base, "P1@3");
    assert.equal(tab.state.formDrafts.has("tasks:0:filter:0"), false);
    assert.equal(tab.state.formDrafts.has("tasks:1:filter:39"), true);
  });

  it("retains an ordinary form's reset values and success status", async () => {
    const tab = editorTab();
    const form = tab.dom.el("form");
    const body = tab.dom.field("Post", "body", "Posted text"); form.append(body.wrapper);
    tab.main.append(tab.dom.retainForm(form, "post:P1"));
    assert.equal(await tab.loop.submitMutation(form, { op: "post" }, () => { body.input.value = ""; }), true);
    assert.deepEqual(tab.state.formDrafts.get("post:P1"), { body: "" });
    assert.deepEqual(tab.state.formStatuses.get("post:P1"), { message: "Saved.", tone: "muted" });
  });

  it("preserves the edited text and original base when a save fails", async () => {
    const tab = editorTab();
    const form = await tab.open();
    tab.type(form, "body", "Keep my changes"); tab.type(form, "summary", "My change");
    tab.fail({ code: "stale_revision", message: "Head changed" });
    assert.equal(await tab.submit(form), false);
    assert.deepEqual(editorEntries(tab.state, "edit:P1@3"), { formDraft: true, status: true, draft: true });
    assert.equal(tab.state.pendingForms.size, 0);
    await tab.open("edit", "P1@4");
    const restored = await tab.open();
    assert.equal(restored.querySelector("textarea").value, "Keep my changes");
    assert.equal(tab.state.drafts.get("edit:P1@3").base, "P1@3");
    assert.match(restored.querySelector(".form-status").textContent, /original base are preserved/);
    assert.equal(tab.sent.find(op => op.op === "edit").base, "P1@3");
  });
});
