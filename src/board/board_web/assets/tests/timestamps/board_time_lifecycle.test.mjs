import { afterEach, beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { createBoardDom } from "../../board_dom.js";
import { installDomShim, resetDomShim } from "../support/dom_shim.mjs";
import { createBoardTestClock } from "../support/board_test_clock.mjs";

installDomShim();
const NOW = 1705332600000;
const replaced = ["fetch", "window", "location", "history", "setInterval", "clearInterval",
  "setTimeout", "clearTimeout"];
let originals, originalNow, clock, windows, intervals, reads, jump, imports = 0;

async function settle() { for (let turn = 0; turn < 80; turn++) await Promise.resolve(); }
function dispatch(type, event = {}) {
  for (const listener of windows.get(type) || []) listener(event);
}
function timerCount(delay) {
  return [...intervals.values()].filter(value => value === delay).length;
}
function seedShell() {
  for (const [name, content] of [["board-api", "8"], ["board-id", "time-lifecycle"]]) {
    const meta = document.createElement("meta");
    meta.setAttribute("name", name); meta.content = content; document.body.append(meta);
  }
  for (const [tag, id] of [
    ["main", "main"], ["span", "connection"], ["span", "connection-announce"],
    ["time", "freshness"], ["div", "notice"], ["button", "refresh"], ["span", "attention-count"],
  ]) {
    const item = document.createElement(tag); item.id = id; document.body.append(item);
  }
}
function data(op) {
  if (op === "projects") return [];
  if (op === "overview") return { plans: [], server_now: Math.floor(Date.now() / 1000) };
  if (op === "attention") return { entries: [] };
  if (op === "feed") return { events: [{
    seq: "40", kind: "note", subject: "E40", summary: "Timer marker", created_at: NOW / 1000 - 59,
    actor: { user: "josh", host: "board", harness: "codex", session: "time-lifecycle" },
  }] };
  throw new Error(`unexpected operation ${op}`);
}
beforeEach(() => {
  resetDomShim(); seedShell();
  originals = new Map(replaced.map(name => [name,
    Object.getOwnPropertyDescriptor(globalThis, name)]));
  originalNow = Date.now;
  clock = createBoardTestClock(NOW); windows = new Map(); intervals = new Map();
  reads = []; jump = 0;
  Date.now = () => clock.now() + jump;
  globalThis.setTimeout = clock.setTimeout; globalThis.clearTimeout = clock.clearTimeout;
  globalThis.setInterval = (callback, delay, ...args) => {
    const id = clock.setInterval(callback, delay, ...args); intervals.set(id, delay); return id;
  };
  globalThis.clearInterval = id => { intervals.delete(id); clock.clearInterval(id); };
  let url = new URL("https://board.test/");
  globalThis.location = {
    get href() { return url.href; }, get hash() { return url.hash; },
    get pathname() { return url.pathname; }, get search() { return url.search; },
  };
  globalThis.history = {
    state: null,
    replaceState: (_, __, value) => { url = new URL(value, url); },
    pushState: (_, __, value) => { url = new URL(value, url); },
  };
  globalThis.window = {
    addEventListener(type, listener) {
      (windows.get(type) || windows.set(type, []).get(type)).push(listener);
    },
    scrollTo() {},
  };
  globalThis.fetch = async (url, options) => {
    if (String(url).includes("/api/v1/events")) return new Promise((_, reject) => {
      options.signal.addEventListener("abort", () =>
        reject(new DOMException("Aborted", "AbortError")));
    });
    const op = JSON.parse(options.body).op.op; reads.push(op);
    return { ok: true, status: 200, text: async () => JSON.stringify({
      api: 8, result: { result: op, data: data(op) }, snapshot_seq: "40", warnings: [],
    }) };
  };
  sessionStorage.setItem("trufflepig-board-token", "a".repeat(64));
});
afterEach(async () => {
  dispatch("pagehide"); await settle(); clock.clear();
  Date.now = originalNow;
  for (const [name, descriptor] of originals) {
    if (descriptor) Object.defineProperty(globalThis, name, descriptor);
    else delete globalThis[name];
  }
});
async function load() {
  await import(`../../board_web_main.js?time-lifecycle=${++imports}`);
  await settle();
  assert.equal(document.querySelectorAll(".event-row").length, 1,
    "real bootstrap rendered the fixture");
}

describe("board timestamp refresh lifecycle", () => {
  it("updates timestamps and freshness through one local timer without reads", async () => {
    await load();
    const item = document.querySelector(".event-row").querySelector("time");
    const before = reads.length;
    assert.equal(item.textContent, "59s ago");
    await clock.advance(1000); await settle();
    assert.equal(item.textContent, "1m ago");
    assert.equal(reads.length, before, "local timestamp ticks perform no API read");
    assert.equal(timerCount(1000), 1);
    const freshness = document.getElementById("freshness");
    assert.equal(freshness.textContent, "Updated 1s ago");
    assert.equal(freshness.dateTime, "2024-01-15T15:30:00.000Z");
    assert.ok(freshness.title.includes("2024"));
  });

  it("does not rewrite a timestamp while its displayed unit is unchanged", async () => {
    await load();
    const item = createBoardDom({ clockOffsetMs: 0 }).timeNode(NOW / 1000 - 120);
    document.body.append(item);
    let text = item.textContent, writes = 0;
    Object.defineProperty(item, "textContent", {
      get: () => text, set: value => { text = value; writes++; }, configurable: true,
    });
    await clock.advance(1000);
    assert.equal(item.textContent, "2m ago");
    assert.equal(writes, 0, "unchanged text preserves selection and avoids DOM mutations");
    jump = 60000; await clock.advance(1000);
    assert.equal(item.textContent, "3m ago");
    assert.equal(writes, 1);
  });

  it("clears page timers and resumes immediately without duplicate intervals", async () => {
    await load();
    assert.equal(timerCount(1000), 1); assert.equal(timerCount(15000), 1);
    dispatch("pageshow", { persisted: false });
    assert.equal(timerCount(1000), 1); assert.equal(timerCount(15000), 1);
    dispatch("pagehide"); await settle();
    assert.equal(timerCount(1000), 0); assert.equal(timerCount(15000), 0);
    const before = reads.length;
    await clock.advance(16000); await settle();
    assert.equal(reads.length, before, "a suspended page has no refresh interval");
    const item = document.querySelector(".event-row").querySelector("time");
    jump = 60000;
    dispatch("pageshow", { persisted: true });
    assert.equal(item.textContent, "2m ago",
      "resume refreshes existing dates before snapshot completion");
    assert.equal(timerCount(1000), 1); assert.equal(timerCount(15000), 1);
    dispatch("pageshow", { persisted: false });
    assert.equal(timerCount(1000), 1); assert.equal(timerCount(15000), 1);
  });

  it("keeps idle refreshes and updates relative text immediately on foreground", async () => {
    await load();
    const before = reads.length;
    await clock.advance(15200); await settle();
    assert.ok(reads.length > before, "the original idle snapshot refresh remains active");
    document.visibilityState = "hidden";
    document.dispatchEvent({ type: "visibilitychange" }); await settle();
    const hiddenReads = reads.length;
    await clock.advance(15000); await settle();
    assert.equal(reads.length, hiddenReads, "hidden idle ticks do not refetch");
    const item = document.querySelector(".event-row").querySelector("time");
    jump = 60000; document.visibilityState = "visible";
    document.dispatchEvent({ type: "visibilitychange" });
    assert.equal(item.textContent, "2m ago", "foreground updates without awaiting a read");
  });
});
