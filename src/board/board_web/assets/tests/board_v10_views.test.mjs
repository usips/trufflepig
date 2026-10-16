import { beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardViews } from "../board_views.js";
import { createBoardGrantPanel } from "../board_grants.js";

installDomShim();
beforeEach(() => resetDomShim());

const NOW = 10_000;
const actor = (user, harness = "codex", session = user) => ({
  user, host: "board.test", harness, session,
});

function harness({ denied = false } = {}) {
  const operations = [], notices = [];
  const state = {
    clockOffsetMs: NOW * 1000 - Date.now(), formDrafts: new Map(), formStatuses: new Map(),
    pendingForms: new Set(), seenAtOpen: null,
  };
  const dom = createBoardDom(state), noop = () => {};
  const views = createBoardViews({
    state, dom, board: noop, jsonFetch: noop, navigate: noop, scheduleRefresh: noop,
    submitMutation: (form, op) => {
      operations.push(op);
      if (denied) dom.formStatus(form, "The backend denied this authority change.");
    },
    notice: (...args) => notices.push(args), apiVersion: 11,
    entryRecord: record => record?.entry && typeof record.entry === "object" ? record.entry : record,
    collection: (data, key) => Array.isArray(data?.[key]) ? data[key] : [],
    permitted: () => false, nextAfter: () => null, pageParams: route => ({ ...route }),
    queryFilters: () => ({}), postKinds: ["note"], entryKinds: ["note"],
    columns: ["todo", "doing", "review", "blocked", "done"],
    errorMessage: error => error?.message || String(error),
  });
  return { state, dom, views, operations, notices };
}

function dispatch(element, type) {
  const event = { type, target: element, preventDefault() {} };
  for (let target = element; target; target = target.parentElement) {
    for (const listener of target.listeners.get(type) || []) listener(event);
  }
}

function visibleText(node) {
  return `${node.textContent || ""}${node.children.map(visibleText).join("")}`;
}

function worker(handle, options = {}) {
  return {
    address: { task: "P1.2", handle }, claim: "E10", entry: `E-${handle}`,
    actor: actor(handle, options.harness || "claude"), role: options.role || "code",
    model: options.model || "gpt-6-luna", effort: "xhigh", scope: `Working on ${handle}`,
    files: ["src/deeply/nested/path/with-a-long-component/board.rs"],
    state: options.state || null, added_at: NOW - 500, last_active: options.lastActive || NOW - 10,
    ended_at: options.endedAt ?? null, note: null, progress: options.progress || null,
    verdict: options.verdict || null,
  };
}

function claim(overrides = {}) {
  return {
    task: "P1.2", actor: actor("lead"), vendor: "codex", entry: "E10", scope: "Own task plan",
    claimed_at: NOW - 300, last_active: NOW - 10, ended_at: null, end_reason: null, stale: false,
    model: "gpt-6.1-sol", effort: "max", delegated_by: actor("owner", "human"),
    workers: [], workers_omitted: 0, progress: null, open_questions: 0,
    oldest_open_question: null, oldest_open_question_at: null, commit_count: 0,
    latest_commit: null, latest_commit_at: null, ...overrides,
  };
}

function progress(updatedAt = NOW - 100) {
  return {
    claim: "E10", worker: null,
    head: { value: { oid: "a".repeat(40), verified: true }, entry: "E11", updated_at: NOW - 9_000 },
    step: { value: { current: 3, total: 5 }, entry: "E11", updated_at: updatedAt },
    tests: { value: { passed: 21, failed: 2 }, entry: "E11", updated_at: updatedAt },
    gates: [{ name: "browser", status: "Fail", entry: "E12", updated_at: updatedAt }],
    eta_at: { value: NOW - 1, entry: "E11", updated_at: updatedAt },
  };
}

