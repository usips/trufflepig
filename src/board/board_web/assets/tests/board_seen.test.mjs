import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardTestClock, createBoardTestSignal } from "./support/board_test_clock.mjs";
import { seenKey, readSeenMark, writeSeenMark, resyncSeenMark } from "../board_seen.js";

installDomShim();
beforeEach(() => resetDomShim());

describe("board seen mark", () => {
  it("a lagging tab never lowers the shared mark", () => {
    const key = seenKey("board-one");
    writeSeenMark(localStorage, key, "90");
    writeSeenMark(localStorage, key, "50");
    assert.equal(localStorage.getItem(key), "90");
  });

  it("marks are independent per board id", () => {
    writeSeenMark(localStorage, seenKey("board-one"), "90");
    writeSeenMark(localStorage, seenKey("board-two"), "50");
    assert.equal(localStorage.getItem(seenKey("board-one")), "90");
    assert.equal(localStorage.getItem(seenKey("board-two")), "50");
  });

  it("resync lowers the mark only above the server watermark", () => {
    const key = seenKey("board-one");
    writeSeenMark(localStorage, key, "90");
    assert.equal(resyncSeenMark(localStorage, key, "40"), "40");
    assert.equal(localStorage.getItem(key), "40");
    localStorage.setItem(key, "30");
    assert.equal(resyncSeenMark(localStorage, key, "40"), "30");
    assert.equal(localStorage.getItem(key), "30");
  });

  it("rejects malformed sequences without touching storage", () => {
    const key = seenKey("board-one");
    assert.equal(writeSeenMark(localStorage, key, "not-a-seq"), null);
    assert.equal(readSeenMark(localStorage, key), null);
    localStorage.setItem(key, "007");
    assert.equal(readSeenMark(localStorage, key), null);
  });
});

// Full-bootstrap resync: a delivered frame raises the stored mark, then a
// stream resync forces a fresh baseline snapshot whose lower watermark lowers
// it — the lowering path for a database replaced underneath the tab.
const BOOTSTRAP_TOKEN = "a".repeat(64);
const MAIN_SEEN_KEY = "trufflepig-board-seen:board-test";

function seedBoardShell() {
  const meta = (name, content) => {
    const item = document.createElement("meta");
    item.setAttribute("name", name); item.setAttribute("content", content);
    item.content = content; document.body.append(item);
  };
  meta("board-api", "7"); meta("board-id", "board-test");
  const main = document.createElement("main");
  main.id = "main"; main.setAttribute("tabindex", "-1"); document.body.append(main);
  for (const [tag, id] of [
    ["span", "connection"], ["span", "connection-announce"], ["span", "freshness"],
    ["div", "notice"], ["button", "refresh"], ["span", "attention-count"],
  ]) {
    const item = document.createElement(tag);
    item.id = id; document.body.append(item);
  }
}

function boardReply(op, data, seq) {
  return {
    ok: true, status: 200,
    text: async () => JSON.stringify({
      api: 7, result: { result: op, data }, snapshot_seq: seq, warnings: [],
    }),
  };
}

function boardData(op) {
  if (op === "overview") return { plans: [], through: null, server_now: 1700000000 };
  if (op === "feed") return { events: [] };
  if (op === "attention") return { entries: [] };
  throw new Error(`unexpected board op ${op}`);
}

function sseResponse(chunks, clock, gapReady) {
  const parts = chunks.map(text => new TextEncoder().encode(text));
  let index = 0;
  return {
    ok: true, status: 200,
    headers: { get: name => (name === "Content-Type" ? "text/event-stream" : null) },
    body: {
      getReader: () => ({
        read: async () => {
          if (index >= parts.length) return { done: true };
          if (index > 0) {
            const gap = createBoardTestSignal();
            clock.setTimeout(gap.release, 100);
            gapReady.release();
            await gap.ready;
          }
          const value = parts[index];
          index += 1;
          return { done: false, value };
        },
        cancel: async () => {},
      }),
    },
  };
}

