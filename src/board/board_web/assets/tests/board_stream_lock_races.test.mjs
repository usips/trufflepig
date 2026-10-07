import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createStreamElection } from "../stream/stream_election.js";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { abortNextGrantBeforeCallback, abortedGrantCallbacks } from "./support/locks_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

async function waitFor(predicate, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) return false;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  return true;
}

function makeElection() {
  const session = {
    mode: "following", channel: null, channelName: "", elect: null, lockEnd: null,
    lockNames: null, electionCursor: "41", heartbeatTimer: null, freshnessTimer: null,
    lastHeartbeat: 0, stealRequested: false, leaderHealthy: false,
    relayBuffer: [], bufferOverran: false, bufferToken: null,
  };
  const state = { streamGeneration: 0, watermark: "41", resyncing: false };
  let fetchStarts = 0, rejoins = 0;
  const election = createStreamElection({
    session, state, authToken: () => "stream-test-token", setConnection: () => {},
    heartbeatMs: 10000, freshnessMs: 10000, deliver: () => false,
    runFetch: () => { fetchStarts++; state.streamGeneration++; },
    rejoinAfterSteal: () => { rejoins++; },
  });
  return { session, election, fetchStarts: () => fetchStarts, rejoins: () => rejoins };
}

describe("board stream lock races", () => {
  it("abort_before_grant_callback_never_leads", async () => {
    const tab = makeElection();
    try {
      const names = await tab.election.streamNames();
      tab.session.lockNames = names;
      abortNextGrantBeforeCallback();
      tab.election.requestLock(names, false);
      assert.equal(await waitFor(() => abortedGrantCallbacks() === 1), true,
        "the shim dispatches the callback after rejecting its granted request");
      assert.equal(tab.fetchStarts(), 0, "an already rejected lock request never starts a reader");
      assert.equal(tab.session.mode, "following");
      assert.equal(tab.session.lockEnd, null);
    } finally {
      tab.election.resetElection();
      tab.session.channel?.close();
    }
  });

  it("granted_steal_takes_over_lock_release_without_another_reader", async () => {
    const tab = makeElection();
    let queued;
    try {
      const names = await tab.election.streamNames();
      tab.session.lockNames = names;
      tab.election.requestLock(names, false);
      assert.equal(await waitFor(() => tab.fetchStarts() === 1), true);
      const oldRelease = tab.session.lockEnd;
      tab.election.requestLock(names, true);
      assert.equal(await waitFor(() => tab.session.lockEnd !== oldRelease), true,
        "the granted steal replaces the old lock release");
      assert.equal(tab.fetchStarts(), 1, "the new lock keeps the existing reader");
      assert.equal(tab.rejoins(), 0, "an in-flight same-tab steal owns the handover");
      let queuedRan = false;
      queued = navigator.locks.request(names.lock, () => { queuedRan = true; });
      await Promise.resolve();
      assert.equal(queuedRan, false, "the replacement lock remains held");
      tab.election.resetElection();
      await queued;
      assert.equal(queuedRan, true, "stop releases the replacement lock");
    } finally {
      tab.election.resetElection();
      tab.session.channel?.close();
      if (queued) await queued;
    }
  });

  it("queued_grant_before_steal_dispatch_keeps_the_reader", async () => {
    const tab = makeElection();
    const locks = navigator.locks, request = locks.request;
    const releases = [];
    locks.request = function (name, options, callback) {
      return request.call(this, name, options, () => {
        const held = callback();
        releases.push(tab.session.lockEnd);
        return held;
      });
    };
    try {
      const names = await tab.election.streamNames();
      tab.session.lockNames = names;
      tab.election.requestLock(names, false);
      tab.election.requestLock(names, true);
      assert.equal(await waitFor(() => releases.length === 2), true,
        "the already dispatched queued callback runs ahead of the steal callback");
      assert.equal(typeof releases[0], "function");
      assert.equal(typeof releases[1], "function");
      assert.notEqual(releases[0], releases[1], "the steal replaces the first grant's release");
      assert.equal(tab.fetchStarts(), 1);
      assert.equal(tab.rejoins(), 0, "the pending steal survives the queued grant becoming leader");
      assert.equal(tab.session.stealRequested, false, "the granted steal completes the handover");
    } finally {
      locks.request = request;
      tab.election.resetElection();
      tab.session.channel?.close();
    }
  });

  it("aborted_takeover_rejoins_after_the_old_lock_was_evicted", async () => {
    const tab = makeElection();
    try {
      const names = await tab.election.streamNames();
      tab.session.lockNames = names;
      tab.election.requestLock(names, false);
      assert.equal(await waitFor(() => tab.fetchStarts() === 1), true);
      abortNextGrantBeforeCallback();
      tab.election.requestLock(names, true);
      assert.equal(await waitFor(() => abortedGrantCallbacks() === 1), true);
      assert.equal(tab.rejoins(), 1, "a rejected handover cannot retain the evicted leader");
      assert.equal(tab.fetchStarts(), 1, "the rejected takeover callback starts no new reader");
    } finally {
      tab.election.resetElection();
      tab.session.channel?.close();
    }
  });
});
