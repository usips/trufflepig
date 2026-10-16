import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { createBoardDom } from "../board_dom.js";
import { createBoardGrantPanel } from "../board_grants.js";
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

// Mounts a fresh grant panel the way a render pass does, into a new page root.
function renderGrants(state, dom, operations) {
  const { grantPanel } = createBoardGrantPanel({
    state, dom, submitMutation: (form, op) => operations.push(op), scheduleRefresh() {}, notice() {},
  });
  const root = document.createElement("div");
  root.append(grantPanel({ plan: { id: "P1" }, grants: [], can_manage_grants: true }));
  return root;
}

function roleBoxes(root) {
  return root.querySelectorAll("input").filter(input => input.name === "roles");
}

function submitGrant(root) {
  const form = root.querySelector(".grant-form");
  form.querySelectorAll("input").find(input => input.name === "grantee").value = "u@h/codex/s";
  for (const listener of form.listeners.get("submit") || []) listener({ preventDefault() {} });
}

describe("board grant form drafts", () => {
  it("submits the role ticked after a live refresh", () => {
    const state = liveState();
    const dom = createBoardDom(state);
    const operations = [];
    const first = renderGrants(state, dom, operations);
    dom.captureForms(first);
    const second = renderGrants(state, dom, operations);
    dom.restoreForms(second);
    assert.deepEqual(roleBoxes(second).map(box => box.value), ["triage", "release", "link", "assign", "decide"]);
    roleBoxes(second)[0].checked = true;
    submitGrant(second);
    assert.deepEqual(operations[0].roles, ["triage"]);
  });

  it("keeps a ticked role checked through a live refresh", () => {
    const state = liveState();
    const dom = createBoardDom(state);
    const operations = [];
    const first = renderGrants(state, dom, operations);
    roleBoxes(first)[0].checked = true;
    roleBoxes(first)[3].checked = true;
    dom.captureForms(first);
    const second = renderGrants(state, dom, operations);
    dom.restoreForms(second);
    assert.deepEqual(roleBoxes(second).map(box => box.checked), [true, false, false, true, false]);
    submitGrant(second);
    assert.deepEqual(operations[0].roles, ["triage", "assign"]);
  });

  it("drops a role unticked before a live refresh", () => {
    const state = liveState();
    const dom = createBoardDom(state);
    const operations = [];
    const first = renderGrants(state, dom, operations);
    roleBoxes(first)[0].checked = true;
    roleBoxes(first)[1].checked = true;
    dom.captureForms(first);
    const second = renderGrants(state, dom, operations);
    dom.restoreForms(second);
    roleBoxes(second)[0].checked = false;
    dom.captureForms(second);
    const third = renderGrants(state, dom, operations);
    dom.restoreForms(third);
    submitGrant(third);
    assert.deepEqual(operations[0].roles, ["release"]);
  });

  it("syncs checked roles into another mounted copy of the form", () => {
    const state = liveState();
    const dom = createBoardDom(state);
    const operations = [];
    const source = renderGrants(state, dom, operations);
    const copy = renderGrants(state, dom, operations);
    roleBoxes(source)[2].checked = true;
    dom.syncForm(source.querySelector(".grant-form"), copy);
    assert.deepEqual(roleBoxes(copy).map(box => box.checked), [false, false, true, false, false]);
    assert.deepEqual(roleBoxes(copy).map(box => box.value), ["triage", "release", "link", "assign", "decide"]);
  });

  it("skips a snapshot when the checked roles are unchanged", () => {
    const state = liveState();
    const stores = createDraftStores();
    let sets = 0;
    state.formDrafts = {
      get: key => stores.formDrafts.get(key),
      set: (key, value) => { sets++; return stores.formDrafts.set(key, value); },
    };
    const dom = createBoardDom(state);
    const operations = [];
    const root = renderGrants(state, dom, operations);
    dom.captureForms(root);
    assert.equal(sets, 1);
    dom.captureForms(root);
    assert.equal(sets, 1);
    roleBoxes(root)[0].checked = true;
    dom.captureForms(root);
    assert.equal(sets, 2);
    assert.deepEqual(stores.formDrafts.get("grant:P1").roles, ["triage"]);
  });
});
