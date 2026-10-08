import { installDomShim, resetDomShim } from "../support/dom_shim.mjs";

installDomShim();
const replacedGlobals = ["fetch", "setInterval", "location", "history", "window"];
const originalGlobals = new Map(replacedGlobals.map(name =>
  [name, Object.getOwnPropertyDescriptor(globalThis, name)]));
const windows = [];
let imports = 0;

export const projectRecords = [
  { id: "w1", name: "One", repo_keys: ["A", "B"], unavailable: [
    { name: "missing-B", root: "/missing/B", available: false },
  ], plan_count: 1 },
  { id: "w2", name: "Two", repo_keys: ["C"], unavailable: [], plan_count: 1 },
  { id: "stale", name: "Stale", repo_keys: [], unavailable: [
    { name: "deleted", root: "/missing/deleted", available: false },
  ], plan_count: 0 },
];
const actor = { user: "josh", host: "test", harness: "codex", session: "fixture" };
const claim = id => ({ task: `${id}.1`, actor, model: "gpt-6.1-sol", effort: "max", vendor: "codex",
  scope: `${id} work`, stale: false, ended_at: null, last_active: 1700000000 });
const plans = [["P1", ["A"]], ["P2", ["C"]], ["P3", []]].map(([id, repoKeys]) => ({
  plan: { id, title: `${id} plan`, head_revision: 1, owner_user: "josh" }, repo_keys: repoKeys,
  tasks: [{ id: `${id}.1`, title: `${id} task`, column: "doing" }], claims: [claim(id)],
}));
const entry = item => ({
  id: `E${item.plan.id.slice(1)}`, plan: item.plan.id, actor, vendor: "codex",
  kind: "question", body: `${item.plan.id} question`, created_at: 1700000000 });
const event = item => ({ seq: String(97 + Number(item.plan.id.slice(1))), plan: item.plan.id,
  subject: item.plan.id, kind: "note", summary: `${item.plan.id} activity`, actor, vendor: "codex",
  created_at: 1700000000 });

function selectedPlans(request) {
  const project = projectRecords.find(item => item.id === request.project);
  return request.op.scope === "unscoped" ? plans.filter(item => !item.repo_keys.length)
    : project ? plans.filter(item => item.repo_keys.some(key => project.repo_keys.includes(key)))
      : plans;
}
function dataFor(request) {
  const rows = selectedPlans(request), op = request.op;
  if (op.op === "projects") return projectRecords;
  if (op.op === "overview") return { plans: rows, through: "100", server_now: 1700000000 };
  if (op.op === "attention") return { entries: rows.map(entry), through: "100" };
  if (op.op === "feed") return { events: rows.map(event), through: "100" };
  if (op.op === "claims") return { claims: rows.map(item => ({ claim: claim(item.plan.id) })) };
  if (op.op === "repositories") {
    return plans.find(item => item.plan.id === op.plan).repo_keys
      .map(key => ({ registration: { repo_key: key } }));
  }
  if (op.op === "show") {
    const row = plans.find(item => item.plan.id === op.target);
    return { ...row, revision: { id: `${op.target}@1` }, can_edit: false, entries: [], commits: [],
      sections_without_tasks: [] };
  }
  throw new Error(`unexpected board op ${op.op}`);
}
export function responseFor(request) {
  if (request.project && !projectRecords.some(project => project.id === request.project)) {
    return { ok: false, status: 400, text: async () => JSON.stringify({
      error: { code: "invalid_options", message: `unknown project ${request.project}` },
    }) };
  }
  return { ok: true, status: 200, text: async () => JSON.stringify({
    api: request.api, result: { result: request.op.op, data: dataFor(request) },
    snapshot_seq: "100", warnings: [],
  }) };
}

export function setupProjectPage({ hash = "#/", storedProject = "", handler } = {}) {
  resetDomShim();
  globalThis.setInterval = () => 0;
  let url = new URL(`https://board.test/${hash}`);
  for (const [name, content] of [["board-api", "8"], ["board-id", "project-fixture"]]) {
    const meta = document.createElement("meta");
    meta.setAttribute("name", name); meta.content = content; document.body.append(meta);
  }
  for (const [tag, id] of [
    ["main", "main"], ["select", "project"], ["span", "project-warning"], ["span", "connection"],
    ["span", "connection-announce"], ["span", "freshness"], ["div", "notice"],
    ["button", "refresh"], ["span", "attention-count"],
  ]) {
    const node = document.createElement(tag); node.id = id;
    if (id === "main") node.setAttribute("tabindex", "-1");
    document.body.append(node);
  }
  globalThis.location = {
    get href() { return url.href; }, get hash() { return url.hash; },
    get pathname() { return url.pathname; }, get search() { return url.search; },
  };
  globalThis.history = {
    state: null,
    pushState: (_, __, next) => { url = new URL(next, url); },
    replaceState: (_, __, next) => { url = new URL(next, url); },
  };
  const listeners = new Map(); windows.push(listeners);
  globalThis.window = {
    addEventListener: (type, listener) => {
      if (!listeners.has(type)) listeners.set(type, []);
      listeners.get(type).push(listener);
    },
    scrollTo: () => {},
  };
  const calls = [];
  globalThis.fetch = async (address, options = {}) => {
    if (String(address).includes("/api/v1/events")) return new Promise(() => {});
    if (String(address).includes("/api/v1/render/")) {
      return { ok: true, status: 200, text: async () => JSON.stringify({
        api: 8, html: "", headings: [], snapshot_seq: "100",
      }) };
    }
    const request = JSON.parse(options.body); calls.push(request);
    return handler ? handler(request) : responseFor(request);
  };
  sessionStorage.setItem("trufflepig-board-token", "a".repeat(64));
  if (storedProject) {
    sessionStorage.setItem("trufflepig-board-project:project-fixture", storedProject);
  }
  return { calls, listeners, setHash: hash => { url.hash = hash; } };
}
export async function loadProjectPage() {
  await import(`../../board_web_main.js?projects=${++imports}`);
}
export async function waitFor(predicate) {
  const deadline = Date.now() + 2000;
  while (!predicate()) {
    if (Date.now() > deadline) return false;
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  return true;
}
export function chooseProject(value) {
  const select = document.getElementById("project"); select.value = value;
  for (const listener of select.listeners.get("change") || []) listener({ target: select });
}
export function textOf(node) {
  return [node.textContent || "", ...node.children.map(textOf)].join(" ");
}
export function closeProjectPages() {
  for (const listeners of windows.splice(0)) {
    for (const listener of listeners.get("pagehide") || []) listener({});
  }
}
export function restoreProjectGlobals() {
  closeProjectPages();
  for (const [name, descriptor] of originalGlobals) {
    if (descriptor) Object.defineProperty(globalThis, name, descriptor);
    else delete globalThis[name];
  }
}
