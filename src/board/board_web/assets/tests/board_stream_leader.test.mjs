import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardStream } from "../board_stream.js";
import { installDomShim, resetDomShim } from "./dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));

function makeTab(fetchCalls) {
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
  });
  return { state, stream };
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
});
