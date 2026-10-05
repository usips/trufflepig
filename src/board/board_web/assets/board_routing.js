import { decodeBoardFragment } from "./board_dom.js";

export function routeFromLocation() {
  const [path = "", queryText = ""] = (location.hash.startsWith("#/") ? location.hash.slice(2) : "").split("?");
  const query = new URLSearchParams(queryText);
  const route = Object.fromEntries([
    "tab", "q", "plan", "kind", "harness", "user", "host", "task", "after", "through", "state",
    "heading", "oid", "repo", "taskAfter", "taskCeiling", "claimAfter", "repliesAfter",
    "backrefsAfter", "own_stale", "taskThrough", "claimThrough", "repliesThrough",
    "backrefsThrough",
  ].map(key => [key, query.get(key) || ""]));
  const target = decodeBoardFragment(path);
  route.view = /^P[1-9]\d*/.test(target) ? "plan"
    : /^E[1-9]\d*$/.test(target) ? "entry"
    : target.startsWith("edit/") ? "edit" : target || "overview";
  route.ref = ["plan", "entry"].includes(route.view) ? target : route.view === "edit" ? target.slice(5) : "";
  return route;
}

export function createBoardRouting({ state, routeUrl, notice, load }) {
  function navigate(view, params = {}, replace = false) {
    history[replace ? "replaceState" : "pushState"]({}, "", routeUrl(view, params));
    state.navigation++;
    state.route = routeFromLocation();
    notice("");
    void load(true);
  }
  return { navigate };
}
