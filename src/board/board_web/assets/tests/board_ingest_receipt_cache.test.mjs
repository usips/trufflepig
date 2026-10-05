import { describe, it } from "node:test";
import assert from "node:assert/strict";

const OWN = "ticket-own";
const FOREIGN = "ticket-foreign";

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
