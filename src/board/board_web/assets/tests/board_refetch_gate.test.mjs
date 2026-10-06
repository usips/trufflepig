import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardStream } from "../stream/board_stream.js";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

const tick = (ms = 10) => new Promise(resolve => setTimeout(resolve, ms));
const CHANNEL = "trufflepig-board-stream";

function makeTab(route) {
  const state = {
    watermark: "41", stream: null, streamGeneration: 0, reconnect: null,
    streamFailures: 0, resyncing: false, route, overview: null,
  };
  let refreshes = 0;
  const stream = createBoardStream({
    state,
    privateFetch: () => new Promise(() => {}),
    setConnection: () => {},
    loadRoute: async () => {},
    scheduleRefresh: () => { refreshes++; },
    parseBoardJson: JSON.parse,
    addTickerEvent: () => {},
    addLiveEntry: () => {},
    noteSeen: () => {},
    onIngest: () => {},
    authToken: () => "",
  });
  return { state, stream, refreshes: () => refreshes };
}

async function followingPair(route) {
  const leader = makeTab({ view: "overview" });
  const follower = makeTab(route);
  leader.state.watermark = "41";
  leader.stream.startStream("41");
  follower.stream.startStream("41");
  await tick(50);
  return { leader, follower };
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
      await relayBoardFrame(follower, "51", { seq: "51", kind: "feedback", plan: null, subject: "E5" });
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
