import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";
import { actorSiteNodes, doneSiteNodes, markFixtures, markViews, MARK_VENDORS, MARK_GLYPHS }
  from "./support/agent_mark_sites.mjs";

installDomShim();
beforeEach(() => resetDomShim());

function visibleText(node) {
  if (node.tagName === "TITLE") return "";
  return node.textContent + node.children.map(visibleText).join("");
}

test("every_actor_site_renders_a_mark", async () => {
  const actual = {}, expected = {};
  for (const [site, node] of await actorSiteNodes()) {
    actual[site] = node.querySelectorAll(".agent-mark").map(mark => mark.dataset.vendor);
    expected[site] = site === "ticker" ? [...MARK_VENDORS].reverse() : MARK_VENDORS;
  }
  assert.deepEqual(actual, expected, "each actor site renders all nine vendor marks");
  for (const [site, node] of await actorSiteNodes()) {
    assert.deepEqual(node.querySelectorAll(".agent-mark").map(mark =>
      mark.querySelector("text").textContent),
    site === "ticker" ? [...MARK_GLYPHS].reverse() : MARK_GLYPHS, `${site} has the vendor glyphs`);
  }
  const markers = (await actorSiteNodes()).get("plan claim markers")
    .querySelectorAll('.badge[data-kind="claimed"]');
  for (const [index, marker] of markers.entries()) {
    const vendor = MARK_VENDORS[index];
    assert.equal(visibleText(marker), MARK_GLYPHS[index]
      + `fixture@board.test/${vendor}/session-${index} · ${vendor}-fixture · max`,
    "a claim marker replaces the status text with the actor label");
  }
});

test("mark_has_accessible_label", () => {
  const { views } = markViews();
  const record = markFixtures().records[1];
  const mark = views.entryCard(record).querySelector('[role="img"]');
  const label = "codex-fixture · codex · fixture@board.test/session-1";
  assert.ok(mark, "the production entry actor has an accessible mark");
  assert.equal(mark.attributes.get("aria-label"), label);
  assert.equal(mark.querySelector("title").textContent, label);
  assert.equal(mark.namespaceURI, "http://www.w3.org/2000/svg");
  assert.equal(mark.attributes.get("width"), "16");
  assert.equal(mark.attributes.get("height"), "16");
  assert.equal(mark.querySelector("rect").attributes.get("rx"), "3");
  assert.equal(mark.querySelector("rect").attributes.get("fill"), "var(--mark-codex)");
  const task = { id: "P1.99", title: "Unclaimed task", column: "todo" };
  const assigned = views.taskCard({ ...task, assignee: "fixture-user" }, [])
    .querySelector('[role="img"]');
  assert.ok(assigned, "a recipient label has a mark without a fabricated claim");
  assert.equal(assigned.dataset.vendor, "unknown", "a bare user is not a human harness");
  assert.equal(assigned.attributes.get("aria-label"), "Assigned to fixture-user");
  assert.equal(assigned.querySelector("text").textContent, "?");
  assert.equal(views.taskCard(task, []).querySelector('[role="img"]'), null,
    "an unassigned task has no fabricated actor");
});

test("done_assignees_keep_available_identity", () => {
  const task = { title: "Done task", column: "done", done_at: 1791417000 };
  const tasks = [
    { ...task, id: "P1.1", assignee: "fixture@board.test/codex/session-1" },
    { ...task, id: "P1.2", assignee: "codex" },
    { ...task, id: "P1.3" },
  ];
  for (const [site, node] of doneSiteNodes(tasks)) {
    const marks = node.querySelectorAll(".agent-mark");
    assert.equal(marks.length, 2, `${site} marks only the two available recipients`);
    assert.equal(marks[0].dataset.vendor, "codex", `${site} uses the complete actor harness`);
    assert.equal(marks[0].attributes.get("aria-label"),
      "unclaimed · codex · fixture@board.test/session-1");
    assert.equal(marks[1].dataset.vendor, "unknown", `${site} keeps a bare recipient ambiguous`);
    assert.equal(marks[1].attributes.get("aria-label"), "Assigned to codex");
    assert.equal(marks[1].querySelector("text").textContent, "?");
  }
});

test("no_image_requests_without_logo", async () => {
  let imagesCreated = 0;
  const createElement = document.createElement;
  document.createElement = tag => {
    if (tag.toLowerCase() === "img") imagesCreated++;
    return createElement(tag);
  };
  try {
    for (const [site, node] of await actorSiteNodes()) {
      assert.equal(node.querySelectorAll(".agent-mark").length, 9, `${site} has nine inline marks`);
      assert.equal(node.querySelectorAll("img,image").length, 0, `${site} makes no logo request`);
    }
    assert.equal(imagesCreated, 0);
  } finally {
    document.createElement = createElement;
  }
});
