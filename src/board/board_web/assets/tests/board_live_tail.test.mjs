import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom, resetFakeDom } from "./fake_dom.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardEntries } from "../board_entries.js";
import { createBoardReader } from "../board_reader.js";

installFakeDom();
beforeEach(() => resetFakeDom());

function entry(id, seq) {
  return {
    seq,
    entry: {
      id, seq, kind: "note", plan: "P1", body: `body ${id}`, refs: [],
      actor: { user: "u", host: "h", harness: "human", session: "s" },
      created_at: 1,
    },
  };
}

function entriesContext(state) {
  const dom = createBoardDom(state);
  return createBoardEntries({
    state, dom, navigate: () => {}, entryRecord: record => record?.entry || record,
    collection: (data, key) => data[key], entryKinds: ["note"],
    nextAfter: data => data?.next_after ?? data?.next_before ?? null,
    pageParams: (route, data) => ({ ...route, after: data.next_after }),
  });
}

function entriesRoute() {
  return {
    view: "entries", ref: "", after: "", through: "", plan: "", kind: "",
    harness: "", user: "", host: "", task: "",
  };
}

describe("board live tail", () => {
  it("renders no live rows or divider from stale live state", () => {
    const state = {
      clockOffsetMs: 0, formDrafts: new Map(), formStatuses: new Map(), pendingForms: new Set(),
      liveEntries: [entry("E99", "99")], liveGap: false,
    };
    const { entriesPage } = entriesContext(state);
    const page = entriesPage({ entries: [entry("E1", "50")], through: "60", omitted: 0 }, entriesRoute());
    const dividers = page.querySelectorAll(".live-divider");
    assert.equal(dividers.length, 0);
    const rows = page.querySelectorAll("tr").filter(row => row.dataset.entryId);
    assert.deepEqual(rows.map(row => row.dataset.entryId), ["E1"]);
  });

  it("reads #/entries with exactly one entries read and no show fetch", async () => {
    const state = {
      watermark: "7", clockOffsetMs: 0, liveEntries: [], liveGap: false,
      formDrafts: new Map(), formStatuses: new Map(), pendingForms: new Set(),
    };
    const ops = [];
    const dom = createBoardDom(state);
    const { entriesPage } = entriesContext(state);
    const { fetchRoute } = createBoardReader({
      state, dom, views: { entriesPage },
      board: async op => {
        ops.push(op.op);
        return { seq: "7", data: { entries: [], through: "7", omitted: 0 }, warnings: [] };
      },
      jsonFetch: async () => { throw new Error("no render reads on this route"); },
      apiVersion: 1, readOp: (op, fields = {}) => ({ op, ...fields }),
      snapshotSeq: reply => (/^(0|[1-9]\d*)$/.test(String(reply?.seq)) ? String(reply.seq) : null),
      minimumSeq: replies => replies.map(reply => String(reply.seq)).reduce((a, b) => a < b ? a : b),
      cursorFromRoute: () => null, parseBoardJson: JSON.parse, queryFilters: () => ({}),
    });
    const result = await fetchRoute(entriesRoute(), null, false);
    assert.deepEqual(ops, ["entries"]);
    assert.equal(result.watermark, "7");
  });
});
