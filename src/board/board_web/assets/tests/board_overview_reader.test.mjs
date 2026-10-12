import { it } from "node:test";
import assert from "node:assert/strict";
import { createBoardReader } from "../board_reader.js";

function readerHarness() {
  const calls = [];
  const board = async op => {
    calls.push(op);
    const data = op.op === "projects" ? []
      : op.op === "overview" ? { plans: [], through: "100", next_after: null }
        : op.op === "attention" ? { entries: [] }
          : op.op === "feed" ? { events: [] }
            : (() => { throw new Error(`unexpected op ${op.op}`); })();
    return { seq: "100", warnings: [], data };
  };
  const reader = createBoardReader({
    state: { projects: [], watermark: "100", overview: null }, dom: {},
    views: {
      renderOverview: (overview, attention, route) => ({ overview, attention, route }),
      renderAttention: data => data,
    },
    board,
    readOp: (op, fields = {}) => ({ op, ...fields }),
    snapshotSeq: reply => reply.seq,
    minimumSeq: replies => replies[0]?.seq,
    cursorFromRoute: (route, composite = false) => !route.after ? null
      : composite ? JSON.parse(route.after) : route.after,
    parseBoardJson: JSON.parse,
  });
  return { calls, reader };
}

it("overview sends active-only by default and follows the composite cursor", async () => {
  const { calls, reader } = readerHarness();
  const cursor = { seq: "80", plan: "P8" };
  await reader.fetchRoute({ view: "overview", after: JSON.stringify(cursor), through: "100" });
  const op = calls.find(item => item.op === "overview");
  assert.equal(op.active_only, true);
  assert.deepEqual(op.after, cursor);
  assert.equal(op.through, "100");
});

it("showing inactive plans disables active-only filtering on later pages", async () => {
  const { calls, reader } = readerHarness();
  await reader.fetchRoute({ view: "overview", showInactive: "1" });
  assert.equal(calls.find(item => item.op === "overview").active_only, false);
});
