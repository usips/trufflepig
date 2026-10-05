import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createDraftStores } from "../board_lru.js";

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
