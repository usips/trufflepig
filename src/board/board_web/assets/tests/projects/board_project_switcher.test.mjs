import { it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardReader } from "../../board_reader.js";
import { routeFromLocation } from "../../board_routing.js";
import { installDomShim, resetDomShim } from "../support/dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const PROJECTS = [
  { id: "w1", name: "One", repo_keys: ["A", "B"], unavailable: [], plan_count: 1 },
  { id: "w2", name: "Two", repo_keys: ["C"], unavailable: [], plan_count: 1 },
];
const PLANS = [
  { plan: { id: "P1" }, repo_keys: ["A"] },
  { plan: { id: "P2" }, repo_keys: ["C"] },
  { plan: { id: "P3" }, repo_keys: [] },
];

function readerHarness() {
  const calls = [];
  const board = async (op, signal, project) => {
    calls.push({ op, project });
    const selected = PROJECTS.find(item => item.id === project);
    const plans = op.scope === "unscoped" ? PLANS.filter(item => !item.repo_keys.length)
      : selected ? PLANS.filter(item => item.repo_keys.some(key =>
        selected.repo_keys.includes(key)))
        : PLANS;
    const data = op.op === "projects" ? PROJECTS
      : op.op === "overview" ? { plans, server_now: 1700000000 }
        : op.op === "feed" ? { events: plans.map(item => ({ plan: item.plan.id })) }
          : op.op === "attention" ? { entries: plans.map(item => ({ plan: item.plan.id })) }
            : op.op === "claims" ? { claims: plans.map(item => ({ plan: item.plan.id })) }
              : (() => { throw new Error(`unexpected op ${op.op}`); })();
    return { seq: "40", data, warnings: [] };
  };
  const reader = createBoardReader({
    state: { watermark: "40" }, dom: {},
    views: {
      renderOverview: (overview, attention) => ({ overview, attention }),
      renderAttention: data => data, renderClaims: data => data,
    },
    board, readOp: (op, fields = {}) => ({ op, ...fields }),
    snapshotSeq: reply => reply.seq, minimumSeq: () => "40", cursorFromRoute: () => null,
    parseBoardJson: JSON.parse,
  });
  return { calls, reader };
}

it("switcher_filters_overview_to_project", async () => {
  const { calls, reader } = readerHarness();
  const result = await reader.fetchRoute({ view: "overview", project: "w1" });
  assert.deepEqual(result.page.overview.plans.map(item => item.plan.id), ["P1"]);
  assert.deepEqual(result.page.attention.entries.map(item => item.plan), ["P1"]);
  assert.deepEqual(result.page.overview.events.map(item => item.plan), ["P1"]);
  for (const { op, project } of calls.filter(call => call.op.op !== "projects")) {
    assert.equal(project, "w1", `${op.op} uses the selected project`);
    assert.equal(op.scope, "all");
  }
});

it("unscoped_choice_requests_unscoped_scope", async () => {
  const { calls, reader } = readerHarness();
  const result = await reader.fetchRoute({ view: "overview", project: "unscoped" });
  const overview = calls.find(call => call.op.op === "overview");
  assert.equal(overview.op.scope, "unscoped");
  assert.equal(overview.project, undefined, "Unscoped is a scope without a project selector");
  assert.deepEqual(result.page.overview.plans.map(item => item.plan.id), ["P3"]);
  for (const { op, project } of calls.filter(call => call.op.op !== "projects")) {
    assert.equal(op.scope, "unscoped", `${op.op} follows Unscoped`);
    assert.equal(project, undefined);
  }
});

it("project_query_survives_reload", () => {
  globalThis.location = { hash: "#/?project=w1", search: "" };
  assert.equal(routeFromLocation().project, "w1");
  assert.equal(routeFromLocation().project, "w1", "a fresh route keeps the query selection");
});
