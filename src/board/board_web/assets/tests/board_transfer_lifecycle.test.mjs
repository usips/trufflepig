import { beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardTaskView } from "../board_task_view.js";
import { createPlanPage } from "../pages/plan_page.js";

installDomShim();
beforeEach(() => resetDomShim());

function visibleText(node) {
  return `${node.textContent || ""}${node.children.map(visibleText).join("")}`;
}

function submit(form) {
  for (const listener of form.listeners.get("submit") || []) {
    listener({ preventDefault() {} });
  }
}

function taskDetail(task) {
  const state = { clockOffsetMs: 0, formDrafts: new Map(), formStatuses: new Map() };
  const dom = createBoardDom(state);
  const { taskViewDetails } = createBoardTaskView({
    dom,
    entryCard: entry => dom.el("li", "", `Entry ${entry.id}`),
    workerTree: () => dom.el("div"),
    claimSignals: value => dom.el("p", "", `${value.open_questions} open questions`),
  });
  return taskViewDetails({
    task, current_claim: null, claim_history: [{
      task: task.id, entry: "E10", actor: { user: "josh", host: "board.test", harness: "codex", session: "ui" },
      claimed_at: 10, ended_at: null, end_reason: null, model: null, effort: null,
    }],
    entries: [{ id: "E10" }], commits: [], open_questions: 2, oldest_open_question: "E11",
    oldest_open_question_at: 9, latest_commit_at: null, server_now: 12, claim_ttl_secs: 7200,
    claims_omitted: 0, entries_omitted: 0, commits_omitted: 0,
  });
}

function planHarness() {
  const state = {
    clockOffsetMs: 0, formDrafts: new Map(), formStatuses: new Map(), pendingForms: new Set(),
  };
  const dom = createBoardDom(state), operations = [];
  const noop = () => {};
  const context = {
    state, dom, board: noop, jsonFetch: noop, navigate: noop, scheduleRefresh: noop,
    submitMutation: (form, op, success) => {
      operations.push(op);
      success?.({}, true);
      return Promise.resolve(true);
    },
    notice: noop, apiVersion: 13, entryRecord: record => record?.entry || record,
    collection: (data, key) => Array.isArray(data?.[key]) ? data[key] : [],
    permitted: (view, capability) => view?.[capability] === true,
    nextAfter: () => null, pageParams: route => ({ ...route }), queryFilters: () => ({}),
    postKinds: ["note"], entryKinds: ["note"], entryCard: () => dom.el("div"),
    taskCard: () => dom.el("div"), workingCard: () => dom.el("div"), postForm: () => dom.el("form"),
    taskDetails: () => dom.el("div"), doneTasks: () => dom.el("div"), projectChips: () => null,
  };
  return { dom, operations, page: createPlanPage(context) };
}

function renderPlan(harness, lifecycle_state, can_change_lifecycle) {
  return harness.page.renderPlan({
    plan: { id: "P4", title: "Lifecycle", owner_user: "josh", head_revision: 2, lifecycle_state },
    revision: { id: "P4@2" }, repo_keys: [], grants: [], can_manage_grants: false,
    can_edit: false, can_change_lifecycle,
  }, { html: "", headings: [] }, {}, { tab: "done" }, []);
}

describe("Board task transfer and plan lifecycle views", () => {
  it("links both sides of transfer lineage while keeping task evidence visible", () => {
    const source = taskDetail({ id: "P1.2", title: "Source", transferred_to: "P2.3" });
    const successor = taskDetail({ id: "P2.3", title: "Successor", transferred_from: "P1.2" });

    assert.match(visibleText(source), /Transferred to P2\.3/);
    assert.match(visibleText(successor), /Transferred from P1\.2/);
    assert.ok(source.querySelectorAll("a").some(link => link.href.includes("task=P2.3")));
    assert.ok(successor.querySelectorAll("a").some(link => link.href.includes("task=P1.2")));
    assert.match(visibleText(source), /Claim history \(1\)/);
    assert.match(visibleText(source), /2 open questions/);
    assert.match(visibleText(source), /Entry E10/);
  });

  it("shows lifecycle state and requires a note to close or reopen", () => {
    const openHarness = planHarness();
    const open = renderPlan(openHarness, "open", true);
    const closeForm = open.querySelectorAll("form").find(form => form.className === "plan-lifecycle-control");
    assert.match(visibleText(open), /Plan open/);
    assert.ok(closeForm);
    submit(closeForm);
    assert.equal(openHarness.operations.length, 0);
    assert.match(visibleText(closeForm), /Enter a note/);
    closeForm.querySelector("textarea").value = "  No work remains.  ";
    submit(closeForm);
    assert.deepEqual(openHarness.operations[0], {
      op: "plan_close", plan: "P4", note: "No work remains.",
    });

    const closedHarness = planHarness();
    const closed = renderPlan(closedHarness, "closed", true);
    const reopenForm = closed.querySelectorAll("form").find(form => form.className === "plan-lifecycle-control");
    assert.match(visibleText(closed), /Plan closed/);
    assert.match(visibleText(reopenForm), /Reopen plan/);
    reopenForm.querySelector("textarea").value = "New work was approved.";
    submit(reopenForm);
    assert.deepEqual(closedHarness.operations[0], {
      op: "plan_reopen", plan: "P4", note: "New work was approved.",
    });
  });

  it("omits lifecycle controls without lifecycle authority", () => {
    const harness = planHarness();
    const page = renderPlan(harness, "open", false);
    assert.match(visibleText(page), /Plan open/);
    assert.equal(page.querySelectorAll(".plan-lifecycle-control").length, 0);
  });
});
