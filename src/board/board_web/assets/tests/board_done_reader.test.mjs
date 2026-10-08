import { it } from "node:test";
import assert from "node:assert/strict";
import { createBoardReader } from "../board_reader.js";

function harness(ordinalTasks = false) {
  const calls = [], state = { watermark: "40", clockOffsetMs: 0 };
  const cursor = { seq: "9007199254741000", id: "P7.1" };
  const plan = { plan: { id: "P7" }, revision: { id: "P7@1" }, server_now: 1700000000,
    task_ceiling: { plan: "P7", ordinal: 120 }, through: "39" };
  const board = async (op, _, project) => {
    calls.push({ op, project });
    let data;
    if (op.op === "show") data = plan;
    else if (op.op === "tasks" && ordinalTasks) {
      const after = Number(op.after || 0);
      data = { tasks: Array.from({ length: 50 }, (_, index) => ({ id: `P7.${after + index + 1}` })),
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
    state, board, dom: { planId: ref => /^P\d+/.exec(ref)?.[0] }, apiVersion: 8,
    views: {
      renderDone: (data, route) => ({ data, route, offset: state.clockOffsetMs }),
      renderPlan: (_, __, data, route) => ({ data, route, offset: state.clockOffsetMs }),
    },
    jsonFetch: async () => ({ api: 8, html: "", headings: [], snapshot_seq: "40" }),
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
  const { calls, reader } = harness(true);
  const route = { view: "plan", ref: "P7", tab: "tasks", task: "P7.99" };
  const result = await reader.fetchRoute(route, undefined, false);
  assert.equal(result.page.route.task, "P7.99");
  assert.ok(result.page.data.tasks.tasks.some(task => task.id === route.task));
  const operation = calls.find(call => call.op.op === "tasks").op;
  assert.equal(operation.after, "98");
  assert.deepEqual(operation.ceiling, { plan: "P7", ordinal: 120 });
  assert.equal(operation.through, "39");
});

it("done_task_target_respects_explicit_paging_and_rejects_foreign_or_zero_ids", async () => {
  const { calls, reader } = harness(true);
  await reader.fetchRoute({ view: "plan", ref: "P7", tab: "tasks", task: "P7.99", taskAfter: "50",
    taskCeiling: JSON.stringify({ plan: "P7", ordinal: 110 }), taskThrough: "35" }, undefined, false);
  const operation = calls.find(call => call.op.op === "tasks").op;
  assert.equal(operation.after, "50");
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
