import { it } from "node:test";
import assert from "node:assert/strict";
import { createBoardReader } from "../board_reader.js";

function harness(ordinalTasks = false, planId = "P7") {
  const calls = [], state = { watermark: "40", clockOffsetMs: 0 };
  const cursor = { seq: "9007199254741000", id: `${planId}.1` };
  const plan = { plan: { id: planId }, revision: { id: `${planId}@1` }, server_now: 1700000000,
    task_ceiling: { plan: planId, ordinal: 120 }, through: "39" };
  const board = async (op, _, project) => {
    calls.push({ op, project });
    let data;
    if (op.op === "show") data = op.target.includes(".")
      ? { task: { id: op.target }, current_claim: null, claim_history: [], entries: [], commits: [],
          open_questions: 0, server_now: 1700000000, claim_ttl_secs: 7_200 }
      : plan;
    else if (op.op === "tasks" && ordinalTasks) {
      const after = op.after == null ? 0 : Number(/^(?:P[1-9]\d*)\.([1-9]\d*)$/.exec(op.after)?.[1]);
      data = { tasks: Array.from({ length: 50 }, (_, index) => ({ id: `${planId}.${after + index + 1}` })),
        ceiling: op.ceiling || plan.task_ceiling, through: op.through || "40" };
    } else if (["tasks", "done_tasks"].includes(op.op)) data = {
      tasks: [{ id: "P7.1", column: "done", done_at: 1700000000 }],
      next_before: op.before ? null : cursor, server_now: 1700000000,
    };
    else if (op.op === "projects") data = { projects: [] };
    else if (op.op === "claims") data = { claims: [] };
    else throw new Error(`Unexpected operation ${op.op}`);
    return { data, seq: "40", warnings: [] };
  };
  const reader = createBoardReader({
    state, board, dom: { planId: ref => /^P\d+/.exec(ref)?.[0] }, apiVersion: 11,
    views: {
      renderDone: (data, route) => ({ data, route, offset: state.clockOffsetMs }),
      renderPlan: (_, __, data, route) => ({ data, route, offset: state.clockOffsetMs }),
    },
    jsonFetch: async () => ({ api: 11, html: "", headings: [], snapshot_seq: "40" }),
    readOp: (op, fields = {}) => ({ op, ...fields }), snapshotSeq: reply => reply.seq ?? null,
    minimumSeq: () => "40", parseBoardJson: JSON.parse, queryFilters: () => ({}),
  });
  return { calls, cursor, reader, state };
}

it("board_done_reader_follows_project_selection_and_older_cursor", async () => {
  const { calls, reader, cursor } = harness();
  const first = await reader.fetchRoute({ view: "done", project: "W1", before: "" }, undefined, false);
  const done = calls.find(call => call.op.op === "done_tasks");
  assert.equal(done.op.scope, "all");
  assert.equal(done.project, "W1");
  assert.equal(done.op.before, null);
  assert.deepEqual(first.page.data.next_before, cursor);
  await reader.fetchRoute({ view: "done", project: "W1", before: JSON.stringify(cursor) }, undefined, false);
  assert.deepEqual(calls.filter(call => call.op.op === "done_tasks")[1].op.before, cursor);
  const unscoped = harness();
  await unscoped.reader.fetchRoute({ view: "done", project: "unscoped" }, undefined, false);
  assert.equal(unscoped.calls.find(call => call.op.op === "done_tasks").op.scope, "unscoped");
  assert.equal(unscoped.calls.find(call => call.op.op === "done_tasks").project, undefined);
});

it("done_task_card_target_beyond_fifty_loads_its_captured_ordinal_window", async () => {
  const { calls, reader } = harness(true, "P10");
  const route = { view: "plan", ref: "P10", tab: "tasks", task: "P10.71" };
  const result = await reader.fetchRoute(route, undefined, false);
  assert.equal(result.page.route.task, "P10.71");
  assert.ok(result.page.data.tasks.tasks.some(task => task.id === route.task));
  const operation = calls.find(call => call.op.op === "tasks").op;
  assert.equal(operation.after, "P10.70");
  assert.deepEqual(operation.ceiling, { plan: "P10", ordinal: 120 });
  assert.equal(operation.through, "39");
});

it("plan_task_details_read_the_selected_task_view_separately_from_plan_context", async () => {
  const { calls, reader } = harness();
  const selected = { id: "P7.1", title: "Selected task", column: "doing" };
  const result = await reader.fetchRoute({ view: "plan", ref: "P7", tab: "tasks", task: selected.id },
    undefined, false);
  const taskRead = calls.find(call => call.op.op === "show" && call.op.target === selected.id);
  const planRead = calls.find(call => call.op.op === "show" && call.op.target === "P7");
  assert.ok(planRead, "the page still reads plan context");
  assert.ok(taskRead, "the selected task is fetched through its own show operation");
  assert.equal(result.page.data.taskView.task.id, selected.id);
  assert.ok(result.page.data.tasks.tasks.some(task => task.id === selected.id));
});

it("done_task_target_respects_explicit_paging_and_rejects_foreign_or_zero_ids", async () => {
  const { calls, reader } = harness(true);
  await reader.fetchRoute({ view: "plan", ref: "P7", tab: "tasks", task: "P7.99", taskAfter: "P7.50",
    taskCeiling: JSON.stringify({ plan: "P7", ordinal: 110 }), taskThrough: "35" }, undefined, false);
  const operation = calls.find(call => call.op.op === "tasks").op;
  assert.equal(operation.after, "P7.50");
  assert.deepEqual(operation.ceiling, { plan: "P7", ordinal: 110 });
  assert.equal(operation.through, "35");
  for (const task of ["P8.99", "P7.0"]) {
    await assert.rejects(harness(true).reader.fetchRoute({ view: "plan", ref: "P7", tab: "tasks", task },
      undefined, false), /current plan/);
  }
});

it("plan_done_reader_uses_recent_order_and_the_server_clock_on_first_render", async () => {
  const { calls, reader } = harness();
  const realNow = Date.now;
  Date.now = () => 1700000000000 - 9 * 86400000;
  try {
    const first = await reader.fetchRoute({ view: "plan", ref: "P7", tab: "done", project: "W1" },
      undefined, false);
    const tasks = calls.find(call => call.op.op === "tasks");
    assert.equal(tasks.op.column, "done");
    assert.equal(tasks.op.order, "recent_first");
    assert.equal(tasks.op.after, null);
    assert.equal(tasks.op.ceiling, null);
    assert.equal(tasks.project, undefined);
    assert.equal(first.page.offset, 9 * 86400000);
  } finally { Date.now = realNow; }
});
