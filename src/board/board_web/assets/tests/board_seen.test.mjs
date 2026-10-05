import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./dom_shim.mjs";
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
