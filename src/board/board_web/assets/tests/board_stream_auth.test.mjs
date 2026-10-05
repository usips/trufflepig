import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardStream } from "../board_stream.js";
import { installDomShim, resetDomShim } from "./dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));
// Independent oracle for the expired indicator: asserted literally, never imported.
const EXPIRED = "Authorization expired. Run `trufflepig board web`.";
const T1 = "a".repeat(64);
const T2 = "b".repeat(64);
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

function authFailure(status) {
  return { ok: false, status, headers: { get: () => null }, body: null };
}

function makeTab({ token, fetchImpl, heartbeatMs = 20, freshnessMs = 60 }) {
  const state = {
    watermark: "41", stream: null, streamGeneration: 0, reconnect: null,
    streamFailures: 0, resyncing: false,
  };
  const connections = [];
  const events = [];
  const expiredCalls = [];
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
    onAuthExpired: () => expiredCalls.push(Date.now()),
    heartbeatMs,
    freshnessMs,
  });
  return { state, stream, connections, events, expiredCalls };
}

async function waitFor(predicate, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) return false;
    await tick(10);
  }
  return true;
}

describe("board stream token rotation", () => {
  it("tabs on a rotated token expire without retrying and release the lock", async () => {
    const callsA = [], callsB = [];
    const tabA = makeTab({
      token: T1,
      fetchImpl: (...args) => { callsA.push(args); return Promise.resolve(authFailure(401)); },
    });
    const tabB = makeTab({
      token: T1,
      fetchImpl: (...args) => { callsB.push(args); return Promise.resolve(authFailure(403)); },
    });
    try {
      tabA.stream.startStream("41");
      tabB.stream.startStream("41");
      assert.equal(
        await waitFor(() => tabA.expiredCalls.length === 1 && tabB.expiredCalls.length === 1),
        true, "both tabs enter the expired state",
      );
      assert.deepEqual(tabA.connections.at(-1), [EXPIRED, "expired"]);
      assert.deepEqual(tabB.connections.at(-1), [EXPIRED, "expired"]);
      assert.equal(callsA.length, 1, "no retry while unauthorized");
      assert.equal(callsB.length, 1, "no retry while unauthorized");
      assert.equal(tabA.state.stream, null);
      assert.equal(tabA.state.reconnect, null);
      assert.equal(tabB.state.stream, null);
      assert.equal(tabB.state.reconnect, null);
      // The lock is released: a probe on the same generation leads immediately.
      const callsC = [];
      const probe = makeTab({
        token: T1,
        fetchImpl: (...args) => { callsC.push(args); return new Promise(() => {}); },
      });
      try {
        probe.stream.startStream("41");
        assert.equal(await waitFor(() => callsC.length === 1), true, "the expired tabs released the lock");
      } finally {
        probe.stream.stopStream();
      }
    } finally {
      tabA.stream.stopStream();
      tabB.stream.stopStream();
    }
  });

  it("tabs on different tokens elect independent leaders", async () => {
    const callsA = [], callsB = [];
    const tabA = makeTab({
      token: T1, fetchImpl: (...args) => { callsA.push(args); return new Promise(() => {}); },
    });
    const tabB = makeTab({
      token: T2, fetchImpl: (...args) => { callsB.push(args); return new Promise(() => {}); },
    });
    try {
      tabA.stream.startStream("41");
      tabB.stream.startStream("41");
      assert.equal(
        await waitFor(() => callsA.length === 1 && callsB.length === 1),
        true, "each generation leads its own lock",
      );
    } finally {
      tabA.stream.stopStream();
      tabB.stream.stopStream();
    }
  });

  it("fresh-token tabs share a leader and relay while a rotated tab stays expired", async () => {
    const staleCalls = [];
    const stale = makeTab({
      token: T1,
      fetchImpl: (...args) => { staleCalls.push(args); return Promise.resolve(authFailure(401)); },
    });
    try {
      stale.stream.startStream("41");
      assert.equal(await waitFor(() => stale.expiredCalls.length === 1), true, "stale tab expires first");
      const freshCalls = [], fresh2Calls = [];
      // Whoever wins the election serves one frame, but only after the test
      // releases it: both tabs have their channels open by then, so the relay
      // cannot slip through the async setup gap. A second fetch hangs.
      let releaseFrame = null;
      const frameGate = new Promise(resolve => { releaseFrame = resolve; });
      const gatedSse = () => {
        let sent = false;
        return {
          ok: true,
          status: 200,
          headers: { get: name => (name === "Content-Type" ? "text/event-stream" : null) },
          body: {
            getReader: () => ({
              read: async () => {
                // Like a real SSE stream the reader stays open after the frame,
                // so the leader keeps heartbeating live for the whole test.
                if (sent) return new Promise(() => {});
                sent = true;
                await frameGate;
                return { done: false, value: new TextEncoder().encode(FRAME) };
              },
              cancel: async () => {},
            }),
          },
        };
      };
      let served = false;
      const serveOnce = calls => (...args) => {
        calls.push(args);
        return served ? new Promise(() => {}) : (served = true, Promise.resolve(gatedSse()));
      };
      const fresh = makeTab({ token: T2, fetchImpl: serveOnce(freshCalls) });
      const fresh2 = makeTab({ token: T2, fetchImpl: serveOnce(fresh2Calls) });
      try {
        fresh.stream.startStream("41");
        fresh2.stream.startStream("41");
        assert.equal(
          await waitFor(() => fresh.connections.length >= 1 && fresh2.connections.length >= 1
            && freshCalls.length + fresh2Calls.length === 1),
          true, "both tabs set up and one leads",
        );
        releaseFrame();
        assert.equal(
          await waitFor(() => fresh.events.length + fresh2.events.length >= 2),
          true, "leader relays its frame to the follower",
        );
        assert.equal(freshCalls.length + fresh2Calls.length, 1, "exactly one fresh leader fetches");
        const follower = freshCalls.length === 1 ? fresh2 : fresh;
        assert.equal(follower.events.length, 1);
        assert.deepEqual(follower.events[0], { seq: "42", subject: "E1" });
        assert.deepEqual(follower.connections[0], ["Connecting…", ""], "followers never show eager Live");
        assert.equal(
          await waitFor(() => follower.connections.some(([label]) => label === "Live")),
          true, "follower shows Live after a heartbeat",
        );
        assert.equal(staleCalls.length, 1, "stale tab never retries");
        assert.deepEqual(stale.connections.at(-1), [EXPIRED, "expired"]);
        assert.equal(stale.events.length, 0, "stale tab never applies fresh frames");
        assert.equal(stale.state.watermark, "41");
      } finally {
        fresh.stream.stopStream();
        fresh2.stream.stopStream();
      }
    } finally {
      stale.stream.stopStream();
    }
  });

  it("an expired tab adopts a fresh URL token and a live tab never does", async () => {
    // Dynamic import keeps this file loadable before the fix, so the red run
    // fails on this assertion instead of a static import error.
    const tokenModule = await import("../board_web_token.js");
    assert.equal(typeof tokenModule.resolveAdoptionToken, "function", "adoption helper exists");
    const { resolveAdoptionToken } = tokenModule;
    assert.equal(resolveAdoptionToken({ locationHash: `#token=${T2}`, expired: true }), T2);
    assert.equal(resolveAdoptionToken({ locationHash: `#token=${T2}`, expired: false }), null);
    assert.equal(resolveAdoptionToken({ locationHash: "", expired: true }), null);
    assert.equal(resolveAdoptionToken({ locationHash: `#token=${T2.toUpperCase()}`, expired: true }), null);
    assert.equal(resolveAdoptionToken({ locationHash: "#/P1", expired: true }), null);
  });
});