describe("board seen mark resync", () => {
  it("seen_mark_lowers_on_resync", async () => {
    let snapshotSeq = "40";
    let eventsCalls = 0;
    const windowListeners = new Map();
    const clock = createBoardTestClock();
    const raised = createBoardTestSignal();
    const lowered = createBoardTestSignal();
    const gapReady = createBoardTestSignal();
    const restarted = createBoardTestSignal();
    const globalNames = ["fetch", "location", "history", "window",
      "setTimeout", "clearTimeout", "setInterval", "clearInterval"];
    const originalGlobals = new Map(globalNames.map(name => [name,
      Object.getOwnPropertyDescriptor(globalThis, name)]));
    const originalNow = Date.now;
    const originalStorageMethod = Object.getOwnPropertyDescriptor(localStorage, "setItem");
    const setItem = localStorage.setItem;
    localStorage.setItem = function (key, value) {
      setItem.call(this, key, value);
      if (key === MAIN_SEEN_KEY && value === "90") raised.release();
      if (key === MAIN_SEEN_KEY && value === "45") lowered.release();
    };
    Date.now = clock.now;
    for (const name of ["setTimeout", "clearTimeout", "setInterval", "clearInterval"]) {
      globalThis[name] = clock[name];
    }
    let fakeUrl = new URL("https://board.test/");
    globalThis.location = {
      get href() { return fakeUrl.href; },
      get pathname() { return fakeUrl.pathname; },
      get search() { return fakeUrl.search; },
      get hash() { return fakeUrl.hash; },
    };
    globalThis.history = {
      state: null,
      replaceState: (_, __, url) => { fakeUrl = new URL(url, fakeUrl.href); },
      pushState: (_, __, url) => { fakeUrl = new URL(url, fakeUrl.href); },
    };
    globalThis.window = {
      addEventListener: (type, listener) => {
        if (!windowListeners.has(type)) windowListeners.set(type, []);
        windowListeners.get(type).push(listener);
      },
      scrollTo: () => {},
    };
    globalThis.fetch = async (url, options = {}) => {
      if (String(url).includes("/api/v1/events")) {
        eventsCalls += 1;
        if (eventsCalls > 1) { restarted.release(); return new Promise(() => {}); }
        // The forced snapshot the resync frame triggers reads the replaced
        // database's lower watermark.
        snapshotSeq = "45";
        return sseResponse([
          "event: board\nid: 90\ndata: {\"seq\": 90, \"kind\": \"note\"}\n\n",
          "event: resync\ndata: {}\n\n",
        ], clock, gapReady);
      }
      const op = JSON.parse(options.body).op.op;
      return boardReply(op, boardData(op), snapshotSeq);
    };
    try {
      sessionStorage.setItem("trufflepig-board-token", BOOTSTRAP_TOKEN);
      localStorage.setItem(MAIN_SEEN_KEY, "40");
      seedBoardShell();
      await import("../board_web_main.js?seen-resync=1");
      await Promise.all([raised.ready, gapReady.ready]);
      assert.equal(localStorage.getItem(MAIN_SEEN_KEY), "90", "a delivered frame raises the mark");
      await clock.advance(100);
      await Promise.all([lowered.ready, restarted.ready]);
      assert.equal(localStorage.getItem(MAIN_SEEN_KEY), "45", "the resync's fresh baseline lowers the mark");
    } finally {
      for (const listener of windowListeners.get("pagehide") || []) listener({});
      clock.clear();
      Date.now = originalNow;
      if (originalStorageMethod) Object.defineProperty(localStorage, "setItem", originalStorageMethod);
      else delete localStorage.setItem;
      for (const [name, descriptor] of originalGlobals) {
        if (descriptor) Object.defineProperty(globalThis, name, descriptor);
        else delete globalThis[name];
      }
    }
  });
});
