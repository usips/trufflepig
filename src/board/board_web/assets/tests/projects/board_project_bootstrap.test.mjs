import { it, after, afterEach } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  setupProjectPage, loadProjectPage, waitFor, chooseProject, textOf,
  closeProjectPages, restoreProjectGlobals, responseFor, projectRecords,
} from "./board_project_bootstrap_support.mjs";

afterEach(closeProjectPages);
after(restoreProjectGlobals);

function scopedCalls(calls) {
  const scoped = ["overview", "attention", "claims", "feed"];
  return calls.filter(request => scoped.includes(request.op.op));
}
function laneIds() {
  return document.getElementById("main").querySelectorAll(".swimlane")
    .map(node => node.dataset.focusKey);
}

it("header_select_drives_all_overview_panels_and_scoped_wire", async () => {
  const html = await readFile(new URL("../../index.html", import.meta.url), "utf8");
  assert.match(html, /<select id="project"/);
  const { calls } = setupProjectPage();
  await loadProjectPage();
  assert.equal(await waitFor(() => laneIds().length === 3), true);
  const select = document.getElementById("project");
  assert.deepEqual(select.children.map(option => option.textContent),
    ["All", "One", "Two", "Stale", "Unscoped"]);
  chooseProject("w1");
  assert.equal(await waitFor(() => laneIds().join() === "swimlane:P1"), true);
  const main = document.getElementById("main");
  assert.equal(main.querySelectorAll(".working-card").length, 1);
  assert.equal(main.querySelectorAll(".attention-card").length, 1);
  assert.equal(main.querySelectorAll(".event-row").length, 1);
  assert.match(textOf(main), /P1 activity/);
  assert.doesNotMatch(textOf(main), /P2|P3/);
  assert.equal(document.getElementById("attention-count").textContent, 1);
  const selected = scopedCalls(calls).filter(request => request.project === "w1");
  assert.deepEqual(new Set(selected.map(request => request.op.op)),
    new Set(["overview", "attention", "feed"]));
  assert.ok(selected.every(request => request.api === 8 && request.op.scope === "all"));
  assert.match(location.hash, /project=w1/);
  assert.equal(sessionStorage.getItem("trufflepig-board-project:project-fixture"), "w1");
});

it("unavailable_projects_have_header_and_plan_warning_chips", async () => {
  setupProjectPage({ hash: "#/?project=w1" });
  await loadProjectPage();
  assert.equal(await waitFor(() => laneIds().length === 1), true);
  const warning = document.getElementById("project-warning");
  assert.equal(warning.hidden, false);
  assert.equal(warning.textContent, "Unavailable members");
  assert.equal(warning.title, "missing-B");
  const chip = document.getElementById("main").querySelector(".project-chip");
  assert.equal(chip.textContent, "One · unavailable");
  assert.equal(chip.dataset.kind, "stale");
  assert.match(chip.href, /project=w1/);
});

it("a_fully_unavailable_project_remains_selectable_with_an_empty_scope", async () => {
  const { calls } = setupProjectPage({ hash: "#/?project=stale" });
  await loadProjectPage();
  assert.equal(await waitFor(() => document.getElementById("main").querySelector(".empty")), true);
  assert.deepEqual(laneIds(), []);
  assert.equal(document.getElementById("project").value, "stale");
  assert.equal(document.getElementById("project-warning").hidden, false);
  assert.match(textOf(document.getElementById("main")), /No plans in this scope/);
  assert.ok(scopedCalls(calls).every(request => request.project === "stale"));
});

it("a_removed_project_warns_and_keeps_the_selector_available_for_recovery", async () => {
  setupProjectPage({ hash: "#/?project=removed" });
  await loadProjectPage();
  assert.equal(await waitFor(() => document.getElementById("project-warning")
    .hidden === false), true);
  assert.equal(document.getElementById("project-warning").textContent, "Project unavailable");
  assert.ok(document.getElementById("project").children.some(option => option.value === "w1"));
  assert.match(textOf(document.getElementById("main")), /unknown project removed/);
  chooseProject("w1");
  assert.equal(await waitFor(() => laneIds().join() === "swimlane:P1"), true);
  assert.doesNotMatch(textOf(document.getElementById("main")), /P2|P3/);
});

