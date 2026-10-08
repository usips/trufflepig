import { afterEach, beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createBoardDom } from "../../board_dom.js";
import { installDomShim, resetDomShim } from "../support/dom_shim.mjs";

installDomShim();
const NOW = 1705332600000; // 2024-01-15T15:30:00Z
const originalNow = Date.now;
let dom;
beforeEach(() => {
  resetDomShim();
  Date.now = () => NOW;
  dom = createBoardDom({ clockOffsetMs: 90000 });
});
afterEach(() => { Date.now = originalNow; });

describe("board relative timestamps", () => {
  for (const builder of ["timeNode", "ageNode"]) {
    const name = builder === "timeNode" ? "relative_timestamps_include_exact_tooltips"
      : "claim ages render corrected relative time with a machine-readable date";
    it(name, () => {
      const timestamp = NOW / 1000 + 90 - 59;
      const item = dom[builder](timestamp);
      assert.equal(item.tagName, "TIME");
      assert.equal(item.textContent, "59s ago");
      assert.equal(item.dataset.timestamp, String(timestamp));
      assert.equal(item.dateTime, "2024-01-15T15:30:31.000Z");
      assert.ok(item.title.includes("2024"));
      assert.equal(item.attributes.get("aria-live"), undefined);
      assert.equal(item.attributes.get("role"), undefined);
    });
  }

  it("uses short readable units on both sides of the clock", () => {
    const now = NOW / 1000 + 90;
    for (const [seconds, text] of [
      [0, "now"], [59, "59s ago"], [60, "1m ago"], [3599, "59m ago"],
      [3600, "1h ago"], [86399, "23h ago"], [86400, "1d ago"],
      [-30, "in 30s"], [-60, "in 1m"], [-3600, "in 1h"], [-86400, "in 1d"],
    ]) assert.equal(dom.timeNode(now - seconds).textContent, text, `offset ${seconds}`);
    assert.equal(dom.timeNode(String(now - 60)).textContent, "1m ago");
  });

  it("does not turn missing, invalid, or out-of-range values into an epoch date", () => {
    for (const value of [
      null, undefined, "", " ", "bad", false, true, [], {}, NaN, Infinity, 1e15,
    ]) {
      const item = dom.timeNode(value);
      assert.equal(item.textContent, "Unknown time", String(value));
      assert.equal(item.title, "Unknown time");
      assert.ok(!item.dateTime);
      assert.equal(item.dataset.timestamp, undefined);
    }
  });

  it("allows epoch zero and uses the client clock when requested", () => {
    assert.equal(dom.timeNode(0).dateTime, "1970-01-01T00:00:00.000Z");
    assert.equal(dom.age(NOW / 1000 - 60, 0), "1m ago");
  });

  it("uses the client clock when a server offset is missing or invalid", () => {
    for (const clockOffsetMs of [undefined, null, NaN, Infinity]) {
      const item = createBoardDom({ clockOffsetMs }).timeNode(NOW / 1000 - 60);
      assert.equal(item.textContent, "1m ago");
    }
  });

  it("includes local date, seconds, offset, and timezone on hover", () => {
    function inTimezone(timezone, timestamp = NOW / 1000) {
      const domUrl = new URL("../../board_dom.js", import.meta.url).href;
      const shimUrl = new URL("../support/dom_shim.mjs", import.meta.url).href;
      const source = `
        import { createBoardDom } from ${JSON.stringify(domUrl)};
        import { installDomShim } from ${JSON.stringify(shimUrl)};
        installDomShim();
        const item = createBoardDom({ clockOffsetMs: 0 }).timeNode(${timestamp});
        console.log(JSON.stringify({ title: item.title, dateTime: item.dateTime }));`;
      return JSON.parse(execFileSync(process.execPath,
        ["--experimental-default-type=module", "--input-type=module", "-e", source], {
          env: { ...process.env, LANG: "en_US.UTF-8", LC_ALL: "en_US.UTF-8", TZ: timezone },
          encoding: "utf8",
        }));
    }
    for (const [timezone, clock, offset, zone] of [
      ["America/New_York", /10:30:00/, "GMT-05:00", /America\/New_York/],
      ["Asia/Kolkata", /09:00:00|21:00:00/, "GMT+05:30", /Asia\/(Kolkata|Calcutta)/],
      ["UTC", /03:30:00|15:30:00/, "GMT", /UTC/],
    ]) {
      const item = inTimezone(timezone);
      assert.match(item.title, /2024/);
      assert.match(item.title, /Jan(?:uary)?\s+15|15\s+Jan(?:uary)?/);
      assert.match(item.title, clock);
      assert.ok(item.title.includes(offset), item.title);
      assert.match(item.title, zone);
      assert.equal(item.dateTime, "2024-01-15T15:30:00.000Z");
    }
    const summer = inTimezone("America/New_York", Date.parse("2024-07-15T15:30:00Z") / 1000);
    assert.match(summer.title, /GMT-04:00/);
    const fractional = inTimezone("UTC", NOW / 1000 + 0.125);
    assert.match(fractional.title, /30:00\.125/);
    assert.equal(fractional.dateTime, "2024-01-15T15:30:00.125Z");
  });
});
