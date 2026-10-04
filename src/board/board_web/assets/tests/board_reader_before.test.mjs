import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createBoardReader } from "../board_reader.js";

// Newest entry is E120 at seq 120; oldest is E1 at seq 1.
function makeEntries(count) {
  return Array.from({ length: count }, (_, index) => {
    const number = index + 1;
    return { id: `E${number}`, seq: String(number), kind: "note", body: `entry ${number}` };
  });
}

function cursorKey(cursor) {
  return [BigInt(String(cursor.seq)), BigInt(String(cursor.entry).slice(1))];
}

// Faithful post-W5.6 server: `after` pages ascending (legacy), `before` and
// no cursor page descending newest-first, both cursors is invalid_options.
function serveEntries(all, op) {
  if (op.after !== null && op.after !== undefined
    && op.before !== null && op.before !== undefined) {
    throw new Error("invalid_options: entries accepts at most one of after and before");
  }
  const through = op.through === null || op.through === undefined
    ? BigInt(all.length)
    : BigInt(String(op.through));
  let rows = all.filter(entry => BigInt(entry.seq) <= through);
  if (op.after !== null && op.after !== undefined) {
    const [seq, id] = cursorKey(op.after);
    rows = rows.filter(entry => {
      const at = [BigInt(entry.seq), BigInt(entry.id.slice(1))];
      return at[0] > seq || (at[0] === seq && at[1] > id);
    });
    const shown = rows.slice(0, op.limit);
    const last = rows.length > op.limit ? shown[shown.length - 1] : null;
    return {
      entries: shown,
      through: String(through),
      next_after: last ? { seq: last.seq, entry: last.id } : null,
    };
  }
  if (op.before !== null && op.before !== undefined) {
    const [seq, id] = cursorKey(op.before);
    rows = rows.filter(entry => {
      const at = [BigInt(entry.seq), BigInt(entry.id.slice(1))];
      return at[0] < seq || (at[0] === seq && at[1] < id);
    });
  }
  const shown = rows.slice(-op.limit).reverse();
  const oldest = rows.length > op.limit ? shown[shown.length - 1] : null;
  return {
    entries: shown,
    through: String(through),
    next_before: oldest ? { seq: oldest.seq, entry: oldest.id } : null,
  };
}

function harness(entryCount = 120) {
  const all = makeEntries(entryCount);
  const calls = [];
  const state = { watermark: "1", liveGap: true, liveEntries: [] };
  const board = async op => {
    calls.push(op);
    const data = op.op === "entries"
      ? serveEntries(all, op)
      : (() => { throw new Error(`unexpected op ${op.op}`); })();
    return { seq: data.through, warnings: [], data };
  };
  const context = {
    state,
    dom: {},
    views: { entriesPage: (data, route) => ({ data, route }) },
    board,
    jsonFetch: async () => { throw new Error("unexpected fetch"); },
    apiVersion: 4,
    readOp: (op, fields = {}) => ({ op, ...fields }),
    snapshotSeq: reply => reply.seq ?? null,
    minimumSeq: () => String(all.length),
    cursorFromRoute: () => null,
    parseBoardJson: text => JSON.parse(text),
    queryFilters: route => ({
      plan: route.plan || null, kind: route.kind || null, harness: route.harness || null,
      user: route.user || null, host: route.host || null, task: route.task || null,
      references: null, before: route.after ? JSON.parse(route.after) : null,
      through: route.through || null, limit: 50,
    }),
  };
  const route = (overrides = {}) => ({
    view: "entries", plan: "", kind: "", harness: "", user: "", host: "", task: "",
    after: "", through: "", ...overrides,
  });
  return { all, calls, state, reader: createBoardReader(context), route };
}

function ids(entries) {
  return entries.map(entry => entry.id);
}

describe("entries before-cursor paging", () => {
  it("first page reads the newest entries in one before-cursor read", async () => {
    const { calls, state, reader, route } = harness();
    const { page } = await fetchPage(reader, route({ plan: "P7" }));
    assert.equal(calls.length, 1);
    assert.ok(!("after" in calls[0]), "the probe-era after read is gone");
    assert.equal(calls[0].before, null);
    assert.equal(calls[0].through, null);
    assert.equal(calls[0].limit, 50);
    assert.equal(calls[0].plan, "P7");
    assert.deepEqual(ids(page.data.entries), ids(makeEntries(120).slice(70).reverse()));
    assert.deepEqual(page.data.next_before, { seq: "71", entry: "E71" });
    assert.equal(page.data.through, "120");
    assert.equal(state.liveGap, false);
  });

  it("older pages follow next_before across three pages exactly once", async () => {
    const { all, calls, reader, route } = harness();
    const seen = [];
    let current = route();
    for (let page = 0; page < 3; page++) {
      const before = calls.length;
      const { page: rendered } = await fetchPage(reader, current);
      assert.equal(calls.length - before, 1, `page ${page} costs one read`);
      const op = calls[calls.length - 1];
      assert.ok(!("after" in op), `page ${page} sends no probe cursor`);
      assert.deepEqual(
        op.before ?? null,
        current.after ? JSON.parse(current.after) : null,
        `page ${page} follows the before cursor`,
      );
      seen.push(...rendered.data.entries);
      if (page < 2) {
        assert.ok(rendered.data.next_before, `page ${page} continues`);
        current = route({
          after: JSON.stringify(rendered.data.next_before),
          through: rendered.data.through,
        });
      } else {
        assert.equal(rendered.data.next_before, null);
      }
    }
    assert.deepEqual(ids(seen), ids(all.slice().reverse()));
    assert.deepEqual(
      ids(seen.slice(50, 100)),
      ids(makeEntries(120).slice(20, 70).reverse()),
    );
  });
});

async function fetchPage(reader, route) {
  return reader.fetchRoute(route, undefined, false);
}