describe("Board v10 task and worker views", () => {
  it("renders bounded worker metadata and ages every progress fact", () => {
    const { views } = harness();
    const workers = [
      worker("reviewer", { role: "review", verdict: "findings=2", progress: progress() }),
      ...Array.from({ length: 17 }, (_, index) => worker(`live-${index}`)),
      worker("recent-ended", { state: "done", endedAt: NOW - 900, lastActive: NOW - 900 }),
      worker("old-ended", { state: "stopped", endedAt: NOW - 4_000, lastActive: NOW - 4_000 }),
      worker("overflow-ended", { state: "failed", endedAt: NOW - 5_000, lastActive: NOW - 5_000 }),
    ];
    const working = claim({
      workers, workers_omitted: 2, progress: progress(), open_questions: 3,
      oldest_open_question: "E42", oldest_open_question_at: NOW - 4_000,
      commit_count: 7, latest_commit: "b".repeat(40), latest_commit_at: NOW - 200,
    });
    const card = views.workingCard(working, { title: "Task detail" }, {
      now: NOW, ttlSecs: 7_200,
      entries: [{ id: "E10", origin: { harness: "muse", session: "observed-session" }, authority: "grant:E3" },
        { id: "E-reviewer", origin: { harness: "kimi", session: "review-origin" }, authority: "owner" }],
    });
    const cardText = visibleText(card);
    assert.match(cardText, /Task lead/);
    assert.match(cardText, /Assigned by/);
    assert.match(cardText, /origin muse\/observed-session/);
    assert.match(cardText, /authority grant:E3/);
    assert.match(cardText, /Reviewer/);
    assert.match(cardText, /gpt-6-luna · xhigh/);
    assert.match(cardText, /Files: src\/deeply\/nested/);
    assert.match(cardText, /Review: findings \(2\)/);
    assert.match(cardText, /3 open questions/);
    assert.equal(card.querySelector(".question-signal").dataset.age, "old");
    assert.match(cardText, new RegExp("b".repeat(40)));
    assert.equal(card.querySelectorAll(".worker-row").length, 20);
    assert.match(card.querySelector(".worker-omitted").textContent, /3 workers omitted/);
    const ended = card.querySelector(".agent-ended-group");
    assert.equal(Boolean(ended.open), false);
    assert.match(ended.querySelector("summary").textContent, /Ended workers \(1\)/);
    assert.equal(card.querySelectorAll('.worker-row[data-state="live"]').length, 18);
    assert.equal(card.querySelectorAll('.worker-row[data-state="done"]').length, 1);
    assert.ok(card.querySelectorAll('.progress-fact[data-stale="true"]').length >= 1);
    assert.equal(card.querySelector('.progress-eta').dataset.overdue, "true");
    assert.match(cardText, /Reported tests/);
    assert.match(cardText, /21 passed · 2 failed/);
    assert.match(cardText, /Gate · browser/);
  });

  it("keeps task entries and commits scoped to TaskView and marks every coauthor", () => {
    const { views } = harness();
    const task = { id: "P1.2", title: "Review protocol", column: "doing", section: "Protocol" };
    const taskView = {
      task, current_claim: claim({ workers: [
        worker("clean", { role: "review", verdict: "clean" }),
        worker("blocking", { role: "review", verdict: "blocking=1" }),
      ] }),
      claim_history: [claim({ entry: "E19", ended_at: NOW - 2_000, end_reason: "stopped",
        workers: [worker("old-reviewer", { role: "review", verdict: "blocking=1", progress: progress() })],
        progress: progress(NOW - 2_000) })],
      entries: [{ id: "E50", task: task.id, kind: "question", body: "Task-only question",
        refs: [task.id], actor: actor("reviewer", "muse"), vendor: "muse", model: "muse-spark",
        created_at: NOW - 1_000, origin: { harness: "codex", session: "observer" }, authority: "grant:E3" }],
      commits: [
        { repo_key: "repo", oid: "c".repeat(40), subject: "newest task change", committed_at: NOW - 100,
          author: "Lead", files: 1, insertions: 2, deletions: 0,
          coauthors: [{ harness: "codex", model: "gpt-6.1-sol", email: "lead@example.test" },
            { harness: "muse", model: "muse-spark", email: "review@example.test" }],
          worker_refs: [{ task: task.id, handle: "reviewer" }],
          claim_links: [{ task: task.id, claim: "E10", basis: "claim_window", actor: actor("lead") }] },
        { repo_key: "repo", oid: "d".repeat(40), subject: "older task change", committed_at: NOW - 200,
          author: "Worker", files: 1, insertions: 1, deletions: 0,
          coauthors: [{ harness: "claude", model: "claude-sonnet", email: "worker@example.test" }],
          worker_refs: [], claim_links: [] },
      ],
      open_questions: 2, oldest_open_question: "E50", oldest_open_question_at: NOW - 9_000,
      latest_commit_at: NOW - 100, through: "50", server_now: NOW, claim_ttl_secs: 7_200,
      claims_omitted: 1, entries_omitted: 2, commits_omitted: 0,
    };
    const details = views.taskDetails({ plan: { id: "P1" }, tasks: [task], claims: [], task_view: taskView }, []);
    const entriesSection = details.querySelector(".task-view-entries");
    assert.equal(entriesSection.querySelectorAll(".entry-card").length, 1);
    const entryText = visibleText(entriesSection);
    assert.match(entryText, /Task-only question/);
    assert.match(entryText, /origin codex\/observer/);
    assert.match(entryText, /authority grant:E3/);
    assert.match(visibleText(details.querySelector(".task-view-detail")), /2 open questions/);
    assert.equal(details.querySelector(".question-signal").dataset.age, "expired");
    assert.equal(details.querySelector(".task-view-commits").querySelectorAll(".coauthor").length, 3);
    assert.equal(details.querySelector(".task-view-commits").querySelectorAll(".agent-mark").length, 4);
    assert.match(visibleText(details.querySelector(".task-latest-commit")), new RegExp("c".repeat(40)));
    const detailText = visibleText(details.querySelector(".task-view-detail"));
    assert.match(detailText, /Review: clean/);
    assert.match(detailText, /Review: blocking \(1\)/);
    assert.match(detailText, /Claim history \(1\)/);
    const historySnapshot = details.querySelector(".task-claim-snapshot");
    assert.ok(historySnapshot);
    assert.equal(Boolean(historySnapshot.open), false, "historical agent snapshots start collapsed");
    historySnapshot.open = true;
    dispatch(historySnapshot, "toggle");
    const historyText = visibleText(historySnapshot);
    assert.match(historyText, /old-reviewer/);
    assert.match(historyText, /Review: blocking \(1\)/);
    assert.match(historyText, /21 passed · 2 failed/);
    assert.equal(historySnapshot.querySelectorAll(".worker-row").length, 1);
    assert.match(detailText, /2 more task entries omitted/);
  });

  it("lets the viewer release a stale claim through the existing task move", () => {
    const { views, operations } = harness();
    const task = { id: "P1.2", title: "Stale task", column: "doing" };
    const details = views.taskDetails({ plan: { id: "P1" }, tasks: [task], claims: [],
      task_view: { task, current_claim: claim({ stale: true }), claim_history: [], entries: [], commits: [],
        open_questions: 0, server_now: NOW, claim_ttl_secs: 7_200 } }, []);
    const release = details.querySelectorAll("button").find(button => button.textContent.includes("Release stale claim"));
    assert.ok(release);
    assert.equal(Boolean(release.disabled), false);
    for (const listener of release.listeners.get("click") || []) listener({ type: "click", target: release });
    assert.deepEqual(operations[0], { op: "task_move", task: "P1.2", column: "todo", to: null });
  });

  it("shows question age and linked commit metadata on task cards", () => {
    const { views } = harness();
    const current = claim({ open_questions: 2, oldest_open_question: "E42",
      oldest_open_question_at: NOW - 4_000, commit_count: 3,
      latest_commit: "f".repeat(40), latest_commit_at: NOW - 200 });
    const card = views.taskCard({ id: "P1.2", title: "Evidence card", column: "doing" }, [current], {
      now: NOW, ttlSecs: 7_200,
      entries: [{ id: current.entry, origin: { harness: "muse", session: "observer" }, authority: "grant:E3" }],
    });
    assert.equal(card.querySelector(".question-signal").dataset.age, "old");
    assert.match(visibleText(card), /2 open questions/);
    assert.match(visibleText(card), /latest/);
    assert.match(visibleText(card), /3 linked commits/);
    assert.match(visibleText(card), new RegExp("f".repeat(40)));
    assert.match(visibleText(card), /origin muse\/observer/);
    assert.match(visibleText(card), /authority grant:E3/);
  });
});

