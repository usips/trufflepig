import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createBoardStream } from "../stream/board_stream.js";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));
const TOKEN = "stream-test-token";
const GENERATION = createHash("sha256").update(TOKEN).digest("hex").slice(0, 16);
const CHANNEL = `trufflepig-board-stream-${GENERATION}`;

function makeTab(route) {
  const state = {
    watermark: "41", stream: null, streamGeneration: 0, reconnect: null,
    streamFailures: 0, resyncing: false, route, overview: null,
  };
  let refreshes = 0, fetches = 0;
  const stream = createBoardStream({
    state,
    privateFetch: () => { fetches++; return new Promise(() => {}); },
    setConnection: () => {},
    loadRoute: async () => {},
    scheduleRefresh: () => { refreshes++; },
    parseBoardJson: JSON.parse,
    addTickerEvent: () => {},
    addLiveEntry: () => {},
    noteSeen: () => {},
    onIngest: () => {},
    authToken: () => TOKEN,
  });
  return { state, stream, refreshes: () => refreshes, fetches: () => fetches };
}

async function waitFor(predicate, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) return false;
    await tick(10);
  }
  return true;
}

async function followingPair(route) {
  const leader = makeTab({ view: "overview" });
  const follower = makeTab(route);
  const locks = navigator.locks, request = locks.request;
  let requests = 0;
  locks.request = function (...args) { requests++; return request.apply(this, args); };
  try {
    leader.stream.startStream("41");
    assert.equal(await waitFor(() => leader.fetches() === 1), true,
      "the overview tab leads before the route-specific follower starts");
    follower.stream.startStream("41");
    assert.equal(await waitFor(() => requests === 2), true,
      "the follower opens its channel and enters the lock queue");
    return { leader, follower };
  } catch (error) {
    leader.stream.stopStream();
    follower.stream.stopStream();
    throw error;
  } finally {
    locks.request = request;
  }
}

async function relayBoardFrame(follower, id, event) {
  const channel = new BroadcastChannel(CHANNEL);
  try {
    channel.postMessage({ event: "board", id, data: JSON.stringify(event) });
    await tick(20);
  } finally {
    channel.close();
  }
}

describe("board refetch gate", () => {
  it("feedback refetches only on feedback-kind events", async () => {
    const { leader, follower } = await followingPair({ view: "feedback", state: "open" });
    try {
      await relayBoardFrame(follower, "50", { seq: "50", kind: "note", plan: "P9", subject: "E1" });
      assert.equal(follower.refreshes(), 0);
      await relayBoardFrame(follower, "51", {
        seq: "51", kind: "feedback", plan: null, subject: "E5",
      });
      assert.equal(follower.refreshes(), 1);
    } finally {
      leader.stream.stopStream();
      follower.stream.stopStream();
    }
  });

  it("search refetches only on events matching its plan filter", async () => {
    const { leader, follower } = await followingPair({ view: "search", q: "hello", plan: "P1" });
    try {
      await relayBoardFrame(follower, "50", { seq: "50", kind: "note", plan: "P9", subject: "E1" });
      assert.equal(follower.refreshes(), 0);
      await relayBoardFrame(follower, "51", { seq: "51", kind: "note", plan: "P1", subject: "E2" });
      assert.equal(follower.refreshes(), 1);
    } finally {
      leader.stream.stopStream();
      follower.stream.stopStream();
    }
  });

  it("search without a plan filter still refetches on any event", async () => {
    const { leader, follower } = await followingPair({ view: "search", q: "hello", plan: "" });
    try {
      await relayBoardFrame(follower, "50", { seq: "50", kind: "note", plan: "P9", subject: "E1" });
      assert.equal(follower.refreshes(), 1);
    } finally {
      leader.stream.stopStream();
      follower.stream.stopStream();
    }
  });

  it("other routes still refetch on any event", async () => {
    const { leader, follower } = await followingPair({ view: "overview" });
    try {
      await relayBoardFrame(follower, "50", { seq: "50", kind: "note", plan: "P9", subject: "E1" });
      assert.equal(follower.refreshes(), 1);
    } finally {
      leader.stream.stopStream();
      follower.stream.stopStream();
    }
  });
});
