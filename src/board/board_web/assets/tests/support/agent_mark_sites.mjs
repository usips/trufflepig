import { createBoardDom } from "../../board_dom.js";
import { createBoardViews } from "../../board_views.js";
import { createBoardReader } from "../../board_reader.js";
import { createPlanPage } from "../../pages/plan_page.js";

export const MARK_VENDORS = [
  "claude", "codex", "kimi", "grok", "gemini", "qwen", "muse", "human", "unknown",
];
export const MARK_GLYPHS = ["Cl", "Cx", "Ki", "Gk", "Ge", "Qw", "Mu", "Hu", "?"];

export function actorFixture(vendor, index) {
  return { user: "fixture", host: "board.test", harness: vendor, session: `session-${index}` };
}

export function markFixtures() {
  const timestamp = 1791417000;
  const records = MARK_VENDORS.map((vendor, index) => ({
    vendor, actor: actorFixture(vendor, index), model: `${vendor}-fixture`, effort: "max",
    id: `E${index + 1}`, kind: "note", body: `${vendor} evidence`, created_at: timestamp,
    plan: "P1", refs: [],
  }));
  const tasks = records.map((record, index) => ({
    id: `P1.${index + 1}`, title: record.body, column: "doing", section: `Section ${index + 1}`,
    seq: String(index + 1),
  }));
  const claims = records.map((record, index) => ({
    ...record, task: tasks[index].id, scope: record.body, stale: false, ended_at: null,
    since: timestamp, last_active: timestamp,
  }));
  const events = records.map((record, index) => ({
    ...record, subject: tasks[index].id, summary: record.body, seq: String(index + 1),
  }));
  const revisions = records.map((record, index) => ({
    id: `P1@${index + 1}`, actor: record.actor, source: "direct", summary: record.body,
    created_at: timestamp,
  }));
  const coauthors = records.map(record => ({
    harness: record.vendor === "unknown" ? "git:fixture@example.test" : record.vendor,
    model: record.model, email: `${record.vendor}@example.test`,
  }));
  return { records, tasks, claims, events, revisions, coauthors };
}

export function markViews() {
  const state = {
    clockOffsetMs: 0, formDrafts: new Map(), formStatuses: new Map(), pendingForms: new Set(),
    seenAtOpen: null, ingestPending: false, route: {}, watermark: "1",
  };
  const dom = createBoardDom(state);
  const originalEl = dom.el;
  // Seed the sanitized heading DOM; this shim does not parse innerHTML.
  dom.el = (tag, className, text) => {
    const element = originalEl(tag, className, text);
    if (className === "markup") {
      for (let index = 0; index < MARK_VENDORS.length; index++) {
        const heading = originalEl("h2", "", `Section ${index + 1}`);
        heading.id = `section-${index + 1}`; heading.setAttribute("id", heading.id);
        element.append(heading);
      }
    }
    return element;
  };
  const noop = () => {};
  const context = {
    state, dom, board: noop, jsonFetch: noop, navigate: noop, scheduleRefresh: noop,
    submitMutation: noop, notice: noop, apiVersion: 8,
    entryRecord: record => record?.entry && typeof record.entry === "object"
      ? record.entry : record,
    collection: (data, key) => data[key], permitted: () => false, nextAfter: () => null,
    pageParams: route => ({ ...route }), queryFilters: () => ({}),
    postKinds: ["note"], entryKinds: ["note"],
    columns: ["todo", "doing", "review", "done", "blocked"],
    errorMessage: error => error.message,
  };
  const views = createBoardViews(context);
  const planPage = createPlanPage({ ...context, ...views });
  return { dom, views, planPage, context };
}