describe("plan grant controls", () => {
  const grants = [
    { entry: "E1", plan: "P1", owner_user: "owner", roles: ["triage"],
      grantee: "user@host/codex/session", granted_at: NOW - 100, expires_at: NOW + 1_000,
      revoked_by: null, source: "owner" },
    { entry: "E2", plan: "P1", owner_user: "owner", roles: ["release"],
      grantee: "other@host/muse/session", granted_at: NOW - 400, expires_at: NOW - 1,
      revoked_by: null, source: "migration" },
    { entry: "E3", plan: "P1", owner_user: "owner", roles: ["link"],
      grantee: "third@host/kimi/session", granted_at: NOW - 500, expires_at: null,
      revoked_by: "E4", source: "owner" },
  ];

  it("shows live, expired, and revoked grants while hiding controls when the backend denies management", () => {
    const { state, dom } = harness();
    const { grantPanel } = createBoardGrantPanel({ state, dom, submitMutation() {}, scheduleRefresh() {}, notice() {} });
    const panel = grantPanel({ plan: { id: "P1" }, grants, can_manage_grants: false });
    assert.deepEqual(panel.querySelectorAll(".grant-row").map(row => row.dataset.status), ["live", "expired", "revoked"]);
    assert.equal(panel.querySelector(".grant-form"), null);
    assert.equal(panel.querySelector("button"), null);
    const panelText = visibleText(panel);
    assert.match(panelText, /Source migration/);
    assert.match(panelText, /Expires/);
    assert.match(panelText, /Revoked by E4/);
  });

  it("submits explicit session roles and keeps backend denial visible without adding a grant", () => {
    const { state, dom, operations } = harness({ denied: true });
    const { grantPanel } = createBoardGrantPanel({ state, dom, submitMutation: (form, op) => {
      operations.push(op); dom.formStatus(form, "The backend denied this authority change.");
    }, scheduleRefresh() {}, notice() {} });
    const panel = grantPanel({ plan: { id: "P1" }, grants, can_manage_grants: true });
    const form = panel.querySelector(".grant-form");
    const identity = form.querySelectorAll("input").find(input => input.name === "grantee");
    identity.value = "new@host/codex/session";
    const selects = form.querySelectorAll("select");
    const scope = selects.find(input => input.name === "grantee_scope");
    assert.equal(scope.value, "session", "exact session scope is the default");
    const duration = selects.find(input => input.name === "duration");
    assert.equal(duration.value, "", "grants default to no expiry");
    const role = form.querySelectorAll("input").find(input => input.name === "roles" && input.value === "release");
    role.checked = true;
    dispatch(form, "submit");
    assert.deepEqual(operations[0], { op: "grant", plan: "P1", roles: ["release"],
      grantee: "new@host/codex/session", expires_in: null, note: null });
    duration.value = "604800";
    dispatch(form, "submit");
    assert.deepEqual(operations[1], { op: "grant", plan: "P1", roles: ["release"],
      grantee: "new@host/codex/session", expires_in: 604_800, note: null });
    assert.equal(panel.querySelectorAll(".grant-row").length, 3);
    assert.match(visibleText(form), /The backend denied this authority change/);
  });

  it("requires an explicit harness-wide choice for a bare harness and validates target syntax", () => {
    const { state, dom, operations } = harness();
    const { grantPanel } = createBoardGrantPanel({ state, dom, submitMutation: (form, op) => operations.push(op),
      scheduleRefresh() {}, notice() {} });
    const panel = grantPanel({ plan: { id: "P1" }, grants, can_manage_grants: true });
    const form = panel.querySelector(".grant-form");
    const controls = form.querySelectorAll("input,select");
    const identity = controls.find(input => input.name === "grantee");
    const scope = controls.find(input => input.name === "grantee_scope");
    const role = controls.find(input => input.name === "roles" && input.value === "triage");
    role.checked = true;

    identity.value = "codex";
    dispatch(form, "submit");
    assert.equal(operations.length, 0);
    assert.match(visibleText(form), /exact user@host\/harness\/session identity/);

    identity.value = "codex/session-2";
    dispatch(form, "submit");
    assert.equal(operations[0].grantee, "codex/session-2", "backend session shorthand remains available");

    scope.value = "harness";
    identity.value = "codex";
    dispatch(form, "submit");
    assert.equal(operations[1].grantee, "codex");

    identity.value = "user@host/codex/session";
    dispatch(form, "submit");
    assert.equal(operations.length, 2, "harness-wide scope only accepts a bare harness");
    assert.match(visibleText(form), /one valid bare harness label/);
  });
});
