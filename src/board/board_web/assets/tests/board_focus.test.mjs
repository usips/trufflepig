import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";

installDomShim();
beforeEach(() => resetDomShim());

function blankState() {
  return {
    clockOffsetMs: 0, formDrafts: new Map(), formStatuses: new Map(), pendingForms: new Set(),
  };
}

// Two cards linking the same plan: duplicate hrefs share one focus key.
function duplicatePage(dom) {
  const root = document.createElement("div");
  const first = dom.link("Plan P1", "plan", { ref: "P1" });
  const second = dom.link("Plan P1 again", "plan", { ref: "P1" });
  root.append(first, second);
  return { root, first, second };
}

describe("board focus restore", () => {
  it("returns focus to the second of two same-href links", () => {
    const dom = createBoardDom(blankState());
    const before = duplicatePage(dom);
    document.activeElement = before.second;
    const focus = dom.focusedControl(before.root);
    const after = duplicatePage(dom);
    document.activeElement = null;
    dom.restoreFocus(after.root, focus);
    assert.equal(document.activeElement, after.second);
  });

  it("falls back to the nearest keyed ancestor when the link is gone", { todo: "W6.13: restore targets need tabindex under strict focus" }, () => {
    const dom = createBoardDom(blankState());
    const root = document.createElement("div");
    const card = document.createElement("section");
    dom.focusKey(card, "task-card:P1.1");
    const gone = dom.link("Plan P1", "plan", { ref: "P1" });
    card.append(gone);
    root.append(card);
    document.activeElement = gone;
    const focus = dom.focusedControl(root);
    const rebuilt = document.createElement("div");
    const survivor = document.createElement("section");
    dom.focusKey(survivor, "task-card:P1.1");
    rebuilt.append(survivor);
    document.activeElement = null;
    dom.restoreFocus(rebuilt, focus);
    assert.equal(document.activeElement, survivor);
  });

  it("falls back to the enclosing form when link and ancestor are gone", { todo: "W6.13: restore targets need tabindex under strict focus" }, () => {
    const dom = createBoardDom(blankState());
    const root = document.createElement("div");
    const form = document.createElement("form");
    form.dataset.draftKey = "filters:entries:all";
    const gone = dom.link("Older entries", "entries", { after: "7" });
    form.append(gone);
    root.append(form);
    document.activeElement = gone;
    const focus = dom.focusedControl(root);
    const rebuilt = document.createElement("div");
    const survivor = document.createElement("form");
    survivor.dataset.draftKey = "filters:entries:all";
    rebuilt.append(survivor);
    document.activeElement = null;
    dom.restoreFocus(rebuilt, focus);
    assert.equal(document.activeElement, survivor);
  });
});
