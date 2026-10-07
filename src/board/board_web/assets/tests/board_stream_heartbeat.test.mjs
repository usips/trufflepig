import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createBoardStream } from "../stream/board_stream.js";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardTestClock, createBoardTestSignal } from "./support/board_test_clock.mjs";

installDomShim();
afterEach(() => resetDomShim());

const TF = "c".repeat(64);
const LEGACY_NAME = "trufflepig-board-stream";

// Node's hash is independent of the tab's crypto.subtle naming path.
function generationName(token) {
  const generation = createHash("sha256").update(token, "utf8").digest("hex").slice(0, 16);
  return `trufflepig-board-stream-${generation}`;
}

function makeTab({ clock, fetchImpl, heartbeatMs = 20, freshnessMs = 500 }) {
  const state = {
    watermark: "41", stream: null, streamGeneration: 0, reconnect: null,
    streamFailures: 0, resyncing: false,
  };
  const connections = [];
  const stream = createBoardStream({
    state, privateFetch: fetchImpl,
    setConnection: (label, mode) => connections.push([label, mode]),
    loadRoute: async () => {}, scheduleRefresh: () => {}, parseBoardJson: JSON.parse,
    addTickerEvent: () => {}, noteSeen: () => {}, onIngest: () => {},
    authToken: () => TF, onAuthExpired: () => {}, heartbeatMs, freshnessMs,
    now: clock.now, setInterval: clock.setInterval, clearInterval: clock.clearInterval,
  });
  return { state, stream, connections };
}

// Observing a request proves async channel setup and its freshness timer are ready.
function observeLockRequests() {
  const locks = navigator.locks;
  const original = locks.request;
  const requests = [];
  const waiting = new Map();
  locks.request = function (name, options, callback) {
    const entry = { name, options: typeof options === "function" ? {} : options };
    const index = requests.push(entry) - 1;
    waiting.get(index)?.release(entry);
    return original.call(this, name, options, callback);
  };
  return {
    requests,
    at(index) {
      if (index < requests.length) return Promise.resolve(requests[index]);
      if (!waiting.has(index)) waiting.set(index, createBoardTestSignal());
      return waiting.get(index).ready;
    },
    restore: () => { locks.request = original; },
  };
}

async function holdSilently(name) {
  const acquired = createBoardTestSignal();
  const held = createBoardTestSignal();
  const requested = navigator.locks.request(name, () => {
    acquired.release();
    return held.ready;
  });
  requested.catch(() => {});
  await acquired.ready;
  return async () => { held.release(); await requested.catch(() => {}); };
}

describe("board stream heartbeat", () => {
  it("followers show Live only after a leader heartbeat", async () => {
    const clock = createBoardTestClock();
    const name = generationName(TF);
    const releaseLegacy = await holdSilently(LEGACY_NAME);
    const releaseGen = await holdSilently(name);
    const requests = observeLockRequests();
    const rogue = new BroadcastChannel(name);
    const fetchCalls = [];
    const follower = makeTab({
      clock, fetchImpl: (...args) => { fetchCalls.push(args); return new Promise(() => {}); },
    });
    try {
      follower.stream.startStream("41");
      await requests.at(0);
      assert.deepEqual(follower.connections[0], ["Connecting…", ""]);
      rogue.postMessage({ type: "status", state: "live", watermark: 42 });
      rogue.postMessage({ type: "status" });
      rogue.postMessage({ type: "status", state: "napping", watermark: null });
      await clock.advance(100);
      assert.ok(!follower.connections.some(([label]) => label === "Live"),
        "malformed status never shows Live");
      rogue.postMessage({ type: "status", state: "live", watermark: "41" });
      await clock.advance(0);
      assert.ok(follower.connections.some(([label]) => label === "Live"),
        "a live heartbeat shows Live");
      await clock.advance(500);
      assert.equal(fetchCalls.length, 0, "the heartbeat refreshes the follower's deadline");
      await clock.advance(20);
      assert.equal(fetchCalls.length, 1, "the refreshed deadline expires on the next fake tick");
    } finally {
      follower.stream.stopStream();
      requests.restore(); rogue.close();
      await releaseGen(); await releaseLegacy();
      assert.equal(clock.pending(), 0, "stop clears the injected freshness timer");
    }
  });

  it("followers steal the lock after missed heartbeats", async () => {
    const clock = createBoardTestClock();
    const releaseLegacy = await holdSilently(LEGACY_NAME);
    const releaseGen = await holdSilently(generationName(TF));
    const requests = observeLockRequests();
    const fetchCalls = [];
    const follower = makeTab({
      clock, fetchImpl: (...args) => { fetchCalls.push(args); return new Promise(() => {}); },
      heartbeatMs: 20, freshnessMs: 60,
    });
    try {
      follower.stream.startStream("41");
      await requests.at(0);
      await clock.advance(60);
      assert.equal(fetchCalls.length, 0, "the freshness boundary still follows");
      await clock.advance(20);
      assert.equal(fetchCalls.length, 1, "the next fake heartbeat tick steals the silent leader's lock");
      assert.ok(requests.requests.some(({ options }) => options.steal === true));
      assert.ok(follower.connections.some(([label, mode]) => label === "Reconnecting…" && mode === "error"));
    } finally {
      follower.stream.stopStream(); requests.restore();
      await releaseGen(); await releaseLegacy();
      assert.equal(clock.pending(), 0, "stop clears the injected heartbeat timer");
    }
  });

  it("a stolen leader stops fetching and rejoins as a follower", async () => {
    const clock = createBoardTestClock();
    const requests = observeLockRequests();
    const leaderStarted = createBoardTestSignal();
    const leaderCalls = [];
    const leader = makeTab({
      clock,
      fetchImpl: (...args) => { leaderCalls.push(args); leaderStarted.release(); return new Promise(() => {}); },
      heartbeatMs: 10000, freshnessMs: 10000,
    });
    const followerCalls = [];
    const follower = makeTab({
      clock, fetchImpl: (...args) => { followerCalls.push(args); return new Promise(() => {}); },
      heartbeatMs: 20, freshnessMs: 60,
    });
    try {
      leader.stream.startStream("41");
      await leaderStarted.ready;
      follower.stream.startStream("41");
      await requests.at(1);
      assert.equal(leaderCalls.length, 1, "the silent-heartbeat tab leads first");
      assert.equal(followerCalls.length, 0, "the second tab initially follows");
      await clock.advance(80);
      assert.equal(followerCalls.length, 1, "the fake clock drives the stalled leader's eviction");
      assert.equal(leaderCalls[0][1].signal.aborted, true, "the evicted leader stops fetching");
      const rejoin = await requests.at(3);
      assert.equal(rejoin.options.steal, undefined, "the evicted leader queues as a follower");
      assert.equal(rejoin.name, generationName(TF));
      assert.equal(leader.connections.filter(([label]) => label === "Connecting…").length, 2,
        "the evicted leader reports its new follower election");
      await clock.advance(100);
      assert.equal(leaderCalls.length, 1, "the rejoined follower never fetches again");
      assert.equal(followerCalls.length, 1, "the new leader fetches exactly once");
    } finally {
      leader.stream.stopStream(); follower.stream.stopStream(); requests.restore();
      assert.equal(clock.pending(), 0, "both tabs clear their injected timers");
    }
  });
});
