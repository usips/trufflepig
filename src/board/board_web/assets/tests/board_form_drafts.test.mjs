import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createDraftStores } from "../board_lru.js";

installDomShim();
beforeEach(() => resetDomShim());

function liveState() {
  const stores = createDraftStores();
  return {
    clockOffsetMs: 0, formDrafts: stores.formDrafts, formStatuses: stores.formStatuses,
    pendingForms: new Set(),
  };
}

function namedForm(dom, key, entries) {
  const form = dom.el("form");
  for (const [name, value] of entries) form.append(dom.field(name, name, value).wrapper);
  return dom.retainForm(form, key);
}

function moveForm(dom, key) {
  const form = dom.el("form", "actions");
  form.append(dom.button("Move to doing", null, "compact"));
  return dom.retainForm(form, key);
}

function countingStore(store) {
  const counted = {
    sets: 0,
    get: key => store.get(key),
    set: (key, value) => { counted.sets++; return store.set(key, value); },
  };
  return counted;
}

describe("board form drafts", () => {
  it("skips snapshots of forms with no named inputs", () => {
    const state = liveState();
    const dom = createBoardDom(state);
    const root = document.createElement("div");
    root.append(moveForm(dom, "move:P1.1"));
    const readonly = namedForm(dom, "filters:entries:all", [["plan", "P1"]]);
    readonly.querySelector("input").readOnly = true;
    root.append(readonly);
    dom.captureForms(root);
    assert.equal(state.formDrafts.has("move:P1.1"), false);
    assert.equal(state.formDrafts.has("filters:entries:all"), false);
    assert.equal(state.formDrafts.size, 0);
  });

  it("skips snapshots with unchanged values", () => {
    const state = liveState();
    const counted = countingStore(state.formDrafts);
    state.formDrafts = counted;
    const dom = createBoardDom(state);
    const form = namedForm(dom, "filters:entries:all", [["plan", "P1"], ["kind", ""]]);
    dom.saveForm(form);
    assert.equal(counted.sets, 1);
    dom.saveForm(form);
    assert.equal(counted.sets, 1);
    form.querySelectorAll("input")[0].value = "P2";
    dom.saveForm(form);
    assert.equal(counted.sets, 2);
    assert.deepEqual(counted.get("filters:entries:all"), { plan: "P2", kind: "" });
  });

  it("keeps the editor draft across two Tasks tabs", () => {
    const state = liveState();
    const dom = createBoardDom(state);
    const editor = namedForm(dom, "editor:edit:P1@3",
      [["title", "Plan"], ["body", "unsaved plan text"], ["summary", "draft"]]);
    dom.saveForm(editor);
    for (let tab = 0; tab < 2; tab++) {
      const root = document.createElement("div");
      for (let index = 0; index < 40; index++) {
        root.append(namedForm(dom, `tab-${tab}:filters-${index}`, [["plan", `P${index}`]]));
      }
      root.append(moveForm(dom, `tab-${tab}:move:P9.9`));
      dom.captureForms(root);
    }
    assert.deepEqual(state.formDrafts.get("editor:edit:P1@3"),
      { title: "Plan", body: "unsaved plan text", summary: "draft" });
    // The bound still evicts unpinned snapshots around the pinned draft.
    assert.equal(state.formDrafts.size, 65);
    assert.equal(state.formDrafts.has("tab-0:filters-0"), false);
    assert.equal(state.formDrafts.has("tab-1:filters-39"), true);
  });
});
