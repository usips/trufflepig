import { it } from "node:test";
import assert from "node:assert/strict";

// This passing test keeps the event loop alive; the Rust runner must end it on
// its deadline. The safety timer limits any process that misses cleanup.
const interval = setInterval(() => {}, 1000);
setTimeout(() => clearInterval(interval), 15_000);

it("passes while leaking a timer", () => {
  assert.equal(1 + 1, 2);
});
