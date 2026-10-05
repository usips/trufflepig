import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createBoardStream } from "../board_stream.js";
import { installDomShim, resetDomShim } from "./dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));
const TF = "c".repeat(64);

// Independent oracle for the per-generation channel: Node's hash, not the
// tab's crypto.subtle path, so a wrong digest fails loudly.
function generationName(token) {
  const generation = createHash("sha256").update(token, "utf8").digest("hex").slice(0, 16);
  return `trufflepig-board-stream-${generation}`;
}
const LEGACY_NAME = "trufflepig-board-stream";

function makeTab({ token, fetchImpl, heartbeatMs = 20, freshnessMs = 500 }) {
  const state = {
    watermark: "41", stream: null, streamGeneration: 0, reconnect: null,
    streamFailures: 0, resyncing: false,
  };
  const connections = [];
  const events = [];
  const stream = createBoardStream({
    state,
    privateFetch: fetchImpl,
    setConnection: (label, mode) => connections.push([label, mode]),
    loadRoute: async () => {},
    scheduleRefresh: () => {},
    parseBoardJson: JSON.parse,
    addTickerEvent: event => events.push(event),
    addLiveEntry: () => {},
    noteSeen: () => {},
    onIngest: () => {},
    authToken: () => token,
    onAuthExpired: () => {},
    heartbeatMs,
    freshnessMs,
  });
  return { state, stream, connections, events };
}

async function waitFor(predicate, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) return false;
    await tick(10);
  }
  return true;
}

// A silent lock holder with no heartbeat: raw Web Lock requests on both the
// legacy and generation names, so the follower stays a follower before and
// after namespacing lands.
async function holdSilently(name) {
  let release;
  const held = new Promise(resolve => { release = resolve; });
  const requested = navigator.locks.request(name, () => held);
  requested.catch(() => {});
  await tick(20);
  return async () => { release(); await requested.catch(() => {}); };
}

describe("board stream heartbeat", () => {
  it("followers show Live only after a leader heartbeat", async () => {
    const name = generationName(TF);
    const releaseLegacy = await holdSilently(LEGACY_NAME);
    const releaseGen = await holdSilently(name);
    const rogue = new BroadcastChannel(name);
    const fetchCalls = [];
    const follower = makeTab({
      token: TF, fetchImpl: (...args) => { fetchCalls.push(args); return new Promise(() => {}); },
    });
    try {
      follower.stream.startStream("41");
      assert.equal(
        await waitFor(() => follower.connections.length >= 1), true, "follower reports a state",
      );
      assert.deepEqual(follower.connections[0], ["Connecting…", ""], "no eager Live before any heartbeat");
      // Malformed status frames are dropped, never fatal and never Live.
      rogue.postMessage({ type: "status", state: "live", watermark: 42 });
      rogue.postMessage({ type: "status" });
      rogue.postMessage({ type: "status", state: "napping", watermark: null });
      await tick(100);
      assert.ok(
        !follower.connections.some(([label]) => label === "Live"),
        "malformed status never shows Live",
      );
      rogue.postMessage({ type: "status", state: "live", watermark: "41" });
      assert.equal(
        await waitFor(() => follower.connections.some(([label]) => label === "Live")),
        true, "a live heartbeat shows Live",
      );
      assert.equal(fetchCalls.length, 0, "follower never opens its own stream");
    } finally {
      follower.stream.stopStream();
      rogue.close();
      await releaseGen();
      await releaseLegacy();
    }
  });

  it("followers steal the lock after missed heartbeats", { todo: "W6.6: steal must drop the signal under strict locks" }, async () => {
    const name = generationName(TF);
    const seenOptions = [];
    const locks = globalThis.navigator.locks;
    const originalRequest = locks.request;
    locks.request = function (lockName, options, callback) {
      seenOptions.push(typeof options === "function" ? {} : { ...options, signal: undefined });
      return originalRequest.call(this, lockName, options, callback);
    };
    const releaseLegacy = await holdSilently(LEGACY_NAME);
    const releaseGen = await holdSilently(name);
    const fetchCalls = [];
    const follower = makeTab({
      token: TF, fetchImpl: (...args) => { fetchCalls.push(args); return new Promise(() => {}); },
      heartbeatMs: 20, freshnessMs: 60,
    });
    try {
      follower.stream.startStream("41");
      assert.equal(
        await waitFor(() => fetchCalls.length === 1), true, "silent leader is stolen from",
      );
      assert.ok(seenOptions.some(options => options?.steal === true), "takeover requests steal the lock");
      assert.ok(
        follower.connections.some(([label, mode]) => label === "Reconnecting…" && mode === "error"),
        "stale follower downgrades from Live-waiting to reconnecting",
      );
    } finally {
      follower.stream.stopStream();
      locks.request = originalRequest;
      await releaseGen();
      await releaseLegacy();
    }
  });
});
