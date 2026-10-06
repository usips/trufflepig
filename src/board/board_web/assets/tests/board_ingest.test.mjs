import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  INGEST_TIMEOUT_MS,
  INGEST_UNKNOWN_MESSAGE,
  resolveIngestEvent,
} from "../board_ingest.js";

const OWN = "ticket-own";
const FOREIGN = "ticket-foreign";

describe("resolveIngestEvent", () => {
  it("completes only on the held ticket", () => {
    assert.equal(
      resolveIngestEvent(OWN, { type: "receipt", result: { ticket: OWN, inserted: 3 } }).outcome,
      "completed",
    );
  });

  it("ignores foreign tickets so pending stays until the own receipt", () => {
    for (const result of [
      { ticket: FOREIGN, inserted: 3 },
      { inserted: 3 },
      { ticket: 42 },
      { ticket: "" },
      null,
    ]) {
      assert.equal(resolveIngestEvent(OWN, { type: "receipt", result }).outcome, "ignored", JSON.stringify(result));
    }
    assert.equal(
      resolveIngestEvent(null, { type: "receipt", result: { ticket: OWN } }).outcome,
      "ignored",
    );
  });

  it("ignores a duplicate receipt once the ticket is cleared", () => {
    assert.equal(
      resolveIngestEvent(OWN, { type: "receipt", result: { ticket: OWN } }).outcome,
      "completed",
    );
    assert.equal(
      resolveIngestEvent(null, { type: "receipt", result: { ticket: OWN } }).outcome,
      "ignored",
    );
  });

  it("times a pending ticket out to unknown after thirty seconds, never completed", () => {
    assert.equal(INGEST_TIMEOUT_MS, 30000);
    assert.equal(
      resolveIngestEvent(OWN, { type: "timeout" }).outcome,
      "unknown",
    );
    assert.equal(INGEST_UNKNOWN_MESSAGE, "ingest result unknown");
  });

  it("ignores a timeout with no held ticket", () => {
    for (const held of [null, "", undefined]) {
      assert.equal(resolveIngestEvent(held, { type: "timeout" }).outcome, "ignored", String(held));
    }
  });
});

// The receipt cache is new: dynamic import keeps this file red as an
// assertion (not a SyntaxError) on code that lacks the export.
async function requireReceiptCache() {
  const module = await import("../board_ingest.js");
  assert.equal(
    typeof module.createIngestReceiptCache,
    "function",
    "the ingest client caches early receipts by ticket",
  );
  return module.createIngestReceiptCache;
}

describe("ingest receipt cache", () => {
  it("completes the 202 from a receipt that arrived first", async () => {
    const cache = (await requireReceiptCache())();
    cache.observe({ ticket: OWN, inserted: 3 });
    assert.deepEqual(cache.take(OWN), { ticket: OWN, inserted: 3 });
  });

  it("misses on unknown tickets and delivers each receipt once", async () => {
    const cache = (await requireReceiptCache())();
    assert.equal(cache.take("ticket-never-published"), undefined);
    cache.observe({ ticket: OWN, inserted: 3 });
    assert.deepEqual(cache.take(OWN), { ticket: OWN, inserted: 3 });
    assert.equal(cache.take(OWN), undefined);
  });

  it("never completes a foreign or malformed ticket", async () => {
    const cache = (await requireReceiptCache())();
    cache.observe({ ticket: FOREIGN, inserted: 3 });
    for (const junk of [null, undefined, {}, { ticket: 42 }, { ticket: "" }]) {
      cache.observe(junk);
    }
    assert.equal(cache.take(OWN), undefined);
    for (const held of [null, "", undefined, 42]) {
      assert.equal(cache.take(held), undefined, String(held));
    }
    assert.deepEqual(cache.take(FOREIGN), { ticket: FOREIGN, inserted: 3 });
  });

  it("keeps only the last eight receipts", async () => {
    const cache = (await requireReceiptCache())();
    for (let id = 0; id < 9; id++) cache.observe({ ticket: `ticket-${id}` });
    assert.equal(cache.take("ticket-0"), undefined);
    assert.deepEqual(cache.take("ticket-8"), { ticket: "ticket-8" });
  });
});
