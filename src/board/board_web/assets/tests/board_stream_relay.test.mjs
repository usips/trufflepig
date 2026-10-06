import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardStream } from "../stream/board_stream.js";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));
const FRAME = 'event: board\nid: 42\ndata: {"seq":"42","subject":"E1"}\n\n';

function sseResponse(chunks) {
  const queue = chunks.map(text => ({ done: false, value: new TextEncoder().encode(text) }));
  queue.push({ done: true, value: undefined });
  return {
    ok: true,
    status: 200,
    headers: { get: name => (name === "Content-Type" ? "text/event-stream" : null) },
    body: {
      getReader: () => ({ read: async () => queue.shift(), cancel: async () => {} }),
    },
  };
}

function makeTab(privateFetch, events) {
  const state = {
    watermark: null, stream: null, streamGeneration: 0, reconnect: null,
    streamFailures: 0, resyncing: false,
  };
  const stream = createBoardStream({
    state,
    privateFetch,
    setConnection: () => {},
    loadRoute: async () => {},
    scheduleRefresh: () => {},
    parseBoardJson: JSON.parse,
    addTickerEvent: event => events.push(event),
    addLiveEntry: () => {},
    noteSeen: () => {},
    onIngest: () => {},
  });
  return { state, stream };
}

describe("board stream frame relay", () => {
  it("follower applies the leader's frames without opening a stream", async () => {
    const leaderCalls = [], followerCalls = [];
    const leaderEvents = [], followerEvents = [];
    let served = false;
    const leader = makeTab((...args) => {
      leaderCalls.push(args);
      return served ? new Promise(() => {}) : (served = true, Promise.resolve(sseResponse([FRAME])));
    }, leaderEvents);
    const follower = makeTab((...args) => {
      followerCalls.push(args);
      return new Promise(() => {});
    }, followerEvents);
    leader.state.watermark = "41";
    follower.state.watermark = "41";
    leader.stream.startStream("41");
    follower.stream.startStream("41");
    const deadline = Date.now() + 2000;
    while (followerEvents.length === 0 && Date.now() < deadline) await tick(10);
    assert.equal(followerEvents.length, 1);
    assert.deepEqual(followerEvents[0], { seq: "42", subject: "E1" });
    assert.equal(follower.state.watermark, "42");
    assert.equal(followerCalls.length, 0);
    assert.equal(leaderCalls.length, 1);
    assert.equal(leader.state.watermark, "42");
    leader.stream.stopStream();
    follower.stream.stopStream();
  });

  it("malformed relayed frames are dropped", async () => {
    const leader = makeTab(() => new Promise(() => {}), []);
    const followerEvents = [];
    const follower = makeTab(() => new Promise(() => {}), followerEvents);
    leader.state.watermark = "41";
    follower.state.watermark = "41";
    leader.stream.startStream("41");
    follower.stream.startStream("41");
    await tick(50);
    const rogue = new BroadcastChannel("trufflepig-board-stream");
    rogue.postMessage({ event: "board", id: "not-a-seq", data: "{}" });
    rogue.postMessage(null);
    rogue.postMessage({ event: "board", id: "43", data: "not json" });
    await tick(50);
    assert.equal(followerEvents.length, 0);
    assert.equal(follower.state.watermark, "41");
    rogue.close();
    leader.stream.stopStream();
    follower.stream.stopStream();
  });
});
