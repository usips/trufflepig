import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
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
  meta("board-api", "5"); meta("board-id", "board-test");
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
      api: 5, result: { result: op, data }, snapshot_seq: seq, warnings: [],
    }),
  };
}

function boardData(op) {
  if (op === "overview") return { plans: [], through: null, server_now: 1700000000 };
  if (op === "feed") return { events: [] };
  if (op === "attention") return { entries: [] };
  throw new Error(`unexpected board op ${op}`);
}

function sseResponse(chunks, gapMs) {
  const parts = chunks.map(text => new TextEncoder().encode(text));
  let index = 0;
  return {
    ok: true, status: 200,
    headers: { get: name => (name === "Content-Type" ? "text/event-stream" : null) },
    body: {
      getReader: () => ({
        read: async () => {
          if (index >= parts.length) return { done: true };
          // A real gap between chunks lets the raised mark be observed before
          // the resync frame lands; both frames in one chunk apply atomically.
          if (index > 0) await new Promise(resolve => setTimeout(resolve, gapMs));
          const value = parts[index];
          index += 1;
          return { done: false, value };
        },
        cancel: async () => {},
      }),
    },
  };
}

async function waitFor(predicate, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) return false;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  return true;
}

describe("board seen mark resync", () => {
  it("seen_mark_lowers_on_resync", async () => {
    let snapshotSeq = "40";
    let eventsCalls = 0;
    const windowListeners = new Map();
    const realFetch = globalThis.fetch;
    const realSetInterval = globalThis.setInterval;
    globalThis.setInterval = () => 0;
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
        if (eventsCalls > 1) return new Promise(() => {});
        // The forced snapshot the resync frame triggers reads the replaced
        // database's lower watermark.
        snapshotSeq = "45";
        return sseResponse([
          "event: board\nid: 90\ndata: {\"seq\": 90, \"kind\": \"note\"}\n\n",
          "event: resync\ndata: {}\n\n",
        ], 100);
      }
      const op = JSON.parse(options.body).op.op;
      return boardReply(op, boardData(op), snapshotSeq);
    };
    try {
      sessionStorage.setItem("trufflepig-board-token", BOOTSTRAP_TOKEN);
      localStorage.setItem(MAIN_SEEN_KEY, "40");
      seedBoardShell();
      await import("../board_web_main.js?seen-resync=1");
      assert.equal(await waitFor(() => localStorage.getItem(MAIN_SEEN_KEY) === "90"), true,
        "a delivered frame raises the mark");
      assert.equal(await waitFor(() => localStorage.getItem(MAIN_SEEN_KEY) === "45"), true,
        "the resync's fresh baseline lowers the mark");
    } finally {
      for (const listener of windowListeners.get("pagehide") || []) listener({});
      globalThis.fetch = realFetch;
      globalThis.setInterval = realSetInterval;
    }
  });
});