async function revisionHeadingNodes(context, views, revisions) {
  const container = context.dom.el("div");
  for (const revision of revisions) {
    const reader = createBoardReader({
      ...context, views,
      board: async op => ({ seq: "1", warnings: [], data: op.op === "show"
        ? op.target.includes("@") ? revision : { plan: { id: "P1" }, repo_keys: [] } : [] }),
      jsonFetch: async () => ({ api: 8, html: "", headings: [], snapshot_seq: "1" }),
      readOp: (op, options) => ({ op, ...options }), snapshotSeq: reply => reply.seq || null,
      minimumSeq: () => "1", cursorFromRoute: () => null, parseBoardJson: JSON.parse,
    });
    const { page } = await reader.fetchRoute({ view: "plan", ref: revision.id }, null, false);
    container.append(page.querySelector(".page-title"));
  }
  return container;
}

export function doneSiteNodes(tasks) {
  const { views, planPage } = markViews();
  const plan = { id: "P1", title: "Done actors", head_revision: 9, owner_user: "fixture" };
  const view = { plan, revision: { id: "P1@9" }, repo_keys: [] };
  const data = { tasks, next_before: null };
  const plans = tasks.map((task, index) => {
    const id = `P${index + 1}`;
    return {
      plan: { ...plan, id }, repo_keys: [], tasks: [], claims: [], done_count: 1,
      recent_done: [{ ...task, id: `${id}.1` }],
    };
  });
  return new Map([
    ["board Done", views.renderDone(data, { view: "done" })],
    ["plan Done", planPage.renderPlan(view, { html: "", headings: [] }, data,
      { view: "plan", ref: "P1", tab: "done" })],
    ["Overview recent Done", views.renderOverview({ plans, working: [], events: [] },
      { entries: [] }, {})],
  ]);
}

export async function actorSiteNodes() {
  const { records, tasks, claims, events, revisions, coauthors } = markFixtures();
  const { dom, views, planPage, context } = markViews();
  const plan = { id: "P1", title: "Actor sites", head_revision: 9, owner_user: "fixture" };
  const board = views.renderOverview({ plans: [], working: claims, events }, { entries: [] }, {});
  const view = {
    plan, revision: { id: "P1@9" }, tasks, claims, commits: [], entries: [],
    sections_without_tasks: [], repo_keys: [],
  };
  const assignedTasks = tasks.map((task, index) => ({ ...task,
    assignee: `fixture@board.test/${MARK_VENDORS[index]}/session-${index}`,
  }));
  const rendered = {
    html: "<h2>Seeded headings</h2>",
    headings: tasks.map((task, index) => ({
      title: task.section, anchor: `section-${index + 1}`, level: 2,
    })),
  };
  const commit = {
    oid: "a".repeat(40), author: "fixture", subject: "Actor marks", committed_at: 1791417000,
    files: 1, insertions: 1, deletions: 0, coauthors, plans: [{ plan_id: "P1" }],
  };
  return new Map([
    ["claim badges", views.renderClaims({ claims: claims.map(claim => ({ claim })) }, {})],
    ["task holders", dom.add(dom.el("div"), tasks.map(task => views.taskCard(task, claims)))],
    ["assigned task holders", dom.add(dom.el("div"), assignedTasks.map(task =>
      views.taskCard(task, [])))],
    ["Working now", board.querySelector(".working-strip")],
    ["ticker", board.querySelector(".ticker")],
    ["entry cards", dom.add(dom.el("div"), records.map(record => views.entryCard(record)))],
    ["entry table", views.entriesPage({ entries: records }, { view: "entries" })],
    ["task details", views.taskDetails(view, rendered.headings)],
    ["assigned task details", views.taskDetails({ ...view, tasks: assignedTasks, claims: [] },
      rendered.headings)],
    ["plan claim markers", planPage.renderPlan(view, rendered, {}, {}).querySelector(".markup")],
    ["plan history", planPage.historyPage({ revisions }, plan)],
    ["commit coauthors", planPage.commitsPage([commit], 0, "P1")],
    ["Needs you actors", dom.add(dom.el("div"), records.map(record =>
      views.attentionCard({ type: "note", record })))],
    ["standalone revision heading", await revisionHeadingNodes(context, views, revisions)],
    ...doneSiteNodes(assignedTasks.map(task => ({ ...task, column: "done", done_at: 1791417000 }))),
  ]);
}
