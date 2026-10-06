// End-to-end bootstrap tests importing the real board_web_main.js: token
// adoption from the launch URL, URL stripping, stale-session replacement,
// and the seen-mark reset on a snapshot below the stored mark.
import { describe, it, after } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./dom_shim.mjs";

installDomShim();

const T1 = "a".repeat(64);
const T2 = "b".repeat(64);
// Independent oracles, asserted literally and never imported.
const TOKEN_KEY = "trufflepig-board-token";
const SEEN_KEY = "trufflepig-board-seen:board-test";
const MISMATCH = "This link offered a different board token; the tab kept its existing session.";

const realFetch = globalThis.fetch;
const realSetInterval = globalThis.setInterval;
globalThis.setInterval = () => 0;
after(() => {
  // Every imported module instance registered pagehide on the window live at
  // its import; fire them all so reconnect timers and election state stop
  // before the real fetch returns (a relative URL would throw into the
  // reconnect path and keep Node ≤22's event loop alive forever).
  for (const listeners of windows) {
    for (const listener of listeners.get("pagehide") || []) listener({});
  }
  globalThis.fetch = realFetch;
  globalThis.setInterval = realSetInterval;
});

let fakeUrl = new URL("https://board.test/");
let fetchCalls = [];
let fetchHandler = null;
let windowListeners = new Map();
const windows = [];
let imports = 0;

function boardReply(op, data, seq = "40") {
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

function authFailure() {
  return {
    ok: false, status: 401,
    text: async () => JSON.stringify({ error: { code: "auth", message: "denied" } }),
  };
}

function seedShell() {
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

function setup({ hash = "", stored = "", seen = null, handler }) {
  resetDomShim();
  seedShell();
  fakeUrl = new URL(`https://board.test/${hash}`);
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
  windowListeners = new Map();
  windows.push(windowListeners);
  globalThis.window = {
    addEventListener: (type, listener) => {
      if (!windowListeners.has(type)) windowListeners.set(type, []);
      windowListeners.get(type).push(listener);
    },
    scrollTo: () => {},
  };
  fetchCalls = [];
  fetchHandler = handler;
  globalThis.fetch = async (url, options = {}) => {
    const headers = new Headers(options.headers);
    fetchCalls.push({ url: String(url), token: headers.get("X-Board-Token") });
    return fetchHandler(String(url), options, headers);
  };
  if (stored) sessionStorage.setItem(TOKEN_KEY, stored);
  if (seen !== null) localStorage.setItem(SEEN_KEY, seen);
}

async function loadMain() {
  imports++;
  await import(`../board_web_main.js?main=${imports}`);
}

async function waitFor(predicate, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) return false;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  return true;
}

function boardOp(options) {
  return JSON.parse(options.body).op.op;
}

function boardSuccess(url, options) {
  if (url.includes("/api/v1/events")) return new Promise(() => {});
  const op = boardOp(options);
  return Promise.resolve(boardReply(op, boardData(op)));
}

describe("board_web_main bootstrap", () => {
  it("a never-authorized tab adopts the offered token", async () => {
    setup({ hash: `#token=${T2}`, handler: boardSuccess });
    await loadMain();
    assert.equal(sessionStorage.getItem(TOKEN_KEY), T2);
    assert.equal(await waitFor(() => fetchCalls.some(call => call.token === T2)), true);
    assert.equal(await waitFor(() => localStorage.getItem(SEEN_KEY) === "40"), true);
  });

  it("the offered token never lingers in the URL", async () => {
    setup({ hash: `#token=${T2}`, handler: boardSuccess });
    await loadMain();
    assert.equal(location.hash, "");
    assert.ok(!location.href.includes("token="));
  });

  it("a stale stored token gives way to the fresh offered one", async () => {
    setup({
      hash: `#token=${T2}`,
      stored: T1,
      handler: (url, options, headers) => {
        if (url.includes("/api/v1/events")) return new Promise(() => {});
        if (headers.get("X-Board-Token") !== T2) return Promise.resolve(authFailure());
        const op = boardOp(options);
        return Promise.resolve(boardReply(op, boardData(op)));
      },
    });
    await loadMain();
    assert.equal(await waitFor(() => sessionStorage.getItem(TOKEN_KEY) === T2), true);
    assert.ok(fetchCalls.some(call => call.token === T2 && call.url.endsWith("/api/v1/board")));
    assert.equal(await waitFor(() => localStorage.getItem(SEEN_KEY) === "40"), true);
  });

  it("a snapshot below the stored mark resets the seen mark", async () => {
    setup({ stored: T1, seen: "90", handler: boardSuccess });
    await loadMain();
    assert.equal(await waitFor(() => localStorage.getItem(SEEN_KEY) === "40"), true);
  });

  it("a live tab strips a navigated token without adopting it", async () => {
    setup({ stored: T1, handler: boardSuccess });
    await loadMain();
    assert.equal(await waitFor(() => fetchCalls.length >= 3), true, "the tab loads live first");
    fakeUrl.hash = `#token=${T2}`;
    for (const listener of windowListeners.get("hashchange") || []) listener({});
    assert.equal(location.hash, "");
    assert.ok(!location.href.includes("token="));
    assert.equal(sessionStorage.getItem(TOKEN_KEY), T1);
    assert.equal(document.getElementById("notice").textContent, MISMATCH);
  });
});
