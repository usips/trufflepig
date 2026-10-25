import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardStream } from "../../stream/board_stream.js";
import { installDomShim, resetDomShim } from "../support/dom_shim.mjs";
import { rejectSteals, allowSteals, stealRejections } from "../support/locks_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));

function makeTab(fetchCalls, { heartbeatMs, freshnessMs } = {}) {
  const state = {
    watermark: null, stream: null, streamGeneration: 0, reconnect: null,
    streamFailures: 0, resyncing: false,
  };
  const stream = createBoardStream({
    state,
    privateFetch: (...args) => {
      fetchCalls.push(args);
      return new Promise(() => {});
    },
    setConnection: () => {},
    loadRoute: async () => {},
    scheduleRefresh: () => {},
    parseBoardJson: JSON.parse,
    addTickerEvent: () => {},
    addLiveEntry: () => {},
    noteSeen: () => {},
    onIngest: () => {},
    authToken: () => "stream-test-token",
    heartbeatMs,
    freshnessMs,
  });
  return { state, stream };
}

async function waitFor(predicate, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) return false;
    await tick(10);
  }
  return true;
}

async function startKnownLeader(tab, fetchCalls) {
  tab.stream.startStream("41");
  assert.equal(await waitFor(() => fetchCalls.length === 1), true,
    "the intended leader acquires its lock before the follower starts");
}

describe("board stream leader election", () => {
  it("exactly one tab leads while the other follows", async () => {
    const callsA = [], callsB = [];
    const tabA = makeTab(callsA), tabB = makeTab(callsB);
    tabA.state.watermark = "41";
    tabB.state.watermark = "41";
    try {
      tabA.stream.startStream("41");
      tabB.stream.startStream("41");
      assert.equal(await waitFor(() => callsA.length + callsB.length === 1), true);
    } finally {
      tabA.stream.stopStream();
      tabB.stream.stopStream();
    }
  });

  it("follower is promoted when the leader stops", async () => {
    const callsA = [], callsB = [];
    const tabA = makeTab(callsA), tabB = makeTab(callsB);
    tabA.state.watermark = "41";
    tabB.state.watermark = "41";
    const locks = navigator.locks, request = locks.request;
    let requests = 0;
    locks.request = function (...args) { requests++; return request.apply(this, args); };
    try {
      await startKnownLeader(tabA, callsA);
      tabB.stream.startStream("41");
      assert.equal(await waitFor(() => requests === 2), true, "the follower joins the queue");
      assert.equal(callsB.length, 0);
      tabA.stream.stopStream();
      assert.equal(await waitFor(() => callsB.length === 1), true);
    } finally {
      tabA.stream.stopStream();
      tabB.stream.stopStream();
      locks.request = request;
    }
  });

  it("tab opens its own stream without sharing facilities", async () => {
    const navigator_ = globalThis.navigator;
    globalThis.navigator = {};
    let tab;
    try {
      const calls = [];
      tab = makeTab(calls);
      tab.state.watermark = "41";
      tab.stream.startStream("41");
      await tick(20);
      assert.equal(calls.length, 1);
    } finally {
      tab?.stream.stopStream();
      globalThis.navigator = navigator_;
    }
  });

  it("a hidden tab never leads", async () => {
    const requests = [];
    const locks = globalThis.navigator.locks;
    const originalRequest = locks.request;
    locks.request = function (...args) {
      requests.push(args);
      return originalRequest.apply(this, args);
    };
    const calls = [];
    const tab = makeTab(calls);
    tab.state.watermark = "41";
    document.visibilityState = "hidden";
    try {
      tab.stream.startStream("41");
      await tick(50);
      assert.equal(calls.length, 0, "a hidden tab never fetches");
      assert.equal(requests.length, 0, "a hidden tab never enters the election");
      document.visibilityState = "visible";
      document.dispatchEvent({ type: "visibilitychange" });
      assert.equal(
        await waitFor(() => calls.length === 1), true, "a shown tab rejoins the election",
      );
    } finally {
      locks.request = originalRequest;
      tab.stream.stopStream();
    }
  });

  it("a queued follower takes over when a stalled leader closes", async () => {
    const callsA = [], callsB = [];
    // The leader's heartbeat interval outlives the test: one setup beat, then
    // silence, so the follower's freshness window lapses before the close.
    const stalled = makeTab(callsA, { heartbeatMs: 10000, freshnessMs: 10000 });
    const follower = makeTab(callsB, { heartbeatMs: 20, freshnessMs: 60 });
    stalled.state.watermark = "41";
    follower.state.watermark = "41";
    try {
      await startKnownLeader(stalled, callsA);
      follower.stream.startStream("41");
      assert.equal(
        await waitFor(() => callsA.length === 1 && callsB.length === 0),
        true, "the stalled tab leads first",
      );
      await tick(120);
      stalled.stream.stopStream();
      assert.equal(
        await waitFor(() => callsB.length === 1), true, "the queued follower takes over",
      );
      await tick(80);
      assert.equal(callsA.length, 1, "the closed leader stays quiet");
      assert.equal(callsB.length, 1, "exactly one leader fetches");
    } finally {
      stalled.stream.stopStream();
      follower.stream.stopStream();
    }
  });

  it("rejected_steal_keeps_the_queue_position", async () => {
    const callsA = [], callsB = [];
    // Same stalled-leader setup as the takeover test, but the shim refuses
    // every steal: the follower must fall back on its queued lock request.
    const stalled = makeTab(callsA, { heartbeatMs: 10000, freshnessMs: 10000 });
    const follower = makeTab(callsB, { heartbeatMs: 20, freshnessMs: 60 });
    stalled.state.watermark = "41";
    follower.state.watermark = "41";
    rejectSteals();
    try {
      await startKnownLeader(stalled, callsA);
      follower.stream.startStream("41");
      assert.equal(
        await waitFor(() => callsA.length === 1 && callsB.length === 0),
        true, "the stalled tab leads first",
      );
      assert.equal(
        await waitFor(() => stealRejections() > 0), true, "the follower's steal is rejected",
      );
      stalled.stream.stopStream();
      assert.equal(
        await waitFor(() => callsB.length === 1), true,
        "the queued follower keeps its position and takes over",
      );
    } finally {
      allowSteals();
      stalled.stream.stopStream();
      follower.stream.stopStream();
    }
  });

  it("rejected_steal_retries", async () => {
    const leaderCalls = [], followerCalls = [];
    const leader = makeTab(leaderCalls, { heartbeatMs: 10000, freshnessMs: 10000 });
    const follower = makeTab(followerCalls, { heartbeatMs: 20, freshnessMs: 60 });
    leader.state.watermark = "41"; follower.state.watermark = "41";
    rejectSteals();
    try {
      await startKnownLeader(leader, leaderCalls);
      follower.stream.startStream("41");
      assert.equal(await waitFor(() => leaderCalls.length === 1), true);
      assert.equal(await waitFor(() => stealRejections() >= 2), true,
        "each rejected steal re-arms the next freshness tick");
      assert.equal(followerCalls.length, 0, "rejected steals never start a reader");
      leader.stream.stopStream();
      assert.equal(await waitFor(() => followerCalls.length === 1), true,
        "the queued election survives repeated rejected steals");
    } finally {
      allowSteals();
      leader.stream.stopStream();
      follower.stream.stopStream();
    }
  });
});
