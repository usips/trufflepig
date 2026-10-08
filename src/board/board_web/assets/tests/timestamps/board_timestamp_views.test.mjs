import { beforeEach, it } from "node:test";
import assert from "node:assert/strict";
import { createBoardDom } from "../../board_dom.js";
import { createBoardReader } from "../../board_reader.js";
import { createPlanPage } from "../../pages/plan_page.js";
import { installDomShim, resetDomShim } from "../support/dom_shim.mjs";

installDomShim();
beforeEach(resetDomShim);
const timestamp = 1705332540;
function context() {
  const state = { clockOffsetMs: 1705332600000 - Date.now(), route: { view: "plan", ref: "P1" },
    projects: [], overview: {}, watermark: "5" };
  return { state, dom: createBoardDom(state) };
}
function assertRelativeTimestamp(page) {
  const items = page.querySelectorAll("time[data-timestamp]");
  assert.equal(items.length, 1, "timestamp remains a live semantic node, not header text");
  assert.equal(items[0].textContent, "1m ago");
  assert.equal(items[0].dateTime, "2024-01-15T15:29:00.000Z");
  assert.ok(items[0].title.includes("2024"));
}

it("linked commit metadata retains a live timestamp", () => {
  const { dom, state } = context();
  const page = createPlanPage({ dom, state }).commitsPage([{
    oid: "a".repeat(40), subject: "fix(board): retain time", author: "Board Author",
    committed_at: timestamp, files: 2, insertions: 3, deletions: 4, plans: [], coauthors: [],
  }], 0, "P1");
  assertRelativeTimestamp(page);
});

it("a stored revision header retains a live timestamp", async () => {
  const { dom, state } = context();
  const revision = { id: "P1@1", source: "create", actor: {
    user: "josh", host: "board", harness: "codex", session: "timestamps",
  }, created_at: timestamp };
  const reader = createBoardReader({
    dom, state, apiVersion: 8,
    views: { projectChips: () => dom.el("div"), sanitizedMarkup: () => dom.el("div") },
    board: async op => ({ seq: "5", warnings: [],
      data: op.target === revision.id ? revision : { plan: { id: "P1" }, repo_keys: [] } }),
    readOp: (op, fields) => ({ op, ...fields }), snapshotSeq: reply => reply.seq,
    minimumSeq: () => "5",
    jsonFetch: async () => ({ api: 8, html: "", headings: [], snapshot_seq: "5" }),
  });
  const result = await reader.fetchRoute({ view: "plan", ref: "P1@1" });
  assertRelativeTimestamp(result.page);
});
