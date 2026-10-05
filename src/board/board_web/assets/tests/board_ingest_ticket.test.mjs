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
