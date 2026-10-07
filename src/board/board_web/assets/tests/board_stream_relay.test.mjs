import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createBoardStream } from "../stream/board_stream.js";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));
const FRAME = 'event: board\nid: 42\ndata: {"seq":"42","subject":"E1"}\n\n';
const TOKEN = "stream-test-token";
const GENERATION = createHash("sha256").update(TOKEN).digest("hex").slice(0, 16);
const CHANNEL = `trufflepig-board-stream-${GENERATION}`;

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
    authToken: () => TOKEN,
  });
  return { state, stream };
}

describe("board stream frame relay", () => {
  it("follower applies the leader's frames without opening a stream", async () => {
    const callsA = [], callsB = [];
    const eventsA = [], eventsB = [];
    let releaseFrame;
    const frameGate = new Promise(resolve => { releaseFrame = resolve; });
    const locks = navigator.locks, request = locks.request;
    let elections = 0;
    locks.request = function (...args) {
      const pending = request.apply(this, args);
      // Both tabs have opened their token-generation channels before queueing.
      if (++elections === 2) releaseFrame();
      return pending;
    };
    let served = false;
    const serveOnce = calls => (...args) => {
      calls.push(args);
      if (served) return new Promise(() => {});
      served = true;
      return frameGate.then(() => sseResponse([FRAME]));
    };
    const tabA = makeTab(serveOnce(callsA), eventsA);
    const tabB = makeTab(serveOnce(callsB), eventsB);
    try {
      tabA.state.watermark = "41";
      tabB.state.watermark = "41";
      tabA.stream.startStream("41");
      tabB.stream.startStream("41");
      const deadline = Date.now() + 2000;
      while (eventsA.length + eventsB.length < 2 && Date.now() < deadline) await tick(10);
      assert.equal(callsA.length + callsB.length, 1, "exactly one leader opens a reader");
      const followerEvents = callsA.length === 1 ? eventsB : eventsA;
      assert.equal(followerEvents.length, 1);
      assert.deepEqual(followerEvents[0], { seq: "42", subject: "E1" });
      assert.equal(tabA.state.watermark, "42");
      assert.equal(tabB.state.watermark, "42");
    } finally {
      tabA.stream.stopStream();
      tabB.stream.stopStream();
      locks.request = request;
    }
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
    const rogue = new BroadcastChannel(CHANNEL);
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
