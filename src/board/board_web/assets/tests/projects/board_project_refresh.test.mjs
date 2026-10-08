import { it, after, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardDom } from "../../board_dom.js";
import { createBoardRouting } from "../../board_routing.js";
import { createBoardRenderLoop } from "../../board_render_loop.js";
import {
  setupProjectPage, responseFor, waitFor, closeProjectPages, restoreProjectGlobals,
} from "./board_project_bootstrap_support.mjs";

afterEach(closeProjectPages);
after(restoreProjectGlobals);

function refreshHarness(handler) {
  const { calls } = setupProjectPage({ hash: "#/?project=w1", handler });
  const state = {
    route: { view: "overview", project: "w1" }, generation: 1, navigation: 0, globalRefresh: null,
    overview: { plans: [{ plan: { id: "P1" } }], events: [{ seq: "99", plan: "P1" }] },
    attention: { entries: [{ plan: "P1" }] }, projects: [],
  };
  const dom = createBoardDom(state);
  const connections = [];
  const loop = createBoardRenderLoop({
    state, dom, main: document.getElementById("main"), apiVersion: 8,
    getToken: () => "a".repeat(64), isExpired: () => false,
    enterExpired: () => {}, notice: () => {}, noteSeen: () => {}, wiring: {},
    setConnection: (label, mode) => connections.push({ label, mode }),
  });
  const routing = createBoardRouting({
    state, routeUrl: dom.routeUrl, notice: () => {}, load: () => {},
  });
  return { calls, state, loop, routing, dom, connections };
}

it("a_delayed_background_overview_is_discarded_after_a_project_change", async () => {
  let release;
  const held = new Promise(resolve => { release = resolve; });
  const { calls, state, loop, routing } = refreshHarness(request =>
    request.project === "w1" ? held : responseFor(request));
  const old = loop.refreshOverview();
  assert.equal(await waitFor(() => calls.length === 1), true);
  routing.navigate("overview", { project: "w2" });
  assert.equal(state.overview, null, "old project data disappears immediately");
  assert.equal(state.attention, null);
  await loop.refreshOverview();
  release(responseFor(calls[0]));
  await old;
  assert.deepEqual(state.overview.plans.map(item => item.plan.id), ["P2"]);
  assert.deepEqual(state.overview.events, [], "foreign cached ticker events are cleared");
  assert.equal(calls[1].project, "w2");
  assert.equal(calls[1].op.scope, "all");
});

it("switching_project_discards_frozen_cursors_and_keeps_navigation_links_scoped", () => {
  const { state, routing, dom } = refreshHarness(responseFor);
  routing.navigate("claims", { project: "w2", after: '{"seq":90}', through: "100", plan: "P2" });
  assert.equal(state.route.after, "");
  assert.equal(state.route.through, "");
  assert.doesNotMatch(location.hash, /after=|through=/);
  assert.match(dom.routeUrl("attention"), /project=w2/);
  assert.match(dom.routeUrl("plan", { ref: "P2", tab: "tasks" }), /project=w2/);
  assert.doesNotMatch(dom.routeUrl("overview", { project: "" }), /project=/);
});

it("raw_global_events_do_not_pollute_a_scoped_ticker_after_a_navigation", () => {
  const { state, loop, routing } = refreshHarness(responseFor);
  loop.addTickerEvent({ seq: "100", plan: "P2", summary: "Foreign event" });
  assert.deepEqual(state.overview.events.map(item => item.plan), ["P1"]);
  routing.navigate("overview", { project: "unscoped" });
  loop.addTickerEvent({ seq: "101", plan: "P2" });
  assert.equal(state.overview, null);
});

it("an_old_background_error_cannot_change_the_new_project_connection_status", async () => {
  let release;
  const held = new Promise(resolve => { release = resolve; });
  const { calls, loop, routing, connections } = refreshHarness(request =>
    request.project === "w1" ? held : responseFor(request));
  const old = loop.refreshOverview();
  assert.equal(await waitFor(() => calls.length === 1), true);
  routing.navigate("overview", { project: "w2" });
  await loop.refreshOverview();
  release({ ok: false, status: 500, text: async () => JSON.stringify({
    error: { code: "board_unavailable", message: "old project failed" },
  }) });
  await old;
  assert.deepEqual(connections, []);
});
