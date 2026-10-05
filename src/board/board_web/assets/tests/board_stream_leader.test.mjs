import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardStream } from "../board_stream.js";
import { installDomShim, resetDomShim } from "./dom_shim.mjs";

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

describe("board stream leader election", () => {
  it("exactly one tab leads while the other follows", async () => {
    const callsA = [], callsB = [];
    const tabA = makeTab(callsA), tabB = makeTab(callsB);
    tabA.state.watermark = "41";
    tabB.state.watermark = "41";
    tabA.stream.startStream("41");
    tabB.stream.startStream("41");
    await tick(50);
    assert.equal(callsA.length + callsB.length, 1);
    tabA.stream.stopStream();
    tabB.stream.stopStream();
  });

  it("follower is promoted when the leader stops", async () => {
    const callsA = [], callsB = [];
    const tabA = makeTab(callsA), tabB = makeTab(callsB);
    tabA.state.watermark = "41";
    tabB.state.watermark = "41";
    tabA.stream.startStream("41");
    tabB.stream.startStream("41");
    await tick(50);
    assert.equal(callsA.length, 1);
    assert.equal(callsB.length, 0);
    tabA.stream.stopStream();
    await tick(50);
    assert.equal(callsB.length, 1);
    tabB.stream.stopStream();
  });

  it("tab opens its own stream without sharing facilities", async () => {
    const navigator_ = globalThis.navigator;
    globalThis.navigator = {};
    try {
      const calls = [];
      const tab = makeTab(calls);
      tab.state.watermark = "41";
      tab.stream.startStream("41");
      await tick(20);
      assert.equal(calls.length, 1);
      tab.stream.stopStream();
    } finally {
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
      stalled.stream.startStream("41");
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
      follower.stream.stopStream();
    }
  });
});