it("session_selection_restores_and_all_choice_clears_it", async () => {
  const { calls } = setupProjectPage({ storedProject: "w2" });
  await loadProjectPage();
  assert.equal(await waitFor(() => laneIds().join() === "swimlane:P2"), true);
  assert.equal(document.getElementById("project").value, "w2");
  assert.match(location.hash, /project=w2/);
  chooseProject("");
  assert.equal(await waitFor(() => laneIds().length === 3), true);
  assert.doesNotMatch(location.hash, /project=/);
  assert.equal(sessionStorage.getItem("trufflepig-board-project:project-fixture"), null);
  const reads = scopedCalls(calls).slice(-3);
  assert.ok(reads.every(request =>
    request.op.scope === "all" && !Object.hasOwn(request, "project")));
});

it("project_query_overrides_session_and_survives_bootstrap_reload", async () => {
  setupProjectPage({ hash: "#/?project=w1", storedProject: "w2" });
  await loadProjectPage();
  assert.equal(await waitFor(() => laneIds().join() === "swimlane:P1"), true);
  const hash = location.hash;
  closeProjectPages();
  setupProjectPage({ hash });
  await loadProjectPage();
  assert.equal(await waitFor(() => laneIds().join() === "swimlane:P1"), true);
  assert.equal(document.getElementById("project").value, "w1");
});

it("unscoped_choice_changes_panels_without_a_project_selector", async () => {
  const { calls } = setupProjectPage({ hash: "#/?project=w1" });
  await loadProjectPage();
  assert.equal(await waitFor(() => laneIds().join() === "swimlane:P1"), true);
  chooseProject("unscoped");
  assert.equal(await waitFor(() => laneIds().join() === "swimlane:P3"), true);
  assert.doesNotMatch(textOf(document.getElementById("main")), /P1|P2/);
  assert.ok(scopedCalls(calls).slice(-3).every(request =>
    request.op.scope === "unscoped" && !Object.hasOwn(request, "project")));
});

it("late_project_reply_cannot_replace_the_new_selection", async () => {
  let release;
  const held = new Promise(resolve => { release = resolve; });
  const { calls } = setupProjectPage({
    hash: "#/?project=w1", handler: request => request.project === "w1"
      && request.op.op === "overview" ? held : responseFor(request),
  });
  await loadProjectPage();
  assert.equal(await waitFor(() => calls.some(request => request.op.op === "overview")), true);
  chooseProject("w2");
  assert.equal(await waitFor(() => laneIds().join() === "swimlane:P2"), true);
  const first = calls.find(request => request.project === "w1" && request.op.op === "overview");
  release(responseFor(first));
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(laneIds(), ["swimlane:P2"]);
  assert.doesNotMatch(textOf(document.getElementById("main")), /P1|P3/);
});

it("claims_and_plan_header_follow_the_current_project", async () => {
  const { calls, listeners, setHash } = setupProjectPage({ hash: "#/claims?project=w1" });
  await loadProjectPage();
  assert.equal(await waitFor(() => document.getElementById("main")
    .querySelectorAll(".working-card").length === 1), true);
  assert.doesNotMatch(textOf(document.getElementById("main")), /P2|P3/);
  const claimRequest = calls.find(request => request.op.op === "claims");
  assert.equal(claimRequest.project, "w1");
  assert.equal(claimRequest.op.scope, "all");
  setHash("#/P1?project=w1");
  for (const listener of listeners.get("hashchange") || []) listener({});
  assert.equal(await waitFor(() => document.getElementById("main")
    .querySelector(".project-chip")?.textContent === "One · unavailable"), true);
  assert.ok(calls.some(request => request.op.op === "show" && request.op.target === "P1"));
  assert.ok(!calls.some(request => request.op.op === "repositories"));
  assert.ok(calls.filter(request => ["show", "repositories"].includes(request.op.op))
    .every(request => !Object.hasOwn(request, "project")));
});

