import { decodeBoardFragment } from "./board_dom.js";

const PAGE_BOUNDS = [
  "after", "through", "taskAfter", "taskCeiling", "claimAfter", "repliesAfter", "backrefsAfter",
  "taskThrough", "claimThrough", "repliesThrough", "backrefsThrough",
];

function projectSessionKey() {
  const boardId = document.querySelector('meta[name="board-id"]')?.content || "board";
  return `trufflepig-board-project:${boardId}`;
}
function saveProject(project) {
  try {
    if (project) sessionStorage.setItem(projectSessionKey(), project);
    else sessionStorage.removeItem(projectSessionKey());
  } catch (_) { /* The URL still carries the selection. */ }
}
function savedProject() {
  try { return sessionStorage.getItem(projectSessionKey()) || ""; }
  catch (_) { return ""; }
}

export function boardReadScope(project) { return project === "unscoped" ? "unscoped" : "all"; }
export function boardProjectSelector(project) {
  return project && project !== "unscoped" ? project : undefined;
}

export function routeFromLocation() {
  const [path = "", queryText = ""] = (location.hash.startsWith("#/") ? location.hash.slice(2) : "").split("?");
  const query = new URLSearchParams(queryText);
  const route = Object.fromEntries([
    "tab", "q", "plan", "kind", "harness", "user", "host", "task", "after", "through", "state",
    "heading", "oid", "repo", "taskAfter", "taskCeiling", "claimAfter", "repliesAfter",
    "backrefsAfter", "own_stale", "taskThrough", "claimThrough", "repliesThrough",
    "backrefsThrough", "project",
  ].map(key => [key, query.get(key) || ""]));
  const launchQuery = new URLSearchParams(location.search || "");
  route.project = query.has("project") ? query.get("project")
    : launchQuery.has("project") ? launchQuery.get("project") : savedProject();
  saveProject(route.project);
  const target = decodeBoardFragment(path);
  route.view = /^P[1-9]\d*/.test(target) ? "plan"
    : /^E[1-9]\d*$/.test(target) ? "entry"
    : target.startsWith("edit/") ? "edit" : target || "overview";
  route.ref = ["plan", "entry"].includes(route.view) ? target : route.view === "edit" ? target.slice(5) : "";
  return route;
}

export function createBoardRouting({ state, routeUrl, notice, load }) {
  function updateProjectSelector(projects) {
    const select = document.getElementById("project");
    if (!select) return;
    const records = projects || [], selected = state.route.project || "";
    const option = (value, label) => {
      const item = document.createElement("option");
      item.value = value; item.textContent = label; return item;
    };
    select.replaceChildren(option("", "All"), ...records.map(project =>
      option(project.id, project.name)), option("unscoped", "Unscoped"));
    const project = records.find(item => item.id === selected);
    const unknown = Boolean(boardProjectSelector(selected) && !project);
    if (unknown) select.append(option(selected, selected));
    select.value = selected;
    const warning = document.getElementById("project-warning");
    if (warning) {
      const unavailable = project?.unavailable || [];
      warning.hidden = !projects || (!unavailable.length && !unknown);
      warning.textContent = unknown ? "Project unavailable" : "Unavailable members";
      warning.title = unavailable.map(member => member.name).join(", ");
    }
  }
  function adoptRoute(next) {
    const changed = (next.project || "") !== (state.route.project || "");
    if (changed) {
      for (const key of PAGE_BOUNDS) next[key] = "";
      state.generation++;
      state.request?.abort(); state.globalRefresh?.abort(); state.globalRefresh = null;
      state.overview = null; state.attention = null;
      const count = document.getElementById("attention-count");
      if (count) { count.textContent = ""; count.hidden = true; }
    }
    state.route = next; saveProject(next.project || "");
    if (next.project || changed) history.replaceState(history.state, "", routeUrl(next.view, next));
    updateProjectSelector(state.projects);
  }
  function navigate(view, params = {}, replace = false) {
    const project = Object.hasOwn(params, "project") ? params.project : state.route.project;
    const fields = { ...params, project: project || "" };
    if ((project || "") !== (state.route.project || "")) {
      for (const key of PAGE_BOUNDS) delete fields[key];
    }
    saveProject(project || "");
    history[replace ? "replaceState" : "pushState"]({}, "", routeUrl(view, fields));
    state.navigation++;
    adoptRoute(routeFromLocation());
    notice("");
    void load(true);
  }
  document.getElementById("project")?.addEventListener("change", event => {
    navigate(state.route.view, { ...state.route, project: event.target.value });
  });
  updateProjectSelector(state.projects);
  if (state.route.project) {
    history.replaceState(history.state, "", routeUrl(state.route.view, state.route));
  }
  return { navigate, adoptRoute, updateProjectSelector };
}
