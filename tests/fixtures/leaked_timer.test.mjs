import { it } from "node:test";
import assert from "node:assert/strict";

// The runner owns the deadline and process cleanup for this leaked interval.
setInterval(() => {}, 1000);

it("passes while leaking a timer", () => {
  assert.equal(1 + 1, 2);
});
