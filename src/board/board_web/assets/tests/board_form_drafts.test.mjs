import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createDraftStores } from "../state/board_lru.js";

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

describe("board draft stores", () => {
  it("evicts the oldest key past 64 entries", () => {
    const { formDrafts } = createDraftStores();
    for (let index = 0; index < 65; index++) formDrafts.set(`key-${index}`, { index });
    assert.equal(formDrafts.has("key-0"), false);
    for (let index = 1; index < 65; index++) assert.equal(formDrafts.has(`key-${index}`), true);
  });

  it("shares one capacity across all three stores", () => {
    const { formDrafts, formStatuses, drafts } = createDraftStores();
    for (let index = 0; index < 32; index++) formDrafts.set(`form-${index}`, index);
    for (let index = 0; index < 33; index++) drafts.set(`draft-${index}`, index);
    assert.equal(formDrafts.has("form-0"), false);
    assert.equal(formDrafts.has("form-1"), true);
    assert.equal(drafts.has("draft-32"), true);
    assert.equal(formStatuses.has("missing"), false);
  });

  it("counts reads as use", () => {
    const { formDrafts } = createDraftStores();
    for (let index = 0; index < 64; index++) formDrafts.set(`key-${index}`, index);
    assert.equal(formDrafts.get("key-0"), 0);
    formDrafts.set("key-64", 64);
    assert.equal(formDrafts.has("key-0"), true);
    assert.equal(formDrafts.has("key-1"), false);
  });

  it("keeps Map semantics for delete and missing keys", () => {
    const { drafts } = createDraftStores();
    assert.equal(drafts.get("missing"), undefined);
    drafts.set("key", { body: "hello" });
    assert.equal(drafts.delete("key"), true);
    assert.equal(drafts.delete("key"), false);
  });

  it("pins editor drafts outside the 64-key LRU", () => {
    const { formDrafts } = createDraftStores();
    formDrafts.set("editor:edit:P1@3", { body: "unsaved plan text" });
    // Two Tasks tabs snapshot more than a full LRU of filter and move forms.
    for (let tab = 0; tab < 2; tab++) {
      for (let index = 0; index < 40; index++) formDrafts.set(`tab-${tab}:form-${index}`, index);
    }
    assert.deepEqual(formDrafts.get("editor:edit:P1@3"), { body: "unsaved plan text" });
    assert.equal(formDrafts.has("tab-0:form-0"), false);
  });

  it("keeps a production edit draft alive across two task tabs", () => {
    const { formDrafts, drafts } = createDraftStores();
    // The plan editor's draft key, built as pages/board_pages.js does.
    const mode = "edit";
    const base = "P1@3";
    const key = mode === "new" ? "new" : `${mode}:${base}`;
    drafts.set(key, { title: "", body: "unsaved plan text", summary: "", steward: "", repo_key: "", base });
    for (let tab = 0; tab < 2; tab++) {
      for (let index = 0; index < 40; index++) formDrafts.set(`tab-${tab}:form-${index}`, index);
    }
    assert.equal(drafts.get(key)?.body, "unsaved plan text");
  });

  it("caps pinned keys at 8 and demotes the oldest pin into the bound", () => {
    const { formDrafts } = createDraftStores();
    for (let index = 0; index < 12; index++) formDrafts.set(`editor:edit:P1@${index}`, { index });
    for (let index = 0; index < 64; index++) formDrafts.set(`form-${index}`, index);
    // The four oldest pins fell back into the LRU and evicted first.
    for (let index = 0; index < 4; index++) assert.equal(formDrafts.has(`editor:edit:P1@${index}`), false);
    for (let index = 4; index < 12; index++) assert.equal(formDrafts.has(`editor:edit:P1@${index}`), true);
  });

  it("lets pinned keys survive in every store without consuming capacity", () => {
    const { formDrafts, formStatuses, drafts } = createDraftStores();
    formDrafts.set("editor:edit:P1@3", { body: "text" });
    formStatuses.set("editor:edit:P1@3", { message: "Saving…", tone: "muted" });
    drafts.set("editor:new", { title: "fresh" });
    for (let index = 0; index < 64; index++) formDrafts.set(`key-${index}`, index);
    assert.equal(formDrafts.get("key-0"), 0);
    assert.deepEqual(formDrafts.get("editor:edit:P1@3"), { body: "text" });
    assert.deepEqual(formStatuses.get("editor:edit:P1@3"), { message: "Saving…", tone: "muted" });
    assert.deepEqual(drafts.get("editor:new"), { title: "fresh" });
    assert.equal(formDrafts.delete("editor:edit:P1@3"), true);
    assert.equal(formDrafts.has("editor:edit:P1@3"), false);
  });
});

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
    for (const listener of editor.listeners.get("input") || []) listener({ target: editor });
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
