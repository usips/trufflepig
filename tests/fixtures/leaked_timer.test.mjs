import { it } from "node:test";
import assert from "node:assert/strict";

// A passing test whose interval keeps the event loop alive, so `node --test`
// never exits on its own; the Rust runner must fail the run on its deadline
// instead of hanging. The cleanup timer bounds any orphaned copy of this
// process to two minutes.
const interval = setInterval(() => {}, 1000);
setTimeout(() => clearInterval(interval), 120_000);

it("passes while leaking a timer", () => {
  assert.equal(1 + 1, 2);
});
