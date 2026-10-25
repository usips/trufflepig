import { describe, it, afterEach } from "node:test";
import assert from "node:assert/strict";
import { createBoardStream } from "../../stream/board_stream.js";
import { installDomShim, resetDomShim } from "../support/dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

function streamContext(overrides = {}) {
  const state = {
    watermark: "41", stream: null, streamGeneration: 0, reconnect: null,
    streamFailures: 0, resyncing: false,
  };
  return {
    state, privateFetch: () => new Promise(() => {}), setConnection: () => {},
    loadRoute: async () => {}, scheduleRefresh: () => {}, parseBoardJson: JSON.parse,
    addTickerEvent: () => {}, noteSeen: () => {}, onIngest: () => {},
    ...overrides,
  };
}

describe("board stream required authorization", () => {
  it("auth_token_callback_is_required", () => {
    for (const authToken of [undefined, null, "token"]) {
      assert.throws(() => createBoardStream(streamContext({ authToken })),
        { name: "TypeError", message: /authToken/ });
    }
  });

  it("missing_token_is_terminal_before_lock_or_fetch", async () => {
    const connections = [], requests = [], fetches = [];
    const locks = navigator.locks;
    const request = locks.request;
    locks.request = (...args) => { requests.push(args); return request.apply(locks, args); };
    const context = streamContext({
      authToken: () => "",
      privateFetch: (...args) => { fetches.push(args); return new Promise(() => {}); },
      setConnection: (...args) => connections.push(args),
    });
    const stream = createBoardStream(context);
    try {
      stream.startStream("41");
      assert.deepEqual(connections.at(-1), ["Authorization required", "error"]);
      assert.equal(context.state.stream, null);
      assert.equal(context.state.reconnect, null, "authorization refusal schedules no reconnect");
      assert.equal(context.state.streamFailures, 0);
      await Promise.resolve();
      assert.equal(requests.length, 0, "a tokenless tab never joins the lock election");
      assert.equal(fetches.length, 0, "a tokenless tab never sends a stream request");
    } finally {
      stream.stopStream();
      locks.request = request;
    }
  });

  it("token_removed_before_reconnect_stops_without_another_request", async () => {
    const originalNavigator = globalThis.navigator;
    const originalTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    const timers = new Map();
    let timerId = 0, token = "stream-test-token";
    const fetches = [], connections = [];
    globalThis.navigator = {};
    globalThis.setTimeout = (callback, delay) => {
      timers.set(++timerId, { callback, delay });
      return timerId;
    };
    globalThis.clearTimeout = id => timers.delete(id);
    const context = streamContext({
      authToken: () => token,
      privateFetch: (...args) => {
        fetches.push(args);
        return Promise.resolve({
          ok: false, status: 503, body: null, headers: { get: () => null },
        });
      },
      setConnection: (...args) => connections.push(args),
    });
    const stream = createBoardStream(context);
    try {
      stream.startStream("41");
      await Promise.resolve();
      assert.equal(timers.size, 1, "the authorized transient failure schedules one retry");
      const [id, { callback, delay }] = timers.entries().next().value;
      assert.equal(delay, 1000);
      token = "";
      timers.delete(id);
      callback();
      await Promise.resolve();
      assert.equal(fetches.length, 1, "missing authorization stops before a second request");
      assert.deepEqual(connections.at(-1), ["Authorization required", "error"]);
      assert.equal(context.state.stream, null);
      assert.equal(context.state.reconnect, null);
      assert.equal(timers.size, 0, "the refused retry cannot schedule another reconnect");
    } finally {
      stream.stopStream();
      globalThis.navigator = originalNavigator;
      globalThis.setTimeout = originalTimeout;
      globalThis.clearTimeout = originalClearTimeout;
    }
  });
});
