import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardStream } from "../stream/board_stream.js";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));
const CHANNEL = "trufflepig-board-stream";

function makeTab() {
  const state = {
    watermark: null, stream: null, streamGeneration: 0, reconnect: null,
    streamFailures: 0, resyncing: false,
  };
  const events = [];
  const routeCalls = [];
  let refreshes = 0;
  const stream = createBoardStream({
    state,
    privateFetch: () => new Promise(() => {}),
    setConnection: () => {},
    loadRoute: async (...args) => { routeCalls.push(args); },
    scheduleRefresh: () => { refreshes++; },
    parseBoardJson: JSON.parse,
    addTickerEvent: event => events.push(event),
    addLiveEntry: () => {},
    noteSeen: () => {},
    onIngest: () => {},
  });
  return { state, stream, events, routeCalls, refreshCount: () => refreshes };
}

function boardFrame(id) {
  const data = JSON.stringify({ seq: String(id), subject: "E1" });
  return { event: "board", id: String(id), data };
}

describe("board stream relay buffer", () => {
  it("joining follower applies frames relayed before its snapshot lands", async () => {
    const follower = makeTab();
    const rogue = new BroadcastChannel(CHANNEL);
    try {
      await tick(50); // Module setup opens the channel before any snapshot read.
      rogue.postMessage(boardFrame(101));
      rogue.postMessage(boardFrame(102));
      await tick(20);
      assert.equal(follower.events.length, 0, "nothing applies before the snapshot lands");
      // The snapshot lands with watermark 100; the tab starts following.
      follower.state.watermark = "100";
      follower.stream.startStream("100");
      await tick(50);
      assert.deepEqual(follower.events, [
        { seq: "101", subject: "E1" },
        { seq: "102", subject: "E1" },
      ]);
      assert.equal(follower.state.watermark, "102");
      assert.ok(follower.refreshCount() >= 2, "each applied frame schedules a refresh");
      assert.equal(follower.routeCalls.length, 0, "no resync when the buffer covers the gap");
      // Redelivery is harmless: replays at or below the watermark are dropped.
      rogue.postMessage(boardFrame(101));
      rogue.postMessage(boardFrame(102));
      await tick(20);
      assert.equal(follower.events.length, 2, "no frame is applied twice");
    } finally {
      rogue.close();
      follower.stream.stopStream();
    }
  });

  it("an overrun buffer drops its frames and forces a resync", async () => {
    const follower = makeTab();
    const rogue = new BroadcastChannel(CHANNEL);
    try {
      await tick(50);
      for (let id = 101; id <= 601; id++) rogue.postMessage(boardFrame(id));
      await tick(100);
      follower.state.watermark = "100";
      follower.stream.startStream("100");
      await tick(50);
      assert.deepEqual(follower.routeCalls, [[false, true]], "overflow takes the resync path");
      assert.equal(follower.events.length, 0, "dropped frames are never delivered");
      assert.equal(follower.state.resyncing, true);
      assert.equal(follower.state.watermark, null);
    } finally {
      rogue.close();
      follower.stream.stopStream();
    }
  });
});
