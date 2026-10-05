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
});
