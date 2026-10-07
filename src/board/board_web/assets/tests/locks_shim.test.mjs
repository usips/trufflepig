import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import {
  createLocks, resetLocksShim, abortNextGrantBeforeCallback, abortedGrantCallbacks,
} from "./support/locks_shim.mjs";

afterEach(() => resetLocksShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));

function deferred() {
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  return { gate, release };
}

describe("web locks shim", () => {
  it("aborts a granted request before dispatching its callback", async () => {
    const locks = createLocks();
    const order = [];
    abortNextGrantBeforeCallback();
    await locks.request("resource", () => { order.push("callback"); })
      .catch(error => { assert.equal(error.name, "AbortError"); order.push("rejected"); });
    assert.deepEqual(order, ["rejected", "callback"]);
    assert.equal(abortedGrantCallbacks(), 1);
    await locks.request("resource", () => { order.push("next"); });
    assert.equal(order.at(-1), "next", "the aborted grant releases its queue position");
  });

  it("holds the lock until the callback settles", async () => {
    const locks = createLocks();
    const held = deferred();
    let secondRan = false;
    const first = locks.request("resource", async () => { await held.gate; });
    await tick();
    const second = locks.request("resource", () => { secondRan = true; });
    await tick(30);
    assert.equal(secondRan, false, "a queued request waits for the holder");
    held.release();
    await first; await second;
    assert.equal(secondRan, true);
  });

  it("drops a queued request when its signal aborts", async () => {
    const locks = createLocks();
    const held = deferred();
    const first = locks.request("resource", () => held.gate);
    await tick();
    const controller = new AbortController();
    const queued = locks.request("resource", { signal: controller.signal }, () => {});
    controller.abort(new Error("cancelled"));
    await assert.rejects(queued, /cancelled/);
    held.release();
    await first;
  });

  it("steal_goes_to_the_front_of_the_queue", async () => {
    const locks = createLocks();
    const held = deferred();
    const order = [];
    const first = locks.request("resource", () => held.gate);
    await tick();
    const queued = locks.request("resource", () => { order.push("queued"); });
    const stealer = locks.request("resource", { steal: true }, () => { order.push("stealer"); });
    await assert.rejects(first, /stolen/);
    held.release();
    await queued; await stealer;
    assert.deepEqual(order, ["stealer", "queued"]);
  });
});