it("plan_project_chips_use_durable_keys_without_checkout_paths", async () => {
  const { calls } = setupProjectPage({ hash: "#/P1?project=w1", handler: request => {
    if (request.op.op !== "repositories") return responseFor(request);
    return { ok: true, status: 200, text: async () => JSON.stringify({
      api: request.api, result: { result: "repositories", data: [] },
      snapshot_seq: "100", warnings: [],
    }) };
  } });
  await loadProjectPage();
  assert.equal(await waitFor(() => document.getElementById("main")
    .querySelector("h1")?.textContent === "P1 · P1 plan"), true);
  const main = document.getElementById("main");
  assert.deepEqual(main.querySelectorAll(".project-chip")
    .map(chip => chip.dataset.project), ["w1"]);
  assert.ok(!main.querySelectorAll("a").some(link => link.textContent === "Unscoped"));
  assert.ok(!calls.some(request => request.op.op === "repositories"));
});

it("stored_revision_chips_read_current_durable_plan_membership", async () => {
  const { calls } = setupProjectPage({ hash: "#/P1@1?project=w1", handler: request => {
    if (request.op.op !== "show" || request.op.target !== "P1@1") return responseFor(request);
    return { ok: true, status: 200, text: async () => JSON.stringify({
      api: request.api, result: { result: "revision", data: {
        id: "P1@1", source: "create", created_at: 1700000000,
        actor: { user: "josh", host: "test", harness: "codex", session: "fixture" },
      } }, snapshot_seq: "100", warnings: [],
    }) };
  } });
  await loadProjectPage();
  assert.equal(await waitFor(() => document.getElementById("main")
    .querySelector("h1")?.textContent === "Plan revision · P1@1"), true);
  assert.deepEqual(document.getElementById("main").querySelectorAll(".project-chip")
    .map(chip => chip.dataset.project), ["w1"]);
  assert.deepEqual(calls.filter(request => request.op.op === "show")
    .map(request => request.op.target), ["P1@1", "P1"]);
  assert.ok(!calls.some(request => request.op.op === "repositories"));
});

it("navigation_inherits_the_project_and_renders_only_its_attention", async () => {
  const { calls } = setupProjectPage({ hash: "#/?project=w1" });
  await loadProjectPage();
  assert.equal(await waitFor(() => laneIds().join() === "swimlane:P1"), true);
  const anchor = document.createElement("a");
  anchor.href = "/#/attention"; anchor.dataset.route = "attention"; document.body.append(anchor);
  document.dispatchEvent({ type: "click", target: anchor, button: 0, preventDefault: () => {} });
  assert.equal(await waitFor(() => document.getElementById("main")
    .querySelector(".entry-card")), true);
  assert.match(location.hash, /^#\/attention\?project=w1/);
  assert.match(textOf(document.getElementById("main")), /P1 question/);
  assert.doesNotMatch(textOf(document.getElementById("main")), /P2|P3/);
  assert.equal(scopedCalls(calls).at(-1).project, "w1");
});

it("plan_chips_include_every_matching_project_and_unscoped_membership", async () => {
  const shared = { id: "shared", name: "Shared", repo_keys: ["A"], unavailable: [], plan_count: 1 };
  setupProjectPage({ handler: request => request.op.op !== "projects" ? responseFor(request)
    : { ok: true, status: 200, text: async () => JSON.stringify({
      api: request.api, result: { result: "projects", data: [...projectRecords, shared] },
      snapshot_seq: "100", warnings: [],
    }) } });
  await loadProjectPage();
  assert.equal(await waitFor(() => laneIds().length === 3), true);
  const lanes = document.getElementById("main").querySelectorAll(".swimlane");
  assert.deepEqual(lanes[0].querySelectorAll(".project-chip").map(chip => chip.dataset.project),
    ["w1", "shared"]);
  assert.equal(lanes[1].querySelector(".project-chip").dataset.project, "w2");
  assert.ok(lanes[2].querySelectorAll("a").some(link =>
    link.textContent === "Unscoped" && link.href.includes("project=unscoped")));
});
